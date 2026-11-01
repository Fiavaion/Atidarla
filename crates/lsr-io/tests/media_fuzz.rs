//! Deterministic fuzzing of the MOV demuxer and HAP decoder with corrupted
//! copies of the test clips: media files are untrusted, so damage must
//! give errors, never panics or absurd allocations.

use std::path::PathBuf;

use lsr_io::video::hap::{self, HapFormat, HapFrame};
use lsr_io::video::{bc, mov};

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn corrupt(bytes: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = bytes.to_vec();
    for _ in 0..1 + rng.next(8) {
        if b.is_empty() {
            break;
        }
        let i = rng.next(b.len());
        match rng.next(5) {
            0 => b[i] = rng.next(256) as u8,
            // Big-endian size fields: make them huge or zero.
            1 => {
                for k in i..(i + 4).min(b.len()) {
                    b[k] = if rng.next(2) == 0 { 0xFF } else { 0 };
                }
            }
            2 => b.truncate(i),
            3 => {
                let n = rng.next(64);
                b.drain(i..(i + n).min(b.len()));
            }
            _ => {
                let piece: Vec<u8> = b[i..(i + rng.next(32)).min(b.len())].to_vec();
                let at = rng.next(b.len());
                b.splice(at..at, piece);
            }
        }
    }
    b
}

#[test]
fn corrupted_movies_never_panic() {
    let rounds = std::env::var("LSR_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    let clips = ["hap1.mov", "hap_q.mov", "hap1_chunked.mov", "hap_alpha.mov"];
    let originals: Vec<Vec<u8>> = clips
        .iter()
        .map(|c| std::fs::read(data(c)).unwrap())
        .collect();
    let dir = std::env::temp_dir().join(format!("lsr-media-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mov");
    let mut rng = Rng(0xFEED_FACE_CAFE_BEEF);
    let mut raw = Vec::new();
    let mut frame = HapFrame {
        format: HapFormat::Bc1,
        data: Vec::new(),
    };
    let mut rgba = Vec::new();
    for round in 0..rounds {
        let bytes = corrupt(&originals[rng.next(originals.len())], &mut rng);
        std::fs::write(&path, &bytes).unwrap();
        let Ok(track) = mov::open(&path) else {
            continue;
        };
        let len = bytes.len() as u64;
        for s in &track.samples {
            assert!(
                s.offset + u64::from(s.size) <= len,
                "round {round}: sample beyond the file ({} + {} > {len})",
                s.offset,
                s.size
            );
        }
        let Ok(mut reader) = mov::FrameReader::open(&path) else {
            continue;
        };
        for s in track.samples.iter().take(3) {
            if reader.read(s, &mut raw).is_err() {
                continue;
            }
            if hap::decode(&raw, track.width, track.height, &mut frame).is_ok() {
                let _ = bc::to_rgba(&frame, track.width, track.height, &mut rgba);
            }
        }
        let _ = track.frame_at(f64::from(round) * 0.1);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupted_hap_frames_never_panic() {
    let track = mov::open(&data("hap1_chunked.mov")).unwrap();
    let mut reader = mov::FrameReader::open(&data("hap1_chunked.mov")).unwrap();
    let mut raw = Vec::new();
    reader.read(&track.samples[0], &mut raw).unwrap();
    let mut rng = Rng(0xABCD_EF01_2345_6789);
    let mut frame = HapFrame {
        format: HapFormat::Bc1,
        data: Vec::new(),
    };
    let mut rgba = Vec::new();
    for _ in 0..5000 {
        let bytes = corrupt(&raw, &mut rng);
        // Odd sizes too: decode must check, not trust, the frame size.
        let (w, h) = [(64, 32), (1, 1), (63, 31), (4096, 4096)][rng.next(4)];
        if hap::decode(&bytes, w, h, &mut frame).is_ok() {
            let _ = bc::to_rgba(&frame, w, h, &mut rgba);
        }
    }
}
