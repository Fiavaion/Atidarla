//! Normals: area-weighted point normals, flat (split) normals, primitive
//! normals, and a cusp angle that splits points along sharp edges.

use atd_core::geo::names;
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Interp, Mesh, limits, v3};

/// Area-weighted normals per point from the polygons that use it (unit
/// length; zero for points no polygon uses).
pub fn point_normals(m: &Mesh) -> Vec<[f32; 3]> {
    let mut acc = vec![[0.0f64; 3]; m.points()];
    for f in 0..m.prims() {
        if !m.closed[f] {
            continue;
        }
        let n = m.prim_normal(f);
        for &c in m.prim(f) {
            acc[c as usize] = v3::add(acc[c as usize], n);
        }
    }
    acc.into_iter().map(|n| v3::f32(v3::norm(n))).collect()
}

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("mode", "Mode", &["smooth", "flat", "prim"], "smooth").help(
            "smooth: point normals averaged over the polygons around each point (weighted by area), split where the cusp angle says; flat: every polygon gets its own points with its own normal (faceted look); prim: a normal per polygon as a primitive attribute, points untouched.",
        ),
        ParamSchema::float("cusp", "Cusp Angle", 180.0, Some(0.0), Some(180.0)).help(
            "smooth: edges where neighbouring polygons differ by more than this many degrees stay sharp (their points are split). 180 never splits.",
        ),
        ParamSchema::toggle("reverse", "Reverse", false)
            .help("Points the normals the other way (polygons are not changed)."),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let sign = if s.bool("reverse") { -1.0 } else { 1.0 };
    let mut out = match s.str("mode") {
        "prim" => {
            let mut out = m.clone();
            let n: Vec<[f32; 3]> = (0..m.prims())
                .map(|f| v3::f32(v3::scale(v3::norm(m.prim_normal(f)), sign)))
                .collect();
            out.set_prim_attr(Column::vec3(names::N, &n));
            return Ok(out);
        }
        "flat" => split(m, job, |_, _| false)?,
        _ => {
            let cusp = s.f64("cusp").clamp(0.0, 180.0);
            if cusp >= 180.0 {
                let mut out = m.clone();
                out.set_point_attr(Column::vec3(names::N, &point_normals(m)));
                out
            } else {
                let cos = cusp.to_radians().cos();
                let normals: Vec<[f64; 3]> =
                    (0..m.prims()).map(|f| v3::norm(m.prim_normal(f))).collect();
                split(m, job, |a, b| v3::dot(normals[a], normals[b]) >= cos)?
            }
        }
    };
    if sign < 0.0
        && let Some(n) = out.point_attr_mut(names::N).and_then(Column::floats_mut)
    {
        n.iter_mut().for_each(|x| *x = -*x);
    }
    Ok(out)
}

/// Splits each point among the polygons around it: polygons `a` and `b`
/// sharing a point keep one copy when `smooth(a, b)` (transitively through
/// the polygons around the point), else separate copies. Each copy's normal
/// averages its polygons.
fn split(m: &Mesh, job: &Job, smooth: impl Fn(usize, usize) -> bool) -> Result<Mesh, String> {
    limits::check(
        m.corners.len() as u64,
        m.prims() as u64,
        m.corners.len() as u64,
    )?;
    // Polygons around each point (CSR), in polygon order.
    let n = m.points();
    let mut count = vec![0u32; n + 1];
    for f in 0..m.prims() {
        if m.closed[f] {
            for &c in m.prim(f) {
                count[c as usize + 1] += 1;
            }
        }
    }
    for i in 0..n {
        count[i + 1] += count[i];
    }
    let mut fill = count.clone();
    let mut around = vec![0u32; count[n] as usize];
    for f in 0..m.prims() {
        if m.closed[f] {
            for &c in m.prim(f) {
                let slot = &mut fill[c as usize];
                around[*slot as usize] = f as u32;
                *slot += 1;
            }
        }
    }
    let normals: Vec<[f64; 3]> = (0..m.prims()).map(|f| m.prim_normal(f)).collect();
    let mut out = Mesh::new();
    let mut interp = Interp::new(&m.point_attrs, n);
    let mut nrm: Vec<[f32; 3]> = Vec::with_capacity(n);
    // The new point of each (point, polygon) slot, and of loose points.
    let mut new_of = vec![u32::MAX; around.len()];
    let mut loose = vec![u32::MAX; n];
    let mut cluster: Vec<usize> = Vec::new();
    for p in 0..n {
        job.every(p)?;
        let (a, b) = (count[p] as usize, count[p + 1] as usize);
        if a == b {
            // On no polygon (polylines, loose points): kept as it is.
            loose[p] = out.add_point(m.p[p]);
            interp.copy(p as u32);
            nrm.push([0.0; 3]);
            continue;
        }
        for s in a..b {
            if new_of[s] != u32::MAX {
                continue;
            }
            // Grow a cluster of polygons connected through smooth pairs.
            cluster.clear();
            cluster.push(s);
            let mut k = 0;
            while k < cluster.len() {
                let fa = around[cluster[k]] as usize;
                for t in a..b {
                    if new_of[t] == u32::MAX
                        && !cluster.contains(&t)
                        && smooth(fa, around[t] as usize)
                    {
                        cluster.push(t);
                    }
                }
                k += 1;
            }
            let id = out.add_point(m.p[p]);
            interp.copy(p as u32);
            let mut sum = [0.0; 3];
            for &t in &cluster {
                new_of[t] = id;
                sum = v3::add(sum, normals[around[t] as usize]);
            }
            nrm.push(v3::f32(v3::norm(sum)));
        }
    }
    out.point_attrs = interp.finish();
    let mut fill = count.clone();
    let mut corners = Vec::with_capacity(8);
    for f in 0..m.prims() {
        corners.clear();
        for &c in m.prim(f) {
            let c = c as usize;
            if m.closed[f] {
                let slot = &mut fill[c];
                corners.push(new_of[*slot as usize]);
                *slot += 1;
            } else if count[c] < count[c + 1] {
                // A polyline through a split point uses its first copy.
                corners.push(new_of[count[c] as usize]);
            } else {
                corners.push(loose[c]);
            }
        }
        out.add_prim(&corners, m.closed[f]);
    }
    out.prim_attrs = m.prim_attrs.clone();
    out.prim_groups = m.prim_groups.clone();
    out.point_groups = m.point_groups.clone();
    out.set_point_attr(Column::vec3(names::N, &nrm));
    Ok(out)
}
