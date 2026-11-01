//! Measure: writes primitive area, perimeter and normal, and point
//! curvature (mean curvature from the cotangent Laplacian, Gaussian
//! curvature from the angle deficit; Meyer, Desbrun, Schröder and Barr,
//! "Discrete Differential-Geometry Operators for Triangulated
//! 2-Manifolds", 2003) as attributes.

use std::f64::consts::{PI, TAU};

use lsr_core::geo::names;
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::triangulate_polygon;
use crate::mesh::{Column, Mesh, v3};
use crate::topo::Edges;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::toggle("area", "Area", true)
            .help("Writes the primitive attribute `area` (polygon area; 0 for polylines)."),
        ParamSchema::toggle("perimeter", "Perimeter", false)
            .help("Writes the primitive attribute `perimeter` (polygon perimeter, or polyline length)."),
        ParamSchema::toggle("normals", "Normals", false)
            .help("Writes the primitive attribute `N`: each polygon's unit normal."),
        ParamSchema::menu("curvature", "Curvature", &["none", "mean", "gaussian"], "mean").help(
            "Writes the point attribute `curvature`. mean: positive where the surface bulges out (1/r on a sphere of radius r), negative in hollows; gaussian: positive on domes, negative on saddles, zero on flat or cylindrical parts (1/r² on a sphere). Points on open borders get 0 (mean) or the border's geodesic turning per area (gaussian).",
        ),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let mut out = m.clone();
    if s.bool("area") {
        let a: Vec<f32> = (0..m.prims())
            .map(|f| {
                if m.closed[f] {
                    (v3::len(m.prim_normal(f)) / 2.0) as f32
                } else {
                    0.0
                }
            })
            .collect();
        out.set_prim_attr(Column::f32("area", 1, a));
    }
    if s.bool("perimeter") {
        let len: Vec<f32> = (0..m.prims())
            .map(|f| {
                let p = m.prim(f);
                let k = p.len();
                let segs = if m.closed[f] { k } else { k.saturating_sub(1) };
                (0..segs)
                    .map(|i| {
                        let a = v3::of(m.p[p[i] as usize]);
                        let b = v3::of(m.p[p[(i + 1) % k] as usize]);
                        v3::len(v3::sub(b, a))
                    })
                    .sum::<f64>() as f32
            })
            .collect();
        out.set_prim_attr(Column::f32("perimeter", 1, len));
    }
    if s.bool("normals") {
        let n: Vec<[f32; 3]> = (0..m.prims())
            .map(|f| v3::f32(v3::norm(m.prim_normal(f))))
            .collect();
        out.set_prim_attr(Column::vec3(names::N, &n));
    }
    job.check()?;
    match s.str("curvature") {
        "mean" => out.set_point_attr(Column::f32("curvature", 1, curvature(m, true, job)?)),
        "gaussian" => out.set_point_attr(Column::f32("curvature", 1, curvature(m, false, job)?)),
        _ => {}
    }
    Ok(out)
}

/// Mean (`mean`) or Gaussian curvature per point, over the polygons
/// split into triangles. Points on no polygon get 0.
pub fn curvature(m: &Mesh, mean: bool, job: &Job) -> Result<Vec<f32>, String> {
    let n = m.points();
    let pos: Vec<[f64; 3]> = m.p.iter().map(|&p| v3::of(p)).collect();
    let mut area = vec![0.0f64; n];
    let mut angles = vec![0.0f64; n];
    let mut lap = vec![[0.0f64; 3]; n];
    let mut normal = vec![[0.0f64; 3]; n];
    let mut tris = Vec::new();
    for f in 0..m.prims() {
        job.every(f)?;
        if !m.closed[f] {
            continue;
        }
        tris.clear();
        triangulate_polygon(&m.p, m.prim(f), &mut tris);
        for t in &tris {
            let [i, j, k] = t.map(|v| v as usize);
            let (a, b, c) = (pos[i], pos[j], pos[k]);
            let cr = v3::cross(v3::sub(b, a), v3::sub(c, a));
            let twice = v3::len(cr);
            if twice <= 1e-300 {
                continue;
            }
            for (v, p, q, r) in [(i, a, b, c), (j, b, c, a), (k, c, a, b)] {
                area[v] += twice / 6.0;
                normal[v] = v3::add(normal[v], cr);
                let (e1, e2) = (v3::sub(q, p), v3::sub(r, p));
                angles[v] += v3::len(v3::cross(e1, e2)).atan2(v3::dot(e1, e2));
                // The edge opposite p (q–r) gets cot(angle at p).
                let cot = v3::dot(e1, e2) / twice;
                let (qi, ri) = if v == i {
                    (j, k)
                } else if v == j {
                    (k, i)
                } else {
                    (i, j)
                };
                lap[qi] = v3::add(lap[qi], v3::scale(v3::sub(r, q), cot));
                lap[ri] = v3::add(lap[ri], v3::scale(v3::sub(q, r), cot));
            }
        }
    }
    let border = Edges::new(m).border_points(m);
    Ok((0..n)
        .map(|v| {
            if area[v] <= 0.0 {
                return 0.0;
            }
            let k = if mean {
                if border[v] {
                    0.0
                } else {
                    // Δp = -2 H n, with Δp = lap / (2 A).
                    let d = v3::scale(lap[v], 1.0 / (2.0 * area[v]));
                    -0.5 * v3::dot(d, v3::norm(normal[v]))
                }
            } else {
                let full = if border[v] { PI } else { TAU };
                (full - angles[v]) / area[v]
            };
            k as f32
        })
        .collect())
}
