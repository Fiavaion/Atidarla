//! The stamped test pattern shared by the Spout examples and the Spout SDK
//! interop peers (`docs/spikes/spout-interop/`): `readback::test_pattern(w,
//! h, seed)` with its first 16 bytes replaced by a stamp: frame (u32 LE),
//! seed (u32 LE) and send time (u64 LE, Windows FILETIME ticks of 100 ns,
//! as `GetSystemTimePreciseAsFileTime` returns, which `SystemTime` uses).

use std::time::{SystemTime, UNIX_EPOCH};

use lsr_interop::readback::test_pattern;

pub const STAMP_LEN: usize = 16;

/// FILETIME ticks (100 ns since 1601) now, comparable across processes.
pub fn now_ticks() -> u64 {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    11_644_473_600 * 10_000_000 + (unix.as_nanos() / 100) as u64
}

/// Writes the stamp for `frame` into the first bytes of `pixels`.
pub fn stamp(pixels: &mut [u8], frame: u32, seed: u8) {
    pixels[0..4].copy_from_slice(&frame.to_le_bytes());
    pixels[4..8].copy_from_slice(&u32::from(seed).to_le_bytes());
    pixels[8..16].copy_from_slice(&now_ticks().to_le_bytes());
}

/// The pattern for `frame`, stamped.
pub fn frame_pixels(width: u32, height: u32, frame: u32, seed: u8) -> Vec<u8> {
    let mut pixels = test_pattern(width, height, seed);
    stamp(&mut pixels, frame, seed);
    pixels
}

#[derive(Clone, Copy, Debug)]
pub struct Stamp {
    pub frame: u32,
    pub seed: u8,
    /// Milliseconds from the sender's stamp to now.
    pub age_ms: f64,
}

/// Reads the stamp and compares every byte after it with the pattern.
pub fn check(pixels: &[u8], width: u32, height: u32) -> Result<Stamp, String> {
    let u32_at = |at: usize| u32::from_le_bytes(pixels[at..at + 4].try_into().unwrap());
    let frame = u32_at(0);
    let seed = u8::try_from(u32_at(4)).map_err(|_| format!("bad seed {}", u32_at(4)))?;
    let sent = u64::from_le_bytes(pixels[8..16].try_into().unwrap());
    let age_ms = (now_ticks() as f64 - sent as f64) / 1e4;
    let expected = test_pattern(width, height, seed);
    let bad = pixels[STAMP_LEN..]
        .iter()
        .zip(&expected[STAMP_LEN..])
        .filter(|(a, b)| a != b)
        .count();
    if pixels.len() != expected.len() || bad > 0 {
        return Err(format!(
            "frame {frame}: {bad} bytes differ from the pattern"
        ));
    }
    Ok(Stamp {
        frame,
        seed,
        age_ms,
    })
}

/// "mean / p50 / p95 / max" of `values`, in ms.
pub fn summary(values: &mut [f64]) -> String {
    if values.is_empty() {
        return "n/a".into();
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    format!(
        "{mean:.3} / {:.3} / {:.3} / {:.3}",
        values[n / 2],
        values[n * 95 / 100],
        values[n - 1]
    )
}
