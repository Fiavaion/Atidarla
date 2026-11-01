//! Video through FFmpeg (PRD A23, A51), with the `ffmpeg` feature.
//!
//! The C shims (`ffmpeg_shim.c`, `ffmpeg_encode.c`) are compiled against
//! the installed FFmpeg headers and linked to its shared libraries, so no
//! FFmpeg struct is described by hand here. A [`Decoder`] addresses frames
//! by index in display order and decodes them to RGBA8; hardware decoding
//! is used when the platform supports the codec, with software as the
//! fallback. An [`Encoder`] writes RGBA8 frames to a `.mov` or `.mp4` with
//! an encoder named by the caller (only LGPL ones: [`super::record`]).

use std::ffi::{CStr, c_char, c_int, c_uint};
use std::path::Path;
use std::ptr::NonNull;

#[repr(C)]
struct RawVideo {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn lsr_video_open(
        path: *const c_char,
        hardware: c_int,
        err: *mut c_char,
        errlen: c_int,
    ) -> *mut RawVideo;
    fn lsr_video_close(v: *mut RawVideo);
    fn lsr_video_info(
        v: *const RawVideo,
        width: *mut c_int,
        height: *mut c_int,
        frames: *mut i64,
        times: *mut *const f64,
        codec: *mut *const c_char,
        hw: *mut *const c_char,
    );
    fn lsr_video_frame(
        v: *mut RawVideo,
        index: i64,
        rgba: *mut u8,
        err: *mut c_char,
        errlen: c_int,
    ) -> c_int;
    fn lsr_video_avcodec_version() -> c_uint;
}

/// The linked libavcodec's version (major, minor, micro).
pub fn avcodec_version() -> (u32, u32, u32) {
    // SAFETY: a plain call into FFmpeg with no arguments.
    let v = unsafe { lsr_video_avcodec_version() };
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
        // SAFETY: `raw` came from `lsr_video_open` and is closed once.
        unsafe { lsr_video_close(self.raw.as_ptr()) };
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
            lsr_video_open(
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
            lsr_video_info(
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
            lsr_video_frame(
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

#[repr(C)]
struct RawEncoder {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn lsr_encoder_available(name: *const c_char) -> c_int;
    fn lsr_encoder_open(
        path: *const c_char,
        name: *const c_char,
        width: c_int,
        height: c_int,
        fps_num: c_int,
        fps_den: c_int,
        profile: c_int,
        bitrate: i64,
        err: *mut c_char,
        errlen: c_int,
    ) -> *mut RawEncoder;
    fn lsr_encoder_push(
        e: *mut RawEncoder,
        rgba: *const u8,
        err: *mut c_char,
        errlen: c_int,
    ) -> c_int;
    fn lsr_encoder_finish(e: *mut RawEncoder, err: *mut c_char, errlen: c_int) -> c_int;
    fn lsr_encoder_free(e: *mut RawEncoder);
}

/// Whether the linked FFmpeg has an encoder called `name`.
pub fn encoder_available(name: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: a NUL-terminated name; the shim only looks it up.
    unsafe { lsr_encoder_available(c.as_ptr()) != 0 }
}

/// One video stream written to a file, on the calling thread. Dropping it
/// without [`Encoder::finish`] leaves the file without its trailer.
#[derive(Debug)]
pub struct Encoder {
    raw: Option<NonNull<RawEncoder>>,
    pub width: u32,
    pub height: u32,
    /// The FFmpeg encoder's name.
    pub encoder: String,
}

// SAFETY: the handle is used from one thread at a time (`&mut self`),
// which is all FFmpeg requires of a format and codec context.
unsafe impl Send for Encoder {}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            // SAFETY: `raw` came from `lsr_encoder_open` and is freed once.
            unsafe { lsr_encoder_free(raw.as_ptr()) };
        }
    }
}

impl Encoder {
    /// Opens `path` (`.mov` or `.mp4`) for `width` × `height` RGBA frames
    /// at `fps.0 / fps.1` frames per second with the FFmpeg encoder
    /// `encoder`. `profile` is the ProRes profile for `prores_ks` (2
    /// standard, 3 HQ, 4 4444); `bitrate` (bit/s) is for H.264 and H.265.
    pub fn open(
        path: &Path,
        encoder: &str,
        (width, height): (u32, u32),
        fps: (u32, u32),
        profile: i32,
        bitrate: u64,
    ) -> Result<Self, String> {
        let text = path
            .to_str()
            .ok_or_else(|| format!("{}: the path is not valid UTF-8", path.display()))?;
        let c_path = std::ffi::CString::new(text).map_err(|_| "a NUL in the path".to_owned())?;
        let c_name =
            std::ffi::CString::new(encoder).map_err(|_| "a NUL in the encoder name".to_owned())?;
        let int = |v: u32| c_int::try_from(v).map_err(|_| format!("{v} is too large"));
        let mut err = [0 as c_char; 512];
        // SAFETY: valid NUL-terminated strings; `err` is writable for its length.
        let raw = unsafe {
            lsr_encoder_open(
                c_path.as_ptr(),
                c_name.as_ptr(),
                int(width)?,
                int(height)?,
                int(fps.0)?,
                int(fps.1)?,
                profile,
                i64::try_from(bitrate).unwrap_or(i64::MAX),
                err.as_mut_ptr(),
                err.len() as c_int,
            )
        };
        let raw = NonNull::new(raw).ok_or_else(|| format!("{encoder}: {}", message(&err)))?;
        Ok(Self {
            raw: Some(raw),
            width,
            height,
            encoder: encoder.to_owned(),
        })
    }

    /// Encodes one tightly packed RGBA8 frame of the opened size.
    pub fn push(&mut self, rgba: &[u8]) -> Result<(), String> {
        let len = self.width as usize * self.height as usize * 4;
        if rgba.len() != len {
            return Err(format!("a frame of {} bytes, expected {len}", rgba.len()));
        }
        let raw = self.raw.ok_or("the encoder is finished")?;
        let mut err = [0 as c_char; 512];
        // SAFETY: `rgba` holds width × height × 4 bytes, the size the shim
        // reads; `err` is writable.
        let r = unsafe {
            lsr_encoder_push(
                raw.as_ptr(),
                rgba.as_ptr(),
                err.as_mut_ptr(),
                err.len() as c_int,
            )
        };
        if r == 0 { Ok(()) } else { Err(message(&err)) }
    }

    /// Flushes the encoder and writes the file's trailer.
    pub fn finish(mut self) -> Result<(), String> {
        let raw = self.raw.take().ok_or("the encoder is finished")?;
        let mut err = [0 as c_char; 512];
        // SAFETY: `raw` is open; the shim frees it whatever the result.
        let r = unsafe { lsr_encoder_finish(raw.as_ptr(), err.as_mut_ptr(), err.len() as c_int) };
        if r == 0 { Ok(()) } else { Err(message(&err)) }
    }
}
