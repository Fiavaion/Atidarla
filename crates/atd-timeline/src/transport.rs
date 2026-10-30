//! The playhead: play, pause, stop, loop and scrub, moved by the local
//! clock or by chased timecode ([`crate::chase`]).

use crate::chase::{Chase, ChaseConfig};

/// Whether the playhead moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// At rest; the timeline drives nothing (live values cleared).
    Stopped,
    /// Held where it is; values keep their state.
    Paused,
    Playing,
}

/// What moves the playhead while playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSource {
    /// The local clock.
    Internal,
    /// Timecode readings fed to [`Transport::chase`] (MTC, LTC, or a
    /// Timecode In node).
    Timecode,
}

/// How the playhead moved in one [`Transport::advance`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    /// It did not move.
    Still,
    /// Forward over `(from, to]`, or `[from, to]` with `include_from`
    /// (the first frame after starting).
    Forward {
        from: f64,
        to: f64,
        include_from: bool,
    },
    /// Forward to the loop end, then from the loop start to `to`.
    Wrapped {
        from: f64,
        end: f64,
        start: f64,
        to: f64,
    },
    /// Jumped to `to` (a scrub, or relocated timecode): crossing nothing.
    /// `chase` asks to fire what lies before `to`.
    Jumped { to: f64, chase: bool },
}

/// A playhead (see the module docs).
#[derive(Debug, Clone)]
pub struct Transport {
    state: PlayState,
    time: f64,
    /// Timeline length in seconds: playback pauses there unless looping.
    pub length: f64,
    /// Loop region `[start, end]`; used while `looping`.
    pub loop_region: Option<[f64; 2]>,
    pub looping: bool,
    pub clock: ClockSource,
    pub chase: Chase,
    /// Local time of the last advance.
    last: Option<f64>,
    /// Starting: the next forward step includes its start.
    starting: bool,
    /// A seek waiting for the next advance.
    seek: Option<(f64, bool)>,
}

impl Transport {
    pub fn new(length: f64) -> Self {
        Self {
            state: PlayState::Stopped,
            time: 0.0,
            length,
            loop_region: None,
            looping: false,
            clock: ClockSource::Internal,
            chase: Chase::new(ChaseConfig::default()),
            last: None,
            starting: false,
            seek: None,
        }
    }

    pub fn state(&self) -> PlayState {
        self.state
    }

    /// The playhead, in seconds.
    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn play(&mut self) {
        if self.state != PlayState::Playing {
            self.starting = self.state == PlayState::Stopped || self.starting;
            self.state = PlayState::Playing;
            self.last = None;
        }
    }

    pub fn pause(&mut self) {
        if self.state == PlayState::Playing {
            self.state = PlayState::Paused;
        }
    }

    /// Play from a pause or stop, pause while playing.
    pub fn toggle(&mut self) {
        match self.state {
            PlayState::Playing => self.pause(),
            _ => self.play(),
        }
    }

    /// Stops and returns to the start (the loop start while looping).
    pub fn stop(&mut self) {
        self.state = PlayState::Stopped;
        self.time = self.loop_start();
        self.seek = None;
        self.starting = false;
    }

    /// Moves the playhead to `t` on the next advance. A scrub fires no cue
    /// unless `chase` (then every cue before `t` fires, as if played). A
    /// stopped transport pauses there, so the timeline shows that time.
    pub fn seek(&mut self, t: f64, chase: bool) {
        let t = t.clamp(0.0, self.length);
        self.time = t;
        self.seek = Some((t, chase));
        if self.state == PlayState::Stopped {
            self.state = PlayState::Paused;
        }
    }

    /// Puts the playhead at `t` at once, with no motion to report (the
    /// Stage launched the cue there).
    pub fn relocate(&mut self, t: f64) {
        self.time = t.clamp(0.0, self.length);
    }

    fn loop_start(&self) -> f64 {
        match (self.looping, self.loop_region) {
            (true, Some([a, _])) => a,
            _ => 0.0,
        }
    }

    /// Moves the playhead to local time `now` (seconds, any origin).
    pub fn advance(&mut self, now: f64) -> Motion {
        let dt = self.last.map_or(0.0, |l| (now - l).max(0.0));
        self.last = Some(now);
        if let Some((to, chase)) = self.seek.take() {
            // Playing on from a scrub fires a cue right at the playhead; a
            // chase has fired it already.
            self.starting = !chase;
            return Motion::Jumped { to, chase };
        }
        if self.state != PlayState::Playing {
            return Motion::Still;
        }
        let from = self.time;
        let include_from = std::mem::take(&mut self.starting);
        let target = match self.clock {
            ClockSource::Internal => from + dt,
            ClockSource::Timecode => match self.chase.position(now) {
                Some(p) => {
                    let p = p.max(0.0);
                    let frame = 1.0 / self.chase.fps();
                    // Backwards, or ahead of where the clock could take it:
                    // the code relocated.
                    if p < from - frame || p > from + dt + 2.0 * frame {
                        self.time = p.min(self.length);
                        self.starting = true;
                        return Motion::Jumped {
                            to: self.time,
                            chase: false,
                        };
                    }
                    p.max(from)
                }
                // No code: wait where we are.
                None => {
                    self.starting |= include_from;
                    return Motion::Still;
                }
            },
        };
        if self.looping
            && let Some([start, end]) = self.loop_region
            && from < end
            && target >= end
        {
            let span = end - start;
            let to = start + (target - end) % span;
            self.time = to;
            return Motion::Wrapped {
                from,
                end,
                start,
                to,
            };
        }
        if target >= self.length {
            self.time = self.length;
            self.state = PlayState::Paused;
        } else {
            self.time = target;
        }
        Motion::Forward {
            from,
            to: self.time,
            include_from,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_pause_stop_and_the_first_step_includes_its_start() {
        let mut tr = Transport::new(10.0);
        assert_eq!(tr.advance(0.0), Motion::Still);
        tr.play();
        assert_eq!(
            tr.advance(5.0),
            Motion::Forward {
                from: 0.0,
                to: 0.0,
                include_from: true
            }
        );
        assert_eq!(
            tr.advance(5.5),
            Motion::Forward {
                from: 0.0,
                to: 0.5,
                include_from: false
            }
        );
        tr.pause();
        assert_eq!(tr.advance(9.0), Motion::Still);
        tr.play();
        // Resuming a pause does not refire the cue at the playhead.
        assert_eq!(
            tr.advance(9.25),
            Motion::Forward {
                from: 0.5,
                to: 0.5,
                include_from: false
            }
        );
        tr.stop();
        assert_eq!(tr.time(), 0.0);
        assert_eq!(tr.state(), PlayState::Stopped);
    }

    #[test]
    fn playback_pauses_at_the_end_and_loops_inside_a_region() {
        let mut tr = Transport::new(2.0);
        tr.play();
        tr.advance(0.0);
        tr.advance(1.5);
        assert!(matches!(tr.advance(3.0), Motion::Forward { to: 2.0, .. }));
        assert_eq!(tr.state(), PlayState::Paused);

        let mut tr = Transport::new(10.0);
        tr.loop_region = Some([1.0, 3.0]);
        tr.looping = true;
        tr.play();
        tr.advance(0.0);
        tr.advance(2.5);
        assert_eq!(
            tr.advance(3.25),
            Motion::Wrapped {
                from: 2.5,
                end: 3.0,
                start: 1.0,
                to: 1.25
            }
        );
        assert_eq!(tr.time(), 1.25);
        tr.stop();
        assert_eq!(tr.time(), 1.0, "stop returns to the loop start");
    }

    #[test]
    fn a_scrub_jumps_without_crossing() {
        let mut tr = Transport::new(10.0);
        tr.play();
        tr.advance(0.0);
        tr.advance(1.0);
        tr.seek(7.0, false);
        assert_eq!(
            tr.advance(1.1),
            Motion::Jumped {
                to: 7.0,
                chase: false
            }
        );
        assert!(matches!(
            tr.advance(1.2),
            Motion::Forward {
                from: 7.0,
                include_from: true,
                ..
            }
        ));
        tr.seek(99.0, true);
        assert_eq!(
            tr.advance(1.3),
            Motion::Jumped {
                to: 10.0,
                chase: true
            }
        );
    }

    #[test]
    fn timecode_moves_the_playhead_and_relocations_jump() {
        let mut tr = Transport::new(100.0);
        tr.clock = ClockSource::Timecode;
        tr.play();
        // No code yet: the playhead waits, and the start is still pending.
        assert_eq!(tr.advance(0.0), Motion::Still);
        let fps = 25.0;
        for i in 0..4 {
            let t = f64::from(i) * 2.0 / fps;
            tr.chase.reading(10.0 + t, t, fps);
        }
        let now = 6.0 / fps;
        assert_eq!(
            tr.advance(now),
            Motion::Jumped {
                to: 10.0 + now,
                chase: false
            },
            "first lock: the playhead moves to the code"
        );
        let m = tr.advance(now + 0.04);
        assert!(
            matches!(m, Motion::Forward { from, to, .. } if from == 10.0 + now && (to - (10.04 + now)).abs() < 1e-9),
            "{m:?}"
        );
    }
}
