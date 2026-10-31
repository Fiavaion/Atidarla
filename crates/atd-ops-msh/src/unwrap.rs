//! UV Unwrap: cuts the surface into charts, flattens each chart and packs
//! the charts into the unit square, writing point texture coordinates
//! `uv` (points are split along chart seams). Clean-room, after:
//!
//! - charts: polygons grown across shared edges while their normal stays
//!   within an angle of the chart's first polygon (so a chart never folds
//!   over itself when projected onto that polygon's plane);
//! - flattening: least squares conformal maps (Lévy, Petitjean, Ray &
//!   Maillot, "Least Squares Conformal Maps for Automatic Texture Atlas
//!   Generation", SIGGRAPH 2002) with two pinned points, solved by
//!   conjugate gradients from the planar projection; a chart whose
//!   solution flips a triangle keeps the projection;
//! - packing: each chart scaled to its surface area, turned to its
//!   smallest bounding rectangle (an edge of its convex hull), and placed
//!   on shelves, tallest first, with a margin.

use std::collections::HashMap;

use atd_core::geo::names;
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::triangulate_polygon;
use crate::hull::hull2;
use crate::mesh::{Column, Interp, Mesh, limits, v3};

type V = [f64; 3];
type P2 = [f64; 2];

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("method", "Method", &["conformal", "planar"], "conformal").help(
            "conformal: each chart flattened keeping angles (least squares conformal maps); planar: each chart projected onto its first polygon's plane.",
        ),
        ParamSchema::float("angle", "Chart Angle", 60.0, Some(1.0), Some(89.0)).help(
            "Degrees a polygon's normal may differ from its chart's first polygon; smaller makes more, flatter charts.",
        ),
        ParamSchema::float("margin", "Margin", 0.01, Some(0.0), Some(0.25))
            .help("Space around each chart, as a fraction of the square's side."),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(src) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let faces: Vec<usize> = (0..src.prims())
        .filter(|&f| src.closed[f] && src.prim(f).len() >= 3)
        .collect();
    let normals: Vec<V> = (0..src.prims())
        .map(|f| v3::norm(src.prim_normal(f)))
        .collect();
    let charts = charts(
        src,
        &faces,
        &normals,
        s.f64("angle").to_radians().cos(),
        job,
    )?;
    let conformal = s.str("method") != "planar";
    // Per chart: its points (source ids) and their uv.
    let mut flat: Vec<(Vec<u32>, Vec<P2>)> = Vec::with_capacity(charts.len());
    for (k, chart) in charts.iter().enumerate() {
        job.every(k)?;
        flat.push(flatten(src, chart, normals[chart[0]], conformal, job)?);
    }
    let placed = pack(&flat, s.f64("margin"));
    // The result: chart points, then the polylines' points.
    let total: usize = flat.iter().map(|c| c.0.len()).sum();
    limits::check(
        (total + src.points()) as u64,
        src.prims() as u64,
        src.corners.len() as u64,
    )?;
    let mut out = Mesh {
        point_groups: src.point_groups.clone(),
        prim_groups: src.prim_groups.clone(),
        prim_attrs: src.prim_attrs.clone(),
        ..Mesh::new()
    };
    let mut pts = Interp::new(&src.point_attrs, total);
    let mut uv: Vec<f32> = Vec::with_capacity(total * 2);
    // (chart, source point) -> new point.
    let mut map: HashMap<(u32, u32), u32> = HashMap::with_capacity(total);
    let mut chart_of = vec![u32::MAX; src.prims()];
    for (k, (ids, q)) in flat.iter().enumerate() {
        for (&i, &p) in ids.iter().zip(q) {
            map.insert((k as u32, i), out.add_point(src.p[i as usize]));
            pts.copy(i);
            let t = placed[k](p);
            uv.extend([t[0] as f32, t[1] as f32]);
        }
        for &f in &charts[k] {
            chart_of[f] = k as u32;
        }
    }
    let mut loose: HashMap<u32, u32> = HashMap::new();
    let mut chart_attr = Vec::with_capacity(src.prims());
    for f in 0..src.prims() {
        job.every(f)?;
        let k = chart_of[f];
        let c: Vec<u32> = if k == u32::MAX {
            src.prim(f)
                .iter()
                .map(|&i| {
                    *loose.entry(i).or_insert_with(|| {
                        pts.copy(i);
                        uv.extend([0.0, 0.0]);
                        out.add_point(src.p[i as usize])
                    })
                })
                .collect()
        } else {
            src.prim(f).iter().map(|&i| map[&(k, i)]).collect()
        };
        out.add_prim(&c, src.closed[f]);
        chart_attr.push(if k == u32::MAX { 0 } else { k });
    }
    out.point_attrs = pts.finish();
    out.set_point_attr(Column::f32(names::UV, 2, uv));
    out.set_prim_attr(Column::u32("chart", chart_attr));
    Ok(out)
}

/// Charts as lists of polygons (the first is the seed).
fn charts(
    m: &Mesh,
    faces: &[usize],
    normals: &[V],
    cos: f64,
    job: &Job,
) -> Result<Vec<Vec<usize>>, String> {
    // Undirected edge -> polygons using it.
    let mut edges: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for &f in faces {
        let p = m.prim(f);
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            edges.entry((a.min(b), a.max(b))).or_default().push(f);
        }
    }
    let mut taken = vec![false; m.prims()];
    let mut out = Vec::new();
    let mut queue = Vec::new();
    for (k, &seed) in faces.iter().enumerate() {
        job.every(k)?;
        if taken[seed] {
            continue;
        }
        taken[seed] = true;
        let n0 = normals[seed];
        let mut chart = vec![seed];
        queue.clear();
        queue.push(seed);
        while let Some(f) = queue.pop() {
            let p = m.prim(f);
            for i in 0..p.len() {
                let (a, b) = (p[i], p[(i + 1) % p.len()]);
                for &g in &edges[&(a.min(b), a.max(b))] {
                    if !taken[g] && v3::dot(normals[g], n0) >= cos {
                        taken[g] = true;
                        chart.push(g);
                        queue.push(g);
                    }
                }
            }
        }
        out.push(chart);
    }
    Ok(out)
}

/// A chart's points and their flat coordinates (at surface scale).
fn flatten(
    m: &Mesh,
    chart: &[usize],
    n0: V,
    conformal: bool,
    job: &Job,
) -> Result<(Vec<u32>, Vec<P2>), String> {
    let mut local: HashMap<u32, usize> = HashMap::new();
    let mut ids: Vec<u32> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for &f in chart {
        triangulate_polygon(&m.p, m.prim(f), &mut tris);
        for &i in m.prim(f) {
            local.entry(i).or_insert_with(|| {
                ids.push(i);
                ids.len() - 1
            });
        }
    }
    let tris: Vec<[usize; 3]> = tris.iter().map(|t| t.map(|i| local[&i])).collect();
    let p: Vec<V> = ids.iter().map(|&i| v3::of(m.p[i as usize])).collect();
    // The seed polygon's plane.
    let a = if n0[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = v3::norm(v3::cross(a, n0));
    let w = v3::cross(n0, u);
    let planar: Vec<P2> = p.iter().map(|x| [v3::dot(*x, u), v3::dot(*x, w)]).collect();
    let area: f64 = tris
        .iter()
        .map(|t| {
            v3::len(v3::cross(
                v3::sub(p[t[1]], p[t[0]]),
                v3::sub(p[t[2]], p[t[0]]),
            )) / 2.0
        })
        .sum();
    let mut q = if conformal && ids.len() >= 3 {
        let q = lscm(&p, &tris, &planar, job)?;
        let flips = tris
            .iter()
            .any(|t| signed(q[t[0]], q[t[1]], q[t[2]]) <= 0.0);
        if flips { planar } else { q }
    } else {
        planar
    };
    // Scaled so the chart's flat area is its surface area (an even
    // texel density across charts).
    let flat: f64 = tris
        .iter()
        .map(|t| signed(q[t[0]], q[t[1]], q[t[2]]).abs() / 2.0)
        .sum();
    if flat > 1e-30 && area > 0.0 {
        let k = (area / flat).sqrt();
        q.iter_mut().for_each(|x| *x = [x[0] * k, x[1] * k]);
    }
    Ok((ids, q))
}

fn signed(a: P2, b: P2, c: P2) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Least squares conformal map: per triangle, the residuals of the
/// Cauchy-Riemann equations of the linear map from the triangle's own
/// plane to (u, v), weighted by the square root of its area; two points
/// far apart keep their planar coordinates.
fn lscm(p: &[V], tris: &[[usize; 3]], init: &[P2], job: &Job) -> Result<Vec<P2>, String> {
    let n = p.len();
    // Pins: the two extreme points along the planar coordinates' longer side.
    let (lo, hi) = init.iter().enumerate().fold(
        ([(0usize, f64::MAX); 2], [(0usize, f64::MIN); 2]),
        |(mut lo, mut hi), (i, q)| {
            for k in 0..2 {
                if q[k] < lo[k].1 {
                    lo[k] = (i, q[k]);
                }
                if q[k] > hi[k].1 {
                    hi[k] = (i, q[k]);
                }
            }
            (lo, hi)
        },
    );
    let k = if hi[0].1 - lo[0].1 >= hi[1].1 - lo[1].1 {
        0
    } else {
        1
    };
    let pins = [lo[k].0, hi[k].0];
    if pins[0] == pins[1] {
        return Ok(init.to_vec());
    }
    // Rows: per triangle two equations over 6 unknowns (u, v of 3 points).
    // Unknown 2i is u_i, 2i+1 is v_i.
    let mut rows: Vec<[(usize, f64); 6]> = Vec::with_capacity(tris.len() * 2);
    for t in tris {
        let [a, b, c] = t.map(|i| p[i]);
        let e1 = v3::sub(b, a);
        let l1 = v3::len(e1);
        let nrm = v3::cross(e1, v3::sub(c, a));
        let area2 = v3::len(nrm);
        if l1 <= 1e-30 || area2 <= 1e-30 {
            continue;
        }
        let x = v3::scale(e1, 1.0 / l1);
        let y = v3::cross(v3::scale(nrm, 1.0 / area2), x);
        let loc = |q: V| {
            let d = v3::sub(q, a);
            [v3::dot(d, x), v3::dot(d, y)]
        };
        let q = [[0.0, 0.0], loc(b), loc(c)];
        // grad phi_j = rot90(q[j+2] - q[j+1]) / (2A), rot90(x, y) = (-y, x).
        let s = (area2 / 2.0).sqrt() / area2;
        let g: [P2; 3] = std::array::from_fn(|j| {
            let (u, w) = (q[(j + 1) % 3], q[(j + 2) % 3]);
            [-(w[1] - u[1]) * s, (w[0] - u[0]) * s]
        });
        // du/dx - dv/dy and du/dy + dv/dx.
        rows.push(std::array::from_fn(|e| {
            let j = e / 2;
            if e % 2 == 0 {
                (2 * t[j], g[j][0])
            } else {
                (2 * t[j] + 1, -g[j][1])
            }
        }));
        rows.push(std::array::from_fn(|e| {
            let j = e / 2;
            if e % 2 == 0 {
                (2 * t[j], g[j][1])
            } else {
                (2 * t[j] + 1, g[j][0])
            }
        }));
    }
    let mut fixed = vec![false; 2 * n];
    for &i in &pins {
        fixed[2 * i] = true;
        fixed[2 * i + 1] = true;
    }
    let mut x: Vec<f64> = init.iter().flat_map(|q| [q[0], q[1]]).collect();
    // Normal equations A^T A x = 0 over the free unknowns, by conjugate
    // gradients with a Jacobi preconditioner.
    let apply = |x: &[f64], out: &mut [f64], free_only: bool| {
        out.iter_mut().for_each(|o| *o = 0.0);
        for r in &rows {
            let mut dot = 0.0;
            for &(i, w) in r {
                if !free_only || !fixed[i] {
                    dot += w * x[i];
                }
            }
            for &(i, w) in r {
                out[i] += w * dot;
            }
        }
        for (i, o) in out.iter_mut().enumerate() {
            if fixed[i] {
                *o = 0.0;
            }
        }
    };
    let mut diag = vec![0.0; 2 * n];
    for r in &rows {
        for &(i, w) in r {
            diag[i] += w * w;
        }
    }
    // r = -A^T A x (the residual of the system at the start).
    let mut res = vec![0.0; 2 * n];
    apply(&x, &mut res, false);
    res.iter_mut().for_each(|v| *v = -*v);
    let pre = |r: &[f64], z: &mut [f64]| {
        for i in 0..r.len() {
            z[i] = if fixed[i] || diag[i] <= 0.0 {
                0.0
            } else {
                r[i] / diag[i]
            };
        }
    };
    let mut z = vec![0.0; 2 * n];
    pre(&res, &mut z);
    let mut d = z.clone();
    let mut rz: f64 = res.iter().zip(&z).map(|(a, b)| a * b).sum();
    let start = rz.abs();
    let mut ad = vec![0.0; 2 * n];
    let iters = (4 * n).clamp(50, 5000);
    for it in 0..iters {
        if it % 64 == 0 {
            job.check()?;
        }
        if rz.abs() <= 1e-24 * start.max(1e-300) {
            break;
        }
        apply(&d, &mut ad, true);
        let dad: f64 = d.iter().zip(&ad).map(|(a, b)| a * b).sum();
        if dad <= 0.0 {
            break;
        }
        let alpha = rz / dad;
        for i in 0..x.len() {
            x[i] += alpha * d[i];
            res[i] -= alpha * ad[i];
        }
        pre(&res, &mut z);
        let rz2: f64 = res.iter().zip(&z).map(|(a, b)| a * b).sum();
        let beta = rz2 / rz;
        rz = rz2;
        for i in 0..d.len() {
            d[i] = z[i] + beta * d[i];
        }
    }
    Ok(x.chunks_exact(2).map(|c| [c[0], c[1]]).collect())
}

/// Turns, scales and places each chart; returns per chart the map from
/// its flat coordinates to the unit square.
#[allow(clippy::type_complexity)]
fn pack(charts: &[(Vec<u32>, Vec<P2>)], margin: f64) -> Vec<Box<dyn Fn(P2) -> P2>> {
    // Each chart turned to its smallest rectangle and moved to the
    // origin: (cos, sin, min, size).
    let mut fit: Vec<(f64, f64, P2, P2)> = Vec::with_capacity(charts.len());
    let mut total = 0.0;
    for (_, q) in charts {
        let h = hull2(q, 0.0);
        let mut best = (f64::MAX, 1.0, 0.0, [0.0; 2], [0.0; 2]);
        for i in 0..h.len().max(1) {
            let (c, s) = if h.len() >= 2 {
                let (a, b) = (q[h[i]], q[h[(i + 1) % h.len()]]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let l = (dx * dx + dy * dy).sqrt();
                if l <= 0.0 {
                    (1.0, 0.0)
                } else {
                    (dx / l, dy / l)
                }
            } else {
                (1.0, 0.0)
            };
            let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
            for &k in &h {
                let p = q[k];
                let r = [p[0] * c + p[1] * s, -p[0] * s + p[1] * c];
                for d in 0..2 {
                    lo[d] = lo[d].min(r[d]);
                    hi[d] = hi[d].max(r[d]);
                }
            }
            let size = [hi[0] - lo[0], hi[1] - lo[1]];
            if size[0] * size[1] < best.0 {
                best = (size[0] * size[1], c, s, lo, size);
            }
        }
        if !best.0.is_finite() {
            best = (0.0, 1.0, 0.0, [0.0; 2], [0.0; 2]);
        }
        total += best.4[0] * best.4[1];
        fit.push((best.1, best.2, best.3, best.4));
    }
    let pad = margin * total.sqrt().max(1e-12);
    // Stand each rectangle on its longer side, then shelves, tallest first.
    let mut order: Vec<usize> = (0..charts.len()).collect();
    let dims: Vec<(P2, bool)> = fit
        .iter()
        .map(|f| {
            let tall = f.3[1] > f.3[0];
            (if tall { [f.3[1], f.3[0]] } else { f.3 }, tall)
        })
        .collect();
    order.sort_by(|&a, &b| dims[b].0[1].total_cmp(&dims[a].0[1]).then(a.cmp(&b)));
    let widest = dims.iter().map(|d| d.0[0]).fold(0.0, f64::max);
    let side = (total.sqrt() * 1.15).max(widest) + 2.0 * pad;
    let mut at = vec![[0.0; 2]; charts.len()];
    let (mut x, mut y, mut shelf) = (pad, pad, 0.0f64);
    for &k in &order {
        let [w, h] = dims[k].0;
        if x + w + pad > side && x > pad {
            x = pad;
            y += shelf + pad;
            shelf = 0.0;
        }
        at[k] = [x, y];
        x += w + pad;
        shelf = shelf.max(h);
    }
    let used = [side, y + shelf + pad];
    let norm = used[0].max(used[1]).max(1e-30);
    (0..charts.len())
        .map(|k| {
            let (c, s, lo, size) = fit[k];
            let tall = dims[k].1;
            let o = at[k];
            Box::new(move |p: P2| {
                let r = [p[0] * c + p[1] * s - lo[0], -p[0] * s + p[1] * c - lo[1]];
                // A tall chart is turned a quarter, staying counter-clockwise.
                let r = if tall { [size[1] - r[1], r[0]] } else { r };
                [(o[0] + r[0]) / norm, (o[1] + r[1]) / norm]
            }) as Box<dyn Fn(P2) -> P2>
        })
        .collect()
}
