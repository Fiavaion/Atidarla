//! Spout's shared-memory protocol: the sender list, the active-sender name,
//! the per-sender texture-info map and the per-sender sync objects.
//!
//! Layout and behaviour follow the Spout2 SDK as read at commit 6b68a5b
//! (`SpoutSharedMemory.cpp`, `SpoutSenderNames.cpp`, `SpoutFrameCount.cpp`);
//! see the attribution in [`super`]. Every map is a pagefile-backed file
//! mapping with an ANSI name and no `Global\` prefix, guarded by a named
//! mutex `<map>_mutex`.

use std::collections::BTreeSet;
use std::ffi::c_void;

use windows::Win32::Foundation::{
    ERROR_SUCCESS, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Memory::{
    CreateFileMappingA, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, OpenFileMappingA, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueA};
use windows::Win32::System::Threading::{
    CreateMutexA, CreateSemaphoreA, ReleaseMutex, ReleaseSemaphore, WaitForSingleObject,
};
use windows::core::{PCSTR, s};

use crate::InteropError;
use crate::dx12::{SharedHandle, platform};

/// Bytes per sender name, NUL included (`SpoutMaxSenderNameLen`).
pub(super) const NAME_LEN: usize = 256;
/// Size of `SharedTextureInfo`.
pub(super) const INFO_LEN: usize = 280;
/// How long the SDK waits for a map mutex (`SpoutSharedMemory::Lock`) and
/// for the texture access mutex (`spoutFrameCount::CheckAccess`): 67 ms,
/// four frames at 60 Hz. The header's `SPOUT_WAIT_TIMEOUT` (100) is unused.
pub(super) const LOCK_TIMEOUT_MS: u32 = 67;
/// `MaxSenders` when the registry does not set it.
const DEFAULT_MAX_SENDERS: usize = 64;
const SENDER_NAMES: &[u8] = b"SpoutSenderNames";
const ACTIVE_NAME: &[u8] = b"ActiveSenderName";

fn busy(what: &str) -> InteropError {
    InteropError::Platform(format!(
        "the Spout {what} stayed locked for {LOCK_TIMEOUT_MS} ms"
    ))
}

/// Checks a sender name: 1–255 bytes, no NUL. It also names kernel objects,
/// where a backslash is a namespace separator, so that is rejected too.
pub(super) fn check_name(name: &[u8]) -> Result<(), InteropError> {
    if name.is_empty() || name.len() >= NAME_LEN || name.contains(&0) || name.contains(&b'\\') {
        return Err(InteropError::Platform(format!(
            "invalid Spout sender name {:?}: 1-{} bytes, no NUL or backslash",
            String::from_utf8_lossy(name),
            NAME_LEN - 1
        )));
    }
    Ok(())
}

/// A NUL-terminated ANSI kernel-object name built on the stack, so the
/// per-frame paths do not allocate.
pub(super) struct ObjName([u8; NAME_LEN + 32]);

impl ObjName {
    /// Joins `parts`: a checked name plus a short suffix.
    pub(super) fn new(parts: &[&[u8]]) -> Self {
        let mut buf = [0; NAME_LEN + 32];
        let mut at = 0;
        for part in parts {
            assert!(at + part.len() < buf.len(), "kernel object name too long");
            buf[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        Self(buf)
    }

    pub(super) fn as_pcstr(&self) -> PCSTR {
        PCSTR(self.0.as_ptr())
    }
}

/// A named Win32 mutex, created or opened (`CreateMutexA`).
#[derive(Debug)]
pub(super) struct NamedMutex(SharedHandle);

impl NamedMutex {
    pub(super) fn create(name: &ObjName) -> Result<Self, InteropError> {
        // SAFETY: `name` is NUL-terminated and outlives the call.
        unsafe { CreateMutexA(None, false, name.as_pcstr()) }
            .map(|h| Self(SharedHandle(h)))
            .map_err(platform)
    }

    /// Waits up to [`LOCK_TIMEOUT_MS`]. An abandoned mutex (its owner died
    /// holding it) counts as acquired; the SDK treats it as a failure and
    /// then never releases it.
    pub(super) fn lock(&self) -> Option<MutexGuard<'_>> {
        // SAFETY: the handle is a valid mutex for the lifetime of `self`.
        let r = unsafe { WaitForSingleObject(self.0.raw(), LOCK_TIMEOUT_MS) };
        (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED).then_some(MutexGuard(self))
    }
}

/// Releases the mutex on drop, on the thread that acquired it.
#[derive(Debug)]
pub(super) struct MutexGuard<'a>(&'a NamedMutex);

impl Drop for MutexGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: this thread owns the mutex since `lock` succeeded.
        let _ = unsafe { ReleaseMutex(self.0.0.raw()) };
    }
}

/// A mapped view, unmapped on drop.
#[derive(Debug)]
struct View(*mut u8);

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: the address came from `MapViewOfFile` and is unmapped once.
        let _ = unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.0.cast(),
            })
        };
    }
}

/// A named shared-memory map with its `<name>_mutex` (`SpoutSharedMemory`).
#[derive(Debug)]
pub(super) struct SharedMemory {
    view: View,
    /// Mapped length: the map's size rounded up to whole pages.
    len: usize,
    _map: SharedHandle,
    mutex: NamedMutex,
}

// SAFETY: the view is process-wide memory; all access goes through `lock`,
// which takes the named mutex, and `&mut self`.
unsafe impl Send for SharedMemory {}

impl SharedMemory {
    /// Creates a map of `size` bytes, or opens it if it exists, keeping the
    /// size it was created with (`SpoutSharedMemory::Create`).
    pub(super) fn create(name: &[u8], size: u32) -> Result<Self, InteropError> {
        let obj = ObjName::new(&[name]);
        // SAFETY: pagefile-backed mapping; `obj` is NUL-terminated.
        let map = unsafe {
            CreateFileMappingA(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                size,
                obj.as_pcstr(),
            )
        }
        .map_err(platform)?;
        Self::map(SharedHandle(map), name)
    }

    /// Opens an existing map; `None` if there is none (`SpoutSharedMemory::Open`).
    pub(super) fn open(name: &[u8]) -> Option<Self> {
        let obj = ObjName::new(&[name]);
        // SAFETY: `obj` is NUL-terminated and outlives the call.
        let map = unsafe { OpenFileMappingA(FILE_MAP_ALL_ACCESS.0, false, obj.as_pcstr()) }.ok()?;
        Self::map(SharedHandle(map), name).ok()
    }

    fn map(map: SharedHandle, name: &[u8]) -> Result<Self, InteropError> {
        // SAFETY: `map` is a valid file-mapping handle; map all of it.
        let addr = unsafe { MapViewOfFile(map.raw(), FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if addr.Value.is_null() {
            return Err(platform(windows::core::Error::from_thread()));
        }
        let view = View(addr.Value.cast());
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: `view` is a live mapping; `info` is a local out-parameter.
        let n = unsafe {
            VirtualQuery(
                Some(view.0 as *const c_void),
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if n == 0 {
            return Err(platform(windows::core::Error::from_thread()));
        }
        let mutex = NamedMutex::create(&ObjName::new(&[name, b"_mutex"]))?;
        Ok(Self {
            view,
            len: info.RegionSize,
            _map: map,
            mutex,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Takes the map's mutex (up to [`LOCK_TIMEOUT_MS`]).
    pub(super) fn lock(&mut self) -> Option<Locked<'_>> {
        let guard = self.mutex.lock()?;
        // SAFETY: the view is `len` bytes long and mapped for `self`'s
        // lifetime; the named mutex keeps other well-behaved processes out
        // and `&mut self` keeps this process to one borrower.
        let bytes = unsafe { std::slice::from_raw_parts_mut(self.view.0, self.len) };
        Some(Locked {
            bytes,
            _guard: guard,
        })
    }
}

/// A locked map's bytes.
#[derive(Debug)]
pub(super) struct Locked<'a> {
    pub(super) bytes: &'a mut [u8],
    _guard: MutexGuard<'a>,
}

/// `MaxSenders` from `HKCU\Software\Leading Edge\Spout`, default 64.
fn max_senders() -> usize {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: constant NUL-terminated strings; `value` holds a REG_DWORD.
    let r = unsafe {
        RegGetValueA(
            HKEY_CURRENT_USER,
            s!("Software\\Leading Edge\\Spout"),
            s!("MaxSenders"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    if r == ERROR_SUCCESS && value > 0 {
        value as usize
    } else {
        DEFAULT_MAX_SENDERS
    }
}

/// Reads the sorted name slots up to the first empty or unterminated one
/// (`readSenderSetFromBuffer`).
pub(super) fn read_names(buf: &[u8], capacity: usize) -> BTreeSet<Vec<u8>> {
    buf.chunks_exact(NAME_LEN)
        .take(capacity)
        .map_while(|slot| {
            let len = slot.iter().position(|&b| b == 0)?;
            (len > 0).then(|| slot[..len].to_vec())
        })
        .collect()
}

/// Writes `names` in order, one per slot, then an empty slot if there is
/// room (`writeBufferFromSenderSet`). Names must be shorter than [`NAME_LEN`].
pub(super) fn write_names(buf: &mut [u8], capacity: usize, names: &BTreeSet<Vec<u8>>) {
    let mut slots = buf.chunks_exact_mut(NAME_LEN).take(capacity);
    for name in names {
        let Some(slot) = slots.next() else { return };
        slot.fill(0);
        slot[..name.len()].copy_from_slice(name);
    }
    if let Some(slot) = slots.next() {
        slot[0] = 0;
    }
}

/// The bytes before the first NUL.
fn c_str(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())]
}

fn write_c_str(slot: &mut [u8], name: &[u8]) {
    slot.fill(0);
    slot[..name.len()].copy_from_slice(name);
}

/// The sender-list and active-name maps, held open for a sender's
/// lifetime as the SDK does, so the maps outlive short-lived readers.
#[derive(Debug)]
pub(super) struct SenderList {
    names: SharedMemory,
    active: SharedMemory,
    capacity: usize,
}

impl SenderList {
    pub(super) fn open_or_create() -> Result<Self, InteropError> {
        let max = max_senders();
        let names = SharedMemory::create(SENDER_NAMES, (max * NAME_LEN) as u32)?;
        // An existing map keeps the size it was created with.
        let capacity = max.min(names.len() / NAME_LEN);
        let active = SharedMemory::create(ACTIVE_NAME, NAME_LEN as u32)?;
        Ok(Self {
            names,
            active,
            capacity,
        })
    }

    /// Registers `base`, or `base_1`, `base_2`, ... if taken, and makes it
    /// the active sender (`RegisterSenderName` with `bNewname`). `create`
    /// runs with the list locked and makes the sender's info map, so no
    /// receiver sees the name before its map exists (the SDK creates the
    /// map after unlocking).
    pub(super) fn register<T>(
        &mut self,
        base: &[u8],
        create: impl FnOnce(&[u8]) -> Result<T, InteropError>,
    ) -> Result<(Vec<u8>, T), InteropError> {
        let capacity = self.capacity;
        let list = self.names.lock().ok_or_else(|| busy("sender list"))?;
        let mut names = read_names(list.bytes, capacity);
        if names.len() >= capacity {
            return Err(InteropError::Platform(format!(
                "the Spout sender list is full ({capacity} senders)"
            )));
        }
        let mut name = base.to_vec();
        let mut n = 1;
        while names.contains(&name) {
            name = [base, format!("_{n}").as_bytes()].concat();
            n += 1;
        }
        check_name(&name)?;
        let value = create(&name)?;
        names.insert(name.clone());
        write_names(list.bytes, capacity, &names);
        if let Some(active) = self.active.lock() {
            write_c_str(&mut active.bytes[..NAME_LEN], &name);
        }
        Ok((name, value))
    }

    /// Removes `name` (`ReleaseSenderName`). If it was the active sender,
    /// or one sender is left, the first remaining one becomes active. If
    /// none is left and it was active, the active name is cleared (the SDK
    /// leaves it stale).
    pub(super) fn release(&mut self, name: &[u8]) {
        let capacity = self.capacity;
        let Some(list) = self.names.lock() else {
            return;
        };
        let mut names = read_names(list.bytes, capacity);
        if !names.remove(name) {
            return;
        }
        write_names(list.bytes, capacity, &names);
        let Some(active) = self.active.lock() else {
            return;
        };
        let slot = &mut active.bytes[..NAME_LEN];
        let was_active = c_str(slot) == name;
        match names.first() {
            Some(first) if was_active || names.len() == 1 => write_c_str(slot, first),
            None if was_active => slot.fill(0),
            _ => {}
        }
    }
}

/// The registered sender names, sorted. Creates nothing.
pub(super) fn list_names() -> Vec<Vec<u8>> {
    let Some(mut mem) = SharedMemory::open(SENDER_NAMES) else {
        return Vec::new();
    };
    let capacity = max_senders().min(mem.len() / NAME_LEN);
    let Some(list) = mem.lock() else {
        return Vec::new();
    };
    read_names(list.bytes, capacity).into_iter().collect()
}

/// Copies the active sender's name into `out` and returns its length;
/// `None` if no name is set. Creates nothing.
pub(super) fn active_name(out: &mut [u8; NAME_LEN]) -> Option<usize> {
    let mut mem = SharedMemory::open(ACTIVE_NAME)?;
    let lock = mem.lock()?;
    out.copy_from_slice(&lock.bytes[..NAME_LEN]);
    let len = out.iter().position(|&b| b == 0)?;
    (len > 0).then_some(len)
}

/// `SharedTextureInfo`: 280 bytes of native-endian `u32`s and a description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TextureInfo {
    /// Legacy D3D11 shared handle, truncated to 32 bits.
    pub share_handle: u32,
    pub width: u32,
    pub height: u32,
    /// `DXGI_FORMAT` value.
    pub format: u32,
    /// Unused by the SDK; written as 0.
    pub usage: u32,
    /// The sender's executable path, NUL-terminated.
    pub description: [u8; 256],
    /// Top bits flag CPU sharing (0x8000_0000) and GL/DX interop
    /// (0x4000_0000); D3D11 senders write 0.
    pub partner_id: u32,
}

impl TextureInfo {
    pub(super) fn to_bytes(self) -> [u8; INFO_LEN] {
        let mut out = [0; INFO_LEN];
        for (i, v) in [
            self.share_handle,
            self.width,
            self.height,
            self.format,
            self.usage,
        ]
        .into_iter()
        .enumerate()
        {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
        }
        out[20..276].copy_from_slice(&self.description);
        out[276..280].copy_from_slice(&self.partner_id.to_ne_bytes());
        out
    }

    pub(super) fn from_bytes(b: &[u8; INFO_LEN]) -> Self {
        let u = |at: usize| u32::from_ne_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let mut description = [0; 256];
        description.copy_from_slice(&b[20..276]);
        Self {
            share_handle: u(0),
            width: u(4),
            height: u(8),
            format: u(12),
            usage: u(16),
            description,
            partner_id: u(276),
        }
    }
}

/// Reads a sender's info map (`getSharedInfo`). The map is opened and
/// closed on every call, as the SDK does, so a sender that has gone is
/// noticed when its map disappears.
pub(super) fn read_info(name: &[u8]) -> Option<TextureInfo> {
    let mut mem = SharedMemory::open(name)?;
    let lock = mem.lock()?;
    let bytes: &[u8; INFO_LEN] = lock.bytes.get(..INFO_LEN)?.try_into().ok()?;
    Some(TextureInfo::from_bytes(bytes))
}

/// A sender's own info map, kept open for its lifetime.
#[derive(Debug)]
pub(super) struct InfoMap(SharedMemory);

impl InfoMap {
    pub(super) fn create(name: &[u8]) -> Result<Self, InteropError> {
        SharedMemory::create(name, INFO_LEN as u32).map(Self)
    }

    pub(super) fn write(&mut self, info: &TextureInfo) -> Result<(), InteropError> {
        let lock = self.0.lock().ok_or_else(|| busy("sender info"))?;
        lock.bytes[..INFO_LEN].copy_from_slice(&info.to_bytes());
        Ok(())
    }
}

/// A sender's texture access mutex and frame-count semaphore
/// (`spoutFrameCount`). Sender and receivers all create-or-open them.
#[derive(Debug)]
pub(super) struct FrameSync {
    access: NamedMutex,
    count: SharedHandle,
}

impl FrameSync {
    pub(super) fn open(name: &[u8]) -> Result<Self, InteropError> {
        let access = NamedMutex::create(&ObjName::new(&[name, b"_SpoutAccessMutex"]))?;
        let count_name = ObjName::new(&[name, b"_Count_Semaphore"]);
        // SAFETY: `count_name` is NUL-terminated and outlives the call.
        let count = unsafe { CreateSemaphoreA(None, 1, i32::MAX, count_name.as_pcstr()) }
            .map_err(platform)?;
        Ok(Self {
            access,
            count: SharedHandle(count),
        })
    }

    /// Takes the texture access mutex (up to [`LOCK_TIMEOUT_MS`]).
    pub(super) fn lock(&self) -> Option<MutexGuard<'_>> {
        self.access.lock()
    }

    /// Counts one more frame (`SetNewFrame`): take one, give two back.
    pub(super) fn count_frame(&self) {
        // SAFETY: the handle is a valid semaphore for the lifetime of `self`.
        unsafe {
            if WaitForSingleObject(self.count.raw(), 0) == WAIT_OBJECT_0 {
                let _ = ReleaseSemaphore(self.count.raw(), 2, None);
            }
        }
    }

    /// The sender's frame count (`GetNewFrame`): take one, give it back and
    /// read the count before. 0 if the sender does not count frames.
    pub(super) fn frame_count(&self) -> i32 {
        let mut previous = 0;
        // SAFETY: as above; `previous` is a local out-parameter.
        unsafe {
            if WaitForSingleObject(self.count.raw(), 0) == WAIT_OBJECT_0 {
                let _ = ReleaseSemaphore(self.count.raw(), 1, Some(&mut previous));
            }
        }
        previous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<Vec<u8>> {
        names.iter().map(|n| n.as_bytes().to_vec()).collect()
    }

    #[test]
    fn names_round_trip_sorted_and_terminated() {
        let mut buf = vec![0xAA; 4 * NAME_LEN];
        let names = set(&["zeta", "Alpha", "beta"]);
        write_names(&mut buf, 4, &names);
        assert_eq!(&buf[..6], b"Alpha\0");
        assert_eq!(&buf[NAME_LEN..NAME_LEN + 5], b"beta\0");
        assert_eq!(buf[3 * NAME_LEN], 0, "empty slot ends the list");
        assert_eq!(read_names(&buf, 4), names);
    }

    #[test]
    fn full_list_has_no_terminator_and_reads_back() {
        let mut buf = vec![0; 2 * NAME_LEN];
        let names = set(&["a", "b"]);
        write_names(&mut buf, 2, &names);
        assert_eq!(read_names(&buf, 2), names);
        // Capacity smaller than the buffer: the rest is ignored.
        assert_eq!(read_names(&buf, 1), set(&["a"]));
    }

    #[test]
    fn info_layout_matches_shared_texture_info() {
        let mut description = [0; 256];
        description[..4].copy_from_slice(b"C:\\x");
        let info = TextureInfo {
            share_handle: 0xC000_1234,
            width: 1920,
            height: 1080,
            format: 87,
            usage: 0,
            description,
            partner_id: 0x4000_0000,
        };
        let b = info.to_bytes();
        assert_eq!(&b[0..4], &0xC000_1234u32.to_ne_bytes());
        assert_eq!(&b[4..8], &1920u32.to_ne_bytes());
        assert_eq!(&b[12..16], &87u32.to_ne_bytes());
        assert_eq!(&b[20..24], b"C:\\x");
        assert_eq!(&b[276..280], &0x4000_0000u32.to_ne_bytes());
        assert_eq!(TextureInfo::from_bytes(&b), info);
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name(b"Lasair").is_ok());
        assert!(check_name(b"").is_err());
        assert!(check_name(&[b'x'; NAME_LEN]).is_err());
        assert!(check_name(b"a\0b").is_err());
        assert!(check_name(b"Local\\x").is_err());
    }

    /// Registers in the real, machine-wide sender list (as any Spout app
    /// would), under a name unique to this process.
    #[test]
    fn register_renames_and_release_removes() {
        let base = format!("lsr-unit-{}", std::process::id());
        let base = base.as_bytes();
        let info = |n: &[u8]| InfoMap::create(n);
        let mut list_a = SenderList::open_or_create().unwrap();
        let mut list_b = SenderList::open_or_create().unwrap();
        let (a, _info_a) = list_a.register(base, info).unwrap();
        let (b, _info_b) = list_b.register(base, info).unwrap();
        assert_eq!(a, base);
        assert_eq!(b, [base, b"_1"].concat());
        let names = list_names();
        assert!(names.contains(&a) && names.contains(&b));

        list_a.release(&a);
        let names = list_names();
        assert!(!names.contains(&a) && names.contains(&b));
        let (a2, _info_a2) = list_a.register(base, info).unwrap();
        assert_eq!(a2, base, "a released name is free again");
        list_a.release(&a2);
        list_b.release(&b);
        let names = list_names();
        assert!(!names.contains(&a2) && !names.contains(&b));
        let mut active = [0; NAME_LEN];
        if let Some(len) = active_name(&mut active) {
            assert!(
                !active[..len].starts_with(base),
                "released names are not active"
            );
        }
    }
}
