//! macOS: Syphon servers and clients, the protocol layer under the IMG
//! Share Out / Share In operators (PRD §9 M1).
//!
//! Unlike [`spout`](crate) on Windows, this is not a reimplementation: it
//! drives the official Syphon framework (`SyphonMetalServer`,
//! `SyphonMetalClient`, `SyphonServerDirectory`), built from source by
//! `build.rs` and loaded at run time (see [`load`]), so any Syphon app sees
//! exactly what the framework publishes.
//!
//! - A [`SyphonServer`] publishes a wgpu texture under a name. Syphon
//!   copies it into its own IOSurface on a command buffer we commit on the
//!   wgpu queue's `MTLCommandQueue`, so the copy runs on the GPU after
//!   everything submitted to the queue so far and before anything later.
//!   Syphon announces the frame once that command buffer has completed.
//! - A [`SyphonReceiver`] opens a server's IOSurface (the framework wraps it
//!   as an `MTLTexture`) and blits it into a wgpu texture, again on a
//!   command buffer on the wgpu queue. Syphon frames are 8-bit BGRA.
//!
//! No call waits for the GPU and nothing goes through the CPU.
//!
//! # Finding servers
//! Syphon's directory learns about servers from distributed notifications,
//! which macOS delivers only on the **main thread's run loop**. The app's
//! event loop runs it; a headless process (the CLI, tests) sees no other
//! process's servers unless its main thread runs [`run_main_loop`]. Servers
//! created in this process are also kept in a process-local list, so a
//! receiver finds them by name either way (a Share Out → Share In loopback).
//!
//! # Blocking (CLAUDE.md rule 1)
//! Creating a server or client, publishing and receiving are Objective-C
//! calls that take Syphon's locks and send mach messages to other
//! processes; they normally return in microseconds but a stuck peer can
//! hold them. Dropping a server waits up to 100 ms for clients that are
//! leaving (see its `Drop`). Make all of them off the render thread (the
//! operators use a worker thread per node). Loading the framework (first
//! call) maps a dylib.
//!
//! # Credits
//! Syphon by bangnoise (Tom Butterworth) and vade (Anton Marini), Metal
//! classes with Maxime Touroute and Philippe Chaurand (BSD-3-Clause,
//! <https://github.com/Syphon/Syphon-Framework>; licence text in
//! `third-party/Syphon/License.txt`).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{Message, msg_send};
use objc2_core_foundation::{CFRunLoop, CGPoint, CGRect, CGSize, kCFRunLoopDefaultMode};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLDevice,
    MTLPixelFormat, MTLTexture,
};
use wgpu::hal::api::Metal;

use crate::InteropError;

/// The format of every Syphon frame.
pub const SYPHON_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

const KEY_UUID: &str = "SyphonServerDescriptionUUIDKey";
const KEY_NAME: &str = "SyphonServerDescriptionNameKey";
const KEY_APP: &str = "SyphonServerDescriptionAppNameKey";

type Object = Retained<AnyObject>;
type Description = Retained<NSDictionary<NSString, AnyObject>>;
type MtlTexture = Retained<ProtocolObject<dyn MTLTexture>>;
type MtlQueue = Retained<ProtocolObject<dyn MTLCommandQueue>>;
type MtlDevice = Retained<ProtocolObject<dyn MTLDevice>>;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 0x2;
const RTLD_LOCAL: c_int = 0x4;

struct Classes {
    server: &'static AnyClass,
    client: &'static AnyClass,
    directory: &'static AnyClass,
}

/// Where the framework is looked for, in order: `LSR_SYPHON_FRAMEWORK` at
/// run time, `Contents/Frameworks` of an app bundle, beside the
/// executable, and where `build.rs` built it.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("LSR_SYPHON_FRAMEWORK") {
        out.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    {
        out.push(dir.join("../Frameworks/Syphon.framework"));
        out.push(dir.join("Syphon.framework"));
    }
    out.push(PathBuf::from(env!("LSR_SYPHON_FRAMEWORK")));
    out
}

fn open_framework() -> Result<Classes, String> {
    let mut tried = Vec::new();
    for framework in candidates() {
        let binary = framework.join("Syphon");
        if !binary.exists() {
            continue;
        }
        let path = CString::new(binary.as_os_str().as_encoded_bytes())
            .map_err(|_| format!("bad path {}", binary.display()))?;
        // SAFETY: a valid C string; the image stays loaded for the process.
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: dlerror returns a C string or null after a failure.
            let err = unsafe { dlerror() };
            let why = if err.is_null() {
                "unknown error".into()
            } else {
                unsafe { CStr::from_ptr(err) }
                    .to_string_lossy()
                    .into_owned()
            };
            tried.push(format!("{}: {why}", binary.display()));
            continue;
        }
        let class = |name: &CStr| {
            AnyClass::get(name).ok_or_else(|| format!("{} has no class {name:?}", binary.display()))
        };
        return Ok(Classes {
            server: class(c"SyphonMetalServer")?,
            client: class(c"SyphonMetalClient")?,
            directory: class(c"SyphonServerDirectory")?,
        });
    }
    Err(if tried.is_empty() {
        "Syphon.framework was not found (set LSR_SYPHON_FRAMEWORK to its path)".into()
    } else {
        format!("Syphon.framework would not load: {}", tried.join("; "))
    })
}

fn classes() -> Result<&'static Classes, InteropError> {
    static CLASSES: OnceLock<Result<Classes, String>> = OnceLock::new();
    CLASSES
        .get_or_init(open_framework)
        .as_ref()
        .map_err(|e| InteropError::Platform(e.clone()))
}

/// Loads the Syphon framework if it is not loaded yet. Every other call
/// does this itself; call it early to report a missing framework.
pub fn load() -> Result<(), InteropError> {
    classes().map(|_| ())
}

/// Runs the calling thread's run loop for `seconds`. On the main thread
/// this lets Syphon's directory hear other processes' servers (see the
/// module docs); the app's event loop does it already.
pub fn run_main_loop(seconds: f64) {
    // SAFETY: the default mode constant is a valid CFString.
    let mode = unsafe { kCFRunLoopDefaultMode };
    CFRunLoop::run_in_mode(mode, seconds, false);
}

fn metal_device(device: &wgpu::Device) -> Result<MtlDevice, InteropError> {
    // SAFETY: the raw device is only retained, to create Metal objects on.
    let hal = unsafe { device.as_hal::<Metal>() }.ok_or(InteropError::WrongBackend("Metal"))?;
    Ok(hal.raw_device().clone())
}

fn metal_queue(queue: &wgpu::Queue) -> Result<MtlQueue, InteropError> {
    // SAFETY: the raw queue is only used to create and commit command
    // buffers, which MTLCommandQueue allows from any thread.
    let hal = unsafe { queue.as_hal::<Metal>() }.ok_or(InteropError::WrongBackend("Metal"))?;
    Ok(hal.as_raw().retain())
}

fn metal_texture(texture: &wgpu::Texture) -> Result<MtlTexture, InteropError> {
    // SAFETY: the raw texture is only read or written by command buffers on
    // the same device's queue, ordered with wgpu's own work.
    let hal = unsafe { texture.as_hal::<Metal>() }.ok_or(InteropError::WrongBackend("Metal"))?;
    Ok(hal.raw_handle().retain())
}

/// The cached Metal texture behind a wgpu texture; holding the wgpu
/// texture keeps its identity from being reused while it is cached.
#[derive(Debug, Default)]
struct RawCache(Option<(wgpu::Texture, MtlTexture)>);

impl RawCache {
    fn get(
        &mut self,
        texture: &wgpu::Texture,
    ) -> Result<&ProtocolObject<dyn MTLTexture>, InteropError> {
        if self.0.as_ref().is_none_or(|(t, _)| t != texture) {
            self.0 = None;
            self.0 = Some((texture.clone(), metal_texture(texture)?));
        }
        Ok(&self.0.as_ref().expect("cached above").1)
    }
}

fn command_buffer(
    queue: &MtlQueue,
) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, InteropError> {
    queue
        .commandBuffer()
        .ok_or_else(|| InteropError::Gpu("could not create a Metal command buffer".into()))
}

fn string_for(desc: &NSDictionary<NSString, AnyObject>, key: &str) -> String {
    desc.objectForKey(&NSString::from_str(key))
        .and_then(|v| v.downcast::<NSString>().ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// A server as the directory describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    /// The server's name; may be empty or shared by several servers.
    pub name: String,
    /// The name of the app publishing it.
    pub app_name: String,
    /// Unique per server.
    pub uuid: String,
}

impl ServerInfo {
    fn from_description(desc: &NSDictionary<NSString, AnyObject>) -> Self {
        Self {
            name: string_for(desc, KEY_NAME),
            app_name: string_for(desc, KEY_APP),
            uuid: string_for(desc, KEY_UUID),
        }
    }

    /// What a user picks the server by: its name, or its app's name when
    /// the name is empty.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.app_name
        } else {
            &self.name
        }
    }

    /// True if `wanted` names this server: its [`label`](Self::label), or
    /// "app - name" as Syphon apps list it.
    pub fn matches(&self, wanted: &str) -> bool {
        wanted == self.label()
            || wanted
                .strip_prefix(self.app_name.as_str())
                .and_then(|rest| rest.strip_prefix(" - "))
                == Some(self.name.as_str())
    }
}

/// A server created in this process.
struct Local {
    info: ServerInfo,
    description: Description,
    /// Frames whose copy into the server's surface has completed on the GPU.
    done: Arc<AtomicU64>,
}
// SAFETY: the description is an immutable NSDictionary of immutable
// values, documented as safe to use from any thread.
unsafe impl Send for Local {}

/// Servers created in this process, oldest first.
static LOCAL: Mutex<Vec<Local>> = Mutex::new(Vec::new());

fn local() -> std::sync::MutexGuard<'static, Vec<Local>> {
    LOCAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The directory's servers, oldest first. Without a main run loop (see the
/// module docs) this is empty, or stale.
fn directory() -> Result<Vec<Description>, InteropError> {
    let classes = classes()?;
    Ok(autoreleasepool(|_| {
        // SAFETY: `sharedDirectory` and `servers` take no arguments and
        // return a singleton and an NSArray of NSDictionary.
        let dir: Object = unsafe { msg_send![classes.directory, sharedDirectory] };
        let servers: Retained<NSArray<NSDictionary<NSString, AnyObject>>> =
            unsafe { msg_send![&dir, servers] };
        servers.to_vec()
    }))
}

/// Every server visible from this process, oldest first: the directory's
/// and this process's own, each once.
pub fn list_servers() -> Result<Vec<ServerInfo>, InteropError> {
    let mut out: Vec<ServerInfo> = directory()?
        .iter()
        .map(|d| ServerInfo::from_description(d))
        .collect();
    for l in local().iter() {
        if !out.iter().any(|o| o.uuid == l.info.uuid) {
            out.push(l.info.clone());
        }
    }
    Ok(out)
}

/// The servers the directory knows about (other processes' and ours), for
/// checks of the directory itself; see [`list_servers`].
pub fn directory_servers() -> Result<Vec<ServerInfo>, InteropError> {
    Ok(directory()?
        .iter()
        .map(|d| ServerInfo::from_description(d))
        .collect())
}

/// A server found by [`find`].
struct Found {
    info: ServerInfo,
    description: Description,
    /// Its completed-frame count, for a server in this process.
    done: Option<Arc<AtomicU64>>,
}

/// The newest server matching `wanted` (any server for `None`).
fn find(wanted: Option<&str>) -> Result<Option<Found>, InteropError> {
    let matches = |info: &ServerInfo| wanted.is_none_or(|w| info.matches(w));
    if let Some(l) = local().iter().rev().find(|l| matches(&l.info)) {
        return Ok(Some(Found {
            info: l.info.clone(),
            description: l.description.clone(),
            done: Some(Arc::clone(&l.done)),
        }));
    }
    Ok(directory()?.into_iter().rev().find_map(|d| {
        let info = ServerInfo::from_description(&d);
        matches(&info).then_some(Found {
            info,
            description: d,
            done: None,
        })
    }))
}

fn check_name(name: &str) -> Result<(), InteropError> {
    if name.len() > 255 || name.contains('\0') {
        return Err(InteropError::Platform(
            "a Syphon server name is at most 255 bytes, with no NUL".into(),
        ));
    }
    Ok(())
}

/// A Syphon server. Dropping it stops it (clients see it go).
pub struct SyphonServer {
    server: Object,
    /// The wgpu queue, submitted before each publish, and its Metal queue.
    wgpu_queue: wgpu::Queue,
    queue: MtlQueue,
    info: ServerInfo,
    source: RawCache,
    /// Frames whose copy has completed (shared with [`LOCAL`]).
    done: Arc<AtomicU64>,
}

// SAFETY: SyphonMetalServer "is safe to access across threads" (its
// header); the Metal queue is thread-safe and the cached texture is only
// used through command buffers.
unsafe impl Send for SyphonServer {}

impl std::fmt::Debug for SyphonServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyphonServer")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl SyphonServer {
    /// Starts a server called `name` (names need not be unique) on the
    /// Metal device behind `device`; `queue` is that device's queue, on
    /// which [`publish`](Self::publish) copies frames.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: &str,
    ) -> Result<Self, InteropError> {
        check_name(name)?;
        let classes = classes()?;
        let mtl = metal_device(device)?;
        let wgpu_queue = queue.clone();
        let queue = metal_queue(queue)?;
        let (server, desc) = autoreleasepool(|_| {
            let name = NSString::from_str(name);
            // SAFETY: `initWithName:device:options:` takes an NSString, an
            // MTLDevice and an optional NSDictionary, and returns nil on
            // failure.
            let server: Option<Object> = unsafe {
                let alloc: Allocated<AnyObject> = msg_send![classes.server, alloc];
                msg_send![alloc, initWithName: &*name, device: &*mtl, options: None::<&AnyObject>]
            };
            let server = server.ok_or_else(|| {
                InteropError::Platform("SyphonMetalServer would not start".into())
            })?;
            // SAFETY: returns an NSDictionary<NSString, id>.
            let desc: Description = unsafe { msg_send![&server, serverDescription] };
            Ok::<_, InteropError>((server, desc))
        })?;
        let info = ServerInfo::from_description(&desc);
        let done = Arc::new(AtomicU64::new(0));
        local().push(Local {
            info: info.clone(),
            description: desc,
            done: Arc::clone(&done),
        });
        Ok(Self {
            server,
            wgpu_queue,
            queue,
            info,
            source: RawCache::default(),
            done,
        })
    }

    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// True while at least one client is attached.
    pub fn has_clients(&self) -> bool {
        // SAFETY: a BOOL property.
        let has: Bool = unsafe { msg_send![&self.server, hasClients] };
        has.as_bool()
    }

    /// Publishes `source` (8-bit RGBA or BGRA, from the device given to
    /// [`new`](Self::new)). Submits the queue (so pending `write_texture`
    /// calls land); Syphon then copies `source` into its surface on the GPU,
    /// after everything submitted so far, and announces the frame when the
    /// copy is done. `source` may be overwritten by work submitted after
    /// this returns. Does not wait for the GPU.
    pub fn publish(&mut self, source: &wgpu::Texture) -> Result<(), InteropError> {
        crate::check_format(source.format())?;
        let size = source.size();
        let region = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(f64::from(size.width), f64::from(size.height)),
        );
        let texture = self.source.get(source)?;
        self.wgpu_queue.submit([]);
        autoreleasepool(|_| {
            let cb = command_buffer(&self.queue)?;
            // SAFETY: an MTLTexture, an MTLCommandBuffer, an NSRect and a
            // BOOL; the texture is on the server's device. Not flipped:
            // wgpu and Metal share a top-left origin.
            unsafe {
                let () = msg_send![
                    &self.server,
                    publishFrameTexture: texture,
                    onCommandBuffer: &*cb,
                    imageRegion: region,
                    flipped: Bool::NO
                ];
            }
            let done = Arc::clone(&self.done);
            let handler = RcBlock::new(move |_cb: *mut AnyObject| {
                done.fetch_add(1, Ordering::Release);
            });
            // SAFETY: takes a block called with the command buffer once it
            // has completed; Metal copies the block.
            unsafe {
                let () = msg_send![&*cb, addCompletedHandler: &*handler];
            }
            cb.commit();
            Ok(())
        })
    }
}

/// How long dropping a server waits for clients that are leaving.
const LEAVE_WAIT: Duration = Duration::from_millis(100);

impl Drop for SyphonServer {
    /// Waits up to [`LEAVE_WAIT`] for attached clients to leave, then stops
    /// the server. The wait works around a use-after-free in Syphon (seen
    /// at commit f476167): a client's "remove me" message still queued when
    /// the server stops is delivered to the server's freed message receiver
    /// and crashes the process. Our loopback tests, running in parallel,
    /// hit it when a client in this process stopped just before its server
    /// (as a Share In and Share Out loopback closing together would).
    /// `hasClients` turns false only after the server has handled the
    /// client's last message.
    fn drop(&mut self) {
        local().retain(|l| l.info.uuid != self.info.uuid);
        let start = Instant::now();
        while self.has_clients() && start.elapsed() < LEAVE_WAIT {
            std::thread::sleep(Duration::from_millis(1));
        }
        // SAFETY: `stop` takes no arguments.
        unsafe {
            let () = msg_send![&self.server, stop];
        }
    }
}

/// What [`SyphonReceiver::receive_into`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    /// No server to receive from: none matches, or it has gone. The target
    /// is untouched.
    NoSender,
    /// The server's frames differ from the target in size or format.
    /// Create a target like this and call again; the target is untouched.
    Resize {
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    },
    /// A new frame was copied into the target.
    NewFrame,
    /// No frame since the last copy (or none published yet).
    NoNewFrame,
}

struct Connection {
    info: ServerInfo,
    client: Object,
    /// Frames the server has announced since we attached, counted by the
    /// client's frame handler once each frame's GPU copy has completed.
    published: Arc<AtomicU64>,
    /// `published` when we last copied.
    copied: u64,
    /// Copy on the next call even if no frame was announced since the last
    /// copy: after attaching (the server may have published before), after
    /// a resize, or for a new target.
    fresh: bool,
    /// For a server in this process, its completed frames: until the first
    /// has completed its surface may exist but hold nothing yet. Another
    /// process's server is taken at its word (Syphon shows a server's
    /// surface as soon as it exists).
    done: Option<Arc<AtomicU64>>,
}

/// A Syphon client that copies a server's frames into wgpu textures.
/// Survives the server going away, resizing, and coming back.
pub struct SyphonReceiver {
    device: MtlDevice,
    queue: MtlQueue,
    /// The server asked for; `None` takes the newest.
    wanted: Option<String>,
    conn: Option<Connection>,
    target: RawCache,
}

// SAFETY: Syphon clients may be used from any thread (their frame handler
// already runs on Syphon's own queue); the Metal objects are thread-safe.
unsafe impl Send for SyphonReceiver {}

impl std::fmt::Debug for SyphonReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyphonReceiver")
            .field("wanted", &self.wanted)
            .field("server", &self.conn.as_ref().map(|c| &c.info))
            .finish_non_exhaustive()
    }
}

impl SyphonReceiver {
    /// A receiver for the server `server` names (see
    /// [`ServerInfo::matches`]), or with `None` for the newest server
    /// (resolved when not attached, then kept until it goes). The server
    /// need not be running yet.
    pub fn connect(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        server: Option<&str>,
    ) -> Result<Self, InteropError> {
        if let Some(name) = server {
            check_name(name)?;
        }
        classes()?;
        Ok(Self {
            device: metal_device(device)?,
            queue: metal_queue(queue)?,
            wanted: server.map(str::to_owned),
            conn: None,
            target: RawCache::default(),
        })
    }

    /// The server attached to, if any.
    pub fn server(&self) -> Option<&ServerInfo> {
        self.conn.as_ref().map(|c| &c.info)
    }

    fn attach(&mut self) -> Result<bool, InteropError> {
        if let Some(conn) = &self.conn {
            // SAFETY: a BOOL property.
            let valid: Bool = unsafe { msg_send![&conn.client, isValid] };
            if valid.as_bool() {
                return Ok(true);
            }
            self.conn = None;
        }
        let Some(Found {
            info,
            description: desc,
            done,
        }) = find(self.wanted.as_deref())?
        else {
            return Ok(false);
        };
        let classes = classes()?;
        let published = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&published);
        let handler = RcBlock::new(move |_client: *mut AnyObject| {
            counter.fetch_add(1, Ordering::Release);
        });
        let client: Option<Object> = autoreleasepool(|_| {
            // SAFETY: `initWithServerDescription:device:options:newFrameHandler:`
            // takes an NSDictionary, an MTLDevice, an optional NSDictionary
            // and a block taking the client; it returns nil for a bad
            // description.
            unsafe {
                let alloc: Allocated<AnyObject> = msg_send![classes.client, alloc];
                msg_send![
                    alloc,
                    initWithServerDescription: &*desc,
                    device: &*self.device,
                    options: None::<&AnyObject>,
                    newFrameHandler: &*handler
                ]
            }
        });
        let Some(client) = client else {
            return Ok(false);
        };
        self.conn = Some(Connection {
            info,
            client,
            published,
            copied: 0,
            fresh: true,
            done,
        });
        Ok(true)
    }

    /// Copies the server's latest frame into `target` if there is a new
    /// one; see [`Received`]. `target` must be on the device given to
    /// [`connect`](Self::connect); the copy runs on the GPU after
    /// everything submitted to its queue so far, and later work waits for
    /// it. Does not wait for the GPU.
    pub fn receive_into(&mut self, target: &wgpu::Texture) -> Result<Received, InteropError> {
        if !self.attach()? {
            return Ok(Received::NoSender);
        }
        if self.target.0.as_ref().is_none_or(|(t, _)| t != target)
            && let Some(conn) = &mut self.conn
        {
            conn.fresh = true;
        }
        let dst = self.target.get(target)?;
        let Some(conn) = self.conn.as_mut() else {
            unreachable!("attached above");
        };
        autoreleasepool(|_| {
            let published = conn.published.load(Ordering::Acquire);
            let ready = conn
                .done
                .as_ref()
                .is_none_or(|d| d.load(Ordering::Acquire) > 0);
            if !ready || (published == conn.copied && !conn.fresh) {
                return Ok(Received::NoNewFrame);
            }
            // SAFETY: returns the current frame as a +1 MTLTexture on our
            // device, or nil before the first frame.
            let frame: Option<MtlTexture> = unsafe { msg_send![&conn.client, newFrameImage] };
            let Some(frame) = frame else {
                return Ok(Received::NoNewFrame);
            };
            let (w, h) = (frame.width() as u32, frame.height() as u32);
            let size = target.size();
            if (size.width, size.height, target.format()) != (w, h, SYPHON_FORMAT)
                || frame.pixelFormat() != MTLPixelFormat::BGRA8Unorm
            {
                conn.fresh = true;
                return Ok(Received::Resize {
                    width: w,
                    height: h,
                    format: SYPHON_FORMAT,
                });
            }
            let cb = command_buffer(&self.queue)?;
            let blit = cb
                .blitCommandEncoder()
                .ok_or_else(|| InteropError::Gpu("could not create a blit encoder".into()))?;
            // SAFETY: same size, format and device; one mip, one slice.
            unsafe { blit.copyFromTexture_toTexture(&frame, dst) };
            blit.endEncoding();
            cb.commit();
            conn.copied = published;
            conn.fresh = false;
            Ok(Received::NewFrame)
        })
    }
}

impl Drop for SyphonReceiver {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // SAFETY: `stop` takes no arguments.
            unsafe {
                let () = msg_send![&conn.client, stop];
            }
        }
    }
}
