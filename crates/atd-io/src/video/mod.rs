//! Video input. HAP (PRD A15, "HAP-first") is decoded natively: [`mov`]
//! demuxes, [`hap`] unpacks frames to block-compressed texture data for the
//! GPU, and [`bc`] is the CPU fallback. Other codecs go through FFmpeg
//! (`ffmpeg` feature, PRD A23), with hardware decoding where available,
//! behind the same [`MoviePlayer`] interface.
//!
//! [`MoviePlayer`] opens and decodes on its own thread. The cook names the
//! frame it wants; the thread decodes it and the next few frames into a
//! small cache. The cook takes frames from the cache with a non-blocking
//! `try_lock` and never waits on the disk (CLAUDE.md rule 1).

pub mod bc;
#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;
pub mod hap;
pub mod mov;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use hap::{HapFormat, HapFrame};

/// Frames decoded ahead of the requested one.
const LOOKAHEAD: usize = 3;

/// What a movie contains.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieInfo {
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub duration: f64,
    /// Frames per second (average).
    pub rate: f64,
    pub codec: String,
    /// Start time of each frame in seconds.
    pub times: Vec<f64>,
}

impl MovieInfo {
    /// The frame showing at `seconds`.
    pub fn frame_at(&self, seconds: f64) -> usize {
        match self.times.binary_search_by(|t| t.total_cmp(&seconds)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    info: OnceLock<MovieInfo>,
    error: OnceLock<String>,
    wanted: AtomicUsize,
    stop: AtomicBool,
    /// Decoded frames by index, at most LOOKAHEAD + 1.
    cache: Mutex<Vec<(usize, Arc<HapFrame>)>>,
    wake: Condvar,
    wake_lock: Mutex<bool>,
}

/// A movie decoding on its own thread.
#[derive(Debug)]
pub struct MoviePlayer {
    shared: Arc<Shared>,
}

impl Drop for MoviePlayer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.poke();
    }
}

impl MoviePlayer {
    /// Starts opening `path`. Never blocks.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let path = path.into();
        let spawned = std::thread::Builder::new()
            .name("atd-movie".into())
            .spawn(move || {
                if let Err(e) = run(&path, &thread_shared) {
                    log::warn!("movie {}: {e}", path.display());
                    let _ = thread_shared.error.set(e);
                }
            });
        if let Err(e) = spawned {
            let _ = shared.error.set(format!("cannot start movie thread: {e}"));
        }
        Self { shared }
    }

    pub fn info(&self) -> Option<&MovieInfo> {
        self.shared.info.get()
    }

    pub fn error(&self) -> Option<&str> {
        self.shared.error.get().map(String::as_str)
    }

    /// Asks for frame `index` (and the ones after it) to be decoded.
    pub fn request(&self, index: usize) {
        if self.shared.wanted.swap(index, Ordering::Relaxed) != index {
            self.poke();
        }
    }

    /// Frame `index` if it is decoded; never blocks.
    pub fn frame(&self, index: usize) -> Option<Arc<HapFrame>> {
        let cache = self.shared.cache.try_lock().ok()?;
        cache
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, f)| f.clone())
    }

    /// Blocks until the movie is open or has failed, at most `timeout`. For
    /// offline renders only (FR-H1), never on the app's render thread.
    pub fn wait_ready(&self, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while self.info().is_none()
            && self.error().is_none()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Asks for frame `index` and blocks until it is decoded, at most
    /// `timeout`; `None` if the movie failed or time ran out. For offline
    /// renders only (FR-H1).
    pub fn wait_frame(&self, index: usize, timeout: Duration) -> Option<Arc<HapFrame>> {
        let deadline = std::time::Instant::now() + timeout;
        self.request(index);
        loop {
            if let Some(frame) = self.frame(index) {
                return Some(frame);
            }
            if self.error().is_some() || std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn poke(&self) {
        if let Ok(mut flag) = self.shared.wake_lock.lock() {
            *flag = true;
            self.shared.wake.notify_one();
        }
    }
}

/// A still image: one frame, decoded once.
fn run_still(path: &std::path::Path, shared: &Shared) -> Result<(), String> {
    let image = crate::image::load(path)?;
    let codec = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("image")
        .to_ascii_uppercase();
    let _ = shared.info.set(MovieInfo {
        width: image.width,
        height: image.height,
        frames: 1,
        duration: 0.0,
        rate: 0.0,
        codec,
        times: vec![0.0],
    });
    if let Ok(mut cache) = shared.cache.lock() {
        cache.push((
            0,
            Arc::new(HapFrame {
                format: HapFormat::Rgba8,
                data: image.rgba,
            }),
        ));
    }
    Ok(())
}

fn run(path: &std::path::Path, shared: &Shared) -> Result<(), String> {
    if crate::image::is_still(path) {
        return run_still(path, shared);
    }
    // HAP natively; anything else (or a container the MOV reader does not
    // know) through FFmpeg.
    let track = match mov::open(path) {
        Ok(track) if track.codec_str().starts_with("Hap") => track,
        Ok(track) => return run_other(path, shared, &track.codec_str()),
        Err(e) => return run_other(path, shared, &e.to_string()),
    };
    let codec = track.codec_str();
    let scale = f64::from(track.timescale.max(1));
    let frames = track.samples.len();
    let duration = track.duration_secs();
    let _ = shared.info.set(MovieInfo {
        width: track.width,
        height: track.height,
        frames,
        duration,
        rate: if duration > 0.0 {
            frames as f64 / duration
        } else {
            0.0
        },
        codec,
        times: track
            .samples
            .iter()
            .map(|s| s.time as f64 / scale)
            .collect(),
    });
    let mut reader = mov::FrameReader::open(path).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let wanted = shared
            .wanted
            .load(Ordering::Relaxed)
            .min(frames.saturating_sub(1));
        let window: Vec<usize> = (0..=LOOKAHEAD)
            .map(|k| (wanted + k) % frames.max(1))
            .collect();
        // Drop frames outside the window, then decode the missing ones in
        // order, nearest first.
        if let Ok(mut cache) = shared.cache.lock() {
            cache.retain(|(i, _)| window.contains(i));
        }
        for &index in &window {
            if shared.stop.load(Ordering::Relaxed)
                || shared.wanted.load(Ordering::Relaxed) != wanted
            {
                break;
            }
            let cached = shared
                .cache
                .lock()
                .map(|c| c.iter().any(|(i, _)| *i == index))
                .unwrap_or(true);
            if cached {
                continue;
            }
            reader
                .read(&track.samples[index], &mut raw)
                .map_err(|e| format!("reading frame {index}: {e}"))?;
            let mut frame = HapFrame {
                format: HapFormat::Bc1,
                data: Vec::new(),
            };
            hap::decode(&raw, track.width, track.height, &mut frame)
                .map_err(|e| format!("frame {index}: {e}"))?;
            if let Ok(mut cache) = shared.cache.lock() {
                cache.push((index, Arc::new(frame)));
            }
        }
        // Sleep until a new frame is wanted (or a timeout, to notice stop).
        let Ok(mut flag) = shared.wake_lock.lock() else {
            break;
        };
        if !*flag {
            flag = shared
                .wake
                .wait_timeout(flag, Duration::from_millis(50))
                .map(|(g, _)| g)
                .map_err(|_| "movie thread lock poisoned".to_owned())?;
        }
        *flag = false;
    }
    Ok(())
}

/// A movie that is not HAP: FFmpeg when built with it.
#[cfg(not(feature = "ffmpeg"))]
fn run_other(_path: &std::path::Path, _shared: &Shared, what: &str) -> Result<(), String> {
    Err(format!(
        "{what}: this build plays HAP movies and PNG/JPEG stills; other codecs need FFmpeg \
         (build with the `ffmpeg` feature)"
    ))
}

/// A movie that is not HAP, decoded by FFmpeg to RGBA.
#[cfg(feature = "ffmpeg")]
fn run_other(path: &std::path::Path, shared: &Shared, _what: &str) -> Result<(), String> {
    let mut decoder = ffmpeg::Decoder::open(path, true)?;
    let frames = decoder.frames();
    let duration = match decoder.times.as_slice() {
        [.., last] if frames > 1 => last * frames as f64 / (frames - 1) as f64,
        _ => 0.0,
    };
    let codec = match &decoder.hardware {
        Some(hw) => format!("{} ({hw})", decoder.codec),
        None => decoder.codec.clone(),
    };
    let _ = shared.info.set(MovieInfo {
        width: decoder.width,
        height: decoder.height,
        frames,
        duration,
        rate: if duration > 0.0 {
            frames as f64 / duration
        } else {
            0.0
        },
        codec,
        times: decoder.times.clone(),
    });
    while !shared.stop.load(Ordering::Relaxed) {
        let wanted = shared
            .wanted
            .load(Ordering::Relaxed)
            .min(frames.saturating_sub(1));
        let window: Vec<usize> = (0..=LOOKAHEAD)
            .map(|k| (wanted + k) % frames.max(1))
            .collect();
        if let Ok(mut cache) = shared.cache.lock() {
            cache.retain(|(i, _)| window.contains(i));
        }
        for &index in &window {
            if shared.stop.load(Ordering::Relaxed)
                || shared.wanted.load(Ordering::Relaxed) != wanted
            {
                break;
            }
            let cached = shared
                .cache
                .lock()
                .map(|c| c.iter().any(|(i, _)| *i == index))
                .unwrap_or(true);
            if cached {
                continue;
            }
            let mut data = Vec::new();
            decoder
                .frame(index, &mut data)
                .map_err(|e| format!("frame {index}: {e}"))?;
            if let Ok(mut cache) = shared.cache.lock() {
                cache.push((
                    index,
                    Arc::new(HapFrame {
                        format: HapFormat::Rgba8,
                        data,
                    }),
                ));
            }
        }
        let Ok(mut flag) = shared.wake_lock.lock() else {
            break;
        };
        if !*flag {
            flag = shared
                .wake
                .wait_timeout(flag, Duration::from_millis(50))
                .map(|(g, _)| g)
                .map_err(|_| "movie thread lock poisoned".to_owned())?;
        }
        *flag = false;
    }
    Ok(())
}
