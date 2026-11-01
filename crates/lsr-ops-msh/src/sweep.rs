//! Sweep (a profile along a curve with parallel-transport frames) and Skin
//! (a surface lofted through a sequence of profiles).

use std::f64::consts::TAU;

use lsr_core::geo::names;
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Interp, Mesh, limits, v3};
use crate::normals::point_normals;

type V = [f64; 3];

/// The first primitive's points (or, without primitives, all points in
/// order) and whether it is closed.
fn first_curve(m: &Mesh) -> (Vec<u32>, bool) {
    if m.prims() > 0 {
        (m.prim(0).to_vec(), m.closed[0])
    } else {
        ((0..m.points() as u32).collect(), false)
    }
}

/// Rotates `v` about unit `axis` by `angle` radians (Rodrigues).
fn rotate(v: V, axis: V, angle: f64) -> V {
    let (s, c) = angle.sin_cos();
    let k = axis;
    v3::add(
        v3::add(v3::scale(v, c), v3::scale(v3::cross(k, v), s)),
        v3::scale(k, v3::dot(k, v) * (1.0 - c)),
    )
}

/// Tangents and parallel-transported normals along a curve; a closed
/// curve spreads the leftover twist evenly so the frame meets itself.
pub fn frames(c: &[V], closed: bool) -> Vec<(V, V, V)> {
    let n = c.len();
    let tangent = |i: usize| {
        let (a, b) = if closed {
            (c[(i + n - 1) % n], c[(i + 1) % n])
        } else {
            (c[i.saturating_sub(1)], c[(i + 1).min(n - 1)])
        };
        let t = v3::norm(v3::sub(b, a));
        if v3::len(t) > 0.0 { t } else { [0.0, 1.0, 0.0] }
    };
    let ts: Vec<V> = (0..n).map(tangent).collect();
    // A start normal perpendicular to the first tangent.
    let t0 = ts[0];
    let axis = if t0[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let mut normal = v3::norm(v3::cross(v3::cross(t0, axis), t0));
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if i > 0 {
            let (a, b) = (ts[i - 1], ts[i]);
            let ax = v3::cross(a, b);
            let l = v3::len(ax);
            if l > 1e-12 {
                normal = rotate(normal, v3::scale(ax, 1.0 / l), l.atan2(v3::dot(a, b)));
            }
            // Keep it exactly perpendicular.
            normal = v3::norm(v3::sub(normal, v3::scale(b, v3::dot(normal, b))));
        }
        out.push((ts[i], normal, v3::cross(ts[i], normal)));
    }
    if closed && n > 2 {
        // Transport once more to the start and spread the mismatch.
        let (a, b) = (ts[n - 1], ts[0]);
        let ax = v3::cross(a, b);
        let l = v3::len(ax);
        let mut end = out[n - 1].1;
        if l > 1e-12 {
            end = rotate(end, v3::scale(ax, 1.0 / l), l.atan2(v3::dot(a, b)));
        }
        let start = out[0].1;
        let twist = v3::dot(v3::cross(end, start), b).atan2(v3::dot(end, start));
        for (i, f) in out.iter_mut().enumerate() {
            let ang = twist * i as f64 / n as f64;
            f.1 = rotate(f.1, f.0, ang);
            f.2 = v3::cross(f.0, f.1);
        }
    }
    out
}

pub fn sweep_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("radius", "Radius", 0.1, Some(0.0), None)
            .help("Without a profile input: the radius of the circle swept."),
        ParamSchema::int("divisions", "Divisions", 12, Some(3), Some(4096))
            .help("Without a profile input: points around the circle."),
        ParamSchema::float("scale_end", "End Scale", 1.0, Some(0.0), None).help(
            "The profile's scale at the end of the curve (it grows or shrinks linearly from 1).",
        ),
        ParamSchema::float("twist", "Twist", 0.0, None, None)
            .help("Degrees the profile turns about the curve from start to end."),
        ParamSchema::toggle("caps", "Caps", true)
            .help("Closes both ends of an open curve when the profile is closed."),
    ]
}

/// Sweeps input 1's first primitive (drawn in the XY plane, origin on the
/// curve) along input 0's first primitive.
pub fn sweep(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(curve) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let (cids, cclosed) = first_curve(curve);
    if cids.len() < 2 {
        return Ok(Mesh::new());
    }
    let mut c: Vec<V> = cids.iter().map(|&i| v3::of(curve.p[i as usize])).collect();
    if !cclosed && c.len() > 2 && c.first() == c.last() {
        // An outline whose last point is its first: a closed curve.
        c.pop();
    }
    let closed_curve = cclosed || cids.first() == cids.last() && cids.len() > 2;
    let (profile, pclosed): (Vec<[f64; 2]>, bool) = match inputs.get(1).and_then(Option::as_ref) {
        Some(p) if p.points() > 1 => {
            let (ids, closed) = first_curve(p);
            let mut pts: Vec<[f64; 2]> = ids
                .iter()
                .map(|&i| [f64::from(p.p[i as usize][0]), f64::from(p.p[i as usize][1])])
                .collect();
            let loops = pts.len() > 2 && ids.first() == ids.last();
            if loops {
                pts.pop();
            }
            (pts, closed || loops)
        }
        _ => {
            let n = s.i64("divisions").clamp(3, 4096) as usize;
            let r = s.f64("radius");
            (
                (0..n)
                    .map(|i| {
                        let a = TAU * i as f64 / n as f64;
                        [r * a.cos(), r * a.sin()]
                    })
                    .collect(),
                true,
            )
        }
    };
    let (rows, cols) = (c.len(), profile.len());
    limits::check(
        (rows * cols) as u64,
        (rows * cols + 2) as u64,
        (rows * cols * 4 + 2 * cols) as u64,
    )?;
    let fr = frames(&c, closed_curve);
    let (end_scale, twist) = (s.f64("scale_end"), s.f64("twist").to_radians());
    let mut m = Mesh::new();
    let mut uv = Vec::with_capacity(rows * cols * 2);
    for (i, &(_, n, b)) in fr.iter().enumerate() {
        job.every(i)?;
        let u = if rows > 1 {
            i as f64 / (rows - 1) as f64
        } else {
            0.0
        };
        let sc = 1.0 + (end_scale - 1.0) * u;
        let (st, ct) = (twist * u).sin_cos();
        for (j, q) in profile.iter().enumerate() {
            let (x, y) = (q[0] * ct - q[1] * st, q[0] * st + q[1] * ct);
            let p = v3::add(
                c[i],
                v3::scale(v3::add(v3::scale(n, x), v3::scale(b, y)), sc),
            );
            m.add_point(v3::f32(p));
            uv.extend([j as f32 / cols as f32, u as f32]);
        }
    }
    let id = |i: usize, j: usize| ((i % rows) * cols + j % cols) as u32;
    let row_edges = if closed_curve { rows } else { rows - 1 };
    let col_edges = if pclosed { cols } else { cols - 1 };
    for i in 0..row_edges {
        for j in 0..col_edges {
            m.add_polygon(&[id(i, j), id(i, j + 1), id(i + 1, j + 1), id(i + 1, j)]);
        }
    }
    if s.bool("caps") && pclosed && !closed_curve && cols >= 3 {
        let start: Vec<u32> = (0..cols).rev().map(|j| id(0, j)).collect();
        let end: Vec<u32> = (0..cols).map(|j| id(rows - 1, j)).collect();
        m.add_polygon(&start);
        m.add_polygon(&end);
    }
    m.set_point_attr(Column::f32(names::UV, 2, uv));
    let nn = point_normals(&m);
    m.set_point_attr(Column::vec3(names::N, &nn));
    Ok(m)
}

pub fn skin_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::toggle("closed", "Closed", false)
            .help("Also connects the last profile back to the first (a ring)."),
        ParamSchema::toggle("caps", "Caps", true).help(
            "Closes the first and last profiles with polygons (closed profiles, not a ring).",
        ),
        ParamSchema::toggle("reverse", "Reverse", false)
            .help("Flips the surface (use it when it faces inward)."),
    ]
}

/// Lofts through the input's primitives in order; each must have the same
/// number of points.
pub fn skin(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(src) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let rows = src.prims();
    if rows < 2 {
        return Err("Skin needs at least two profiles (primitives) in its input".into());
    }
    let mut profiles: Vec<Vec<u32>> = Vec::with_capacity(rows);
    let mut closed = true;
    for f in 0..rows {
        let mut ids = src.prim(f).to_vec();
        let loops = !src.closed[f] && ids.len() > 2 && ids.first() == ids.last();
        if loops {
            ids.pop();
        }
        closed &= src.closed[f] || loops;
        profiles.push(ids);
    }
    let cols = profiles[0].len();
    if let Some(bad) = profiles.iter().position(|p| p.len() != cols) {
        return Err(format!(
            "profile 0 has {cols} points but profile {bad} has {}; Skin needs equal counts",
            profiles[bad].len()
        ));
    }
    if cols < 2 {
        return Err("profiles need at least two points".into());
    }
    limits::check(
        (rows * cols) as u64,
        (rows * cols + 2) as u64,
        (rows * cols * 4) as u64,
    )?;
    let mut m = Mesh::new();
    let mut pts = Interp::new(&src.point_attrs, rows * cols);
    for prof in &profiles {
        job.check()?;
        for &i in prof {
            m.add_point(src.p[i as usize]);
            pts.copy(i);
        }
    }
    m.point_attrs = pts.finish();
    m.point_attrs.retain(|c| c.name != names::N);
    let ring = s.bool("closed");
    let id = |i: usize, j: usize| ((i % rows) * cols + j % cols) as u32;
    let row_edges = if ring { rows } else { rows - 1 };
    let col_edges = if closed { cols } else { cols - 1 };
    for i in 0..row_edges {
        for j in 0..col_edges {
            m.add_polygon(&[id(i, j), id(i, j + 1), id(i + 1, j + 1), id(i + 1, j)]);
        }
    }
    if s.bool("caps") && closed && !ring && cols >= 3 {
        let start: Vec<u32> = (0..cols).rev().map(|j| id(0, j)).collect();
        let end: Vec<u32> = (0..cols).map(|j| id(rows - 1, j)).collect();
        m.add_polygon(&start);
        m.add_polygon(&end);
    }
    if s.bool("reverse") {
        crate::prims::flip(&mut m);
    }
    let nn = point_normals(&m);
    m.set_point_attr(Column::vec3(names::N, &nn));
    Ok(m)
}
