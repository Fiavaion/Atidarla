//! Primitive generators: Grid, Box, Sphere, Tube (cylinder and cone),
//! Torus, Circle and Line. Closed shapes are welded closed manifolds with
//! outward polygons and smooth point normals `N` (design M1: no vertex
//! attributes, so hard edges come from Normals in `flat` mode).

use std::f64::consts::TAU;

use atd_core::geo::names;
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Mesh, limits};
use crate::normals::point_normals;

pub const PLANES: &[&str] = &["xy", "xz", "yz"];

/// Maps plane coordinates to 3D for `plane`; returns the point and the
/// plane's normal (polygons counter-clockwise in (u, v) face it).
pub fn on_plane(plane: &str, u: f64, v: f64) -> ([f64; 3], [f32; 3]) {
    match plane {
        "xz" => ([u, 0.0, -v], [0.0, 1.0, 0.0]),
        "yz" => ([0.0, u, v], [1.0, 0.0, 0.0]),
        _ => ([u, v, 0.0], [0.0, 0.0, 1.0]),
    }
}

pub fn centre_param() -> ParamSchema {
    ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("Centre of the shape.")
}

pub fn plane_param(default: &str) -> ParamSchema {
    ParamSchema::menu("plane", "Plane", PLANES, default)
        .help("The plane the shape lies in (its normal is +Z for xy, +Y for xz, +X for yz).")
}

fn at(c: [f32; 3], p: [f64; 3]) -> [f32; 3] {
    [
        (f64::from(c[0]) + p[0]) as f32,
        (f64::from(c[1]) + p[1]) as f32,
        (f64::from(c[2]) + p[2]) as f32,
    ]
}

fn count(s: &Snap, name: &str, min: i64, max: i64) -> usize {
    s.i64(name).clamp(min, max) as usize
}

fn with_normals(mut m: Mesh) -> Mesh {
    let n = point_normals(&m);
    m.set_point_attr(Column::vec3(names::N, &n));
    m
}

pub fn grid_params() -> Vec<ParamSchema> {
    vec![
        plane_param("xy"),
        ParamSchema::vec("size", "Size", &[2.0, 2.0]).help("Width and height."),
        ParamSchema::int("rows", "Rows", 10, Some(2), Some(4096))
            .help("Points along the second axis (v)."),
        ParamSchema::int("columns", "Columns", 10, Some(2), Some(4096))
            .help("Points along the first axis (u)."),
        centre_param(),
    ]
}

/// A rectangle of quads with `N` and `uv` (0 to 1 across it).
pub fn grid(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let (rows, cols) = (count(s, "rows", 2, 4096), count(s, "columns", 2, 4096));
    limits::check(
        (rows * cols) as u64,
        ((rows - 1) * (cols - 1)) as u64,
        ((rows - 1) * (cols - 1) * 4) as u64,
    )?;
    let size = s.vec2("size");
    let c = s.vec3("centre");
    let plane = s.str("plane");
    let mut m = Mesh::new();
    let (mut n, mut uv) = (Vec::new(), Vec::new());
    for r in 0..rows {
        job.every(r)?;
        for k in 0..cols {
            let fu = k as f64 / (cols - 1) as f64;
            let fv = r as f64 / (rows - 1) as f64;
            let (p, nn) = on_plane(
                plane,
                (fu - 0.5) * f64::from(size[0]),
                (fv - 0.5) * f64::from(size[1]),
            );
            m.add_point(at(c, p));
            n.push(nn);
            uv.extend([fu as f32, fv as f32]);
        }
    }
    for r in 0..rows - 1 {
        for k in 0..cols - 1 {
            let i = (r * cols + k) as u32;
            let w = cols as u32;
            m.add_polygon(&[i, i + 1, i + 1 + w, i + w]);
        }
    }
    m.set_point_attr(Column::vec3(names::N, &n));
    m.set_point_attr(Column::f32(names::UV, 2, uv));
    Ok(m)
}

pub fn box_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::vec("size", "Size", &[1.0, 1.0, 1.0]).help("Extent along X, Y and Z."),
        ParamSchema::int("divisions", "Divisions", 1, Some(1), Some(128))
            .help("Quads along each edge."),
        centre_param(),
    ]
}

/// A closed box of quads, welded at its edges.
pub fn cube(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let d = count(s, "divisions", 1, 128);
    let size = s.vec3("size");
    let c = s.vec3("centre");
    let side = d + 1;
    let mut index = vec![u32::MAX; side * side * side];
    let mut m = Mesh::new();
    let mut point = |m: &mut Mesh, l: [usize; 3]| -> u32 {
        let slot = &mut index[(l[2] * side + l[1]) * side + l[0]];
        if *slot == u32::MAX {
            let p = [0, 1, 2].map(|k| (l[k] as f64 / d as f64 - 0.5) * f64::from(size[k]));
            *slot = m.add_point(at(c, p));
        }
        *slot
    };
    // Each face: the fixed axis and side, and two in-plane axes whose cross
    // product points outward.
    let faces: [(usize, usize, usize, usize); 6] = [
        (0, d, 1, 2),
        (0, 0, 2, 1),
        (1, d, 2, 0),
        (1, 0, 0, 2),
        (2, d, 0, 1),
        (2, 0, 1, 0),
    ];
    for (f, &(axis, fixed, a, b)) in faces.iter().enumerate() {
        job.check()?;
        let _ = f;
        for i in 0..d {
            for j in 0..d {
                let corner = |di: usize, dj: usize| {
                    let mut l = [0; 3];
                    l[axis] = fixed;
                    l[a] = i + di;
                    l[b] = j + dj;
                    l
                };
                let q = [
                    point(&mut m, corner(0, 0)),
                    point(&mut m, corner(1, 0)),
                    point(&mut m, corner(1, 1)),
                    point(&mut m, corner(0, 1)),
                ];
                m.add_polygon(&q);
            }
        }
    }
    Ok(with_normals(m))
}

pub fn sphere_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("type", "Type", &["polar", "ico"], "polar").help(
            "polar: rings of quads between two poles (triangle fans); ico: a subdivided icosahedron of even triangles.",
        ),
        ParamSchema::float("radius", "Radius", 0.5, Some(0.0), None).help("Radius."),
        ParamSchema::int("rows", "Rows", 16, Some(2), Some(2048))
            .help("polar: bands from pole to pole."),
        ParamSchema::int("columns", "Columns", 32, Some(3), Some(4096))
            .help("polar: segments around."),
        ParamSchema::int("level", "Level", 3, Some(0), Some(7))
            .help("ico: subdivisions (20 × 4^level triangles)."),
        centre_param(),
    ]
}

pub fn sphere(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let r = s.f64("radius").max(0.0);
    let c = s.vec3("centre");
    let mut m = if s.str("type") == "ico" {
        icosphere(count(s, "level", 0, 7), job)?
    } else {
        let (rows, cols) = (count(s, "rows", 2, 2048), count(s, "columns", 3, 4096));
        limits::check(
            ((rows - 1) * cols + 2) as u64,
            (rows * cols) as u64,
            (rows * cols * 4) as u64,
        )?;
        let mut m = Mesh::new();
        m.add_point([0.0, 1.0, 0.0]);
        for j in 1..rows {
            job.every(j)?;
            let phi = std::f64::consts::PI * j as f64 / rows as f64;
            for i in 0..cols {
                let th = TAU * i as f64 / cols as f64;
                m.add_point(
                    [phi.sin() * th.cos(), phi.cos(), -phi.sin() * th.sin()].map(|x| x as f32),
                );
            }
        }
        let south = m.add_point([0.0, -1.0, 0.0]);
        let ring = |j: usize, i: usize| (1 + (j - 1) * cols + i % cols) as u32;
        for i in 0..cols {
            m.add_polygon(&[0, ring(1, i), ring(1, i + 1)]);
        }
        for j in 1..rows - 1 {
            for i in 0..cols {
                m.add_polygon(&[
                    ring(j, i),
                    ring(j + 1, i),
                    ring(j + 1, i + 1),
                    ring(j, i + 1),
                ]);
            }
        }
        for i in 0..cols {
            m.add_polygon(&[south, ring(rows - 1, i + 1), ring(rows - 1, i)]);
        }
        m
    };
    // Unit sphere → radius and centre; the normal is the direction.
    let n: Vec<[f32; 3]> = m.p.clone();
    for p in &mut m.p {
        *p = at(c, [0, 1, 2].map(|k| f64::from(p[k]) * r));
    }
    m.set_point_attr(Column::vec3(names::N, &n));
    Ok(m)
}

/// A unit icosphere: an icosahedron with each triangle split in four
/// `level` times, new points pushed onto the sphere.
fn icosphere(level: usize, job: &Job) -> Result<Mesh, String> {
    let tris = 20u64 << (2 * level);
    limits::check(tris / 2 + 2, tris, tris * 3)?;
    let t = (1.0 + 5f64.sqrt()) / 2.0;
    let mut p: Vec<[f64; 3]> = vec![
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ];
    let mut f: Vec<[u32; 3]> = vec![
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let unit = |v: [f64; 3]| crate::mesh::v3::norm(v);
    for q in &mut p {
        *q = unit(*q);
    }
    for _ in 0..level {
        job.check()?;
        let mut mid = std::collections::HashMap::new();
        let mut next = Vec::with_capacity(f.len() * 4);
        for tri in &f {
            let mut m3 = [0u32; 3];
            for e in 0..3 {
                let (a, b) = (tri[e], tri[(e + 1) % 3]);
                let key = (a.min(b), a.max(b));
                m3[e] = *mid.entry(key).or_insert_with(|| {
                    let (pa, pb) = (p[a as usize], p[b as usize]);
                    p.push(unit([0, 1, 2].map(|k| pa[k] + pb[k])));
                    (p.len() - 1) as u32
                });
            }
            next.push([tri[0], m3[0], m3[2]]);
            next.push([tri[1], m3[1], m3[0]]);
            next.push([tri[2], m3[2], m3[1]]);
            next.push(m3);
        }
        f = next;
    }
    let mut m = Mesh::new();
    m.p = p.iter().map(|q| q.map(|x| x as f32)).collect();
    for tri in &f {
        m.add_polygon(tri);
    }
    Ok(m)
}

pub fn tube_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("radius_bottom", "Bottom Radius", 0.5, Some(0.0), None)
            .help("Radius at the bottom; 0 makes a cone point."),
        ParamSchema::float("radius_top", "Top Radius", 0.5, Some(0.0), None)
            .help("Radius at the top; 0 makes a cone point."),
        ParamSchema::float("height", "Height", 1.0, None, None).help("Height along Y."),
        ParamSchema::int("rows", "Rows", 2, Some(2), Some(4096))
            .help("Rings from bottom to top (2: one band)."),
        ParamSchema::int("columns", "Columns", 24, Some(3), Some(4096)).help("Segments around."),
        ParamSchema::toggle("caps", "Caps", true)
            .help("Closes each end with one polygon (an end of radius 0 needs none)."),
        centre_param(),
    ]
}

/// A cylinder or cone along Y.
pub fn tube(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let (rows, cols) = (count(s, "rows", 2, 4096), count(s, "columns", 3, 4096));
    limits::check(
        (rows * cols) as u64,
        (rows * cols + 2) as u64,
        (rows * cols * 4 + 2 * cols) as u64,
    )?;
    let (rb, rt) = (
        s.f64("radius_bottom").max(0.0),
        s.f64("radius_top").max(0.0),
    );
    let h = s.f64("height");
    let c = s.vec3("centre");
    let mut m = Mesh::new();
    // Ring j: its point indices (one point when the radius is zero).
    let mut rings: Vec<Vec<u32>> = Vec::with_capacity(rows);
    for j in 0..rows {
        job.every(j)?;
        let f = j as f64 / (rows - 1) as f64;
        let r = rb + (rt - rb) * f;
        let y = -h / 2.0 + h * f;
        let ring = if r == 0.0 {
            vec![m.add_point(at(c, [0.0, y, 0.0])); cols]
        } else {
            (0..cols)
                .map(|i| {
                    let th = TAU * i as f64 / cols as f64;
                    m.add_point(at(c, [r * th.cos(), y, -r * th.sin()]))
                })
                .collect()
        };
        rings.push(ring);
    }
    let mut quad = Vec::with_capacity(4);
    for j in 0..rows - 1 {
        for i in 0..cols {
            let k = (i + 1) % cols;
            quad.clear();
            for q in [rings[j][i], rings[j][k], rings[j + 1][k], rings[j + 1][i]] {
                if quad.last() != Some(&q) && quad.first() != Some(&q) {
                    quad.push(q);
                }
            }
            if quad.len() >= 3 {
                m.add_polygon(&quad);
            }
        }
    }
    if s.bool("caps") {
        if rb > 0.0 {
            let ring: Vec<u32> = rings[0].iter().rev().copied().collect();
            m.add_polygon(&ring);
        }
        if rt > 0.0 {
            m.add_polygon(&rings[rows - 1]);
        }
    }
    if h < 0.0 {
        // Upside down: flip every polygon so it still faces outward.
        flip(&mut m);
    }
    Ok(with_normals(m))
}

/// Reverses the winding of every polygon.
pub fn flip(m: &mut Mesh) {
    for i in 0..m.prims() {
        let (a, b) = (m.start[i] as usize, m.start[i + 1] as usize);
        if m.closed[i] {
            m.corners[a..b].reverse();
        }
    }
}

pub fn torus_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("radius", "Radius", 0.5, Some(0.0), None)
            .help("Distance from the centre to the middle of the tube."),
        ParamSchema::float("thickness", "Tube Radius", 0.2, Some(0.0), None)
            .help("Radius of the tube."),
        ParamSchema::int("rows", "Rows", 12, Some(3), Some(4096)).help("Segments around the tube."),
        ParamSchema::int("columns", "Columns", 32, Some(3), Some(4096))
            .help("Segments around the ring."),
        centre_param(),
    ]
}

/// A torus around Y.
pub fn torus(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let (rows, cols) = (count(s, "rows", 3, 4096), count(s, "columns", 3, 4096));
    limits::check(
        (rows * cols) as u64,
        (rows * cols) as u64,
        (rows * cols * 4) as u64,
    )?;
    let (big, small) = (s.f64("radius").max(0.0), s.f64("thickness").max(0.0));
    let c = s.vec3("centre");
    let mut m = Mesh::new();
    let mut n = Vec::with_capacity(rows * cols);
    for i in 0..cols {
        job.every(i)?;
        let th = TAU * i as f64 / cols as f64;
        for j in 0..rows {
            let ph = TAU * j as f64 / rows as f64;
            let dir = [ph.cos() * th.cos(), ph.sin(), -ph.cos() * th.sin()];
            let ring = [big * th.cos(), 0.0, -big * th.sin()];
            m.add_point(at(c, [0, 1, 2].map(|k| ring[k] + small * dir[k])));
            n.push(dir.map(|x| x as f32));
        }
    }
    let id = |i: usize, j: usize| ((i % cols) * rows + j % rows) as u32;
    for i in 0..cols {
        for j in 0..rows {
            m.add_polygon(&[id(i, j), id(i + 1, j), id(i + 1, j + 1), id(i, j + 1)]);
        }
    }
    m.set_point_attr(Column::vec3(names::N, &n));
    Ok(m)
}

pub fn circle_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("type", "Type", &["polygon", "polyline", "points"], "polygon").help(
            "polygon: one filled polygon; polyline: a closed outline (its last point is its first); points: points only.",
        ),
        plane_param("xy"),
        ParamSchema::float("radius", "Radius", 0.5, Some(0.0), None).help("Radius."),
        ParamSchema::int("divisions", "Divisions", 32, Some(3), Some(1_000_000))
            .help("Points around."),
        ParamSchema::float("arc", "Arc", 360.0, Some(0.0), Some(360.0)).help(
            "Degrees covered; below 360 the outline is open (polygon: a pie slice through the centre).",
        ),
        centre_param(),
    ]
}

pub fn circle(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let n = count(s, "divisions", 3, 1_000_000);
    let r = s.f64("radius").max(0.0);
    let arc = s.f64("arc").clamp(0.0, 360.0);
    let full = arc >= 360.0;
    let c = s.vec3("centre");
    let plane = s.str("plane");
    let mut m = Mesh::new();
    let mut normals = Vec::with_capacity(n + 1);
    let steps = if full { n } else { n - 1 };
    for i in 0..n {
        job.every(i)?;
        let a = arc.to_radians() * i as f64 / steps as f64;
        let (p, nn) = on_plane(plane, r * a.cos(), r * a.sin());
        m.add_point(at(c, p));
        normals.push(nn);
    }
    let mut ids: Vec<u32> = (0..n as u32).collect();
    match s.str("type") {
        "points" => {}
        "polyline" => {
            if full {
                ids.push(0);
            }
            m.add_prim(&ids, false);
        }
        _ => {
            if !full {
                let (p, nn) = on_plane(plane, 0.0, 0.0);
                ids.push(m.add_point(at(c, p)));
                normals.push(nn);
            }
            m.add_polygon(&ids);
        }
    }
    m.set_point_attr(Column::vec3(names::N, &normals));
    Ok(m)
}

pub fn line_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::vec("start", "Start", &[0.0, 0.0, 0.0]).help("First point."),
        ParamSchema::vec("end", "End", &[0.0, 1.0, 0.0]).help("Last point."),
        ParamSchema::int("points", "Points", 2, Some(2), Some(1_000_000))
            .help("Points along the line, evenly spaced."),
    ]
}

/// One polyline from start to end.
pub fn line(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let n = count(s, "points", 2, 1_000_000);
    let (a, b) = (s.vec3("start"), s.vec3("end"));
    let mut m = Mesh::new();
    for i in 0..n {
        job.every(i)?;
        let t = i as f32 / (n - 1) as f32;
        m.add_point([0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t));
    }
    let ids: Vec<u32> = (0..n as u32).collect();
    m.add_prim(&ids, false);
    Ok(m)
}
