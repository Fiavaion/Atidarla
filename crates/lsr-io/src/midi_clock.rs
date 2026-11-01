//! MIDI beat clock (24 pulses per quarter note): following an external
//! master ([`ClockFollower`]).
//!
//! The follower is pure logic over [`Message`]s with arrival times, so it
//! runs on the cook path without blocking and is tested from synthetic
//! streams, no device needed.

use crate::midi::Message;

/// Clock pulses per quarter note.
pub const PPQN: u32 = 24;
/// MIDI beats (the Song Position unit) are sixteenth notes: 6 pulses.
const TICKS_PER_SIXTEENTH: f64 = 6.0;
/// Timestamps kept for the tempo estimate: 23 intervals, a little under a
/// beat, so per-message jitter averages out while tempo changes still show
/// within a beat.
const WINDOW: usize = 24;
/// A gap this long between pulses (µs) restarts the tempo estimate.
const STALL_US: u64 = 500_000;

/// Follows a MIDI clock master: tempo from the pulse spacing, position from
/// counted pulses, transport from Start/Continue/Stop/Song Position.
///
/// A port that only ever sends pulses (no Start) runs from position 0, as
/// much DJ and drum-machine gear does; Stop pauses it and Start rewinds.
#[derive(Debug, Clone)]
pub struct ClockFollower {
    running: bool,
    /// Position in pulses at the last pulse.
    pos_ticks: f64,
    /// Arrival time of the last pulse; `None` until one arrives after a
    /// Start, Continue or Song Position (the next pulse lands on `pos_ticks`).
    last_us: Option<u64>,
    stamps: [u64; WINDOW],
    count: usize,
    head: usize,
    /// Microseconds per pulse, from the window (0 = unknown).
    period_us: f64,
    /// Last position given out, so extrapolation never steps backwards
    /// between pulses.
    shown: f64,
    /// Counts jumps (Start, Continue, Song Position): the beat clock turns
    /// a change into a downbeat/beat pulse.
    jumps: u32,
}

impl Default for ClockFollower {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockFollower {
    pub fn new() -> Self {
        Self {
            running: true,
            pos_ticks: 0.0,
            last_us: None,
            stamps: [0; WINDOW],
            count: 0,
            head: 0,
            period_us: 0.0,
            shown: 0.0,
            jumps: 0,
        }
    }

    /// Takes one message; anything that is not clock transport is ignored.
    pub fn handle(&mut self, msg: &Message) {
        match *msg {
            Message::Clock { micros } => self.tick(micros),
            Message::Start { .. } => {
                self.running = true;
                self.jump(0.0);
            }
            Message::Continue { .. } => {
                self.running = true;
                self.jump(self.pos_ticks);
            }
            Message::Stop { .. } => {
                self.running = false;
                self.pos_ticks = self.shown;
                self.last_us = None;
            }
            Message::SongPosition { sixteenths } => {
                self.jump(f64::from(sixteenths) * TICKS_PER_SIXTEENTH);
            }
            _ => {}
        }
    }

    /// The next pulse lands exactly on `ticks`.
    fn jump(&mut self, ticks: f64) {
        self.pos_ticks = ticks;
        self.shown = ticks;
        self.last_us = None;
        self.jumps = self.jumps.wrapping_add(1);
    }

    fn tick(&mut self, micros: u64) {
        if self
            .last_stamp()
            .is_some_and(|last| micros.saturating_sub(last) > STALL_US || micros < last)
        {
            self.count = 0;
            self.head = 0;
        }
        self.stamps[self.head] = micros;
        self.head = (self.head + 1) % WINDOW;
        self.count = (self.count + 1).min(WINDOW);
        if self.count >= 2 {
            let oldest = self.stamps[(self.head + WINDOW - self.count) % WINDOW];
            let span = micros.saturating_sub(oldest) as f64;
            if span > 0.0 {
                self.period_us = span / (self.count - 1) as f64;
            }
        }
        if !self.running {
            return;
        }
        if self.last_us.is_some() {
            self.pos_ticks += 1.0;
        }
        self.last_us = Some(micros);
    }

    fn last_stamp(&self) -> Option<u64> {
        (self.count > 0).then(|| self.stamps[(self.head + WINDOW - 1) % WINDOW])
    }

    /// Whether the master is playing.
    pub fn running(&self) -> bool {
        self.running
    }

    /// Tempo from the pulse spacing, once two pulses have arrived.
    pub fn bpm(&self) -> Option<f64> {
        (self.period_us > 0.0).then(|| 60e6 / (self.period_us * f64::from(PPQN)))
    }

    /// Increments on every Start, Continue and Song Position.
    pub fn jumps(&self) -> u32 {
        self.jumps
    }

    /// Song position in beats (quarter notes) at `now_micros`, on the same
    /// clock as the pulses' arrival times. Between pulses it extrapolates
    /// at the measured tempo (at most one pulse ahead) so the phase moves
    /// smoothly at frame rate instead of stepping 24 times a beat.
    pub fn position_beats(&mut self, now_micros: u64) -> f64 {
        let ppqn = f64::from(PPQN);
        let Some(last) = self.last_us else {
            return self.pos_ticks / ppqn;
        };
        let mut ticks = self.pos_ticks;
        if self.running && self.period_us > 0.0 {
            let ahead = now_micros.saturating_sub(last) as f64 / self.period_us;
            ticks += ahead.min(1.0);
        }
        // Never step back between pulses; a jump already reset `shown`.
        let ticks = ticks.max(self.shown.min(self.pos_ticks + 1.0));
        self.shown = ticks;
        ticks / ppqn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic jitter in ±`amp` µs.
    struct Jitter(u64, i64);
    impl Jitter {
        fn next(&mut self) -> i64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as i64 % (2 * self.1 + 1)) - self.1
        }
    }

    /// Feeds `ticks` pulses at `bpm` starting at `t0` µs with arrival
    /// jitter, returning the arrival times.
    fn feed(
        f: &mut ClockFollower,
        bpm: f64,
        t0: u64,
        ticks: u32,
        jitter: i64,
        seed: u64,
    ) -> Vec<u64> {
        let period = 60e6 / (bpm * f64::from(PPQN));
        let mut j = Jitter(seed, jitter);
        (0..ticks)
            .map(|k| {
                let t = (t0 as f64 + f64::from(k) * period).round() as i64 + j.next();
                let micros = t.max(0) as u64;
                f.handle(&Message::Clock { micros });
                micros
            })
            .collect()
    }

    #[test]
    fn tempo_accuracy_at_120_and_174_bpm() {
        for bpm in [120.0, 174.0] {
            // Clean pulses, then pulses with ±500 µs arrival jitter (a busy
            // driver callback).
            for (jitter, limit) in [(0, 0.01), (500, 0.3)] {
                let mut f = ClockFollower::new();
                feed(&mut f, bpm, 1_000_000, 96, jitter, 7); // 4 beats
                let est = f.bpm().unwrap();
                let err = (est - bpm).abs() / bpm * 100.0;
                println!(
                    "tempo {bpm} BPM, jitter ±{jitter} µs: estimate {est:.4} ({err:.4} % error)"
                );
                assert!(err < limit, "{bpm} BPM, jitter {jitter}: {err} %");
            }
        }
    }

    #[test]
    fn tempo_follows_a_change_within_a_beat() {
        let mut f = ClockFollower::new();
        let t = feed(&mut f, 120.0, 0, 48, 0, 1);
        let t0 = *t.last().unwrap() + 20_833;
        feed(&mut f, 150.0, t0, 30, 0, 1);
        let err = (f.bpm().unwrap() - 150.0).abs() / 150.0;
        assert!(err < 0.001, "{err}");
    }

    #[test]
    fn start_locks_the_first_pulse_to_beat_zero() {
        let mut f = ClockFollower::new();
        f.handle(&Message::Start { micros: 0 });
        let period = 60e6 / (120.0 * 24.0);
        let mut worst: f64 = 0.0;
        let mut j = Jitter(3, 400);
        let mut times = Vec::new();
        for k in 0..96u32 {
            let t = (5_000.0 + f64::from(k) * period).round() as i64 + j.next();
            times.push((
                k,
                t as u64,
                (5_000.0 + f64::from(k) * period).round() as u64,
            ));
            f.handle(&Message::Clock { micros: t as u64 });
            // Exactly on the pulse: pulse k is beat k/24.
            let at = f.position_beats(t as u64);
            assert!((at - f64::from(k) / 24.0).abs() < 1e-9, "pulse {k}: {at}");
        }
        // Between pulses (the cook runs at its own rate): the error against
        // the ideal grid, once the tempo estimate has settled.
        let mut g = ClockFollower::new();
        g.handle(&Message::Start { micros: 0 });
        let mut last_pulse = 0;
        for &(k, t, ideal) in &times {
            g.handle(&Message::Clock { micros: t });
            last_pulse = k;
            if k >= 30 {
                for frac in [0.25, 0.5, 0.75] {
                    let now = ideal + (period * frac) as u64; // true time, not the jittered arrival
                    let want = (f64::from(k) + frac) / 24.0;
                    worst = worst.max((g.position_beats(now) - want).abs());
                }
            }
        }
        assert_eq!(last_pulse, 95);
        println!(
            "phase error between pulses after Start at 120 BPM, ±400 µs jitter: worst {worst:.5} beats"
        );
        assert!(worst < 0.01, "{worst}");
        assert_eq!(f.jumps(), 1);
    }

    #[test]
    fn stop_holds_continue_resumes_and_song_position_jumps() {
        let mut f = ClockFollower::new();
        f.handle(&Message::Start { micros: 0 });
        let t = feed(&mut f, 120.0, 1_000, 25, 0, 1); // 24 pulses = 1 beat
        let now = *t.last().unwrap();
        assert!((f.position_beats(now) - 1.0).abs() < 1e-9);
        f.handle(&Message::Stop { micros: now + 100 });
        assert!(!f.running());
        // Stopped: pulses may still arrive (tempo keeps updating) but the
        // position holds.
        feed(&mut f, 120.0, now + 10_000, 12, 0, 1);
        assert!((f.position_beats(now + 400_000) - 1.0).abs() < 0.05);
        // Song Position 8 sixteenths = beat 2, then Continue.
        f.handle(&Message::SongPosition { sixteenths: 8 });
        assert!((f.position_beats(now + 500_000) - 2.0).abs() < 1e-9);
        f.handle(&Message::Continue {
            micros: now + 500_100,
        });
        assert!(f.running());
        let t2 = feed(&mut f, 120.0, now + 600_000, 13, 0, 1);
        let at = f.position_beats(*t2.last().unwrap());
        assert!((at - 2.5).abs() < 1e-9, "{at}");
        assert_eq!(f.jumps(), 3, "Start, Song Position, Continue");
    }

    #[test]
    fn a_stalled_master_does_not_run_away() {
        let mut f = ClockFollower::new();
        let t = feed(&mut f, 120.0, 0, 30, 0, 1);
        let last = *t.last().unwrap();
        let held = f.position_beats(last + 10_000_000);
        assert!(held <= 29.0 / 24.0 + 1.0 / 24.0 + 1e-9, "{held}");
    }
}
