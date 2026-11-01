//! Carve: keeps the part of each curve between two fractions of its
//! length, or the part of each polygon whose texture coordinate `u` or `v`
//! lies in a range. New points interpolate every point attribute; cuts are
//! shared between neighbouring polygons, so a welded surface stays welded.

use std::collections::HashMap;

use lsr_core::geo::names;
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Interp, Mesh, v3};

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("domain", "Domain", &["length", "u", "v"], "length").help(
            "length: polylines, by the fraction of each one's length (polygons pass through); u or v: polygons, by that texture coordinate of `uv` (polylines pass through).",
        ),
        ParamSchema::float("start", "Start", 0.0, None, None)
            .help("Where the kept part begins (0 is the start of the curve, or that uv value)."),
        ParamSchema::float("end", "End", 1.0, None, None)
            .help("Where the kept part ends; an end below the start keeps nothing."),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(src) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let (lo, hi) = (s.f64("start"), s.f64("end"));
    match s.str("domain") {
        "u" => by_uv(src, 0, lo, hi, job),
        "v" => by_uv(src, 1, lo, hi, job),
        _ => by_length(src, lo, hi, job),
    }
}

/// Builds the result: points copied or blended from the source, with the
/// source's primitive attributes copied per kept primitive.
struct Out<'a> {
    src: &'a Mesh,
    m: Mesh,
    pts: Interp<'a>,
    prims: Interp<'a>,
    copied: HashMap<u32, u32>,
}

impl<'a> Out<'a> {
    fn new(src: &'a Mesh) -> Self {
        Out {
            src,
            m: Mesh {
                point_groups: src.point_groups.clone(),
                prim_groups: src.prim_groups.clone(),
                ..Mesh::new()
            },
            pts: Interp::new(&src.point_attrs, src.points()),
            prims: Interp::new(&src.prim_attrs, src.prims()),
            copied: HashMap::new(),
        }
    }

    /// Source point `i` (once).
    fn copy(&mut self, i: u32) -> u32 {
        if let Some(&o) = self.copied.get(&i) {
            return o;
        }
        let o = self.m.add_point(self.src.p[i as usize]);
        self.pts.copy(i);
        self.copied.insert(i, o);
        o
    }

    /// A new point between source points `a` and `b` at `t` (0 at `a`).
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

fn by_length(src: &Mesh, lo: f64, hi: f64, job: &Job) -> Result<Mesh, String> {
    let mut out = Out::new(src);
    let (lo, hi) = (lo.clamp(0.0, 1.0), hi.clamp(0.0, 1.0));
    for f in 0..src.prims() {
        job.every(f)?;
        let ids = src.prim(f);
        if src.closed[f] {
            let c: Vec<u32> = ids.iter().map(|&i| out.copy(i)).collect();
            out.prim(f, &c, true);
            continue;
        }
        if hi <= lo || ids.len() < 2 {
            continue;
        }
        // Cumulative length at each point.
        let mut acc = Vec::with_capacity(ids.len());
        let mut total = 0.0;
        acc.push(0.0);
        for w in ids.windows(2) {
            total += v3::len(v3::sub(
                v3::of(src.p[w[1] as usize]),
                v3::of(src.p[w[0] as usize]),
            ));
            acc.push(total);
        }
        if total <= 0.0 {
            continue;
        }
        let (a, b) = (lo * total, hi * total);
        // The point at length `x` on segment `k` (acc[k] <= x <= acc[k+1]).
        let at = |out: &mut Out, k: usize, x: f64| {
            let len = acc[k + 1] - acc[k];
            let t = if len > 0.0 { (x - acc[k]) / len } else { 0.0 };
            if t <= 1e-12 {
                out.copy(ids[k])
            } else if t >= 1.0 - 1e-12 {
                out.copy(ids[k + 1])
            } else {
                out.between(ids[k], ids[k + 1], t)
            }
        };
        let seg = |x: f64| {
            acc.windows(2)
                .position(|w| x <= w[1])
                .unwrap_or(acc.len() - 2)
        };
        let (ka, kb) = (seg(a), seg(b));
        let mut c = vec![at(&mut out, ka, a)];
        for k in ka + 1..=kb {
            let p = out.copy(ids[k]);
            if c.last() != Some(&p) {
                c.push(p);
            }
        }
        let end = at(&mut out, kb, b);
        if c.last() != Some(&end) {
            c.push(end);
        }
        if c.len() >= 2 {
            out.prim(f, &c, false);
        }
    }
    Ok(out.finish())
}

fn by_uv(src: &Mesh, k: usize, lo: f64, hi: f64, job: &Job) -> Result<Mesh, String> {
    let uv = src
        .point_attr(names::UV)
        .filter(|c| c.comps >= 2)
        .and_then(|c| c.floats().map(|v| (v, usize::from(c.comps))))
        .ok_or("carving by u or v needs texture coordinates `uv` on the points (add UV Project)")?;
    let value = |i: u32| f64::from(uv.0[i as usize * uv.1 + k]);
    let mut out = Out::new(src);
    // Cuts by (lower point, higher point, bound), shared between polygons.
    let mut cuts: HashMap<(u32, u32, u8), u32> = HashMap::new();
    for f in 0..src.prims() {
        job.every(f)?;
        let ids = src.prim(f);
        if !src.closed[f] {
            let c: Vec<u32> = ids.iter().map(|&i| out.copy(i)).collect();
            out.prim(f, &c, false);
            continue;
        }
        if hi < lo {
            continue;
        }
        // Clip against u >= lo, then u <= hi (Sutherland-Hodgman), on
        // source points and cut points (each a blend of two source points).
        #[derive(Clone, Copy)]
        enum P {
            Src(u32),
            Cut(u32, u32, f64, u8),
        }
        let val = |p: P| match p {
            P::Src(i) => value(i),
            P::Cut(a, b, t, _) => value(a) * (1.0 - t) + value(b) * t,
        };
        let mut poly: Vec<P> = ids.iter().map(|&i| P::Src(i)).collect();
        for (bound, x, keep_above) in [(0u8, lo, true), (1u8, hi, false)] {
            let inside = |p: P| {
                let v = val(p);
                if keep_above { v >= x } else { v <= x }
            };
            let mut next = Vec::with_capacity(poly.len() + 2);
            for e in 0..poly.len() {
                let (p, q) = (poly[e], poly[(e + 1) % poly.len()]);
                let (ip, iq) = (inside(p), inside(q));
                if ip {
                    next.push(p);
                }
                if ip != iq {
                    // Only an edge between two source points can cross a
                    // bound (a cut point lies on the other bound, whose
                    // edges are along it or inside), so cuts stay on
                    // source edges.
                    let (a, b) = match (p, q) {
                        (P::Src(a), P::Src(b)) => (a, b),
                        (P::Cut(a, b, _, _), _) | (_, P::Cut(a, b, _, _)) => (a, b),
                    };
                    let (va, vb) = (value(a), value(b));
                    let t = if vb != va { (x - va) / (vb - va) } else { 0.5 };
                    next.push(P::Cut(a, b, t.clamp(0.0, 1.0), bound));
                }
            }
            poly = next;
            if poly.len() < 3 {
                break;
            }
        }
        if poly.len() < 3 {
            continue;
        }
        let mut c: Vec<u32> = Vec::with_capacity(poly.len());
        for p in poly {
            let id = match p {
                P::Src(i) => out.copy(i),
                P::Cut(a, b, t, bound) => {
                    let (a, b, t) = if a < b { (a, b, t) } else { (b, a, 1.0 - t) };
                    *cuts
                        .entry((a, b, bound))
                        .or_insert_with(|| out.between(a, b, t))
                }
            };
            if c.last() != Some(&id) && c.first() != Some(&id) {
                c.push(id);
            }
        }
        if c.len() >= 3 {
            out.prim(f, &c, true);
        }
    }
    Ok(out.finish())
}
