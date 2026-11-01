//! Mirror: reflects geometry across a plane, keeping or replacing the
//! original, and optionally welds the seam so a half model becomes one
//! closed piece.

use lsr_core::geo::{AttrClass, names};
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Interp, Mesh, limits, v3};

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::vec("origin", "Origin", &[0.0, 0.0, 0.0]).help("A point on the mirror plane."),
        ParamSchema::vec("normal", "Normal", &[1.0, 0.0, 0.0])
            .help("The mirror plane's normal (the default mirrors X to -X)."),
        ParamSchema::toggle("keep_original", "Keep Original", true)
            .help("Outputs the original and its mirror image; off, only the mirror image."),
        ParamSchema::toggle("merge_seam", "Merge Seam", true).help(
            "Keep Original: points within Seam Distance of the plane are moved onto it and shared by both halves, and polygons lying in the plane (which would be inside) are removed.",
        ),
        ParamSchema::float("distance", "Seam Distance", 1e-4, Some(0.0), None)
            .help("Merge Seam: how close to the plane a point must be to join the seam."),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let n = v3::of(s.vec3("normal"));
    if v3::len(n) < 1e-12 {
        return Err("the mirror plane's normal is zero".into());
    }
    let n = v3::norm(n);
    let o = v3::of(s.vec3("origin"));
    let dist = |p: [f32; 3]| v3::dot(v3::sub(v3::of(p), o), n);
    let reflect_point = |p: [f32; 3]| v3::f32(v3::sub(v3::of(p), v3::scale(n, 2.0 * dist(p))));
    let reflect_vec = |v: [f64; 3]| v3::sub(v, v3::scale(n, 2.0 * v3::dot(v, n)));
    let keep = s.bool("keep_original");
    let merge = keep && s.bool("merge_seam");
    let tol = s.f64("distance").max(0.0);
    let np = m.points();
    let seam: Vec<bool> = m.p.iter().map(|&p| merge && dist(p).abs() <= tol).collect();
    // A flat input lying in the plane mirrors onto itself: nothing is
    // inside, so nothing is removed.
    let all_in_plane = seam.iter().all(|&b| b);
    let in_plane = |f: usize| {
        merge && !all_in_plane && m.closed[f] && m.prim(f).iter().all(|&c| seam[c as usize])
    };
    let base = if keep { np } else { 0 };
    let extra = np - seam.iter().filter(|&&b| b).count();
    limits::check(
        (base + extra) as u64,
        (m.prims() * 2) as u64,
        (m.corners.len() * 2) as u64,
    )?;
    let mut out = Mesh {
        point_groups: m.point_groups.clone(),
        prim_groups: m.prim_groups.clone(),
        ..Mesh::new()
    };
    let mut pts = Interp::new(&m.point_attrs, base + extra);
    if keep {
        for (i, &p) in m.p.iter().enumerate() {
            // Seam points move onto the plane, so both halves meet exactly.
            out.add_point(if seam[i] {
                v3::f32(v3::sub(v3::of(p), v3::scale(n, dist(p))))
            } else {
                p
            });
            pts.copy(i as u32);
        }
    }
    let mut image = vec![0u32; np];
    for (i, &p) in m.p.iter().enumerate() {
        job.every(i)?;
        image[i] = if seam[i] {
            i as u32
        } else {
            pts.copy(i as u32);
            out.add_point(reflect_point(p))
        };
    }
    out.point_attrs = pts.finish();
    // Vector attributes that are directions turn with the reflection.
    if let Some(col) = out
        .point_attrs
        .iter_mut()
        .find(|c| c.name == names::N && c.comps == 3)
        && let Some(v) = col.floats_mut()
    {
        for c in v.chunks_exact_mut(3).skip(base) {
            let r = reflect_vec([c[0], c[1], c[2]].map(f64::from));
            c.copy_from_slice(&v3::f32(r));
        }
    }
    let mut prims = Interp::new(&m.prim_attrs, m.prims() * 2);
    let mut from_copy = Vec::with_capacity(m.prims() * 2);
    if keep {
        for f in 0..m.prims() {
            if !in_plane(f) {
                out.add_prim(m.prim(f), m.closed[f]);
                prims.copy(f as u32);
                from_copy.push(false);
            }
        }
    }
    let mut ids = Vec::new();
    for f in 0..m.prims() {
        job.every(f)?;
        if in_plane(f) {
            continue;
        }
        ids.clear();
        ids.extend(m.prim(f).iter().map(|&c| image[c as usize]));
        // A reflection turns polygons inside out; reversing them keeps
        // them facing outward.
        if m.closed[f] {
            ids.reverse();
        }
        out.add_prim(&ids, m.closed[f]);
        prims.copy(f as u32);
        from_copy.push(true);
    }
    out.prim_attrs = prims.finish();
    if let Some(col) = out
        .prim_attrs
        .iter_mut()
        .find(|c| c.name == names::N && c.comps == 3)
        && let Some(v) = col.floats_mut()
    {
        for (c, &mirrored) in v.chunks_exact_mut(3).zip(&from_copy) {
            if mirrored {
                let r = reflect_vec([c[0], c[1], c[2]].map(f64::from));
                c.copy_from_slice(&v3::f32(r));
            }
        }
    }
    out.set_group(AttrClass::Prim, "mirrored", &from_copy)?;
    if keep && merge && m.prims() > 0 {
        out.remove_unused_points();
    }
    if out.point_attr(names::N).is_some() && merge {
        // Seam points are shared: their normals average both sides.
        let nn = crate::normals::point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}
