//! Revolve (a lathe): turns every primitive of the input about an axis,
//! making a surface of quads (triangles where a profile point lies on the
//! axis, which is welded into one point).

use std::f64::consts::TAU;

use lsr_core::geo::names;
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Mesh, limits, v3};
use crate::normals::point_normals;

/// Rotates `v` about unit `axis` by `angle` radians (Rodrigues).
fn rotate(v: [f64; 3], axis: [f64; 3], angle: f64) -> [f64; 3] {
    let (s, c) = angle.sin_cos();
    v3::add(
        v3::add(v3::scale(v, c), v3::scale(v3::cross(axis, v), s)),
        v3::scale(axis, v3::dot(axis, v) * (1.0 - c)),
    )
}

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::vec("origin", "Origin", &[0.0, 0.0, 0.0])
            .help("A point on the axis."),
        ParamSchema::vec("axis", "Axis", &[0.0, 1.0, 0.0])
            .help("The direction of the axis turned about."),
        ParamSchema::float("start", "Start Angle", 0.0, None, None)
            .help("Degrees where the surface starts."),
        ParamSchema::float("end", "End Angle", 360.0, None, None).help(
            "Degrees where it ends; a full turn (360 from the start) closes the surface.",
        ),
        ParamSchema::int("divisions", "Divisions", 32, Some(1), Some(4096))
            .help("Steps around the axis."),
        ParamSchema::toggle("caps", "Caps", true).help(
            "Closes both ends of a partial turn with the profile, when the profile is a closed polygon.",
        ),
        ParamSchema::toggle("reverse", "Reverse", false)
            .help("Flips the surface (a profile drawn downward faces inward)."),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(src) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let axis = v3::norm(v3::of(s.vec3("axis")));
    if v3::len(axis) == 0.0 {
        return Err("the axis has no direction".into());
    }
    let origin = v3::of(s.vec3("origin"));
    let (a0, a1) = (s.f64("start").to_radians(), s.f64("end").to_radians());
    let sweep = a1 - a0;
    let full = (sweep.abs() - TAU).abs() < 1e-9 || sweep.abs() > TAU;
    let div = s.i64("divisions").clamp(1, 4096) as usize;
    let rings = if full { div } else { div + 1 };
    let profiles: Vec<(Vec<u32>, bool)> = if src.prims() > 0 {
        (0..src.prims())
            .map(|f| (src.prim(f).to_vec(), src.closed[f]))
            .collect()
    } else {
        vec![((0..src.points() as u32).collect(), false)]
    };
    let corners: usize = profiles.iter().map(|(p, _)| p.len()).sum();
    limits::check(
        (corners * rings) as u64,
        (corners * rings + 2 * profiles.len()) as u64,
        (corners * rings * 4 + 2 * corners) as u64,
    )?;
    let Some((lo, hi)) = src.bounds() else {
        return Ok(Mesh::new());
    };
    let eps = 1e-7 * v3::len(v3::sub(v3::of(hi), v3::of(lo))).max(1e-6);
    let mut m = Mesh::new();
    let mut uv: Vec<f32> = Vec::new();
    let step = if full {
        sweep.signum() * TAU / div as f64
    } else {
        sweep / div as f64
    };
    for (ids, closed) in &profiles {
        job.check()?;
        let n = ids.len();
        if n < 2 {
            continue;
        }
        // Each profile point's id in every ring (one id on the axis).
        let mut grid: Vec<Vec<u32>> = vec![Vec::with_capacity(n); rings];
        for (j, &i) in ids.iter().enumerate() {
            let p = v3::of(src.p[i as usize]);
            let r = v3::sub(p, origin);
            let along = v3::scale(axis, v3::dot(r, axis));
            let off = v3::sub(r, along);
            let v = j as f32 / (n - 1).max(1) as f32;
            if v3::len(off) <= eps {
                let id = m.add_point(src.p[i as usize]);
                uv.extend([0.5, v]);
                for row in &mut grid {
                    row.push(id);
                }
                continue;
            }
            for (k, row) in grid.iter_mut().enumerate() {
                let q = rotate(off, axis, a0 + step * k as f64);
                row.push(m.add_point(v3::f32(v3::add(v3::add(origin, along), q))));
                uv.extend([k as f32 / div as f32, v]);
            }
        }
        let ring_edges = if full { rings } else { rings - 1 };
        let col_edges = if *closed { n } else { n - 1 };
        let mut poly: Vec<u32> = Vec::with_capacity(4);
        for k in 0..ring_edges {
            let (r0, r1) = (&grid[k], &grid[(k + 1) % rings]);
            for j in 0..col_edges {
                let j1 = (j + 1) % n;
                poly.clear();
                for c in [r0[j], r1[j], r1[j1], r0[j1]] {
                    if poly.last() != Some(&c) && poly.first() != Some(&c) {
                        poly.push(c);
                    }
                }
                if poly.len() >= 3 {
                    m.add_polygon(&poly);
                }
            }
        }
        if !full && *closed && s.bool("caps") && n >= 3 {
            let first: Vec<u32> = grid[0].clone();
            let last: Vec<u32> = grid[rings - 1].iter().rev().copied().collect();
            m.add_polygon(&first);
            m.add_polygon(&last);
        }
    }
    // A negative sweep turns the other way, which mirrors the winding.
    if s.bool("reverse") != (step < 0.0) {
        crate::prims::flip(&mut m);
    }
    m.set_point_attr(Column::f32(names::UV, 2, uv));
    let nn = point_normals(&m);
    m.set_point_attr(Column::vec3(names::N, &nn));
    Ok(m)
}
