//! Topology and attribute edits: Triangulate, Transform, Merge, Delete,
//! Group and Fuse.

use std::collections::HashMap;

use lsr_core::geo::{AttrClass, names};
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Interp, Mesh, limits, v3};
use crate::normals::point_normals;

fn first(inputs: &[Option<Mesh>]) -> Option<&Mesh> {
    inputs.first().and_then(Option::as_ref)
}

// ------------------------------------------------------------ triangulate --

/// Splits polygon `ids` into triangles (ear clipping in the polygon's
/// dominant plane; convex polygons are fanned). Degenerate parts are
/// dropped.
pub fn triangulate_polygon(p: &[[f32; 3]], ids: &[u32], out: &mut Vec<[u32; 3]>) {
    let n = ids.len();
    if n < 3 {
        return;
    }
    if n == 3 {
        out.push([ids[0], ids[1], ids[2]]);
        return;
    }
    let normal = crate::mesh::newell(ids.iter().map(|&i| p[i as usize]));
    // Drop the dominant axis; keep orientation counter-clockwise.
    let k = (0..3)
        .max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs()))
        .unwrap_or(2);
    let (a, b) = match k {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    let flip = normal[k] < 0.0;
    let pt = |i: u32| {
        let q = p[i as usize];
        let (x, y) = (f64::from(q[a]), f64::from(q[b]));
        if flip { (y, x) } else { (x, y) }
    };
    let cross = |o: (f64, f64), u: (f64, f64), v: (f64, f64)| {
        (u.0 - o.0) * (v.1 - o.1) - (u.1 - o.1) * (v.0 - o.0)
    };
    let pts: Vec<(f64, f64)> = ids.iter().map(|&i| pt(i)).collect();
    let convex = (0..n).all(|i| cross(pts[i], pts[(i + 1) % n], pts[(i + 2) % n]) >= 0.0);
    if convex {
        for i in 1..n - 1 {
            out.push([ids[0], ids[i], ids[i + 1]]);
        }
        return;
    }
    let mut v: Vec<usize> = (0..n).collect();
    let inside = |t: [(f64, f64); 3], q: (f64, f64)| {
        cross(t[0], t[1], q) >= 0.0 && cross(t[1], t[2], q) >= 0.0 && cross(t[2], t[0], q) >= 0.0
    };
    let mut guard = 0;
    while v.len() > 3 && guard < 2 * n * n {
        guard += 1;
        let m = v.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (v[(i + m - 1) % m], v[i], v[(i + 1) % m]);
            let t = [pts[ia], pts[ib], pts[ic]];
            if cross(t[0], t[1], t[2]) <= 0.0 {
                continue;
            }
            let blocked = v.iter().any(|&j| {
                j != ia
                    && j != ib
                    && j != ic
                    && pts[j] != t[0]
                    && pts[j] != t[1]
                    && pts[j] != t[2]
                    && inside(t, pts[j])
            });
            if blocked {
                continue;
            }
            out.push([ids[ia], ids[ib], ids[ic]]);
            v.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            // Self-intersecting or degenerate: drop the flattest corner.
            let m = v.len();
            let worst = (0..m)
                .min_by(|&x, &y| {
                    let c = |i: usize| {
                        cross(pts[v[(i + m - 1) % m]], pts[v[i]], pts[v[(i + 1) % m]]).abs()
                    };
                    c(x).total_cmp(&c(y))
                })
                .unwrap_or(0);
            v.remove(worst);
        }
    }
    if v.len() == 3 && cross(pts[v[0]], pts[v[1]], pts[v[2]]) > 0.0 {
        out.push([ids[v[0]], ids[v[1]], ids[v[2]]]);
    }
}

/// Every polygon as triangles (polylines kept); primitive attributes are
/// copied to the triangles of their polygon.
pub fn triangulated(m: &Mesh, job: &Job) -> Result<Mesh, String> {
    let mut out = Mesh {
        p: m.p.clone(),
        point_attrs: m.point_attrs.clone(),
        point_groups: m.point_groups.clone(),
        prim_groups: m.prim_groups.clone(),
        ..Mesh::new()
    };
    let mut interp = Interp::new(&m.prim_attrs, m.prims());
    let mut tris = Vec::new();
    for f in 0..m.prims() {
        job.every(f)?;
        if !m.closed[f] {
            out.add_prim(m.prim(f), false);
            interp.copy(f as u32);
            continue;
        }
        tris.clear();
        triangulate_polygon(&m.p, m.prim(f), &mut tris);
        limits::check(0, (out.prims() + tris.len()) as u64, 0)?;
        for t in &tris {
            out.add_polygon(t);
            interp.copy(f as u32);
        }
    }
    out.prim_attrs = interp.finish();
    Ok(out)
}

pub fn triangulate_params() -> Vec<ParamSchema> {
    vec![]
}

pub fn triangulate(_: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    first(inputs).map_or_else(|| Ok(Mesh::new()), |m| triangulated(m, job))
}

// -------------------------------------------------------------- transform --

pub fn transform_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::vec("translate", "Translate", &[0.0, 0.0, 0.0]).help("Moves the geometry."),
        ParamSchema::vec("rotate", "Rotate", &[0.0, 0.0, 0.0])
            .help("Degrees about X, then Y, then Z."),
        ParamSchema::vec("scale", "Scale", &[1.0, 1.0, 1.0]).help("Scale along X, Y and Z."),
        ParamSchema::float("uniform", "Uniform Scale", 1.0, None, None)
            .help("Multiplies the scale on every axis."),
        ParamSchema::vec("pivot", "Pivot", &[0.0, 0.0, 0.0])
            .help("The point that scaling and rotation keep in place."),
    ]
}

/// A 3×3 rotation, X then Y then Z (degrees).
pub fn rotation(deg: [f32; 3]) -> [[f64; 3]; 3] {
    let [x, y, z] = deg.map(|d| f64::from(d).to_radians());
    let (sx, cx, sy, cy, sz, cz) = (x.sin(), x.cos(), y.sin(), y.cos(), z.sin(), z.cos());
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    mul(rz, mul(ry, rx))
}

pub fn mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

pub fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

pub fn transform(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = first(inputs) else {
        return Ok(Mesh::new());
    };
    let mut out = m.clone();
    let r = rotation(s.vec3("rotate"));
    let u = s.f64("uniform");
    let sc = s.vec3("scale").map(|x| f64::from(x) * u);
    let t = v3::of(s.vec3("translate"));
    let pivot = v3::of(s.vec3("pivot"));
    for (i, p) in out.p.iter_mut().enumerate() {
        job.every(i)?;
        let q = v3::sub(v3::of(*p), pivot);
        let q = apply(r, [q[0] * sc[0], q[1] * sc[1], q[2] * sc[2]]);
        *p = v3::f32(v3::add(v3::add(q, pivot), t));
    }
    // Normals: the inverse transpose (rotation of n / scale), renormalised.
    let flip = sc[0] * sc[1] * sc[2] < 0.0;
    let inv = sc.map(|x| if x == 0.0 { 0.0 } else { 1.0 / x });
    for cols in [&mut out.point_attrs, &mut out.prim_attrs] {
        if let Some(v) = cols
            .iter_mut()
            .find(|c| c.name == names::N && c.comps == 3)
            .and_then(Column::floats_mut)
        {
            for n in v.chunks_exact_mut(3) {
                let q = [0, 1, 2].map(|k| f64::from(n[k]) * inv[k]);
                n.copy_from_slice(&v3::f32(v3::norm(apply(r, q))));
            }
        }
    }
    // A mirror (negative scale) turns polygons inside out; flip them back.
    if flip {
        crate::prims::flip(&mut out);
    }
    Ok(out)
}

// ------------------------------------------------------------------ merge --

pub fn merge_params() -> Vec<ParamSchema> {
    vec![]
}

/// Appends meshes: attributes are the union by name (missing values are
/// zero, or one for `Cd` and `pscale`), groups the union by name.
pub fn merged(meshes: &[&Mesh], job: &Job) -> Result<Mesh, String> {
    let points: usize = meshes.iter().map(|m| m.points()).sum();
    let prims: usize = meshes.iter().map(|m| m.prims()).sum();
    let corners: usize = meshes.iter().map(|m| m.corners.len()).sum();
    limits::check(points as u64, prims as u64, corners as u64)?;
    let mut out = Mesh::new();
    for class in [AttrClass::Point, AttrClass::Prim] {
        let mut cols: Vec<Column> = Vec::new();
        let total = if class == AttrClass::Point {
            points
        } else {
            prims
        };
        for m in meshes {
            let src = if class == AttrClass::Point {
                &m.point_attrs
            } else {
                &m.prim_attrs
            };
            for c in src {
                if c.name != names::GROUP && cols.iter().all(|o| o.name != c.name) {
                    cols.push(c.empty_like(total));
                }
            }
        }
        for m in meshes {
            job.check()?;
            let (src, n) = if class == AttrClass::Point {
                (&m.point_attrs, m.points())
            } else {
                (&m.prim_attrs, m.prims())
            };
            for o in &mut cols {
                match src.iter().find(|c| c.name == o.name && c.ty() == o.ty()) {
                    Some(c) => (0..n).for_each(|i| o.push_copy(c, i)),
                    None => (0..n).for_each(|_| o.push_default()),
                }
            }
        }
        if class == AttrClass::Point {
            out.point_attrs = cols;
        } else {
            out.prim_attrs = cols;
        }
    }
    for m in meshes {
        let base = out.points() as u32;
        out.p.extend_from_slice(&m.p);
        for f in 0..m.prims() {
            let ids: Vec<u32> = m.prim(f).iter().map(|c| c + base).collect();
            out.add_prim(&ids, m.closed[f]);
        }
    }
    // Groups by name, remapped to the merged bits.
    for class in [AttrClass::Point, AttrClass::Prim] {
        let mut names_all: Vec<String> = Vec::new();
        for m in meshes {
            let g = if class == AttrClass::Point {
                &m.point_groups
            } else {
                &m.prim_groups
            };
            for name in g {
                if !names_all.contains(name) {
                    names_all.push(name.clone());
                }
            }
        }
        for name in names_all.iter().take(lsr_core::geo::MAX_GROUPS) {
            let mut members = Vec::new();
            for m in meshes {
                let n = if class == AttrClass::Point {
                    m.points()
                } else {
                    m.prims()
                };
                match m.group_members(class, name) {
                    Some(v) => members.extend(v),
                    None => members.extend(std::iter::repeat_n(false, n)),
                }
            }
            out.set_group(class, name, &members)?;
        }
    }
    Ok(out)
}

pub fn merge(_: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let meshes: Vec<&Mesh> = inputs.iter().flatten().collect();
    merged(&meshes, job)
}

// ----------------------------------------------------------------- delete --

pub fn delete_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::string("group", "Group", "")
            .help("The group whose elements are deleted (empty: none, or all with Invert)."),
        ParamSchema::menu("class", "Class", &["prim", "point"], "prim")
            .help("prim: deletes primitives; point: deletes points and the primitives using them."),
        ParamSchema::toggle("invert", "Invert", false)
            .help("Deletes everything outside the group instead."),
        ParamSchema::toggle("keep_points", "Keep Points", false)
            .help("prim: keeps points no remaining primitive uses."),
    ]
}

pub fn delete(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = first(inputs) else {
        return Ok(Mesh::new());
    };
    let class = if s.str("class") == "point" {
        AttrClass::Point
    } else {
        AttrClass::Prim
    };
    let n = if class == AttrClass::Point {
        m.points()
    } else {
        m.prims()
    };
    let name = s.str("group");
    let mut doomed = if name.is_empty() {
        vec![false; n]
    } else {
        m.group_members(class, name)
            .ok_or_else(|| format!("the input has no {class} group `{name}`"))?
    };
    if s.bool("invert") {
        doomed.iter_mut().for_each(|d| *d = !*d);
    }
    let mut out = m.clone();
    let mut keep_prim: Vec<bool> = (0..m.prims())
        .map(|f| class == AttrClass::Point || !doomed[f])
        .collect();
    if class == AttrClass::Point {
        for (f, keep) in keep_prim.iter_mut().enumerate() {
            *keep = m.prim(f).iter().all(|&c| !doomed[c as usize]);
        }
    }
    job.check()?;
    filter_prims(&mut out, &keep_prim);
    if class == AttrClass::Point {
        let keep: Vec<bool> = doomed.iter().map(|d| !d).collect();
        out.keep_points(&keep);
    } else if !s.bool("keep_points") && m.prims() > 0 {
        out.remove_unused_points();
    }
    Ok(out)
}

/// Keeps the primitives flagged in `keep`, with their attributes.
pub fn filter_prims(m: &mut Mesh, keep: &[bool]) {
    let mut out = Mesh::new();
    let mut interp = Interp::new(&m.prim_attrs, m.prims());
    for (f, &k) in keep.iter().enumerate() {
        if k {
            out.add_prim(m.prim(f), m.closed[f]);
            interp.copy(f as u32);
        }
    }
    m.start = out.start;
    m.corners = out.corners;
    m.closed = out.closed;
    m.prim_attrs = interp.finish();
}

// ------------------------------------------------------------------ group --

pub fn group_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::string("name", "Name", "group1").help("The group's name."),
        ParamSchema::menu("class", "Class", &["prim", "point"], "prim")
            .help("Whether the group holds primitives or points."),
        ParamSchema::menu("method", "Method", &["box", "normal", "range", "all"], "box").help(
            "box: elements (points, or primitive centres) inside a box; normal: elements facing a direction; range: every n-th element of an index range; all: everything.",
        ),
        ParamSchema::vec("box_centre", "Box Centre", &[0.0, 0.0, 0.0]).help("box: centre."),
        ParamSchema::vec("box_size", "Box Size", &[1.0, 1.0, 1.0]).help("box: extent."),
        ParamSchema::vec("direction", "Direction", &[0.0, 1.0, 0.0]).help("normal: direction."),
        ParamSchema::float("angle", "Spread Angle", 45.0, Some(0.0), Some(180.0))
            .help("normal: most degrees between an element's normal and the direction."),
        ParamSchema::int("range_start", "Range Start", 0, Some(0), None)
            .help("range: first index."),
        ParamSchema::int("range_end", "Range End", -1, None, None)
            .help("range: last index (negative counts from the end)."),
        ParamSchema::int("range_step", "Range Step", 2, Some(1), None)
            .help("range: every n-th element."),
        ParamSchema::menu("combine", "Combine", &["replace", "union", "intersect", "subtract"], "replace")
            .help("How the new selection combines with an existing group of that name."),
    ]
}

pub fn group(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = first(inputs) else {
        return Ok(Mesh::new());
    };
    let name = s.str("name");
    if !lsr_core::geo::valid_name(name) {
        return Err(format!(
            "`{name}` is not a valid group name (letters, digits and _)"
        ));
    }
    let class = if s.str("class") == "point" {
        AttrClass::Point
    } else {
        AttrClass::Prim
    };
    let n = if class == AttrClass::Point {
        m.points()
    } else {
        m.prims()
    };
    // Position and normal per element.
    let (pos, nrm): (Vec<[f64; 3]>, Vec<[f64; 3]>) = if class == AttrClass::Point {
        let normals = m.vec3(names::N).unwrap_or_else(|| point_normals(m));
        (
            m.p.iter().map(|&p| v3::of(p)).collect(),
            normals.iter().map(|&n| v3::norm(v3::of(n))).collect(),
        )
    } else {
        (0..m.prims())
            .map(|f| {
                let pts = m.prim(f);
                let mut c = [0.0; 3];
                for &i in pts {
                    c = v3::add(c, v3::of(m.p[i as usize]));
                }
                (
                    v3::scale(c, 1.0 / pts.len().max(1) as f64),
                    v3::norm(m.prim_normal(f)),
                )
            })
            .unzip()
    };
    job.check()?;
    let sel: Vec<bool> = match s.str("method") {
        "all" => vec![true; n],
        "normal" => {
            let d = v3::norm(v3::of(s.vec3("direction")));
            let cos = s.f64("angle").clamp(0.0, 180.0).to_radians().cos();
            nrm.iter().map(|&q| v3::dot(q, d) >= cos - 1e-9).collect()
        }
        "range" => {
            let start = s.i64("range_start").max(0);
            let end = s.i64("range_end");
            let end = if end < 0 { n as i64 + end } else { end };
            let step = s.i64("range_step").max(1);
            (0..n as i64)
                .map(|i| i >= start && i <= end && (i - start) % step == 0)
                .collect()
        }
        _ => {
            let c = v3::of(s.vec3("box_centre"));
            let h = v3::scale(v3::of(s.vec3("box_size")), 0.5);
            pos.iter()
                .map(|p| (0..3).all(|k| (p[k] - c[k]).abs() <= h[k].abs()))
                .collect()
        }
    };
    let old = m
        .group_members(class, name)
        .unwrap_or_else(|| vec![false; n]);
    let members: Vec<bool> = match s.str("combine") {
        "union" => sel.iter().zip(&old).map(|(a, b)| *a || *b).collect(),
        "intersect" => sel.iter().zip(&old).map(|(a, b)| *a && *b).collect(),
        "subtract" => sel.iter().zip(&old).map(|(a, b)| !*a && *b).collect(),
        _ => sel,
    };
    let mut out = m.clone();
    out.set_group(class, name, &members)?;
    Ok(out)
}

// ------------------------------------------------------------------- fuse --

pub fn fuse_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("distance", "Distance", 0.001, Some(0.0), None)
            .help("Points closer than this become one (the lowest-numbered keeps its attributes)."),
    ]
}

/// Merges points closer than `dist` and removes primitives that become
/// degenerate. Deterministic: points are visited in order and each joins
/// the earliest kept point within range.
pub fn fused(m: &Mesh, dist: f64, job: &Job) -> Result<Mesh, String> {
    let n = m.points();
    let mut target = vec![0u32; n];
    if dist <= 0.0 {
        // Exact duplicates only.
        let mut seen: HashMap<[u32; 3], u32> = HashMap::new();
        for (i, p) in m.p.iter().enumerate() {
            job.every(i)?;
            let key = p.map(|x| if x == 0.0 { 0 } else { x.to_bits() });
            target[i] = *seen.entry(key).or_insert(i as u32);
        }
    } else {
        let cell = dist;
        let key = |p: [f32; 3]| p.map(|x| (f64::from(x) / cell).floor() as i64);
        let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
        for (i, &p) in m.p.iter().enumerate() {
            job.every(i)?;
            let k = key(p);
            let mut best: Option<u32> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let Some(list) = grid.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) else {
                            continue;
                        };
                        for &j in list {
                            let d = v3::len(v3::sub(v3::of(p), v3::of(m.p[j as usize])));
                            if d <= dist && best.is_none_or(|b| j < b) {
                                best = Some(j);
                            }
                        }
                    }
                }
            }
            match best {
                Some(j) => target[i] = j,
                None => {
                    target[i] = i as u32;
                    grid.entry(k).or_default().push(i as u32);
                }
            }
        }
    }
    let mut out = m.clone();
    let mut ids = Vec::new();
    let mut fresh = Mesh::new();
    let mut interp = Interp::new(&m.prim_attrs, m.prims());
    for f in 0..m.prims() {
        ids.clear();
        for &c in m.prim(f) {
            let t = target[c as usize];
            if ids.last() != Some(&t) {
                ids.push(t);
            }
        }
        if m.closed[f] && ids.len() > 1 && ids.first() == ids.last() {
            ids.pop();
        }
        let ok = if m.closed[f] {
            ids.len() >= 3
        } else {
            ids.len() >= 2
        };
        if ok {
            fresh.add_prim(&ids, m.closed[f]);
            interp.copy(f as u32);
        }
    }
    out.start = fresh.start;
    out.corners = fresh.corners;
    out.closed = fresh.closed;
    out.prim_attrs = interp.finish();
    let used: Vec<bool> = (0..n).map(|i| target[i] == i as u32).collect();
    out.keep_points(&used);
    Ok(out)
}

pub fn fuse(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    first(inputs).map_or_else(
        || Ok(Mesh::new()),
        |m| fused(m, s.f64("distance").max(0.0), job),
    )
}

/// The number of elements of a column-free kind (for docs and tests).
pub fn group_count(m: &Mesh, class: AttrClass, name: &str) -> usize {
    m.group_members(class, name)
        .map_or(0, |v| v.iter().filter(|&&b| b).count())
}
