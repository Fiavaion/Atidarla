//! Chasing external timecode (FR-T3): the playhead follows MTC or LTC with
//! an offset, keeps going through short dropouts (freewheel) and re-locks.
//!
//! [`Chase`] works on positions in seconds and the local clock (`now`, in
//! seconds), so any source can feed it: [`MtcInput`] (MIDI quarter frames,
//! through `lsr_io::timecode::MtcDecoder`), [`LtcInput`] (audio, through
//! `LtcDecoder`), or a Timecode In node's `time` channel. Nothing here
//! allocates or blocks.

use lsr_io::timecode::{LtcDecoder, MtcDecoder};

/// How a [`Chase`] locks and freewheels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChaseConfig {
    /// Seconds added to the timecode: timeline time = timecode + offset.
    pub offset: f64,
    /// Keep running this many frames after the code stops, then stop.
    pub freewheel_frames: u32,
    /// Consecutive consistent readings needed to lock.
    pub lock_readings: u32,
}

impl Default for ChaseConfig {
    fn default() -> Self {
        Self {
            offset: 0.0,
            freewheel_frames: 12,
            lock_readings: 2,
        }
    }
}

/// Where a [`Chase`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaseState {
    /// No code (never received, or lost after freewheeling).
    NoCode,
    /// Code is arriving but not yet consistent.
    Locking,
    Locked,
    /// The code stopped; running on the local clock.
    Freewheel,
}

/// A readings gap longer than this many frames is a dropout. MTC sends a
/// full position every two frames, so less than that would be noise.
const DROPOUT_FRAMES: f64 = 3.0;

/// A reading may differ from the extrapolated position by this many
/// frames and still count as consistent (LTC readings arrive with audio
/// blocks, up to one block late).
const TOLERANCE_FRAMES: f64 = 1.0;

/// Follows timecode readings (see the module docs).
#[derive(Debug, Clone)]
pub struct Chase {
    pub config: ChaseConfig,
    state: ChaseState,
    /// The last reading: (timecode seconds, local time it was true at).
    last: Option<(f64, f64)>,
    fps: f64,
    good: u32,
}

impl Chase {
    pub fn new(config: ChaseConfig) -> Self {
        Self {
            config,
            state: ChaseState::NoCode,
            last: None,
            fps: 25.0,
            good: 0,
        }
    }

    pub fn state(&self) -> ChaseState {
        self.state
    }

    /// The code's frame rate (from the last reading).
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Forgets everything (another source chosen).
    pub fn reset(&mut self) {
        *self = Self::new(self.config);
    }

    /// The timecode said `position` seconds at local time `now`, at `fps`.
    pub fn reading(&mut self, position: f64, now: f64, fps: f64) {
        if fps > 0.0 {
            self.fps = fps;
        }
        let consistent = self.last.is_some_and(|(p, at)| {
            ((position - p) - (now - at)).abs() <= TOLERANCE_FRAMES / self.fps
        });
        // After a dropout, code that resumes where the freewheel expects it
        // re-locks at once; code that jumped needs a fresh lock.
        self.good = if consistent { self.good + 1 } else { 1 };
        self.last = Some((position, now));
        self.state = if self.good >= self.config.lock_readings {
            ChaseState::Locked
        } else {
            ChaseState::Locking
        };
    }

    /// The timeline time at local time `now` (timecode + offset), while
    /// locked or freewheeling; `None` otherwise.
    pub fn position(&mut self, now: f64) -> Option<f64> {
        let (p, at) = self.last?;
        let age = now - at;
        let frame = 1.0 / self.fps;
        if matches!(self.state, ChaseState::Locked | ChaseState::Freewheel) {
            if age > (DROPOUT_FRAMES + f64::from(self.config.freewheel_frames)) * frame {
                self.state = ChaseState::NoCode;
            } else if age > DROPOUT_FRAMES * frame {
                self.state = ChaseState::Freewheel;
            }
        } else if self.state == ChaseState::Locking && age > DROPOUT_FRAMES * frame {
            self.state = ChaseState::NoCode;
            self.good = 0;
        }
        matches!(self.state, ChaseState::Locked | ChaseState::Freewheel)
            .then(|| p + age + self.config.offset)
    }
}

/// MIDI Time Code into a [`Chase`].
#[derive(Debug, Default, Clone)]
pub struct MtcInput {
    decoder: MtcDecoder,
}

impl MtcInput {
    /// One quarter-frame message (`F1 <piece << 4 | value>`) received at
    /// local time `now`.
    pub fn quarter_frame(&mut self, data: u8, now: f64, chase: &mut Chase) {
        if let Some(tc) = self.decoder.quarter_frame(data >> 4, data & 0x0F) {
            chase.reading(tc.seconds(), now, f64::from(tc.fps));
        }
    }
}

/// Linear Timecode audio into a [`Chase`].
#[derive(Debug, Clone)]
pub struct LtcInput {
    decoder: LtcDecoder,
    rate: f32,
}

impl LtcInput {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            decoder: LtcDecoder::new(sample_rate),
            rate: sample_rate,
        }
    }

    /// A block of samples whose last sample is at local time `now`.
    pub fn process(&mut self, samples: &[f32], now: f64, chase: &mut Chase) {
        if let Some(tc) = self.decoder.process(samples, self.rate) {
            // A frame completes when its last bit ends: the code is then
            // at the start of the next frame.
            let fps = f64::from(tc.fps);
            let frame = if tc.drop_frame() {
                1001.0 / 30_000.0
            } else {
                1.0 / fps
            };
            chase.reading(tc.seconds() + frame, now, fps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsr_io::timecode::{Timecode, encode_ltc_frame};

    /// The eight quarter-frame data bytes for the frame `tc`.
    fn quarter_frames(tc: &Timecode) -> [u8; 8] {
        let rate: u8 = match tc.fps {
            24.0 => 0,
            25.0 => 1,
            f if (f - 29.97).abs() < 0.01 => 2,
            _ => 3,
        };
        let v = [
            tc.frames & 0x0F,
            tc.frames >> 4,
            tc.seconds & 0x0F,
            tc.seconds >> 4,
            tc.minutes & 0x0F,
            tc.minutes >> 4,
            tc.hours & 0x0F,
            (tc.hours >> 4) | (rate << 1),
        ];
        std::array::from_fn(|i| ((i as u8) << 4) | v[i])
    }

    /// Sends MTC from `start` (frame count at `fps`) at local time `t0`,
    /// for `frames` frames, skipping quarter frames when `drop(frame)`.
    /// Calls `each(chase, now, true_position, frame)` after every quarter
    /// frame.
    #[allow(clippy::too_many_arguments)] // a test driver: all are knobs
    fn send_mtc(
        input: &mut MtcInput,
        chase: &mut Chase,
        fps: f32,
        start: i64,
        t0: f64,
        frames: i64,
        drop: impl Fn(i64) -> bool,
        mut each: impl FnMut(&mut Chase, f64, f64, i64),
    ) {
        let frame = 1.0 / f64::from(fps);
        let base = Timecode::from_frame_count(start, fps).seconds();
        for f in (0..frames).step_by(2) {
            let tc = Timecode::from_frame_count(start + f, fps);
            for (q, data) in quarter_frames(&tc).into_iter().enumerate() {
                let local = (f as f64 + q as f64 / 4.0) * frame;
                let now = t0 + local;
                if !drop(f) {
                    input.quarter_frame(data, now, chase);
                }
                each(chase, now, base + local, f);
            }
        }
    }

    #[test]
    fn mtc_locks_within_four_frames_and_tracks_within_a_frame() {
        for fps in [24.0f32, 25.0, 30.0] {
            let mut chase = Chase::new(ChaseConfig::default());
            let mut input = MtcInput::default();
            let frame = 1.0 / f64::from(fps);
            let mut locked_at = None;
            let start = Timecode {
                hours: 1,
                minutes: 0,
                seconds: 0,
                frames: 0,
                fps,
            }
            .frame_count();
            send_mtc(
                &mut input,
                &mut chase,
                fps,
                start,
                100.0,
                50,
                |_| false,
                |chase, now, truth, _| {
                    if let Some(p) = chase.position(now) {
                        locked_at.get_or_insert((now - 100.0) / frame);
                        assert!((p - truth).abs() < frame, "{fps}: {p} vs {truth}");
                    }
                },
            );
            let at = locked_at.expect("locks");
            eprintln!("MTC {fps} fps: locked after {at:.2} frames");
            // First full position after 7 quarter frames (1.75 frames), the
            // second, confirming it, two frames later.
            assert!(at <= 4.0, "{fps} fps: locked after {at} frames");
            assert_eq!(chase.state(), ChaseState::Locked);
        }
    }

    #[test]
    fn mtc_freewheels_through_a_dropout_then_relocks() {
        let fps = 25.0f32;
        let frame = 1.0 / f64::from(fps);
        let mut chase = Chase::new(ChaseConfig {
            freewheel_frames: 8,
            ..Default::default()
        });
        let mut input = MtcInput::default();
        let mut states = Vec::new();
        // Frames 20–29 missing (a 10-frame dropout); code resumes in place.
        send_mtc(
            &mut input,
            &mut chase,
            fps,
            90_000,
            0.0,
            60,
            |f| (20..30).contains(&f),
            |chase, now, truth, f| {
                let p = chase.position(now);
                states.push((f, chase.state()));
                if let Some(p) = p {
                    assert!((p - truth).abs() < frame, "frame {f}: {p} vs {truth}");
                }
            },
        );
        let state_at = |f: i64| states.iter().rev().find(|(g, _)| *g == f).unwrap().1;
        assert_eq!(state_at(18), ChaseState::Locked);
        assert_eq!(state_at(24), ChaseState::Freewheel);
        // Code resumes at frame 30: its first full position at 31.75 frames
        // re-locks at once, as it is where the freewheel expected.
        assert_eq!(state_at(32), ChaseState::Locked);

        // A dropout longer than the freewheel stops the playhead.
        let mut chase = Chase::new(ChaseConfig {
            freewheel_frames: 4,
            ..Default::default()
        });
        let mut input = MtcInput::default();
        let mut lost = false;
        send_mtc(
            &mut input,
            &mut chase,
            fps,
            0,
            0.0,
            40,
            |f| f >= 10,
            |chase, now, _, f| {
                if f >= 20 {
                    lost |= chase.position(now).is_none();
                }
            },
        );
        assert!(lost);
        assert_eq!(chase.state(), ChaseState::NoCode);
    }

    #[test]
    fn a_jump_in_the_code_relocks_at_the_new_place_with_the_offset() {
        let fps = 30.0f32;
        let frame = 1.0 / f64::from(fps);
        let mut chase = Chase::new(ChaseConfig {
            offset: -3600.0,
            ..Default::default()
        });
        let mut input = MtcInput::default();
        // An hour in: with offset −1 h the timeline starts at 0.
        send_mtc(
            &mut input,
            &mut chase,
            fps,
            108_000,
            0.0,
            20,
            |_| false,
            |_, _, _, _| {},
        );
        let p = chase.position(20.0 * frame).unwrap();
        assert!((p - 20.0 * frame).abs() < frame, "{p}");
        // The source seeks to 1 h + 60 s.
        let mut relocked = None;
        send_mtc(
            &mut input,
            &mut chase,
            fps,
            108_000 + 1800,
            20.0 * frame,
            20,
            |_| false,
            |chase, now, truth, _| {
                if chase.state() == ChaseState::Locked
                    && let Some(p) = chase.position(now)
                    && (p - (truth - 3600.0)).abs() < frame
                {
                    relocked.get_or_insert((now - 20.0 * frame) / frame);
                }
            },
        );
        let at = relocked.expect("relocks");
        eprintln!("MTC jump: relocked after {at:.2} frames");
        assert!(at <= 4.0, "relocked after {at} frames");
    }

    #[test]
    fn ltc_locks_within_three_frames_and_tracks_within_a_frame() {
        for (rate, fps) in [(48_000.0f32, 25.0f32), (48_000.0, 30.0), (44_100.0, 24.0)] {
            let frame = 1.0 / f64::from(fps);
            let start = Timecode {
                hours: 10,
                minutes: 0,
                seconds: 0,
                frames: 0,
                fps,
            };
            let mut audio = Vec::new();
            let mut high = false;
            for k in 0..40 {
                let tc = Timecode::from_frame_count(start.frame_count() + k, fps);
                encode_ltc_frame(&tc, rate, 0.5, &mut high, &mut audio);
            }
            let mut chase = Chase::new(ChaseConfig::default());
            let mut input = LtcInput::new(rate);
            // 60 Hz app frames of audio.
            let block = (rate / 60.0) as usize;
            let mut locked_at = None;
            for (i, chunk) in audio.chunks(block).enumerate() {
                let end = ((i * block + chunk.len()) as f64) / f64::from(rate);
                input.process(chunk, end, &mut chase);
                let truth = start.seconds() + end;
                if let Some(p) = chase.position(end) {
                    locked_at.get_or_insert(end / frame);
                    assert!(
                        (p - truth).abs() < frame,
                        "{fps} fps: {p} vs {truth} ({} frames off)",
                        (p - truth) / frame
                    );
                }
            }
            let at = locked_at.expect("locks");
            eprintln!("LTC {fps} fps at {rate} Hz: locked after {at:.2} frames");
            assert!(at <= 3.0, "{fps} fps: locked after {at} frames");
        }
    }
}
