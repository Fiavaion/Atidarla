//! HAP decoding against FFmpeg's own decode of the same clips
//! (`tests/data/README.md`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use atd_io::video::hap::{self, HapFormat, HapFrame};
use atd_io::video::{MoviePlayer, bc, mov};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Decodes frame 2 on the CPU and compares it with FFmpeg's decode.
/// Returns the largest per-channel difference.
fn frame2(name: &str) -> (HapFormat, u8) {
    let track = mov::open(&data(&format!("{name}.mov"))).unwrap();
    assert_eq!((track.width, track.height), (64, 32));
    assert_eq!(track.samples.len(), 8);
    let mut reader = mov::FrameReader::open(&data(&format!("{name}.mov"))).unwrap();
    let mut raw = Vec::new();
    reader.read(&track.samples[2], &mut raw).unwrap();
    let mut frame = HapFrame {
        format: HapFormat::Bc1,
        data: Vec::new(),
    };
    hap::decode(&raw, track.width, track.height, &mut frame).unwrap();
    let mut rgba = Vec::new();
    bc::to_rgba(&frame, 64, 32, &mut rgba).unwrap();
    let want = std::fs::read(data(&format!("{name}.f2.rgba"))).unwrap();
    assert_eq!(rgba.len(), want.len());
    let worst = rgba
        .iter()
        .zip(&want)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    (frame.format, worst)
}

#[test]
fn hap_formats_match_ffmpeg() {
    for (name, format, tolerance) in [
        ("hap1", HapFormat::Bc1, 2),
        ("hap1_chunked", HapFormat::Bc1, 2),
        ("hap1_raw", HapFormat::Bc1, 2),
        ("hap_alpha", HapFormat::Bc3, 2),
        // FFmpeg converts YCoCg in integer maths; allow its rounding.
        ("hap_q", HapFormat::YCoCgBc3, 4),
    ] {
        let (got, worst) = frame2(name);
        assert_eq!(got, format, "{name}");
        assert!(worst <= tolerance, "{name}: max difference {worst}");
    }
}

#[test]
fn frame_times_follow_the_sample_table() {
    let track = mov::open(&data("hap1.mov")).unwrap();
    assert_eq!(track.codec_str(), "Hap1");
    assert!((track.duration_secs() - 1.0).abs() < 1e-6);
    assert_eq!(track.frame_at(0.0), 0);
    assert_eq!(track.frame_at(0.25), 2);
    assert_eq!(track.frame_at(0.374), 2);
    assert_eq!(track.frame_at(5.0), 7);
}

#[test]
fn player_decodes_ahead_without_blocking() {
    let start = Instant::now();
    let player = MoviePlayer::open(data("hap_q.mov"));
    assert!(start.elapsed() < Duration::from_millis(50));
    player.request(6);
    let frame = loop {
        if let Some(f) = player.frame(6) {
            break f;
        }
        assert!(player.error().is_none(), "{:?}", player.error());
        assert!(start.elapsed() < Duration::from_secs(5), "frame decoded");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(frame.format, HapFormat::YCoCgBc3);
    let info = player.info().unwrap();
    assert_eq!(
        (info.frames, info.width, info.codec.as_str()),
        (8, 64, "HapY")
    );
    assert_eq!(info.frame_at(0.8), 6);
    // Lookahead: the next frames follow (wrapping at the end).
    while player.frame(7).is_none() || player.frame(0).is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "lookahead decoded"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn unsupported_files_report_errors() {
    for (file, expect) in [("README.md", "not a QuickTime"), ("missing.mov", "")] {
        let player = MoviePlayer::open(data(file));
        let start = Instant::now();
        while player.error().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{file}: error reported"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            player.error().unwrap().contains(expect),
            "{:?}",
            player.error()
        );
    }
}
