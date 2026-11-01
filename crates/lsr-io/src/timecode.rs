//! SMPTE timecode: MIDI Time Code assembly ([`MtcDecoder`]) and Linear
//! Timecode audio decoding ([`LtcDecoder`]), written from the published
//! formats (MIDI 1.0 MTC; SMPTE 12M biphase-mark LTC). Neither allocates.

/// A timecode position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    /// Nominal frame rate: 24, 25, 29.97 (drop-frame) or 30.
    pub fps: f32,
}

impl Timecode {
    /// Position in seconds. Drop-frame counts are converted exactly.
    pub fn seconds(&self) -> f64 {
        let nominal = self.fps.round() as i64;
        let frames = self.frame_count();
        if self.drop_frame() {
            frames as f64 * 1001.0 / (nominal as f64 * 1000.0)
        } else {
            frames as f64 / nominal as f64
        }
    }

    pub fn drop_frame(&self) -> bool {
        (self.fps - 29.97).abs() < 0.01
    }

    /// Frames since 00:00:00:00 (labels skipped by drop-frame excluded).
    pub fn frame_count(&self) -> i64 {
        let nominal = self.fps.round() as i64;
        let total_minutes = i64::from(self.hours) * 60 + i64::from(self.minutes);
        let mut n =
            ((total_minutes * 60 + i64::from(self.seconds)) * nominal) + i64::from(self.frames);
        if self.drop_frame() {
            n -= 2 * (total_minutes - total_minutes / 10);
        }
        n
    }

    /// The timecode `n` frames from 00:00:00:00 at `fps`.
    pub fn from_frame_count(n: i64, fps: f32) -> Self {
        let nominal = fps.round() as i64;
        let mut n = n.rem_euclid(24 * 3600 * nominal);
        if (fps - 29.97).abs() < 0.01 {
            // Add back the skipped labels: 2 per minute except every 10th.
            let per_10min = 10 * 60 * 30 - 18;
            let d = n / per_10min;
            let m = n % per_10min;
            n += 18 * d
                + if m > 1 {
                    2 * ((m - 2) / (60 * 30 - 2))
                } else {
                    0
                };
        }
        let frames = n % nominal;
        let total_seconds = n / nominal;
        Self {
            hours: (total_seconds / 3600 % 24) as u8,
            minutes: (total_seconds / 60 % 60) as u8,
            seconds: (total_seconds % 60) as u8,
            frames: frames as u8,
            fps,
        }
    }
}

/// MTC rate code (0–3) to frames per second.
pub fn mtc_rate(code: u8) -> f32 {
    match code & 3 {
        0 => 24.0,
        1 => 25.0,
        2 => 29.97,
        _ => 30.0,
    }
}

/// Assembles timecode from MTC quarter-frame messages.
#[derive(Debug, Default, Clone)]
pub struct MtcDecoder {
    pieces: [u8; 8],
    seen: u8,
    next: u8,
}

impl MtcDecoder {
    /// Feeds one quarter frame; returns the current position when piece 7
    /// completes a full set received in order. The eight pieces take two
    /// frames to send, so the position returned is the encoded one plus two
    /// frames (forward playback).
    pub fn quarter_frame(&mut self, piece: u8, value: u8) -> Option<Timecode> {
        let piece = piece & 7;
        if piece != self.next {
            // Out of order (seek, reverse, dropped byte): start again.
            self.seen = 0;
            if piece != 0 {
                self.next = 0;
                return None;
            }
        }
        self.pieces[usize::from(piece)] = value & 0x0F;
        self.seen |= 1 << piece;
        self.next = (piece + 1) & 7;
        if piece != 7 || self.seen != 0xFF {
            return None;
        }
        self.seen = 0;
        let p = &self.pieces;
        let tc = Timecode {
            frames: p[0] | ((p[1] & 1) << 4),
            seconds: p[2] | ((p[3] & 3) << 4),
            minutes: p[4] | ((p[5] & 3) << 4),
            hours: p[6] | ((p[7] & 1) << 4),
            fps: mtc_rate(p[7] >> 1),
        };
        Some(Timecode::from_frame_count(tc.frame_count() + 2, tc.fps))
    }
}

/// LTC sync word, bits 64–79 as transmitted (bit 64 first), read LSB-first.
const SYNC: u16 = 0xBFFC;

/// Decodes LTC (biphase-mark, 80-bit frames) from one audio channel.
#[derive(Debug, Clone)]
pub struct LtcDecoder {
    /// Current signal state, with hysteresis.
    high: bool,
    /// Samples since the last transition.
    since_edge: f32,
    /// Estimated bit period in samples.
    period: f32,
    half_pending: bool,
    /// The last 80 bits, bit 0 of the frame at position 0 once aligned.
    bits: u128,
}

impl LtcDecoder {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            high: false,
            since_edge: 0.0,
            // 2000 bits/s (25 fps); 24–30 fps fall within the tolerance.
            period: sample_rate / 2000.0,
            half_pending: false,
            bits: 0,
        }
    }

    /// Feeds samples; returns the last frame completed in this block.
    pub fn process(&mut self, samples: &[f32], sample_rate: f32) -> Option<Timecode> {
        const THRESHOLD: f32 = 0.02;
        let mut decoded = None;
        for &s in samples {
            self.since_edge += 1.0;
            let flip = if self.high {
                s < -THRESHOLD
            } else {
                s > THRESHOLD
            };
            if !flip {
                // A missing edge for several bits means the signal stopped.
                if self.since_edge > self.period * 4.0 {
                    self.half_pending = false;
                }
                continue;
            }
            self.high = !self.high;
            let interval = self.since_edge;
            self.since_edge = 0.0;
            if interval > self.period * 2.5 {
                // Too long to be data: resynchronise.
                self.half_pending = false;
                continue;
            }
            if interval < self.period * 0.75 {
                // Half a bit: two in a row make a one.
                self.period = 0.95 * self.period + 0.05 * (2.0 * interval);
                if self.half_pending {
                    self.half_pending = false;
                    if let Some(tc) = self.push(true, sample_rate) {
                        decoded = Some(tc);
                    }
                } else {
                    self.half_pending = true;
                }
            } else {
                self.period = 0.95 * self.period + 0.05 * interval;
                self.half_pending = false;
                if let Some(tc) = self.push(false, sample_rate) {
                    decoded = Some(tc);
                }
            }
        }
        decoded
    }

    fn push(&mut self, bit: bool, sample_rate: f32) -> Option<Timecode> {
        self.bits = (self.bits >> 1) | (u128::from(bit) << 79);
        if (self.bits >> 64) as u16 != SYNC {
            return None;
        }
        let b = self.bits;
        let field = |at: u32, width: u32| ((b >> at) & ((1u128 << width) - 1)) as u8;
        let drop = field(10, 1) == 1;
        let bps = sample_rate / self.period;
        let fps = if drop {
            29.97
        } else {
            // Snap the measured rate to a standard one.
            [24.0f32, 25.0, 30.0]
                .into_iter()
                .min_by(|a, b| (a * 80.0 - bps).abs().total_cmp(&(b * 80.0 - bps).abs()))
                .unwrap_or(25.0)
        };
        let tc = Timecode {
            frames: field(0, 4) + 10 * field(8, 2),
            seconds: field(16, 4) + 10 * field(24, 3),
            minutes: field(32, 4) + 10 * field(40, 3),
            hours: field(48, 4) + 10 * field(56, 2),
            fps,
        };
        let valid = tc.frames < 30 && tc.seconds < 60 && tc.minutes < 60 && tc.hours < 24;
        valid.then_some(tc)
    }
}

/// Encodes one LTC frame as biphase-mark audio (±`level`), continuing from
/// `*high`. Used by tests, and the basis of a future Timecode Out.
pub fn encode_ltc_frame(
    tc: &Timecode,
    sample_rate: f32,
    level: f32,
    high: &mut bool,
    out: &mut Vec<f32>,
) {
    let bcd = |v: u8| (u128::from(v % 10), u128::from(v / 10));
    let (fu, ft) = bcd(tc.frames);
    let (su, st) = bcd(tc.seconds);
    let (mu, mt) = bcd(tc.minutes);
    let (hu, ht) = bcd(tc.hours);
    let mut bits: u128 = fu | (ft << 8) | (su << 16) | (st << 24) | (mu << 32) | (mt << 40);
    bits |= (hu << 48) | (ht << 56) | (u128::from(SYNC) << 64);
    if tc.drop_frame() {
        bits |= 1 << 10;
    }
    let per_bit = f64::from(sample_rate) / (f64::from(tc.fps.round()) * 80.0);
    // Sample positions continue from what `out` already holds.
    let origin = out.len() as f64;
    for i in 0..80 {
        let one = (bits >> i) & 1 == 1;
        let start = (origin + i as f64 * per_bit).round() as usize;
        let mid = (origin + (i as f64 + 0.5) * per_bit).round() as usize;
        let end = (origin + (i as f64 + 1.0) * per_bit).round() as usize;
        // A transition at the start of every bit, and mid-bit for a one.
        *high = !*high;
        for n in start..end {
            if one && n == mid {
                *high = !*high;
            }
            out.push(if *high { level } else { -level });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(h: u8, m: u8, s: u8, f: u8, fps: f32) -> Timecode {
        Timecode {
            hours: h,
            minutes: m,
            seconds: s,
            frames: f,
            fps,
        }
    }

    #[test]
    fn frame_counts_round_trip_including_drop_frame() {
        for fps in [24.0, 25.0, 30.0, 29.97] {
            for n in [
                0i64, 1, 1799, 1800, 1801, 17_982, 17_983, 107_892, 2_000_000,
            ] {
                let t = Timecode::from_frame_count(n, fps);
                assert_eq!(t.frame_count(), n, "{fps} fps, frame {n}: {t:?}");
            }
        }
        // Drop-frame skips labels 00 and 01 at each minute except tenths.
        let t = Timecode::from_frame_count(1800, 29.97);
        assert_eq!((t.minutes, t.seconds, t.frames), (1, 0, 2));
        let t = Timecode::from_frame_count(17_982, 29.97);
        assert_eq!((t.minutes, t.seconds, t.frames), (10, 0, 0));
        assert!((tc(1, 0, 0, 0, 25.0).seconds() - 3600.0).abs() < 1e-9);
        assert!((tc(0, 10, 0, 0, 29.97).seconds() - 600.0 * 1.001 * 29.97 / 30.0).abs() < 1e-3);
    }

    #[test]
    fn mtc_assembles_eight_quarter_frames_plus_two() {
        let mut d = MtcDecoder::default();
        // 01:02:03:04 at 25 fps: rate code 1 in piece 7 bits 1–2.
        let pieces = [4, 0, 3, 0, 2, 0, 1, 1 << 1];
        let mut got = None;
        for (i, v) in pieces.iter().enumerate() {
            got = d.quarter_frame(i as u8, *v);
        }
        assert_eq!(got, Some(tc(1, 2, 3, 6, 25.0)));
        // An out-of-order piece resets; the next full set decodes again.
        assert_eq!(d.quarter_frame(5, 0), None);
        for (i, v) in pieces.iter().enumerate().take(7) {
            assert_eq!(d.quarter_frame(i as u8, *v), None);
        }
        assert_eq!(d.quarter_frame(7, pieces[7]), Some(tc(1, 2, 3, 6, 25.0)));
    }

    #[test]
    fn ltc_round_trips_at_each_rate() {
        for (rate, fps) in [
            (48_000.0, 25.0),
            (48_000.0, 30.0),
            (44_100.0, 24.0),
            (48_000.0, 29.97),
        ] {
            let mut audio = Vec::new();
            let mut high = false;
            let start = tc(10, 59, 58, 3, fps).frame_count();
            for k in 0..12 {
                let t = Timecode::from_frame_count(start + k, fps);
                encode_ltc_frame(&t, rate, 0.5, &mut high, &mut audio);
            }
            let mut d = LtcDecoder::new(rate);
            let mut seen = Vec::new();
            // Uneven blocks, as audio arrives.
            for block in audio.chunks(733) {
                if let Some(t) = d.process(block, rate) {
                    seen.push(t);
                }
            }
            assert!(seen.len() >= 8, "{fps} fps: {} frames decoded", seen.len());
            let last = seen.last().unwrap();
            assert_eq!(last.fps, fps, "rate detected");
            let n = last.frame_count();
            assert!(n > start && n < start + 12, "{fps}: {last:?}");
            // Consecutive decodes advance by one frame.
            for w in seen.windows(2) {
                assert!(w[1].frame_count() - w[0].frame_count() >= 1);
            }
        }
    }

    #[test]
    fn ltc_survives_inverted_and_quiet_signals() {
        let mut audio = Vec::new();
        let mut high = true;
        for k in 0..6 {
            let t = Timecode::from_frame_count(1000 + k, 25.0);
            encode_ltc_frame(&t, 48_000.0, 0.05, &mut high, &mut audio);
        }
        let inverted: Vec<f32> = audio.iter().map(|v| -v).collect();
        let mut d = LtcDecoder::new(48_000.0);
        let got = d.process(&inverted, 48_000.0).unwrap();
        // The last frame's final bit ends at the next frame's first edge,
        // which never comes, so the last complete frame is 1004.
        assert_eq!(got.frame_count(), 1004);
    }
}
