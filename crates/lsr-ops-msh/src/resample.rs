//! Resample (PRD §6 PNT "Spatial & structure"): rebuilds every polyline
//! with evenly spaced points, by a target spacing or a fixed count per
//! curve; closed primitives too when asked (as closed outlines). A PNT
//! operator by its type ID (`pnt.resample`), cooked on a worker thread like
//! the MSH operators because it changes topology.
//!
//! New points blend the two points of the segment they fall on, so every
//! point attribute is interpolated; primitive attributes and groups are
//! kept. Curve ends are the source end points (shared ends stay welded).
//! Points used by no resampled primitive are dropped.

use std::collections::HashMap;

use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Interp, Mesh, limits, v3};

/// Most points one curve is resampled to.
pub const MAX_PER_CURVE: i64 = 100_000;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("mode", "Mode", &["spacing", "count"], "spacing").help(
            "spacing: points about `spacing` apart along each curve (adjusted so they divide it evenly); count: exactly `count` points per curve.",
        ),
        ParamSchema::float("spacing", "Spacing", 0.1, Some(0.0001), None)
            .help("spacing: the distance between neighbouring points along the curve."),
        ParamSchema::int("count", "Count", 10, Some(2), Some(MAX_PER_CURVE))
            .help("count: points per curve, both ends included (closed curves: at least 3)."),
        ParamSchema::toggle("polygons", "Polygons Too", false).help(
            "Resamples closed primitives as closed outlines as well; off, they pass through unchanged.",
        ),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(src) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let by_count = s.str("mode") == "count";
    let spacing = s.f64("spacing").max(1e-4);
    let count = s.i64("count").clamp(2, MAX_PER_CURVE) as usize;
    let polygons = s.bool("polygons");
    let mut out = Out {
        src,
        m: Mesh {
            point_groups: src.point_groups.clone(),
            prim_groups: src.prim_groups.clone(),
            ..Mesh::new()
        },
        pts: Interp::new(&src.point_attrs, src.points()),
        prims: Interp::new(&src.prim_attrs, src.prims()),
        copied: HashMap::new(),
    };
    let mut ring: Vec<u32> = Vec::new();
    let mut acc: Vec<f64> = Vec::new();
    let mut ids: Vec<u32> = Vec::new();
    for f in 0..src.prims() {
        job.every(f)?;
        let closed = src.closed[f];
        let corners = src.prim(f);
        if closed && !polygons {
            ids.clear();
            ids.extend(corners.iter().map(|&i| out.copy(i)));
            out.prim(f, &ids, true);
            continue;
        }
        // The curve's points in order; a closed one returns to its start.
        ring.clear();
        ring.extend_from_slice(corners);
        if closed && let Some(&first) = corners.first() {
            ring.push(first);
        }
        if ring.len() < 2 {
            continue;
        }
        acc.clear();
        acc.push(0.0);
        let mut total = 0.0;
        for w in ring.windows(2) {
            total += v3::len(v3::sub(
                v3::of(src.p[w[1] as usize]),
                v3::of(src.p[w[0] as usize]),
            ));
            acc.push(total);
        }
        if total <= 0.0 {
            // A curve of zero length cannot be spaced: kept as it is.
            ids.clear();
            ids.extend(corners.iter().map(|&i| out.copy(i)));
            out.prim(f, &ids, closed);
            continue;
        }
        let n = if by_count {
            if closed { count.max(3) } else { count }
        } else {
            let steps = (total / spacing).round();
            if steps > MAX_PER_CURVE as f64 {
                return Err(format!(
                    "primitive {f} is {total:.3} long: a spacing of {spacing} would give more than {MAX_PER_CURVE} points; increase the spacing"
                ));
            }
            let steps = steps as usize;
            if closed {
                steps.max(3)
            } else {
                steps.max(1) + 1
            }
        };
        limits::check(
            (out.m.points() + n) as u64,
            out.m.prims() as u64 + 1,
            (out.m.corners.len() + n) as u64,
        )?;
        // Distances along the curve: open curves end on their last point,
        // closed ones stop one step before their start.
        let steps = if closed { n } else { n - 1 };
        ids.clear();
        let mut seg = 0;
        for k in 0..n {
            let at = total * k as f64 / steps as f64;
            while seg + 2 < acc.len() && acc[seg + 1] < at {
                seg += 1;
            }
            let len = acc[seg + 1] - acc[seg];
            let t = if len > 0.0 {
                ((at - acc[seg]) / len).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (a, b) = (ring[seg], ring[seg + 1]);
            let id = if t <= 1e-9 {
                out.copy(a)
            } else if t >= 1.0 - 1e-9 {
                out.copy(b)
            } else {
                out.between(a, b, t)
            };
            ids.push(id);
        }
        if !closed && let Some(&last) = ring.last() {
            // Rounding never moves the end off the source end point.
            let end = out.copy(last);
            *ids.last_mut().expect("at least two points") = end;
        }
        out.prim(f, &ids, closed);
    }
    Ok(out.finish())
}

/// Builds the result: source points copied once, new points blended.
struct Out<'a> {
    src: &'a Mesh,
    m: Mesh,
    pts: Interp<'a>,
    prims: Interp<'a>,
    copied: HashMap<u32, u32>,
}

impl Out<'_> {
    fn copy(&mut self, i: u32) -> u32 {
        if let Some(&o) = self.copied.get(&i) {
            return o;
        }
        let o = self.m.add_point(self.src.p[i as usize]);
        self.pts.copy(i);
        self.copied.insert(i, o);
        o
    }

    fn between(&mut self, a: u32, b: u32, t: f64) -> u32 {
        let (pa, pb) = (
            v3::of(self.src.p[a as usize]),
            v3::of(self.src.p[b as usize]),
        );
        let o = self.m.add_point(v3::f32(v3::lerp(pa, pb, t)));
        self.pts.blend(&[(a, (1.0 - t) as f32), (b, t as f32)]);
        o
    }

    fn prim(&mut self, f: usize, ids: &[u32], closed: bool) {
        self.m.add_prim(ids, closed);
        self.prims.copy(f as u32);
    }

    fn finish(mut self) -> Mesh {
        self.m.point_attrs = self.pts.finish();
        self.m.prim_attrs = self.prims.finish();
        self.m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Column;
    use serde_json::json;

    fn run_with(params: &[(&str, serde_json::Value)], input: Mesh) -> Mesh {
        crate::run("pnt.resample", params, &[Some(input)]).unwrap()
    }

    /// An L-shaped polyline: 3 along X, then 1 along Y (length 4), with a
    /// `u` value equal to the distance along it, and a closed unit square.
    fn shapes() -> Mesh {
        let mut m = Mesh::new();
        for p in [[0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [3.0, 1.0, 0.0]] {
            m.add_point(p);
        }
        m.add_prim(&[0, 1, 2], false);
        for p in [
            [0.0, 2.0, 0.0],
            [1.0, 2.0, 0.0],
            [1.0, 3.0, 0.0],
            [0.0, 3.0, 0.0],
        ] {
            m.add_point(p);
        }
        m.add_prim(&[3, 4, 5, 6], true);
        m.set_point_attr(Column::f32("u", 1, vec![0.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0]));
        m.set_prim_attr(Column::f32("tag", 1, vec![7.0, 9.0]));
        m
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn spacing_divides_each_curve_evenly_and_interpolates() {
        let m = run_with(&[("spacing", json!(0.5))], shapes());
        // Length 4 at 0.5: 9 points, 0.5 apart; the square passes through.
        assert_eq!(m.prims(), 2);
        let curve = m.prim(0);
        assert_eq!(curve.len(), 9);
        assert!(!m.closed[0]);
        let u = m.point_attr("u").unwrap().floats().unwrap();
        for (k, &i) in curve.iter().enumerate() {
            let d = 0.5 * k as f32;
            let want = if d <= 3.0 {
                [d, 0.0, 0.0]
            } else {
                [3.0, d - 3.0, 0.0]
            };
            assert!(
                close(m.p[i as usize], want),
                "point {k}: {:?}",
                m.p[i as usize]
            );
            assert!((u[i as usize] - d).abs() < 1e-5, "u at {k}");
        }
        assert_eq!(m.prim(1).len(), 4);
        assert!(m.closed[1]);
        assert_eq!(m.prim_attr("tag").unwrap().floats().unwrap(), [7.0, 9.0]);
        m.validate().unwrap();
    }

    #[test]
    fn count_gives_exact_points_and_closed_outlines() {
        let m = run_with(
            &[
                ("mode", json!("count")),
                ("count", json!(5)),
                ("polygons", json!(true)),
            ],
            shapes(),
        );
        let curve = m.prim(0);
        assert_eq!(curve.len(), 5);
        assert!(close(m.p[curve[0] as usize], [0.0, 0.0, 0.0]));
        assert!(close(m.p[curve[2] as usize], [2.0, 0.0, 0.0]));
        assert!(close(m.p[curve[4] as usize], [3.0, 1.0, 0.0]));
        // The square's perimeter (4) in 5 even steps of 0.8, closed.
        let sq = m.prim(1);
        assert_eq!(sq.len(), 5);
        assert!(m.closed[1]);
        assert!(close(m.p[sq[0] as usize], [0.0, 2.0, 0.0]));
        assert!(close(m.p[sq[1] as usize], [0.8, 2.0, 0.0]));
        assert!(close(m.p[sq[2] as usize], [1.0, 2.6, 0.0]));
        m.validate().unwrap();
    }

    #[test]
    fn a_spacing_too_fine_is_an_error_not_a_huge_mesh() {
        let mut m = Mesh::new();
        m.add_point([0.0, 0.0, 0.0]);
        m.add_point([100.0, 0.0, 0.0]);
        m.add_prim(&[0, 1], false);
        let e = crate::run("pnt.resample", &[("spacing", json!(0.0001))], &[Some(m)]).unwrap_err();
        assert!(e.contains("increase the spacing"), "{e}");
    }
}
