//! MIDI clock out: pulses from a timing thread of their own.
//!
//! The cook only sets the tempo and the transport ([`ClockOut::set_tempo`],
//! [`ClockOut::begin`], [`ClockOut::stop`]); the thread sends a pulse every
//! 1/24 beat on its own schedule, whatever the frame rate, and never blocks
//! the cook (rule 1).

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::midi::OutHandle;
use crate::midi_clock::PPQN;

/// Sends MIDI clock (pulses, Start, Continue with Song Position, Stop).
#[derive(Debug)]
pub struct ClockOut {
    shared: Arc<(Mutex<OutState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct OutState {
    bpm: f64,
    /// Transport changes waiting for the thread, oldest first.
    pending: Vec<Transport>,
    quit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Transport {
    /// Start (`FA`) and a pulse now: position 0.
    Start,
    /// Song Position and Continue (`FB`); pulses begin on the next
    /// sixteenth at or after this position (in pulses).
    Continue(f64),
    Stop,
}

/// The last stretch before a pulse is spent spinning, because a sleeping
/// thread wakes late (by a millisecond or more on a busy desktop). The
/// stretch starts at the minimum and follows the lateness actually seen,
/// up to the maximum.
const MIN_SPIN: Duration = Duration::from_micros(300);
const MAX_SPIN: Duration = Duration::from_millis(3);
const MAX_SLEEP: Duration = Duration::from_millis(1);

impl ClockOut {
    /// Starts the timing thread, sending on `out`. Idle until
    /// [`Self::begin`].
    pub fn new(out: OutHandle) -> Self {
        let shared = Arc::new((Mutex::new(OutState::default()), Condvar::new()));
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("atd-midi-clock".into())
            .spawn(move || run(&thread_shared, &out))
            .map_err(|e| log::warn!("MIDI clock out: cannot start thread: {e}"))
            .ok();
        Self { shared, thread }
    }

    fn with(&self, f: impl FnOnce(&mut OutState)) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut s) = lock.lock() {
            f(&mut s);
            cv.notify_one();
        }
    }

    /// Tempo in beats per minute for the pulses from now on (0 pauses them).
    pub fn set_tempo(&self, bpm: f64) {
        self.with(|s| s.bpm = bpm.max(0.0));
    }

    /// The transport runs from `beats` (quarter notes from the beginning).
    /// Within a pulse of 0 a Start and a pulse go out now; otherwise Song
    /// Position and Continue go out and the pulses begin on the next
    /// sixteenth, so the receiver's grid matches ours.
    pub fn begin(&self, beats: f64) {
        let ticks = beats * f64::from(PPQN);
        self.with(|s| {
            s.pending.push(if ticks < 1.0 {
                Transport::Start
            } else {
                Transport::Continue(ticks)
            });
        });
    }

    /// Stop (`FC`); the pulses end.
    pub fn stop(&self) {
        self.with(|s| s.pending.push(Transport::Stop));
    }
}

impl Drop for ClockOut {
    fn drop(&mut self) {
        self.with(|s| s.quit = true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(shared: &(Mutex<OutState>, Condvar), out: &OutHandle) {
    let (lock, cv) = shared;
    let mut running = false;
    // When the next pulse is due.
    let mut next: Option<Instant> = None;
    let mut spin = MIN_SPIN;
    let Ok(mut state) = lock.lock() else { return };
    loop {
        if state.quit {
            if running {
                out.send_raw(&[0xFC]);
            }
            return;
        }
        let period = (state.bpm > 0.0).then(|| 60.0 / (state.bpm * f64::from(PPQN)));
        let now = Instant::now();
        for t in std::mem::take(&mut state.pending) {
            match t {
                Transport::Start => {
                    out.send_raw(&[0xFA]);
                    running = true;
                    next = Some(now);
                }
                Transport::Continue(ticks) => {
                    // The receiver is told the sixteenth the first pulse
                    // lands on.
                    let sixteenth = (ticks / 6.0).ceil();
                    let wait = (sixteenth * 6.0 - ticks) * period.unwrap_or(0.0);
                    let spp = sixteenth as u16;
                    out.send_raw(&[0xF2, (spp & 0x7F) as u8, ((spp >> 7) & 0x7F) as u8]);
                    out.send_raw(&[0xFB]);
                    running = true;
                    next = Some(now + Duration::from_secs_f64(wait));
                }
                Transport::Stop => {
                    if running {
                        out.send_raw(&[0xFC]);
                    }
                    running = false;
                    next = None;
                }
            }
        }
        let (Some(period), true, Some(due)) = (period, running, next) else {
            // Nothing to time: sleep until the cook says something.
            match cv.wait(state) {
                Ok(s) => state = s,
                Err(_) => return,
            }
            continue;
        };
        let period = Duration::from_secs_f64(period);
        if now >= due {
            out.send_raw(&[0xF8]);
            let following = due + period;
            // After a stall (a suspended laptop) do not burst to catch up.
            next = Some(if following + period < now {
                now + period
            } else {
                following
            });
            continue;
        }
        let remaining = due - now;
        if remaining > spin {
            // macOS wakes a sleeper late by up to half what it asked, so
            // sleep in short steps and keep re-checking the time.
            let asked = (remaining - spin).min(MAX_SLEEP);
            let slept = Instant::now();
            match cv.wait_timeout(state, asked) {
                Ok((s, res)) => {
                    state = s;
                    if res.timed_out() {
                        let late = slept.elapsed().saturating_sub(asked);

                        spin = spin
                            .mul_f64(0.98)
                            .max((late + Duration::from_micros(200)).min(MAX_SPIN))
                            .max(MIN_SPIN);
                    }
                }
                Err(_) => return,
            }
        } else {
            drop(state);
            while Instant::now() < due {
                std::hint::spin_loop();
            }
            match lock.lock() {
                Ok(s) => state = s,
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::MidiOut;

    /// Strict per-interval timing checks, for runs on real hardware.
    fn strict() -> bool {
        std::env::var("ATD_TIMING_STRICT").is_ok_and(|v| v == "1")
    }

    fn pulses(sent: &[(Vec<u8>, Instant)]) -> Vec<Instant> {
        sent.iter()
            .filter(|(m, _)| m == &[0xF8])
            .map(|(_, t)| *t)
            .collect()
    }

    #[test]
    fn pulses_keep_their_spacing_without_help_from_the_cook() {
        let out = MidiOut::detached();
        let clock = ClockOut::new(out.handle());
        clock.set_tempo(120.0);
        let started = Instant::now();
        clock.begin(0.0);
        // The cook says nothing more for a second; the pulses go on.
        std::thread::sleep(Duration::from_millis(1000));
        clock.stop();
        // A busy machine (a CI runner) oversleeps: count against the time
        // that actually passed, 48 pulses a second at 120 BPM.
        let expected = started.elapsed().as_secs_f64() * 48.0;
        std::thread::sleep(Duration::from_millis(50));
        drop(clock);
        let sent = out.sent_raw();
        assert_eq!(sent[0].0, [0xFA], "Start first");
        assert_eq!(sent.last().unwrap().0, [0xFC], "then Stop, once");
        assert_eq!(sent.iter().filter(|(m, _)| m == &[0xFC]).count(), 1);
        let t = pulses(&sent);
        // A shared runner can starve the clock thread for a while; it then
        // drops behind rather than bursting, so allow 15% there.
        let slack = if strict() {
            2.5
        } else {
            (expected * 0.15).max(2.5)
        };
        assert!(
            (t.len() as f64 - expected).abs() <= slack,
            "{} pulses in {:.3} s (expected {expected:.1})",
            t.len(),
            expected / 48.0
        );
        let span = (t[t.len() - 1] - t[0]).as_secs_f64() / (t.len() - 1) as f64;
        let nominal = 60.0 / (120.0 * 24.0);
        let worst = t
            .windows(2)
            .map(|w| ((w[1] - w[0]).as_secs_f64() - nominal).abs())
            .fold(0.0, f64::max);
        println!(
            "clock out at 120 BPM: mean spacing {:.3} ms (nominal {:.3}), worst deviation {:.3} ms",
            span * 1e3,
            nominal * 1e3,
            worst * 1e3
        );
        // A burst per frame would put the mean far off; 1% on real hardware,
        // 5% on a shared runner.
        let tol = if strict() { 0.01 } else { 0.05 };
        assert!((span - nominal).abs() / nominal < tol, "{span}");
        // A busy machine (a shared CI runner) can delay one wake-up by several
        // ms; a burst per frame would be 16 ms or more off every time, which
        // the average above catches. The worst single interval is checked on
        // real hardware only (ATD_TIMING_STRICT=1).
        if strict() {
            assert!(worst < 0.012, "{worst}");
        }
    }

    #[test]
    fn a_tempo_change_changes_the_spacing() {
        let out = MidiOut::detached();
        let clock = ClockOut::new(out.handle());
        clock.set_tempo(60.0);
        clock.begin(0.0);
        std::thread::sleep(Duration::from_millis(400));
        clock.set_tempo(240.0);
        std::thread::sleep(Duration::from_millis(400));
        drop(clock);
        let t = pulses(&out.sent_raw());
        // Mean spacing over several pulses on each side of the change (one
        // late wake-up on a shared runner must not decide it).
        let mean = |w: &[Instant]| (w[w.len() - 1] - w[0]).as_secs_f64() / (w.len() - 1) as f64;
        let early = mean(&t[1..8]);
        let late = mean(&t[t.len() - 12..]);
        // One late wake-up among a dozen 10 ms intervals moves the mean by
        // several percent on a shared runner; a 4x tempo change still shows.
        let tol = if strict() { 0.05 } else { 0.2 };
        assert!((early - 1.0 / 24.0).abs() / (1.0 / 24.0) < tol, "{early}");
        assert!((late - 1.0 / 96.0).abs() / (1.0 / 96.0) < tol, "{late}");
    }

    #[test]
    fn continue_sends_song_position_and_aligns_the_first_pulse() {
        let out = MidiOut::detached();
        let clock = ClockOut::new(out.handle());
        clock.set_tempo(120.0);
        // 55.2 pulses in: the next sixteenth (10 = pulse 60) is 4.8 pulses
        // (100 ms) away.
        let begun = Instant::now();
        clock.begin(2.3);
        std::thread::sleep(Duration::from_millis(250));
        drop(clock);
        let sent = out.sent_raw();
        assert_eq!(sent[0].0, [0xF2, 10, 0]);
        assert_eq!(sent[1].0, [0xFB]);
        let first = sent.iter().find(|(m, _)| m == &[0xF8]).unwrap().1;
        let wait = (first - begun).as_secs_f64();
        let tol = if strict() { 0.02 } else { 0.05 };
        assert!((wait - 0.1).abs() < tol, "{wait}");
    }

    #[test]
    fn dropping_a_running_clock_stops_the_transport() {
        let out = MidiOut::detached();
        let clock = ClockOut::new(out.handle());
        clock.set_tempo(120.0);
        clock.begin(0.0);
        std::thread::sleep(Duration::from_millis(30));
        drop(clock);
        assert_eq!(out.sent_raw().last().unwrap().0, [0xFC]);
    }
}
