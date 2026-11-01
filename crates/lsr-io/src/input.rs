//! Input devices for SIG Input Devices: the pointer and keyboard as the
//! host application sees them, and gamepads (`gilrs`: XInput/WGI, IOKit,
//! evdev).
//!
//! The app writes a [`Pointer`] and the held keys each frame with
//! [`set_pointer`] and [`set_keys`]. Gamepads are polled on their own
//! thread, started on first use. Readers take snapshots with `try_lock`
//! and never wait (rule 1); headless runs simply see nothing pressed.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Pointer state, positions normalised to the viewed image (0–1, y up).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    /// The pointer is over the viewed image.
    pub inside: bool,
    /// Left, middle, right.
    pub buttons: [bool; 3],
    /// Accumulated wheel movement in notches (up is positive).
    pub wheel: f32,
}

#[derive(Debug, Default)]
struct Desktop {
    pointer: Pointer,
    /// Held keys by lower-case name (`space`, `a`, `arrowup`, `1`).
    keys: Vec<String>,
}

fn desktop() -> &'static Mutex<Desktop> {
    static D: OnceLock<Mutex<Desktop>> = OnceLock::new();
    D.get_or_init(Default::default)
}

/// Host side: the current pointer.
pub fn set_pointer(p: Pointer) {
    if let Ok(mut d) = desktop().lock() {
        d.pointer = p;
    }
}

/// Host side: the keys held this frame (any case; stored lower-case).
pub fn set_keys<'a>(held: impl IntoIterator<Item = &'a str>) {
    if let Ok(mut d) = desktop().lock() {
        let mut n = 0;
        for k in held {
            if n < d.keys.len() {
                d.keys[n].clear();
            } else {
                d.keys.push(String::new());
            }
            d.keys[n].extend(k.chars().flat_map(char::to_lowercase));
            n += 1;
        }
        d.keys.truncate(n);
    }
}

/// The current pointer, or `None` if the host is writing right now.
pub fn pointer() -> Option<Pointer> {
    desktop().try_lock().ok().map(|d| d.pointer)
}

/// Calls `f(key, held)` for each whitespace-separated name in `keys`
/// (case-insensitive), without allocating. Returns false if the state was
/// busy (the caller keeps its previous values).
pub fn with_keys(keys: &str, mut f: impl FnMut(&str, bool)) -> bool {
    let Ok(d) = desktop().try_lock() else {
        return false;
    };
    for k in keys.split_whitespace() {
        let held = d.keys.iter().any(|h| h.eq_ignore_ascii_case(k));
        f(k, held);
    }
    true
}

/// Axis and button snapshot of one gamepad.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pad {
    /// Left stick x/y, right stick x/y (−1…1, y up), left/right trigger (0–1).
    pub axes: [f32; 6],
    /// South, east, north, west, left bumper, right bumper, select, start,
    /// mode, left stick, right stick, d-pad up, down, left, right.
    pub buttons: [bool; 15],
}

pub const PAD_AXES: [&str; 6] = ["lx", "ly", "rx", "ry", "lt", "rt"];
pub const PAD_BUTTONS: [&str; 15] = [
    "a", "b", "y", "x", "lb", "rb", "back", "start", "guide", "ls", "rs", "up", "down", "left",
    "right",
];

#[derive(Debug, Default)]
struct Pads {
    pads: Vec<Pad>,
    error: Option<String>,
}

fn pads() -> &'static Mutex<Pads> {
    static P: OnceLock<Mutex<Pads>> = OnceLock::new();
    static STARTED: OnceLock<()> = OnceLock::new();
    let shared = P.get_or_init(Default::default);
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("lsr-gamepads".into())
            .spawn(poll_pads);
        if let Err(e) = spawned
            && let Ok(mut p) = shared.lock()
        {
            p.error = Some(format!("cannot start gamepad thread: {e}"));
        }
    });
    shared
}

fn poll_pads() {
    use gilrs::{Axis, Button};
    let mut gilrs = match gilrs::Gilrs::new() {
        Ok(g) => g,
        Err(gilrs::Error::NotImplemented(g)) => g,
        Err(e) => {
            if let Ok(mut p) = pads().lock() {
                p.error = Some(format!("gamepads unavailable: {e}"));
            }
            return;
        }
    };
    const AXES: [Axis; 4] = [
        Axis::LeftStickX,
        Axis::LeftStickY,
        Axis::RightStickX,
        Axis::RightStickY,
    ];
    const BUTTONS: [Button; 15] = [
        Button::South,
        Button::East,
        Button::North,
        Button::West,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::Select,
        Button::Start,
        Button::Mode,
        Button::LeftThumb,
        Button::RightThumb,
        Button::DPadUp,
        Button::DPadDown,
        Button::DPadLeft,
        Button::DPadRight,
    ];
    let mut snapshot: Vec<Pad> = Vec::new();
    loop {
        while gilrs.next_event().is_some() {}
        snapshot.clear();
        for (_, gp) in gilrs.gamepads() {
            let mut pad = Pad::default();
            for (i, a) in AXES.iter().enumerate() {
                pad.axes[i] = gp.value(*a);
            }
            let trigger = |b: Button| gp.button_data(b).map_or(0.0, |d| d.value());
            pad.axes[4] = trigger(Button::LeftTrigger2);
            pad.axes[5] = trigger(Button::RightTrigger2);
            for (i, b) in BUTTONS.iter().enumerate() {
                pad.buttons[i] = gp.is_pressed(*b);
            }
            snapshot.push(pad);
        }
        if let Ok(mut p) = pads().lock() {
            p.pads.clone_from(&snapshot);
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Gamepad `index` (0 = first connected), starting the poller on first
/// use. `Ok(None)` when not connected or the state is busy.
pub fn pad(index: usize) -> Result<Option<Pad>, String> {
    let Ok(p) = pads().try_lock() else {
        return Ok(None);
    };
    if let Some(e) = &p.error {
        return Err(e.clone());
    }
    Ok(p.pads.get(index).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_case_insensitively_and_pointer_round_trips() {
        set_keys(["Space", "A"]);
        let mut seen = Vec::new();
        assert!(with_keys("space a b", |k, held| seen.push((k.to_owned(), held))));
        assert_eq!(
            seen,
            [
                ("space".into(), true),
                ("a".into(), true),
                ("b".into(), false)
            ]
        );
        set_keys([]);
        assert!(with_keys("a", |_, held| assert!(!held)));
        let p = Pointer {
            x: 0.25,
            y: 0.75,
            inside: true,
            buttons: [true, false, false],
            wheel: 2.0,
        };
        set_pointer(p);
        assert_eq!(pointer(), Some(p));
    }

    #[test]
    fn gamepads_report_without_blocking() {
        let start = std::time::Instant::now();
        // No gamepads on a test machine: either nothing connected or a
        // clear error, never a hang.
        let _ = pad(0);
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
