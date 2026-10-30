//! Recording through FFmpeg (FR-S10, PRD A51): 30 frames of a known
//! pattern encoded with each LGPL encoder this machine has, decoded back
//! with the FFmpeg decoder and compared. Runs with `--features atd-io/ffmpeg`.
#![cfg(feature = "ffmpeg")]

use std::path::PathBuf;

use atd_io::video::ffmpeg::Decoder;
use atd_io::video::record::{Codec, Recorder, Settings, encoder_for};

const W: u32 = 128;
const H: u32 = 64;
const FRAMES: u32 = 30;

/// Smooth gradients that change every frame (lossy codecs keep them well),
/// alpha falling from left to right.
fn pattern(i: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            v.push((x * 255 / (W - 1)) as u8);
            v.push((y * 255 / (H - 1)) as u8);
            v.push((40 + i * 6) as u8);
            v.push(255 - (x * 255 / (W - 1)) as u8);
        }
    }
    v
}

fn out(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atd-record-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// (mean, max) absolute difference over the colour channels.
fn diff(a: &[u8], b: &[u8], alpha: bool) -> (f64, u8) {
    let (mut sum, mut max, mut n) = (0u64, 0u8, 0u64);
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..if alpha { 4 } else { 3 } {
            let d = pa[c].abs_diff(pb[c]);
            sum += u64::from(d);
            max = max.max(d);
            n += 1;
        }
    }
    (sum as f64 / n as f64, max)
}

/// Encodes, decodes and compares; `None` when this machine has no
/// encoder for the codec.
fn round_trip(codec: Codec, file: &str) -> Option<(String, f64, u8)> {
    let encoder = match encoder_for(codec) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skipping {}: {e}", codec.name());
            return None;
        }
    };
    let path = out(file);
    let mut rec = Recorder::start(Settings {
        path: path.clone(),
        width: W,
        height: H,
        fps: (30, 1),
        codec,
        bitrate: Some(8_000_000),
    })
    .unwrap();
    if let Err(e) = rec.wait_open() {
        // A hardware encoder FFmpeg knows but this machine lacks.
        eprintln!("skipping {} ({encoder}): {e}", codec.name());
        return None;
    }
    for i in 0..FRAMES {
        rec.push_wait(&pattern(i)).unwrap();
    }
    let stats = rec.finish().unwrap();
    assert_eq!((stats.frames, stats.dropped), (u64::from(FRAMES), 0));

    let mut d = Decoder::open(&path, false).unwrap();
    assert_eq!((d.width, d.height), (W, H), "{file}");
    assert_eq!(d.frames(), FRAMES as usize, "{file}: every frame");
    for (i, t) in d.times.iter().enumerate() {
        assert!(
            (t - i as f64 / 30.0).abs() < 1e-3,
            "{file}: frame {i} at {t}"
        );
    }
    let alpha = codec == Codec::ProRes4444;
    let (mut mean, mut max) = (0.0f64, 0u8);
    let mut rgba = Vec::new();
    for i in 0..FRAMES {
        d.frame(i as usize, &mut rgba).unwrap();
        let (m, x) = diff(&rgba, &pattern(i), alpha);
        mean = mean.max(m);
        max = max.max(x);
    }
    eprintln!(
        "{} via {encoder} ({}): worst frame mean |diff| {mean:.2}, max {max}",
        codec.name(),
        d.codec
    );
    let _ = std::fs::remove_file(&path);
    Some((encoder.to_owned(), mean, max))
}

#[test]
fn prores_round_trips_within_tolerance() {
    // prores_ks is part of every FFmpeg build.
    let (_, mean, max) = round_trip(Codec::ProRes422, "prores.mov").expect("prores_ks");
    // 10-bit 4:2:2 decoded to 8 bits: the FFmpeg CLI round trip of the
    // same frames gives mean 1.7, max 5.
    assert!(mean < 2.5 && max <= 8, "ProRes 422: mean {mean}, max {max}");
    let (_, mean, max) = round_trip(Codec::ProRes4444, "prores4444.mov").expect("prores_ks");
    assert!(
        mean < 2.5 && max <= 8,
        "ProRes 4444 (with alpha): mean {mean}, max {max}"
    );
}

#[test]
fn hardware_h264_and_h265_round_trip_within_tolerance() {
    // VideoToolbox on macOS; NVENC or Media Foundation on Windows where
    // present. Lossy at 8 Mbit/s: smooth gradients stay close.
    let mut ran = 0;
    for (codec, file) in [(Codec::H264, "h264.mp4"), (Codec::H265, "h265.mov")] {
        if let Some((_, mean, max)) = round_trip(codec, file) {
            assert!(
                mean < 3.0 && max <= 24,
                "{}: mean {mean}, max {max}",
                codec.name()
            );
            ran += 1;
        }
    }
    if cfg!(target_os = "macos") {
        assert_eq!(
            ran, 2,
            "VideoToolbox H.264 and H.265 are always there on macOS"
        );
    }
}

#[test]
fn bad_settings_fail_with_messages() {
    let odd = Recorder::start(Settings {
        path: out("odd.mov"),
        width: 63,
        height: 32,
        fps: (30, 1),
        codec: Codec::ProRes422,
        bitrate: None,
    })
    .unwrap_err();
    assert!(odd.contains("even width and height"), "{odd}");
    // ProRes cannot go in MP4.
    let mut rec = Recorder::start(Settings {
        path: out("prores.mp4"),
        width: 64,
        height: 32,
        fps: (30, 1),
        codec: Codec::ProRes422,
        bitrate: None,
    })
    .unwrap();
    let e = rec.wait_open().unwrap_err();
    assert!(e.contains("use .mov"), "{e}");
    // A folder that does not exist.
    let mut rec = Recorder::start(Settings {
        path: out("missing/x.mov"),
        width: 64,
        height: 32,
        fps: (30, 1),
        codec: Codec::ProRes422,
        bitrate: None,
    })
    .unwrap();
    assert!(rec.wait_open().is_err());
}
