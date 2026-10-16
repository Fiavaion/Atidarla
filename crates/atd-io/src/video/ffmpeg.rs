//! Video through FFmpeg (PRD A23), with the `ffmpeg` feature.
//!
//! The C shim (`ffmpeg_shim.c`) is compiled against the installed FFmpeg
//! headers and linked to its shared libraries, so no FFmpeg struct is
//! described by hand here. A [`Decoder`] addresses frames by index in
//! display order and decodes them to RGBA8; hardware decoding is used when
//! the platform supports the codec, with software as the fallback.

use std::ffi::{CStr, c_char, c_int, c_uint};
use std::path::Path;
use std::ptr::NonNull;

#[repr(C)]
struct RawVideo {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn atd_video_open(
        path: *const c_char,
        hardware: c_int,
        err: *mut c_char,
        errlen: c_int,
    ) -> *mut RawVideo;
    fn atd_video_close(v: *mut RawVideo);
    fn atd_video_info(
        v: *const RawVideo,
        width: *mut c_int,
        height: *mut c_int,
        frames: *mut i64,
        times: *mut *const f64,
        codec: *mut *const c_char,
        hw: *mut *const c_char,
    );
    fn atd_video_frame(
        v: *mut RawVideo,
        index: i64,
        rgba: *mut u8,
        err: *mut c_char,
        errlen: c_int,
    ) -> c_int;
    fn atd_video_avcodec_version() -> c_uint;
}

/// The linked libavcodec's version (major, minor, micro).
pub fn avcodec_version() -> (u32, u32, u32) {
    // SAFETY: a plain call into FFmpeg with no arguments.
    let v = unsafe { atd_video_avcodec_version() };
    (v >> 16, (v >> 8) & 0xff, v & 0xff)
}

/// One video stream, decoded on the calling thread.
#[derive(Debug)]
pub struct Decoder {
    raw: NonNull<RawVideo>,
    pub width: u32,
    pub height: u32,
    /// Start time of each frame in seconds, in display order.
    pub times: Vec<f64>,
    pub codec: String,
    /// The hardware decoder in use (`videotoolbox`, `d3d11va`, …), if any.
    pub hardware: Option<String>,
}

// SAFETY: the handle is used from one thread at a time (`&mut self`), which
// is all FFmpeg requires of a format and codec context.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `atd_video_open` and is closed once.
        unsafe { atd_video_close(self.raw.as_ptr()) };
    }
}

fn message(buf: &[c_char]) -> String {
    // SAFETY: the shim always writes a NUL-terminated string into `buf`
    // (it was zeroed, and snprintf terminates).
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

impl Decoder {
    /// Opens `path`, scanning its video packets for frame times. `hardware`
    /// allows a hardware decoder.
    pub fn open(path: &Path, hardware: bool) -> Result<Self, String> {
        // FFmpeg takes UTF-8 paths (converting to wide strings on Windows).
        let text = path
            .to_str()
            .ok_or_else(|| format!("{}: the path is not valid UTF-8", path.display()))?;
        let c_path = std::ffi::CString::new(text).map_err(|_| "a NUL in the path".to_owned())?;
        let mut err = [0 as c_char; 512];
        // SAFETY: valid NUL-terminated path; `err` is writable for its length.
        let raw = unsafe {
            atd_video_open(
                c_path.as_ptr(),
                c_int::from(hardware),
                err.as_mut_ptr(),
                err.len() as c_int,
            )
        };
        let raw = NonNull::new(raw).ok_or_else(|| message(&err))?;
        let (mut w, mut h, mut n) = (0, 0, 0i64);
        let (mut times, mut codec, mut hw) = (std::ptr::null(), std::ptr::null(), std::ptr::null());
        // SAFETY: `raw` is open; the out-pointers are valid.
        unsafe {
            atd_video_info(
                raw.as_ptr(),
                &mut w,
                &mut h,
                &mut n,
                &mut times,
                &mut codec,
                &mut hw,
            );
        }
        let count = usize::try_from(n).unwrap_or(0);
        // SAFETY: `times` holds `n` doubles owned by the handle; `codec` and
        // `hw` are NUL-terminated strings owned by the handle.
        let (times, codec, hw) = unsafe {
            (
                std::slice::from_raw_parts(times, count).to_vec(),
                CStr::from_ptr(codec).to_string_lossy().into_owned(),
                CStr::from_ptr(hw).to_string_lossy().into_owned(),
            )
        };
        Ok(Self {
            raw,
            width: w as u32,
            height: h as u32,
            times,
            codec,
            hardware: (!hw.is_empty()).then_some(hw),
        })
    }

    pub fn frames(&self) -> usize {
        self.times.len()
    }

    /// Decodes frame `index` into `rgba` (resized to width × height × 4).
    pub fn frame(&mut self, index: usize, rgba: &mut Vec<u8>) -> Result<(), String> {
        let len = self.width as usize * self.height as usize * 4;
        rgba.resize(len, 0);
        let mut err = [0 as c_char; 512];
        // SAFETY: `rgba` holds width × height × 4 bytes, the size the shim
        // writes (it fixes the output size at open); `err` is writable.
        let r = unsafe {
            atd_video_frame(
                self.raw.as_ptr(),
                index as i64,
                rgba.as_mut_ptr(),
                err.as_mut_ptr(),
                err.len() as c_int,
            )
        };
        if r == 0 { Ok(()) } else { Err(message(&err)) }
    }
}
