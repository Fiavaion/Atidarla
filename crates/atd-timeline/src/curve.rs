//! Keyframe curves: the value of a track at a time (step, linear, cubic
//! Bezier). Allocation-free.

use atd_format::timeline::{Interp, Key};

/// The value of `keys` (in time order) at `t`: the first key's value before
/// it, the last key's after it, the segment's curve between. `None` without
/// keys.
pub fn sample(keys: &[Key], t: f64) -> Option<f64> {
    let (first, last) = (keys.first()?, keys.last()?);
    if t <= first.t {
        return Some(first.v);
    }
    if t >= last.t {
        return Some(last.v);
    }
    // The segment [keys[i], keys[i + 1]] holding t.
    let i = keys.partition_point(|k| k.t <= t) - 1;
    let (a, b) = (&keys[i], &keys[i + 1]);
    Some(match a.interp {
        Interp::Step => a.v,
        Interp::Linear => a.v + (b.v - a.v) * (t - a.t) / (b.t - a.t),
        Interp::Bezier => bezier(a, b, t),
    })
}

/// The Bezier handles of the segment from `a` to `b` as absolute control
/// points `[(t1, v1), (t2, v2)]`. A missing handle is a third of the
/// segment, flat. Handle times are clamped inside the segment, so the curve
/// stays a function of time.
pub fn handles(a: &Key, b: &Key) -> [[f64; 2]; 2] {
    let span = b.t - a.t;
    let out = a.tan_out.unwrap_or([span / 3.0, 0.0]);
    let inn = b.tan_in.unwrap_or([-span / 3.0, 0.0]);
    [
        [a.t + out[0].clamp(0.0, span), a.v + out[1]],
        [b.t + inn[0].clamp(-span, 0.0), b.v + inn[1]],
    ]
}

fn cubic(p0: f64, p1: f64, p2: f64, p3: f64, s: f64) -> f64 {
    let r = 1.0 - s;
    r * r * r * p0 + 3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s * p3
}

fn cubic_slope(p0: f64, p1: f64, p2: f64, p3: f64, s: f64) -> f64 {
    let r = 1.0 - s;
    3.0 * (r * r * (p1 - p0) + 2.0 * r * s * (p2 - p1) + s * s * (p3 - p2))
}

/// The curve between `a` and `b` at `t` (`a.t < t < b.t`): solves the
/// curve's time for its parameter (Newton steps kept inside a shrinking
/// bracket), then reads its value.
fn bezier(a: &Key, b: &Key, t: f64) -> f64 {
    let [[x1, y1], [x2, y2]] = handles(a, b);
    let (x0, x3) = (a.t, b.t);
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut s = (t - x0) / (x3 - x0);
    for _ in 0..64 {
        let x = cubic(x0, x1, x2, x3, s) - t;
        if x.abs() < 1e-12 * (x3 - x0).max(1.0) {
            break;
        }
        // The curve's time rises with s (handles inside the segment).
        if x < 0.0 {
            lo = s;
        } else {
            hi = s;
        }
        let d = cubic_slope(x0, x1, x2, x3, s);
        let newton = s - x / d;
        s = if d > 0.0 && newton > lo && newton < hi {
            newton
        } else {
            0.5 * (lo + hi)
        };
    }
    cubic(a.v, y1, y2, b.v, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, v: f64, interp: Interp) -> Key {
        Key::new(t, v).with_interp(interp)
    }

    #[test]
    fn empty_single_and_outside_the_keys() {
        assert_eq!(sample(&[], 1.0), None);
        let one = [key(2.0, 5.0, Interp::Linear)];
        assert_eq!(sample(&one, 0.0), Some(5.0));
        assert_eq!(sample(&one, 9.0), Some(5.0));
        let two = [
            key(1.0, 0.0, Interp::Linear),
            key(3.0, 10.0, Interp::Linear),
        ];
        assert_eq!(sample(&two, 0.0), Some(0.0));
        assert_eq!(sample(&two, 1.0), Some(0.0));
        assert_eq!(sample(&two, 3.0), Some(10.0));
        assert_eq!(sample(&two, 4.0), Some(10.0));
    }

    #[test]
    fn step_and_linear() {
        let keys = [
            key(0.0, 0.0, Interp::Step),
            key(1.0, 4.0, Interp::Linear),
            key(3.0, 0.0, Interp::Linear),
        ];
        assert_eq!(sample(&keys, 0.5), Some(0.0));
        assert_eq!(sample(&keys, 0.999), Some(0.0));
        assert_eq!(sample(&keys, 1.0), Some(4.0));
        assert_eq!(sample(&keys, 1.5), Some(3.0));
        assert_eq!(sample(&keys, 2.5), Some(1.0));
    }

    #[test]
    fn bezier_with_handles_on_the_line_is_linear() {
        // Handles a third of the way along the straight line: the curve is
        // the line, and its parameter is the time.
        let mut a = key(1.0, 2.0, Interp::Bezier);
        a.tan_out = Some([1.0, 3.0]);
        let mut b = key(4.0, 11.0, Interp::Linear);
        b.tan_in = Some([-1.0, -3.0]);
        for i in 0..=30 {
            let t = 1.0 + 3.0 * f64::from(i) / 30.0;
            let want = 2.0 + 3.0 * (t - 1.0);
            let got = sample(&[a, b], t).unwrap();
            assert!((got - want).abs() < 1e-9, "t {t}: {got} vs {want}");
        }
    }

    #[test]
    fn bezier_passes_through_its_midpoint_and_eases() {
        // Default handles: flat at both keys (ease in and out).
        let a = key(0.0, 0.0, Interp::Bezier);
        let b = key(2.0, 10.0, Interp::Linear);
        // At parameter 1/2 a cubic is at (p0 + 3 p1 + 3 p2 + p3) / 8.
        let [[x1, y1], [x2, y2]] = handles(&a, &b);
        let mid_t = (0.0 + 3.0 * x1 + 3.0 * x2 + 2.0) / 8.0;
        let mid_v = (0.0 + 3.0 * y1 + 3.0 * y2 + 10.0) / 8.0;
        assert!((mid_t - 1.0).abs() < 1e-12);
        assert!((sample(&[a, b], mid_t).unwrap() - mid_v).abs() < 1e-9);
        assert!((mid_v - 5.0).abs() < 1e-12);
        // Easing: slower than linear near the keys, symmetric about the middle.
        let early = sample(&[a, b], 0.2).unwrap();
        let late = sample(&[a, b], 1.8).unwrap();
        assert!(early < 1.0 && early > 0.0, "{early}");
        assert!((early + late - 10.0).abs() < 1e-9);
        // Default handles are evenly spaced in time, so time is linear in
        // the curve's parameter and the value is smoothstep: 10(3u² − 2u³).
        for (t, want) in [(0.5, 1.5625), (1.5, 8.4375), (0.2, 0.28)] {
            let got = sample(&[a, b], t).unwrap();
            assert!((got - want).abs() < 1e-9, "t {t}: {got} vs {want}");
        }
    }

    #[test]
    fn bezier_matches_a_dense_parametric_reference() {
        // Asymmetric handles, one overshooting in value.
        let mut a = key(1.0, -1.0, Interp::Bezier);
        a.tan_out = Some([0.2, 6.0]);
        let mut b = key(2.5, 3.0, Interp::Step);
        b.tan_in = Some([-1.2, -0.5]);
        let [[x1, y1], [x2, y2]] = handles(&a, &b);
        for i in 1..1000 {
            let s = f64::from(i) / 1000.0;
            let t = cubic(1.0, x1, x2, 2.5, s);
            let want = cubic(-1.0, y1, y2, 3.0, s);
            let got = sample(&[a, b], t).unwrap();
            assert!((got - want).abs() < 1e-7, "s {s}: {got} vs {want}");
        }
        // Overshoot is allowed in value (the handle rises to 5).
        let peak = (1..100)
            .map(|i| sample(&[a, b], 1.0 + 1.5 * f64::from(i) / 100.0).unwrap())
            .fold(f64::MIN, f64::max);
        assert!(peak > 3.0, "{peak}");
    }

    #[test]
    fn handles_outside_the_segment_are_clamped_so_time_stays_monotonic() {
        let mut a = key(0.0, 0.0, Interp::Bezier);
        a.tan_out = Some([5.0, 1.0]);
        let mut b = key(1.0, 1.0, Interp::Linear);
        b.tan_in = Some([-5.0, 0.0]);
        assert_eq!(handles(&a, &b), [[1.0, 1.0], [0.0, 1.0]]);
        let mut prev = f64::MIN;
        for i in 0..=200 {
            let v = sample(&[a, b], f64::from(i) / 200.0).unwrap();
            assert!(v.is_finite() && v >= prev - 1e-9, "{i}: {v}");
            prev = v;
        }
    }
}
