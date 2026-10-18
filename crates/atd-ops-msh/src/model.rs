//! Modelling operators: Subdivide (Catmull-Clark), Extrude, UV Project and
//! Colour.

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

use atd_core::geo::{AttrClass, names};
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Interp, Mesh, limits, v3};
use crate::normals::point_normals;

fn first(inputs: &[Option<Mesh>]) -> Option<&Mesh> {
    inputs.first().and_then(Option::as_ref)
}

/// Recomputes point normals when the mesh had them.
fn refresh_normals(m: &mut Mesh, had: bool) {
    if had {
        let n = point_normals(m);
        m.set_point_attr(Column::vec3(names::N, &n));
    }
}

// --------------------------------------------------------------- subdivide --

pub fn subdivide_params() -> Vec<ParamSchema> {
    vec![ParamSchema::int("levels", "Levels", 1, Some(0), Some(6))
        .help("Times each polygon is split (each level turns every polygon into quads, about four times as many).")]
}

pub fn subdivide(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = first(inputs) else {
        return Ok(Mesh::new());
    };
    let had = m.point_attr(names::N).is_some();
    let mut m = m.clone();
    for _ in 0..s.i64("levels").clamp(0, 6) {
        // Each corner of a polygon becomes a quad.
        let corners: u64 = (0..m.prims())
            .filter(|&f| m.closed[f])
            .map(|f| m.prim(f).len() as u64)
            .sum();
        limits::check(
            m.points() as u64 + m.prims() as u64 + corners,
            corners + m.prims() as u64,
            corners * 4 + m.corners.len() as u64,
        )?;
        m = catmull_clark(&m, job)?;
    }
    refresh_normals(&mut m, had);
    Ok(m)
}

/// One Catmull-Clark step. Original points keep their indices (moved by
/// the vertex rule); face points and then edge points follow. Boundary
/// edges and points use the crease (B-spline) rules; non-manifold edges
/// behave as boundaries.
fn catmull_clark(m: &Mesh, job: &Job) -> Result<Mesh, String> {
    let n = m.points();
    let faces: Vec<usize> = (0..m.prims())
        .filter(|&f| m.closed[f] && m.prim(f).len() >= 3)
        .collect();
    // Edges in order of first use.
    let mut edge_of: HashMap<(u32, u32), u32> = HashMap::new();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut edge_faces: Vec<(u32, u32, u32)> = Vec::new(); // count, first, second face
    for (fi, &f) in faces.iter().enumerate() {
        job.every(fi)?;
        let p = m.prim(f);
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            let key = (a.min(b), a.max(b));
            let e = *edge_of.entry(key).or_insert_with(|| {
                edges.push(key);
                edge_faces.push((0, u32::MAX, u32::MAX));
                (edges.len() - 1) as u32
            });
            let ef = &mut edge_faces[e as usize];
            match ef.0 {
                0 => ef.1 = fi as u32,
                1 => ef.2 = fi as u32,
                _ => {}
            }
            ef.0 += 1;
        }
    }
    let pos = |i: u32| v3::of(m.p[i as usize]);
    // Face points.
    let face_pts: Vec<[f64; 3]> = faces
        .iter()
        .map(|&f| {
            let p = m.prim(f);
            let mut c = [0.0; 3];
            for &i in p {
                c = v3::add(c, pos(i));
            }
            v3::scale(c, 1.0 / p.len() as f64)
        })
        .collect();
    // Edge points.
    let edge_pts: Vec<[f64; 3]> = edges
        .iter()
        .zip(&edge_faces)
        .map(|(&(a, b), &(count, f0, f1))| {
            let mid = v3::scale(v3::add(pos(a), pos(b)), 0.5);
            if count == 2 {
                let fc = v3::scale(v3::add(face_pts[f0 as usize], face_pts[f1 as usize]), 0.5);
                v3::scale(v3::add(mid, fc), 0.5)
            } else {
                mid
            }
        })
        .collect();
    // Per point: incident faces (sum, count), incident edges.
    let mut fsum = vec![([0.0; 3], 0u32); n];
    for (fi, &f) in faces.iter().enumerate() {
        for &i in m.prim(f) {
            let s = &mut fsum[i as usize];
            s.0 = v3::add(s.0, face_pts[fi]);
            s.1 += 1;
        }
    }
    let mut point_edges: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (e, &(a, b)) in edges.iter().enumerate() {
        point_edges[a as usize].push(e as u32);
        point_edges[b as usize].push(e as u32);
    }
    let mut out = Mesh::new();
    out.p.reserve(n + faces.len() + edges.len());
    for i in 0..n {
        job.every(i)?;
        let p = pos(i as u32);
        let inc = &point_edges[i];
        let (fs, fc) = fsum[i];
        let new = if inc.is_empty() || fc == 0 {
            p
        } else {
            let boundary: Vec<u32> = inc
                .iter()
                .copied()
                .filter(|&e| edge_faces[e as usize].0 != 2)
                .collect();
            match boundary.len() {
                0 => {
                    let k = inc.len() as f64;
                    let f = v3::scale(fs, 1.0 / f64::from(fc));
                    let mut r = [0.0; 3];
                    for &e in inc {
                        let (a, b) = edges[e as usize];
                        r = v3::add(r, v3::scale(v3::add(pos(a), pos(b)), 0.5));
                    }
                    let r = v3::scale(r, 1.0 / k);
                    v3::scale(
                        v3::add(v3::add(f, v3::scale(r, 2.0)), v3::scale(p, k - 3.0)),
                        1.0 / k,
                    )
                }
                2 => {
                    let other = |e: u32| {
                        let (a, b) = edges[e as usize];
                        pos(if a == i as u32 { b } else { a })
                    };
                    let s = v3::add(other(boundary[0]), other(boundary[1]));
                    v3::scale(v3::add(v3::scale(p, 6.0), s), 1.0 / 8.0)
                }
                _ => p,
            }
        };
        out.p.push(v3::f32(new));
    }
    for q in face_pts.iter().chain(&edge_pts) {
        out.p.push(v3::f32(*q));
    }
    limits::check(out.p.len() as u64, 0, 0)?;
    // Attributes: original values at vertex points, averages at face and
    // edge points.
    let mut interp = Interp::new(&m.point_attrs, out.p.len());
    for i in 0..n {
        interp.copy(i as u32);
    }
    let mut w = Vec::new();
    for &f in &faces {
        let p = m.prim(f);
        w.clear();
        w.extend(p.iter().map(|&i| (i, 1.0 / p.len() as f32)));
        interp.blend(&w);
    }
    for &(a, b) in &edges {
        interp.blend(&[(a, 0.5), (b, 0.5)]);
    }
    out.point_attrs = interp.finish();
    // Topology: one quad per polygon corner; polylines kept.
    let face_base = n as u32;
    let edge_base = (n + faces.len()) as u32;
    let edge_pt = |a: u32, b: u32| edge_base + edge_of[&(a.min(b), a.max(b))];
    let mut pinterp = Interp::new(&m.prim_attrs, m.prims() * 4);
    let mut fi = 0;
    for f in 0..m.prims() {
        job.every(f)?;
        let p = m.prim(f);
        if !(m.closed[f] && p.len() >= 3) {
            if !m.closed[f] {
                out.add_prim(p, false);
                pinterp.copy(f as u32);
            }
            continue;
        }
        let k = p.len();
        let fp = face_base + fi as u32;
        for i in 0..k {
            let (prev, cur, next) = (p[(i + k - 1) % k], p[i], p[(i + 1) % k]);
            out.add_polygon(&[cur, edge_pt(cur, next), fp, edge_pt(prev, cur)]);
            pinterp.copy(f as u32);
        }
        fi += 1;
    }
    out.prim_attrs = pinterp.finish();
    out.point_groups = m.point_groups.clone();
    out.prim_groups = m.prim_groups.clone();
    Ok(out)
}

// ----------------------------------------------------------------- extrude --

pub fn extrude_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::string("group", "Group", "")
            .help("The primitive group to extrude (empty: every polygon)."),
        ParamSchema::menu("mode", "Mode", &["individual", "connected"], "individual").help(
            "individual: each polygon on its own, with side walls on all its edges; connected: neighbouring polygons move together and only the region's outline gets walls.",
        ),
        ParamSchema::float("distance", "Distance", 0.2, None, None)
            .help("How far the polygons move along their normals (negative: inwards)."),
        ParamSchema::float("inset", "Inset", 0.0, Some(0.0), Some(1.0))
            .help("individual: shrinks each moved polygon towards its centre (0: none, 1: to a point)."),
    ]
}

/// Adds polygon `ids` copying primitive `from`'s attributes, a member of
/// `kind` and not of `other`.
fn add(
    out: &mut Mesh,
    prims: &mut Interp<'_>,
    ids: &[u32],
    from: usize,
    kind: &mut Vec<bool>,
    other: &mut Vec<bool>,
) {
    out.add_polygon(ids);
    prims.copy(from as u32);
    kind.push(true);
    other.push(false);
}

/// Extrudes polygons along their normals; the moved polygons form the
/// prim group `extrude_front`, the new walls `extrude_side`.
pub fn extrude(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = first(inputs) else {
        return Ok(Mesh::new());
    };
    let name = s.str("group");
    let selected: Vec<bool> = if name.is_empty() {
        m.closed.clone()
    } else {
        let g = m
            .group_members(AttrClass::Prim, name)
            .ok_or_else(|| format!("the input has no primitive group `{name}`"))?;
        g.iter().zip(&m.closed).map(|(a, b)| *a && *b).collect()
    };
    let had = m.point_attr(names::N).is_some();
    let dist = s.f64("distance");
    let corners: usize = (0..m.prims())
        .filter(|&f| selected[f])
        .map(|f| m.prim(f).len())
        .sum();
    limits::check(
        (m.points() + corners) as u64,
        (m.prims() + corners) as u64,
        (m.corners.len() + corners * 5) as u64,
    )?;
    let mut out = Mesh {
        p: m.p.clone(),
        point_groups: m.point_groups.clone(),
        prim_groups: m.prim_groups.clone(),
        ..Mesh::new()
    };
    let mut pts = Interp::new(&m.point_attrs, m.points() + corners);
    for i in 0..m.points() {
        pts.copy(i as u32);
    }
    let mut prims = Interp::new(&m.prim_attrs, m.prims() + corners);
    let mut front = Vec::new();
    let mut side = Vec::new();
    // Unselected primitives first, as they were.
    for f in 0..m.prims() {
        if !selected[f] {
            out.add_prim(m.prim(f), m.closed[f]);
            prims.copy(f as u32);
            front.push(false);
            side.push(false);
        }
    }
    if s.str("mode") == "connected" {
        // One new point per point of the region, moved along the mean of
        // its selected polygons' normals.
        let mut moved = vec![u32::MAX; m.points()];
        let mut dir = vec![[0.0f64; 3]; m.points()];
        for f in 0..m.prims() {
            if selected[f] {
                let n = v3::norm(m.prim_normal(f));
                for &c in m.prim(f) {
                    dir[c as usize] = v3::add(dir[c as usize], n);
                }
            }
        }
        // Region edges used once (in their polygon's direction) are its
        // outline.
        let mut uses: HashMap<(u32, u32), i32> = HashMap::new();
        for f in 0..m.prims() {
            if selected[f] {
                let p = m.prim(f);
                for i in 0..p.len() {
                    let (a, b) = (p[i], p[(i + 1) % p.len()]);
                    *uses.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
        }
        for f in 0..m.prims() {
            job.every(f)?;
            if !selected[f] {
                continue;
            }
            let p = m.prim(f);
            let mut ids = Vec::with_capacity(p.len());
            for &c in p {
                if moved[c as usize] == u32::MAX {
                    let d = v3::norm(dir[c as usize]);
                    let q = v3::add(v3::of(m.p[c as usize]), v3::scale(d, dist));
                    moved[c as usize] = out.add_point(v3::f32(q));
                    pts.copy(c);
                }
                ids.push(moved[c as usize]);
            }
            add(&mut out, &mut prims, &ids, f, &mut front, &mut side);
            for i in 0..p.len() {
                let (a, b) = (p[i], p[(i + 1) % p.len()]);
                if uses[&(a.min(b), a.max(b))] == 1 {
                    let wall = [a, b, moved[b as usize], moved[a as usize]];
                    add(&mut out, &mut prims, &wall, f, &mut side, &mut front);
                }
            }
        }
    } else {
        let inset = s.f64("inset").clamp(0.0, 1.0);
        for f in 0..m.prims() {
            job.every(f)?;
            if !selected[f] {
                continue;
            }
            let p = m.prim(f);
            let n = v3::norm(m.prim_normal(f));
            let mut c = [0.0; 3];
            for &i in p {
                c = v3::add(c, v3::of(m.p[i as usize]));
            }
            let c = v3::scale(c, 1.0 / p.len() as f64);
            let ids: Vec<u32> = p
                .iter()
                .map(|&i| {
                    let q = v3::lerp(v3::of(m.p[i as usize]), c, inset);
                    let id = out.add_point(v3::f32(v3::add(q, v3::scale(n, dist))));
                    pts.copy(i);
                    id
                })
                .collect();
            add(&mut out, &mut prims, &ids, f, &mut front, &mut side);
            for i in 0..p.len() {
                let j = (i + 1) % p.len();
                let wall = [p[i], p[j], ids[j], ids[i]];
                add(&mut out, &mut prims, &wall, f, &mut side, &mut front);
            }
        }
    }
    out.point_attrs = pts.finish();
    out.prim_attrs = prims.finish();
    // Points only the moved polygons used are gone (loose points stay).
    let mut used_before = vec![false; m.points()];
    for &c in &m.corners {
        used_before[c as usize] = true;
    }
    let mut used = vec![false; out.points()];
    for &c in &out.corners {
        used[c as usize] = true;
    }
    let keep: Vec<bool> = (0..out.points())
        .map(|i| used[i] || i >= m.points() || !used_before[i])
        .collect();
    out.keep_points(&keep);
    out.set_group(AttrClass::Prim, "extrude_front", &front)?;
    out.set_group(AttrClass::Prim, "extrude_side", &side)?;
    refresh_normals(&mut out, had);
    Ok(out)
}

// -------------------------------------------------------------- uv project --

pub fn uv_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("projection", "Projection", &["planar", "cylindrical", "spherical", "box"], "planar").help(
            "planar: straight along the axis; cylindrical: around the axis (u) and along it (v); spherical: longitude and latitude around the centre; box: planar along whichever axis each point's normal faces most.",
        ),
        ParamSchema::menu("axis", "Axis", &["x", "y", "z"], "z")
            .help("planar: the projection direction; cylindrical and spherical: the axis (the poles)."),
        ParamSchema::toggle("fit", "Fit to Bounds", true)
            .help("Scales the projection so the geometry's bounds span 0 to 1 (else world units)."),
        ParamSchema::vec("scale", "Scale", &[1.0, 1.0]).help("Multiplies u and v."),
        ParamSchema::vec("offset", "Offset", &[0.0, 0.0]).help("Added to u and v after scaling."),
    ]
}

/// Writes point texture coordinates `uv`.
pub fn uv_project(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = first(inputs) else {
        return Ok(Mesh::new());
    };
    let mut out = m.clone();
    let Some((lo, hi)) = m.bounds() else {
        return Ok(out);
    };
    let (lo, hi) = (v3::of(lo), v3::of(hi));
    let fit = s.bool("fit");
    let size = v3::sub(hi, lo).map(|x| if x > 1e-12 { x } else { 1.0 });
    let centre = v3::scale(v3::add(lo, hi), 0.5);
    let axis = match s.str("axis") {
        "x" => 0,
        "y" => 1,
        _ => 2,
    };
    // The two in-plane axes for a projection along `k` (right-handed).
    let plane = |k: usize| match k {
        0 => (2, 1),
        1 => (0, 2),
        _ => (0, 1),
    };
    // Box projections use one scale for every side, so faces match.
    let uniform = s.str("projection") == "box";
    let big = size[0].max(size[1]).max(size[2]);
    let planar = |p: [f64; 3], k: usize| {
        let (a, b) = plane(k);
        if !fit {
            [p[a], p[b]]
        } else if uniform {
            [(p[a] - lo[a]) / big, (p[b] - lo[b]) / big]
        } else {
            [(p[a] - lo[a]) / size[a], (p[b] - lo[b]) / size[b]]
        }
    };
    let normals = if s.str("projection") == "box" {
        m.vec3(names::N).unwrap_or_else(|| point_normals(m))
    } else {
        Vec::new()
    };
    let (sc, off) = (s.vec2("scale"), s.vec2("offset"));
    let mut uv = Vec::with_capacity(m.points() * 2);
    for (i, &p) in m.p.iter().enumerate() {
        job.every(i)?;
        let p = v3::of(p);
        let q = match s.str("projection") {
            "cylindrical" => {
                let (a, b) = plane(axis);
                let d = v3::sub(p, centre);
                let u = d[b].atan2(d[a]) / TAU + 0.5;
                let v = if fit {
                    (p[axis] - lo[axis]) / size[axis]
                } else {
                    p[axis]
                };
                [u, v]
            }
            "spherical" => {
                let (a, b) = plane(axis);
                let d = v3::sub(p, centre);
                let r = v3::len(d).max(1e-30);
                let u = d[b].atan2(d[a]) / TAU + 0.5;
                let v = 1.0 - (d[axis] / r).clamp(-1.0, 1.0).acos() / PI;
                [u, v]
            }
            "box" => {
                let n = normals[i];
                let k = (0..3)
                    .max_by(|&x, &y| n[x].abs().total_cmp(&n[y].abs()))
                    .unwrap_or(2);
                planar(p, k)
            }
            _ => planar(p, axis),
        };
        uv.push((q[0] as f32) * sc[0] + off[0]);
        uv.push((q[1] as f32) * sc[1] + off[1]);
    }
    out.set_point_attr(Column::f32(names::UV, 2, uv));
    Ok(out)
}

// ------------------------------------------------------------------ colour --

pub fn colour_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu(
            "mode",
            "Mode",
            &["constant", "normal", "uv", "position", "random", "random_prim"],
            "constant",
        )
        .help(
            "constant: the tint; normal: N mapped to 0-1 (computed when missing); uv: (u, v, 0); position: where each point sits in the bounds; random: a seeded colour per point; random_prim: per primitive (its points are split so each primitive is one colour).",
        ),
        ParamSchema::colour("colour", "Tint", [1.0, 1.0, 1.0, 1.0])
            .help("The colour, or the multiplier of the others."),
        ParamSchema::int("seed", "Seed", 1, Some(0), None).help("random: the seed."),
    ]
}

/// Writes point colours `Cd` (RGBA).
pub fn colour(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = first(inputs) else {
        return Ok(Mesh::new());
    };
    let tint = s.vec4("colour");
    let seed = s.i64("seed").max(0) as u64;
    let mode = s.str("mode");
    let mut out = if mode == "random_prim" {
        // Every primitive gets its own points.
        crate::normals::run(
            &Snap::from_values(
                &crate::schemas()
                    .into_iter()
                    .find(|o| o.type_id == "msh.normals")
                    .expect("registered"),
                &[("mode", serde_json::json!("flat"))],
            ),
            inputs,
            job,
        )?
    } else {
        m.clone()
    };
    let n = out.points();
    let mut cd = Vec::with_capacity(n * 4);
    let normals =
        (mode == "normal").then(|| out.vec3(names::N).unwrap_or_else(|| point_normals(&out)));
    let uv = out
        .point_attr(names::UV)
        .filter(|c| c.comps == 2)
        .and_then(Column::floats)
        .map(<[f32]>::to_vec);
    let (lo, hi) = out.bounds().unwrap_or(([0.0; 3], [1.0; 3]));
    let mut prim_of = vec![0u64; n];
    for f in 0..out.prims() {
        for &c in out.prim(f) {
            prim_of[c as usize] = f as u64;
        }
    }
    for i in 0..n {
        job.every(i)?;
        let rgb: [f32; 3] = match mode {
            "normal" => normals.as_ref().expect("computed")[i].map(|x| x * 0.5 + 0.5),
            "uv" => uv
                .as_ref()
                .map_or([0.0; 3], |v| [v[i * 2], v[i * 2 + 1], 0.0]),
            "position" => std::array::from_fn(|k| {
                let e = hi[k] - lo[k];
                if e > 0.0 {
                    (out.p[i][k] - lo[k]) / e
                } else {
                    0.5
                }
            }),
            "random" | "random_prim" => {
                let key = if mode == "random" {
                    i as u64
                } else {
                    prim_of[i]
                };
                std::array::from_fn(|k| crate::points::unit(seed, key, k as u64) as f32)
            }
            _ => [1.0; 3],
        };
        cd.extend([
            rgb[0] * tint[0],
            rgb[1] * tint[1],
            rgb[2] * tint[2],
            tint[3],
        ]);
    }
    out.set_point_attr(Column::f32(names::CD, 4, cd));
    Ok(out)
}
