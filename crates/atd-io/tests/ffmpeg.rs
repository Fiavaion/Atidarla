//! Video through FFmpeg (PRD A23): frames by index, in and out of order,
//! with and without hardware decoding, against FFmpeg's own decode of the
//! same clips. Runs with `--features atd-io/ffmpeg`.
#![cfg(feature = "ffmpeg")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use atd_io::video::ffmpeg::{Decoder, avcodec_version};
use atd_io::video::{MoviePlayer, hap::HapFormat};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Reference frames 0, 5, 11 and 15, decoded by the FFmpeg CLI to RGBA.
fn reference(stem: &str) -> Vec<(usize, Vec<u8>)> {
    let raw = std::fs::read(data(&format!("{stem}.f0-5-11-15.rgba"))).unwrap();
    let size = 64 * 32 * 4;
    [0, 5, 11, 15]
        .into_iter()
        .zip(raw.chunks_exact(size))
        .map(|(i, f)| (i, f.to_vec()))
        .collect()
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

const CLIPS: [(&str, &str); 3] = [
    ("h264.mp4", "h264"),
    ("vp9.webm", "vp9"),
    ("prores.mov", "prores"),
];

#[test]
fn frames_by_index_match_ffmpeg_in_any_order() {
    let (major, minor, _) = avcodec_version();
    eprintln!("libavcodec {major}.{minor}");
    for hardware in [false, true] {
        for (file, stem) in CLIPS {
            let mut d = Decoder::open(&data(file), hardware).unwrap();
            eprintln!(
                "{file}: {} via {}",
                d.codec,
                d.hardware.as_deref().unwrap_or("software")
            );
            assert_eq!((d.width, d.height, d.frames()), (64, 32, 16), "{file}");
            for (i, t) in d.times.iter().enumerate() {
                assert!(
                    (t - i as f64 / 8.0).abs() < 1e-6,
                    "{file}: frame {i} at {t}"
                );
            }
            let refs = reference(stem);
            let mut rgba = Vec::new();
            // Out of order: seeks backwards and forwards across keyframes.
            for &k in &[2usize, 0, 3, 1] {
                let (i, want) = &refs[k];
                d.frame(*i, &mut rgba).unwrap();
                let w = worst(&rgba, want);
                assert!(w <= 8, "{file} (hw {hardware}): frame {i} differs by {w}");
            }
            // In order, every frame; the references are among them.
            for i in 0..16 {
                d.frame(i, &mut rgba).unwrap();
                if let Some((_, want)) = refs.iter().find(|(r, _)| *r == i) {
                    let w = worst(&rgba, want);
                    assert!(w <= 8, "{file}: sequential frame {i} differs by {w}");
                }
            }
            assert!(d.frame(16, &mut rgba).is_err());
        }
    }
}

#[test]
fn movie_player_plays_other_codecs() {
    let player = MoviePlayer::open(data("h264.mp4"));
    let start = Instant::now();
    let refs = reference("h264");
    for (i, want) in &refs {
        player.request(*i);
        let frame = loop {
            if let Some(f) = player.frame(*i) {
                break f;
            }
            assert!(player.error().is_none(), "{:?}", player.error());
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "frame {i} decoded"
            );
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(frame.format, HapFormat::Rgba8);
        assert!(worst(&frame.data, want) <= 8);
    }
    let info = player.info().unwrap();
    assert!(info.codec.contains("H.264"), "{}", info.codec);
    assert!((info.duration - 2.0).abs() < 1e-6, "{}", info.duration);
}

#[test]
fn damaged_files_are_errors() {
    let dir = std::env::temp_dir().join(format!("atd-ffmpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let full = std::fs::read(data("h264.mp4")).unwrap();
    let mut seed = 0x9e37_79b9_u64;
    for round in 0..200 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut bytes = full.clone();
        if round % 2 == 0 {
            bytes.truncate((seed as usize) % bytes.len());
        } else {
            for k in 0..8 {
                let i = (seed as usize).wrapping_add(k * 977) % bytes.len();
                bytes[i] = (seed >> (k * 8)) as u8;
            }
        }
        let path = dir.join(format!("damaged{round}.mp4"));
        std::fs::write(&path, &bytes).unwrap();
        if let Ok(mut d) = Decoder::open(&path, false) {
            let mut rgba = Vec::new();
            for i in 0..d.frames() {
                let _ = d.frame(i, &mut rgba);
            }
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    assert!(Decoder::open(&data("missing.mp4"), true).is_err());
}
