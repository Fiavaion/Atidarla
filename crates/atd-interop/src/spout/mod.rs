//! Windows: Spout 2.007 senders and receivers, the protocol layer under the
//! IMG Share Out / Share In operators (PRD §9 M1).
//!
//! A [`SpoutSender`] publishes a D3D11 texture under a name the way the
//! Spout SDK does, so any Spout receiver can find and open it; a
//! [`SpoutReceiver`] opens another sender's texture. Hand-off with wgpu
//! goes through a [`SharedTexture`] (D3D12, NT handle) opened on a D3D11
//! device on the same adapter: sending copies it into the Spout texture,
//! receiving copies the sender's texture into it. Nothing goes through the
//! CPU.
//!
//! # Protocol
//! - **Sender list:** shared memory `SpoutSenderNames`, `MaxSenders` slots
//!   (`HKCU\Software\Leading Edge\Spout`, default 64) of 256 bytes: sorted,
//!   NUL-terminated names, ended by an empty slot. A taken name gets `_1`,
//!   `_2`, ... The newest sender becomes the active one, whose name is in
//!   the 256-byte map `ActiveSenderName`.
//! - **Sender info:** a 280-byte map named after the sender: 32-bit share
//!   handle, width, height, `DXGI_FORMAT`, usage, a 256-byte description
//!   (the executable path) and a partner id.
//! - Every map is guarded by a named mutex `<map>_mutex`.
//! - **Texture:** D3D11, `MISC_SHARED` (legacy handle), one mip, one
//!   sample, render target and shader resource; receivers open it with
//!   `OpenSharedResource`. A resize makes a new texture and handle.
//! - **Frame sync:** `<sender>_SpoutAccessMutex` is held while copying in or
//!   out, and the semaphore `<sender>_Count_Semaphore` counts frames. Our
//!   sender always counts and our receiver always reads the count; the SDK
//!   does both only when the registry value `Framecount` is 1, and a
//!   receiver that finds no count treats every frame as new.
//! - **Not implemented** (opt-in in the SDK, off by default): keyed-mutex
//!   and NT-handle sender textures, the `<sender>_Sync_Event` frame-sync
//!   event, CPU/memory-share senders (share handle 0, seen here as no
//!   sender), DX9 senders' non-DXGI formats, automatic switching to the
//!   sender's adapter, and cleaning stale names left by crashed senders.
//!
//! # GPU synchronisation
//! Between wgpu (D3D12) and our D3D11 device: a shared fence. The wgpu
//! queue signals it, D3D11 waits on it, copies, signals it and flushes, and
//! the wgpu queue waits on that before later work. Both waits are on the
//! GPU, so no call blocks on the GPU (unlike `Device::poll`, which the
//! spike tests use). Between our D3D11 device and other Spout processes:
//! what the SDK does, `Flush` after each copy while holding the access
//! mutex; legacy shared textures rely on the driver's implicit
//! synchronisation for the rest.
//!
//! # Blocking (CLAUDE.md rule 1)
//! - Every map access waits up to 67 ms for the map's mutex, as the SDK
//!   does: [`SpoutSender::new`], [`SpoutSender::resize`], dropping a sender,
//!   [`SpoutReceiver::list_senders`], [`SpoutReceiver::active_sender`], and
//!   [`SpoutReceiver::receive_into_shared`], which reads the sender's info
//!   map on every call.
//! - [`SpoutSender::send_wgpu`] and [`SpoutReceiver::receive_into_shared`]
//!   wait up to 67 ms for the access mutex if another process holds it,
//!   then skip the frame. Holders keep it for microseconds (a copy and a
//!   flush), so long waits mean a stuck peer.
//! - Creating a sender or receiver creates a D3D11 device (tens of ms):
//!   do it off the render thread.
//! - After warm-up the per-frame calls allocate nothing (CLAUDE.md rule 2).
//!
//! # Credits
//! The protocol follows the Spout2 SDK by Lynn Jarvis (BSD-2-Clause,
//! <https://github.com/leadedge/Spout2>), read at commit 6b68a5b:
//! `SpoutSharedMemory.cpp` (Malcolm Bechard), `SpoutSenderNames.cpp`,
//! `SpoutFrameCount.cpp`, `SpoutDirectX.cpp` and `SpoutDX.cpp`. This is an
//! independent Rust implementation; no SDK code is included.

mod d3d11;
mod shm;

use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};

use crate::InteropError;
use crate::dx12::{SharedTexture, dxgi_format};
use d3d11::{D3D11, wgpu_format};
use shm::{
    FrameSync, InfoMap, NAME_LEN, SenderList, TextureInfo, active_name, check_name, list_names,
    read_info,
};

/// The executable path for the info map's description, NUL-terminated.
fn exe_path() -> [u8; 256] {
    let mut out = [0; 256];
    if let Ok(path) = std::env::current_exe() {
        let path = path.to_string_lossy();
        let n = path.len().min(255);
        out[..n].copy_from_slice(&path.as_bytes()[..n]);
    }
    out
}

/// A Spout sender: a named, shared D3D11 texture that [`send_wgpu`]
/// fills from wgpu. Dropping it removes the name from the sender list.
///
/// [`send_wgpu`]: Self::send_wgpu
#[derive(Debug)]
pub struct SpoutSender {
    name: String,
    list: SenderList,
    info: InfoMap,
    sync: FrameSync,
    d3d: D3D11,
    texture: ID3D11Texture2D,
    value: TextureInfo,
    format: wgpu::TextureFormat,
    /// The last source and its D3D11 view. Holding the wgpu texture keeps
    /// its identity from being reused while the view is cached.
    source: Option<(wgpu::Texture, ID3D11Texture2D)>,
}

impl SpoutSender {
    /// Announces a sender called `name` (`name_1`, `name_2`, ... if taken;
    /// see [`name`](Self::name)) with a `width` × `height` texture in one of
    /// the [`SHAREABLE_FORMATS`](crate::SHAREABLE_FORMATS), on the adapter of
    /// the DX12 `device`, and makes it the active sender. Blocks for D3D11
    /// device creation and up to 67 ms per map mutex.
    pub fn new(
        device: &wgpu::Device,
        name: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Self, InteropError> {
        check_name(name.as_bytes())?;
        let dxgi = dxgi_format(format)?;
        let d3d = D3D11::new(device)?;
        let (texture, share_handle) = d3d.create_shared(width, height, dxgi)?;
        let value = TextureInfo {
            share_handle,
            width,
            height,
            format: dxgi.0 as u32,
            usage: 0,
            description: exe_path(),
            partner_id: 0,
        };
        let mut list = SenderList::open_or_create()?;
        let (name, (info, sync)) = list.register(name.as_bytes(), |name| {
            let mut info = InfoMap::create(name)?;
            info.write(&value)?;
            Ok((info, FrameSync::open(name)?))
        })?;
        Ok(Self {
            name: String::from_utf8_lossy(&name).into_owned(),
            list,
            info,
            sync,
            d3d,
            texture,
            value,
            format,
            source: None,
        })
    }

    /// The registered name, after any `_n` renaming.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The shared texture's width and height.
    pub fn size(&self) -> (u32, u32) {
        (self.value.width, self.value.height)
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Replaces the shared texture with one of a new size or format and
    /// publishes its handle; receivers notice the change and reopen it.
    /// [`send_wgpu`](Self::send_wgpu) calls this when the source changes.
    pub fn resize(
        &mut self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<(), InteropError> {
        let dxgi = dxgi_format(format)?;
        let (texture, share_handle) = self.d3d.create_shared(width, height, dxgi)?;
        let value = TextureInfo {
            share_handle,
            width,
            height,
            format: dxgi.0 as u32,
            ..self.value
        };
        self.info.write(&value)?;
        self.value = value;
        self.format = format;
        self.texture = texture;
        // Let the old texture go now rather than at the next copy.
        self.d3d.flush();
        Ok(())
    }

    /// Copies `source` into the Spout texture and counts a frame, resizing
    /// first if `source`'s size or format differs. Submits `queue` (so
    /// pending `write_texture` calls land); the copy runs on the GPU after
    /// everything submitted so far, and later work on `queue` waits for it.
    /// `queue` must belong to the device given to [`new`](Self::new).
    ///
    /// Returns `false` if the frame was skipped because a receiver held the
    /// texture for 67 ms. Does not wait for the GPU.
    pub fn send_wgpu(
        &mut self,
        queue: &wgpu::Queue,
        source: &SharedTexture,
    ) -> Result<bool, InteropError> {
        let size = source.texture.size();
        let format = source.texture.format();
        if (size.width, size.height, format) != (self.value.width, self.value.height, self.format) {
            self.resize(size.width, size.height, format)?;
        }
        if self
            .source
            .as_ref()
            .is_none_or(|(texture, _)| *texture != source.texture)
        {
            let opened = self.d3d.open_nt(source.handle.raw())?;
            self.source = Some((source.texture.clone(), opened));
        }
        let Some((_, src)) = &self.source else {
            unreachable!("opened above");
        };
        queue.submit([]);
        let Some(_access) = self.sync.lock() else {
            return Ok(false);
        };
        self.d3d.copy(queue, &self.texture, src)?;
        self.sync.count_frame();
        Ok(true)
    }
}

impl Drop for SpoutSender {
    fn drop(&mut self) {
        self.list.release(self.name.as_bytes());
    }
}

/// What [`SpoutReceiver::receive_into_shared`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    /// No sender to receive from: none is active, the named one is not
    /// running (or has gone), or it shares through CPU memory, which is not
    /// supported. The target is untouched.
    NoSender,
    /// The sender's texture differs from the target in size or format.
    /// Create a target like this and call again; the target is untouched.
    Resize {
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    },
    /// A new frame was copied into the target.
    NewFrame,
    /// The sender has not counted a frame since the last copy, or another
    /// process held its texture for 67 ms. The target keeps the last frame.
    NoNewFrame,
}

/// The sender a receiver is attached to.
#[derive(Debug)]
struct Connection {
    name: Vec<u8>,
    sync: FrameSync,
    handle: u32,
    /// The sender's texture, `None` if its handle would not open.
    texture: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
    last_frame: i32,
    /// Copy on the next call even if the frame count has not moved.
    fresh: bool,
}

/// A Spout receiver: copies a sender's texture into a [`SharedTexture`].
/// Survives the sender going away, resizing or changing format.
#[derive(Debug)]
pub struct SpoutReceiver {
    d3d: D3D11,
    /// The sender asked for; `None` follows the active sender.
    wanted: Option<Vec<u8>>,
    conn: Option<Connection>,
    /// The last target and its D3D11 view (see [`SpoutSender`]'s source).
    target: Option<(wgpu::Texture, ID3D11Texture2D)>,
}

impl SpoutReceiver {
    /// The names in the sender list, sorted. Waits up to 67 ms for its mutex.
    pub fn list_senders() -> Vec<String> {
        list_names()
            .into_iter()
            .map(|n| String::from_utf8_lossy(&n).into_owned())
            .collect()
    }

    /// The active sender, if it is running. Waits up to 67 ms per map.
    pub fn active_sender() -> Option<String> {
        let mut buf = [0; NAME_LEN];
        let len = active_name(&mut buf)?;
        read_info(&buf[..len])?;
        Some(String::from_utf8_lossy(&buf[..len]).into_owned())
    }

    /// A receiver for the sender called `sender`, or with `None` for the
    /// active sender (resolved when not attached, then kept until that
    /// sender goes). The sender need not be running yet. The D3D11 device
    /// is created on the adapter of the DX12 `device`.
    pub fn connect(device: &wgpu::Device, sender: Option<&str>) -> Result<Self, InteropError> {
        if let Some(name) = sender {
            check_name(name.as_bytes())?;
        }
        Ok(Self {
            d3d: D3D11::new(device)?,
            wanted: sender.map(|n| n.as_bytes().to_vec()),
            conn: None,
            target: None,
        })
    }

    /// The sender currently attached to, if any.
    pub fn sender_name(&self) -> Option<String> {
        self.conn
            .as_ref()
            .map(|c| String::from_utf8_lossy(&c.name).into_owned())
    }

    /// Copies the sender's latest frame into `target` if there is a new
    /// one; see [`Received`]. The copy runs on the GPU after everything
    /// submitted to `queue` so far, and later work on `queue` waits for it.
    /// `queue` must belong to the device given to [`connect`](Self::connect).
    ///
    /// Reads the sender's info map on every call, as the SDK does (up to
    /// 67 ms per mutex). Does not wait for the GPU.
    pub fn receive_into_shared(
        &mut self,
        queue: &wgpu::Queue,
        target: &SharedTexture,
    ) -> Result<Received, InteropError> {
        let mut buf = [0; NAME_LEN];
        let len = match (&self.wanted, &self.conn) {
            (Some(name), _) | (None, Some(Connection { name, .. })) => {
                buf[..name.len()].copy_from_slice(name);
                name.len()
            }
            (None, None) => match active_name(&mut buf) {
                Some(len) => len,
                None => return Ok(Received::NoSender),
            },
        };
        let name = &buf[..len];
        let Some(info) = read_info(name).filter(|i| i.share_handle != 0) else {
            self.conn = None;
            return Ok(Received::NoSender);
        };

        if self.conn.as_ref().is_none_or(|c| c.name != name) {
            self.conn = Some(Connection {
                name: name.to_vec(),
                sync: FrameSync::open(name)?,
                handle: 0,
                texture: None,
                last_frame: 0,
                fresh: true,
            });
        }
        let Some(conn) = self.conn.as_mut() else {
            unreachable!("connected above");
        };
        if conn.handle != info.share_handle {
            // A new sender, or a resize or format change: a new texture.
            conn.handle = info.share_handle;
            conn.texture = None;
            conn.fresh = true;
            conn.texture = Some(self.d3d.open_legacy(info.share_handle)?);
        }
        let Some((src, desc)) = &conn.texture else {
            return Err(InteropError::Platform(
                "the Spout sender's texture handle does not open on this adapter".into(),
            ));
        };
        let format = wgpu_format(desc.Format).ok_or_else(|| {
            InteropError::Platform(format!(
                "unsupported Spout sender format (DXGI_FORMAT {})",
                desc.Format.0
            ))
        })?;
        let size = target.texture.size();
        if (size.width, size.height, target.texture.format()) != (desc.Width, desc.Height, format) {
            return Ok(Received::Resize {
                width: desc.Width,
                height: desc.Height,
                format,
            });
        }

        if self
            .target
            .as_ref()
            .is_none_or(|(texture, _)| *texture != target.texture)
        {
            let opened = self.d3d.open_nt(target.handle.raw())?;
            self.target = Some((target.texture.clone(), opened));
            conn.fresh = true;
        }
        let Some((_, dst)) = &self.target else {
            unreachable!("opened above");
        };

        let Some(_access) = conn.sync.lock() else {
            return Ok(Received::NoNewFrame);
        };
        // 0: the sender does not count frames, so every frame is new.
        let count = conn.sync.frame_count();
        if !conn.fresh && count != 0 && count == conn.last_frame {
            return Ok(Received::NoNewFrame);
        }
        self.d3d.copy(queue, dst, src)?;
        conn.last_frame = count;
        conn.fresh = false;
        Ok(Received::NewFrame)
    }
}
