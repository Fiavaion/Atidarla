//! Recording RGBA frames to a movie file (FR-S10, PRD A51), on a worker
//! thread.
//!
//! [`Recorder`] owns an FFmpeg [`super::ffmpeg::Encoder`] on its own thread
//! and a small pool of frame buffers allocated when it starts. The caller
//! takes a free buffer, fills it and hands it over; the thread encodes it
//! and returns the buffer. Live, [`Recorder::try_frame`] never waits: when
//! the encoder falls behind the frame is dropped and counted (rule 1).
//! Offline, [`Recorder::push_wait`] waits for a buffer, so every frame is
//! encoded.
//!
//! Only LGPL encoders are ever named (A23, A51, rule 3): ProRes through
//! `prores_ks`, and H.264/H.265 through the platform's hardware encoder
//! (VideoToolbox on macOS; NVENC or Media Foundation on Windows). Without
//! the `ffmpeg` feature, [`Recorder::start`] says how to get one.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;

/// Whether this build can record (it has the `ffmpeg` feature).
pub const ENABLED: bool = cfg!(feature = "ffmpeg");

/// Frame buffers in flight between the caller and the encoder.
const POOL: usize = 4;

/// A codec to record with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Apple ProRes 422 (10-bit 4:2:2), `prores_ks` profile standard.
    ProRes422,
    /// ProRes 422 HQ.
    ProRes422Hq,
    /// ProRes 4444 with alpha.
    ProRes4444,
    /// H.264 on the platform's hardware encoder.
    H264,
    /// H.265 (HEVC) on the platform's hardware encoder.
    H265,
}

impl Codec {
    /// Names accepted on the command line, with their codec.
    pub const NAMES: [(&'static str, Codec); 5] = [
        ("prores", Codec::ProRes422),
        ("prores-hq", Codec::ProRes422Hq),
        ("prores4444", Codec::ProRes4444),
        ("h264", Codec::H264),
        ("h265", Codec::H265),
    ];

    pub fn parse(name: &str) -> Option<Codec> {
        Self::NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, c)| *c)
    }

    pub fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(_, c)| *c == self)
            .map_or("?", |(n, _)| n)
    }

    /// The default for a file extension: ProRes 422 in `.mov`, H.264 in
    /// `.mp4` (MP4 cannot hold ProRes).
    pub fn for_path(path: &Path) -> Option<Codec> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "mov" => Some(Codec::ProRes422),
            "mp4" | "m4v" => Some(Codec::H264),
            _ => None,
        }
    }

    /// The LGPL FFmpeg encoders for this codec on this platform, in order
    /// of preference. Never libx264 or libx265 (GPL).
    pub fn encoders(self) -> &'static [&'static str] {
        match self {
            Codec::ProRes422 | Codec::ProRes422Hq | Codec::ProRes4444 => &["prores_ks"],
            Codec::H264 => {
                if cfg!(target_os = "macos") {
                    &["h264_videotoolbox"]
                } else if cfg!(windows) {
                    &["h264_nvenc", "h264_mf"]
                } else {
                    &["h264_nvenc"]
                }
            }
            Codec::H265 => {
                if cfg!(target_os = "macos") {
                    &["hevc_videotoolbox"]
                } else if cfg!(windows) {
                    &["hevc_nvenc", "hevc_mf"]
                } else {
                    &["hevc_nvenc"]
                }
            }
        }
    }

    /// The `prores_ks` profile (2 standard, 3 HQ, 4 4444); 0 otherwise.
    pub fn profile(self) -> i32 {
        match self {
            Codec::ProRes422 => 2,
            Codec::ProRes422Hq => 3,
            Codec::ProRes4444 => 4,
            Codec::H264 | Codec::H265 => 0,
        }
    }
}

/// What to record.
#[derive(Debug, Clone)]
pub struct Settings {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Frame rate as a fraction (60 fps: `(60, 1)`; 29.97: `(30000, 1001)`).
    pub fps: (u32, u32),
    pub codec: Codec,
    /// H.264/H.265 bit rate in bit/s; `None`: about 0.15 bit per pixel per
    /// frame (25 Mbit/s at 1080p60).
    pub bitrate: Option<u64>,
}

impl Settings {
    /// A frame rate as a fraction: whole numbers exactly, others to 1/1000.
    pub fn fps_fraction(fps: f64) -> (u32, u32) {
        if fps.fract() == 0.0 && fps >= 1.0 && fps <= f64::from(u32::MAX) {
            (fps as u32, 1)
        } else {
            ((fps * 1000.0).round().max(1.0) as u32, 1000)
        }
    }

    /// The H.264/H.265 bit rate in bit/s.
    pub fn bitrate(&self) -> u64 {
        self.bitrate.unwrap_or_else(|| {
            let fps = f64::from(self.fps.0) / f64::from(self.fps.1.max(1));
            (f64::from(self.width) * f64::from(self.height) * fps * 0.15) as u64
        })
    }
}

/// The first encoder for `codec` that the linked FFmpeg has.
pub fn encoder_for(codec: Codec) -> Result<&'static str, String> {
    if !ENABLED {
        return Err(not_built());
    }
    codec
        .encoders()
        .iter()
        .copied()
        .find(|e| available(e))
        .ok_or_else(|| {
            format!(
                "{}: this FFmpeg build has none of the encoders {} (only LGPL encoders are \
                 used; see docs/render.md)",
                codec.name(),
                codec.encoders().join(", ")
            )
        })
}

#[cfg(feature = "ffmpeg")]
fn available(encoder: &str) -> bool {
    super::ffmpeg::encoder_available(encoder)
}

#[cfg(not(feature = "ffmpeg"))]
fn available(_: &str) -> bool {
    false
}

fn not_built() -> String {
    "this build cannot write movie files: it was built without FFmpeg. Rebuild with \
     `--features atd-io/ffmpeg` (FFmpeg's LGPL shared libraries and development files are \
     needed; docs/render.md), or write a PNG sequence to a folder"
        .into()
}

/// Totals of a finished recording.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames encoded.
    pub frames: u64,
    /// Frames dropped because the encoder was behind (live only).
    pub dropped: u64,
}

/// Records frames on a worker thread.
#[derive(Debug)]
pub struct Recorder {
    tx: Option<SyncSender<Vec<u8>>>,
    free: Receiver<Vec<u8>>,
    /// The worker's report on opening the file.
    opened: Receiver<Result<(), String>>,
    thread: Option<JoinHandle<Result<u64, String>>>,
    /// The encoder's name, once it opened.
    pub encoder: &'static str,
    pub width: u32,
    pub height: u32,
    dropped: u64,
    submitted: u64,
}

impl Recorder {
    /// Starts recording: chooses the encoder and opens the file on the
    /// worker thread. Opening errors (no hardware encoder, a folder that
    /// does not exist) come back from the first push or from
    /// [`Self::finish`]; call [`Self::wait_open`] to have them now.
    pub fn start(settings: Settings) -> Result<Self, String> {
        let encoder = encoder_for(settings.codec)?;
        if settings.width == 0 || settings.height == 0 {
            return Err("nothing to record: the image has no size".into());
        }
        if settings.width % 2 == 1 || settings.height % 2 == 1 {
            return Err(format!(
                "{}x{}: movie encoders need an even width and height; change the resolution",
                settings.width, settings.height
            ));
        }
        let len = settings.width as usize * settings.height as usize * 4;
        let (tx, rx) = sync_channel::<Vec<u8>>(POOL);
        let (free_tx, free) = sync_channel::<Vec<u8>>(POOL);
        for _ in 0..POOL {
            free_tx
                .send(vec![0u8; len])
                .map_err(|_| "recorder pool".to_owned())?;
        }
        let (opened_tx, opened) = sync_channel::<Result<(), String>>(1);
        let (width, height) = (settings.width, settings.height);
        let thread = std::thread::Builder::new()
            .name("atd-record".into())
            .spawn(move || encode_thread(&settings, encoder, &opened_tx, &rx, &free_tx))
            .map_err(|e| format!("cannot start the recording thread: {e}"))?;
        Ok(Self {
            tx: Some(tx),
            free,
            opened,
            thread: Some(thread),
            encoder,
            width,
            height,
            dropped: 0,
            submitted: 0,
        })
    }

    /// Waits until the worker has opened the file; its error otherwise.
    pub fn wait_open(&mut self) -> Result<(), String> {
        match self.opened.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(self.take_error()),
        }
    }

    fn take_error(&mut self) -> String {
        self.tx = None;
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(Err(e))) => e,
            Some(Err(_)) => "the recording thread panicked".into(),
            _ => "the recording stopped".into(),
        }
    }

    /// A free frame buffer (width × height × 4 bytes) without waiting;
    /// `None` when the encoder is behind: the frame is counted as dropped.
    pub fn try_frame(&mut self) -> Option<Vec<u8>> {
        match self.free.try_recv() {
            Ok(buf) => Some(buf),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                self.dropped += 1;
                None
            }
        }
    }

    /// Hands a filled buffer from [`Self::try_frame`] to the encoder.
    pub fn submit(&mut self, frame: Vec<u8>) -> Result<(), String> {
        let sent = self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.try_send(frame).is_ok());
        if sent {
            self.submitted += 1;
            Ok(())
        } else {
            Err(self.take_error())
        }
    }

    /// Encodes `rgba`, waiting for a free buffer: no frame is dropped.
    pub fn push_wait(&mut self, rgba: &[u8]) -> Result<(), String> {
        let Ok(mut buf) = self.free.recv() else {
            return Err(self.take_error());
        };
        if buf.len() != rgba.len() {
            return Err(format!(
                "a frame of {} bytes, the recording is {}x{}",
                rgba.len(),
                self.width,
                self.height
            ));
        }
        buf.copy_from_slice(rgba);
        self.submit(buf)
    }

    /// Frames dropped so far (live).
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Frames handed to the encoder so far.
    pub fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Whether the worker has stopped (an error); [`Self::finish`] says why.
    pub fn failed(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Encodes what is queued, closes the file and returns the totals.
    pub fn finish(mut self) -> Result<Stats, String> {
        self.tx = None;
        let frames = match self.thread.take().map(JoinHandle::join) {
            Some(Ok(r)) => r?,
            Some(Err(_)) => return Err("the recording thread panicked".into()),
            None => return Err("the recording stopped".into()),
        };
        Ok(Stats {
            frames,
            dropped: self.dropped,
        })
    }
}

#[cfg(feature = "ffmpeg")]
fn encode_thread(
    s: &Settings,
    encoder: &str,
    opened: &SyncSender<Result<(), String>>,
    rx: &Receiver<Vec<u8>>,
    free: &SyncSender<Vec<u8>>,
) -> Result<u64, String> {
    let open = super::ffmpeg::Encoder::open(
        &s.path,
        encoder,
        (s.width, s.height),
        s.fps,
        s.codec.profile(),
        s.bitrate(),
    )
    .map_err(|e| format!("{}: {e}", s.path.display()));
    let _ = opened.send(open.as_ref().map(|_| ()).map_err(Clone::clone));
    let mut enc = open?;
    let mut frames = 0u64;
    while let Ok(frame) = rx.recv() {
        enc.push(&frame)
            .map_err(|e| format!("{}: frame {frames}: {e}", s.path.display()))?;
        frames += 1;
        let _ = free.send(frame);
    }
    enc.finish()
        .map_err(|e| format!("{}: {e}", s.path.display()))?;
    Ok(frames)
}

#[cfg(not(feature = "ffmpeg"))]
fn encode_thread(
    _: &Settings,
    _: &str,
    _: &SyncSender<Result<(), String>>,
    _: &Receiver<Vec<u8>>,
    _: &SyncSender<Vec<u8>>,
) -> Result<u64, String> {
    Err(not_built())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_parse_and_default_by_extension() {
        for (name, codec) in Codec::NAMES {
            assert_eq!(Codec::parse(name), Some(codec));
            assert_eq!(codec.name(), name);
            assert!(
                codec.encoders().iter().all(|e| !e.starts_with("lib")),
                "{name}: only LGPL encoders (never libx264/libx265)"
            );
        }
        assert_eq!(Codec::for_path(Path::new("a.MOV")), Some(Codec::ProRes422));
        assert_eq!(Codec::for_path(Path::new("a.mp4")), Some(Codec::H264));
        assert_eq!(Codec::for_path(Path::new("a.png")), None);
        assert_eq!(Settings::fps_fraction(60.0), (60, 1));
        assert_eq!(Settings::fps_fraction(29.97), (29970, 1000));
    }

    #[test]
    fn without_ffmpeg_start_explains() {
        if ENABLED {
            return;
        }
        let e = Recorder::start(Settings {
            path: "x.mov".into(),
            width: 64,
            height: 32,
            fps: (30, 1),
            codec: Codec::ProRes422,
            bitrate: None,
        })
        .unwrap_err();
        assert!(e.contains("--features atd-io/ffmpeg"), "{e}");
    }
}
