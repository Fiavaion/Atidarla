//! Shared-memory frame ring: a memory-mapped file of fixed-size RGBA8 slots
//! shared by the app and an AI Host. This is the frame transport available on
//! every OS; zero-copy GPU handles (CUDA IPC on Windows + NVIDIA) are an
//! optional faster transport negotiated through `Capabilities::transports`.
//!
//! The ring does no locking: ownership of a slot is handed over by control
//! messages (`frame.submit` gives the host the input slot and lends it the
//! output slot; the response hands the output slot back). Each slot carries
//! the sequence ID of the frame it holds, so a reader can reject stale data.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! 0   magic "LSRRING1"        8 bytes
//! 8   version      u32        = 1
//! 12  slot_count   u32
//! 16  slot_bytes   u64        payload capacity per slot (multiple of 64)
//! 24  width        u32
//! 28  height       u32
//! 32  format       u32        1 = RGBA8 unorm
//! 36  reserved                up to 64
//! 64  slot 0: seq u64 · len u64 · reserved to 64 · payload[slot_bytes]
//!     slot 1 ...
//! ```

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use memmap2::MmapMut;

pub const MAGIC: &[u8; 8] = b"LSRRING1";
pub const VERSION: u32 = 1;
pub const FORMAT_RGBA8: u32 = 1;
const HEADER: usize = 64;
const SLOT_HEADER: usize = 64;

/// Geometry of a ring of RGBA8 frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingLayout {
    pub slot_count: u32,
    pub width: u32,
    pub height: u32,
}

impl RingLayout {
    pub const fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    const fn slot_bytes(&self) -> usize {
        self.frame_bytes().next_multiple_of(64)
    }

    const fn slot_stride(&self) -> usize {
        SLOT_HEADER + self.slot_bytes()
    }

    pub const fn file_len(&self) -> usize {
        HEADER + self.slot_count as usize * self.slot_stride()
    }
}

/// A mapped frame ring. The creator owns the backing file and deletes it on drop.
#[derive(Debug)]
pub struct FrameRing {
    map: MmapMut,
    layout: RingLayout,
    path: PathBuf,
    owner: bool,
}

/// A fresh path for a ring file: `/dev/shm` on Linux (RAM-backed), the
/// temp directory elsewhere.
pub fn default_ring_path() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = if cfg!(target_os = "linux") && Path::new("/dev/shm").is_dir() {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("lasair-ring-{}-{n}", std::process::id()))
}

impl FrameRing {
    /// Creates (or truncates) the ring file and writes its header.
    pub fn create(path: impl Into<PathBuf>, layout: RingLayout) -> io::Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.set_len(layout.file_len() as u64)?;
        // SAFETY: the file was just created at the right size. Other processes
        // may map it concurrently; slot ownership is coordinated by the control
        // protocol, so no byte is written by one side while the other reads it.
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        map[0..8].copy_from_slice(MAGIC);
        map[8..12].copy_from_slice(&VERSION.to_le_bytes());
        map[12..16].copy_from_slice(&layout.slot_count.to_le_bytes());
        map[16..24].copy_from_slice(&(layout.slot_bytes() as u64).to_le_bytes());
        map[24..28].copy_from_slice(&layout.width.to_le_bytes());
        map[28..32].copy_from_slice(&layout.height.to_le_bytes());
        map[32..36].copy_from_slice(&FORMAT_RGBA8.to_le_bytes());
        Ok(Self {
            map,
            layout,
            path,
            owner: true,
        })
    }

    /// Maps an existing ring created by another process.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        // SAFETY: see `create`.
        let map = unsafe { MmapMut::map_mut(&file)? };
        let bad = |msg: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {msg}", path.display()),
            )
        };
        if map.len() < HEADER || &map[0..8] != MAGIC {
            return Err(bad("not a Lasair frame ring"));
        }
        let u32_at = |o: usize| u32::from_le_bytes(map[o..o + 4].try_into().expect("4 bytes"));
        if u32_at(8) != VERSION || u32_at(32) != FORMAT_RGBA8 {
            return Err(bad("unsupported ring version or format"));
        }
        let layout = RingLayout {
            slot_count: u32_at(12),
            width: u32_at(24),
            height: u32_at(28),
        };
        if map.len() < layout.file_len() {
            return Err(bad("file shorter than its header declares"));
        }
        Ok(Self {
            map,
            layout,
            path,
            owner: false,
        })
    }

    pub fn layout(&self) -> RingLayout {
        self.layout
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn slot_offset(&self, slot: u32) -> usize {
        assert!(slot < self.layout.slot_count, "slot {slot} out of range");
        HEADER + slot as usize * self.layout.slot_stride()
    }

    /// Sequence ID currently stored in `slot`.
    pub fn seq(&self, slot: u32) -> u64 {
        let o = self.slot_offset(slot);
        u64::from_le_bytes(self.map[o..o + 8].try_into().expect("8 bytes"))
    }

    /// The frame pixels in `slot` (RGBA8, `width * height * 4` bytes).
    pub fn frame(&self, slot: u32) -> &[u8] {
        let o = self.slot_offset(slot) + SLOT_HEADER;
        &self.map[o..o + self.layout.frame_bytes()]
    }

    /// Writable pixels of `slot`, for filling a frame in place without a copy.
    pub fn frame_mut(&mut self, slot: u32) -> &mut [u8] {
        let o = self.slot_offset(slot) + SLOT_HEADER;
        let len = self.layout.frame_bytes();
        &mut self.map[o..o + len]
    }

    /// Stamps `slot` with the sequence ID of the frame it now holds.
    pub fn set_seq(&mut self, slot: u32, seq: u64) {
        let o = self.slot_offset(slot);
        let len = self.layout.frame_bytes() as u64;
        self.map[o..o + 8].copy_from_slice(&seq.to_le_bytes());
        self.map[o + 8..o + 16].copy_from_slice(&len.to_le_bytes());
    }
}

impl Drop for FrameRing {
    fn drop(&mut self) {
        if self.owner {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: RingLayout = RingLayout {
        slot_count: 3,
        width: 5,
        height: 3,
    };

    #[test]
    fn layout_is_aligned() {
        assert_eq!(LAYOUT.frame_bytes(), 60);
        assert_eq!(LAYOUT.slot_bytes(), 64);
        assert_eq!(LAYOUT.file_len(), 64 + 3 * 128);
    }

    #[test]
    fn create_write_open_read() {
        let path = default_ring_path();
        let mut ring = FrameRing::create(&path, LAYOUT).unwrap();
        ring.frame_mut(2)
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = i as u8);
        ring.set_seq(2, 42);

        let other = FrameRing::open(&path).unwrap();
        assert_eq!(other.layout(), LAYOUT);
        assert_eq!(other.seq(2), 42);
        assert_eq!(other.frame(2)[59], 59);
        assert_eq!(other.seq(0), 0);

        drop(other);
        drop(ring);
        assert!(!path.exists(), "owner removes the ring file");
    }

    #[test]
    fn open_rejects_foreign_file() {
        let path = default_ring_path();
        std::fs::write(&path, vec![0u8; 256]).unwrap();
        let err = FrameRing::open(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn slot_bounds_checked() {
        let ring = FrameRing::create(default_ring_path(), LAYOUT).unwrap();
        let _ = ring.frame(3);
    }
}
