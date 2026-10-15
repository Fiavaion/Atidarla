//! Minimal QuickTime/MP4 demuxer: the first video track's sample table
//! (where each frame lives in the file and when it plays). Enough for
//! HAP in `.mov`; other codecs go through FFmpeg (PRD §5.2).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// One video frame's location in the file and its presentation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    /// Start time in track timescale units.
    pub time: u64,
}

/// The first video track of a movie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// Sample description four-character code, e.g. `Hap1`.
    pub codec: [u8; 4],
    pub width: u32,
    pub height: u32,
    /// Time units per second.
    pub timescale: u32,
    /// Total duration in timescale units.
    pub duration: u64,
    pub samples: Vec<Sample>,
}

impl Track {
    pub fn codec_str(&self) -> String {
        String::from_utf8_lossy(&self.codec).into_owned()
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration as f64 / f64::from(self.timescale.max(1))
    }

    /// The frame showing at `seconds` (clamped to the track).
    pub fn frame_at(&self, seconds: f64) -> usize {
        let t = (seconds.max(0.0) * f64::from(self.timescale)) as u64;
        match self.samples.binary_search_by(|s| s.time.cmp(&t)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MovError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a QuickTime/MP4 movie: {0}")]
    Format(String),
    #[error("no video track")]
    NoVideo,
}

fn be_u32(b: &[u8], at: usize) -> Result<u32, MovError> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes(s.try_into().expect("4 bytes")))
        .ok_or_else(|| MovError::Format("truncated atom".into()))
}

fn be_u64(b: &[u8], at: usize) -> Result<u64, MovError> {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes(s.try_into().expect("8 bytes")))
        .ok_or_else(|| MovError::Format("truncated atom".into()))
}

fn be_u16(b: &[u8], at: usize) -> Result<u16, MovError> {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes(s.try_into().expect("2 bytes")))
        .ok_or_else(|| MovError::Format("truncated atom".into()))
}

/// Iterates the child atoms of `data` as `(type, body)`.
fn atoms(data: &[u8]) -> impl Iterator<Item = Result<([u8; 4], &[u8]), MovError>> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        if at + 8 > data.len() {
            return None;
        }
        let size = u32::from_be_bytes(data[at..at + 4].try_into().expect("4")) as u64;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().expect("4");
        let (header, size) = match size {
            1 => match data.get(at + 8..at + 16) {
                Some(s) => (16, u64::from_be_bytes(s.try_into().expect("8"))),
                None => return Some(Err(MovError::Format("truncated atom".into()))),
            },
            0 => (8, (data.len() - at) as u64),
            n => (8, n),
        };
        let end = at as u64 + size;
        if size < header as u64 || end > data.len() as u64 {
            return Some(Err(MovError::Format(format!(
                "atom {} overruns its parent",
                String::from_utf8_lossy(&kind)
            ))));
        }
        let body = &data[at + header..end as usize];
        at = end as usize;
        Some(Ok((kind, body)))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>, MovError> {
    for atom in atoms(data) {
        let (k, body) = atom?;
        if &k == kind {
            return Ok(Some(body));
        }
    }
    Ok(None)
}

/// Reads the `moov` atom of a file without loading the media data.
fn read_moov(file: &mut File) -> Result<Vec<u8>, MovError> {
    let len = file.metadata()?.len();
    let mut at = 0u64;
    while at + 8 <= len {
        file.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 16];
        file.read_exact(&mut head[..8])?;
        let mut size = u64::from(u32::from_be_bytes(head[..4].try_into().expect("4")));
        let mut header = 8;
        if size == 1 {
            file.read_exact(&mut head[8..16])?;
            size = u64::from_be_bytes(head[8..16].try_into().expect("8"));
            header = 16;
        } else if size == 0 {
            size = len - at;
        }
        if size < header || at + size > len {
            return Err(MovError::Format("top-level atom overruns the file".into()));
        }
        if &head[4..8] == b"moov" {
            let mut body = vec![0u8; (size - header) as usize];
            file.read_exact(&mut body)?;
            return Ok(body);
        }
        at += size;
    }
    Err(MovError::Format("no moov atom".into()))
}

/// Parses the first video track of the movie at `path`.
pub fn open(path: &Path) -> Result<Track, MovError> {
    let mut file = File::open(path)?;
    let moov = read_moov(&mut file)?;
    for atom in atoms(&moov) {
        let (kind, trak) = atom?;
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = child(trak, b"mdia")? else {
            continue;
        };
        let is_video = match child(mdia, b"hdlr")? {
            Some(h) => h.get(8..12) == Some(b"vide"),
            None => false,
        };
        if !is_video {
            continue;
        }
        return parse_track(mdia);
    }
    Err(MovError::NoVideo)
}

fn need<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<&'a [u8], MovError> {
    child(data, kind)?
        .ok_or_else(|| MovError::Format(format!("missing {} atom", String::from_utf8_lossy(kind))))
}

fn parse_track(mdia: &[u8]) -> Result<Track, MovError> {
    let mdhd = need(mdia, b"mdhd")?;
    let (timescale, duration) = match mdhd.first() {
        Some(1) => (be_u32(mdhd, 20)?, be_u64(mdhd, 24)?),
        _ => (be_u32(mdhd, 12)?, u64::from(be_u32(mdhd, 16)?)),
    };
    let stbl = need(need(mdia, b"minf")?, b"stbl")?;

    // Sample description: codec and size.
    let stsd = need(stbl, b"stsd")?;
    let entry = stsd
        .get(8..)
        .ok_or_else(|| MovError::Format("empty stsd".into()))?;
    let codec: [u8; 4] = entry
        .get(4..8)
        .and_then(|c| c.try_into().ok())
        .ok_or_else(|| MovError::Format("truncated stsd".into()))?;
    // size(4) format(4) reserved(6) dref(2) version(2) revision(2) vendor(4)
    // temporal(4) spatial(4) width(2) height(2)
    let width = u32::from(be_u16(entry, 32)?);
    let height = u32::from(be_u16(entry, 34)?);

    // Sizes.
    let stsz = need(stbl, b"stsz")?;
    let uniform = be_u32(stsz, 4)?;
    let count = be_u32(stsz, 8)? as usize;
    let sizes: Vec<u32> = if uniform != 0 {
        vec![uniform; count]
    } else {
        (0..count)
            .map(|i| be_u32(stsz, 12 + i * 4))
            .collect::<Result<_, _>>()?
    };

    // Chunk offsets.
    let chunks: Vec<u64> = if let Some(stco) = child(stbl, b"stco")? {
        let n = be_u32(stco, 4)? as usize;
        (0..n)
            .map(|i| be_u32(stco, 8 + i * 4).map(u64::from))
            .collect::<Result<_, _>>()?
    } else {
        let co64 = need(stbl, b"co64")?;
        let n = be_u32(co64, 4)? as usize;
        (0..n)
            .map(|i| be_u64(co64, 8 + i * 8))
            .collect::<Result<_, _>>()?
    };

    // Samples per chunk: runs of (first chunk, samples per chunk).
    let stsc = need(stbl, b"stsc")?;
    let runs: Vec<(u32, u32)> = (0..be_u32(stsc, 4)? as usize)
        .map(|i| Ok((be_u32(stsc, 8 + i * 12)?, be_u32(stsc, 12 + i * 12)?)))
        .collect::<Result<_, MovError>>()?;

    // Durations: runs of (count, delta).
    let stts = need(stbl, b"stts")?;
    let mut times = Vec::with_capacity(count);
    let mut t = 0u64;
    for i in 0..be_u32(stts, 4)? as usize {
        let n = be_u32(stts, 8 + i * 8)?;
        let delta = be_u32(stts, 12 + i * 8)?;
        for _ in 0..n {
            times.push(t);
            t += u64::from(delta);
        }
    }

    let mut samples = Vec::with_capacity(count);
    let mut sample = 0usize;
    for (c, &chunk_offset) in chunks.iter().enumerate() {
        let chunk_no = c as u32 + 1;
        let per_chunk = runs
            .iter()
            .take_while(|(first, _)| *first <= chunk_no)
            .last()
            .map_or(1, |(_, n)| *n);
        let mut offset = chunk_offset;
        for _ in 0..per_chunk {
            let Some(&size) = sizes.get(sample) else {
                break;
            };
            samples.push(Sample {
                offset,
                size,
                time: times.get(sample).copied().unwrap_or(t),
            });
            offset += u64::from(size);
            sample += 1;
        }
    }
    if samples.len() != count {
        return Err(MovError::Format(format!(
            "sample table lists {count} frames but chunks hold {}",
            samples.len()
        )));
    }
    Ok(Track {
        codec,
        width,
        height,
        timescale,
        duration: duration.max(t),
        samples,
    })
}

/// Reads frames from a movie file into a reused buffer.
#[derive(Debug)]
pub struct FrameReader {
    file: File,
}

impl FrameReader {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
        })
    }

    /// Reads `sample` into `buf` (resized to fit).
    pub fn read(&mut self, sample: &Sample, buf: &mut Vec<u8>) -> std::io::Result<()> {
        buf.resize(sample.size as usize, 0);
        self.file.seek(SeekFrom::Start(sample.offset))?;
        self.file.read_exact(buf)
    }
}
