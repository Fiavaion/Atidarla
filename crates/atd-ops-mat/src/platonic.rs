//! `msh.platonic`: the five Platonic solids and flat regular polygons, as
//! faceted polygon meshes (points split per face, so each face has its own
//! flat `N` and `uv`).
//!
//! It is the renderer's test and example source until the MSH family's
//! primitives land (docs/milestones/m2/mat-render.md). The geometry is at
//! most 256 points, built on the CPU on parameter edits only; it is uploaded
//! once, when something draws or deforms it.

use std::sync::Arc;

use atd_core::geo::AttrClass;
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::{Attribute, GeoBuffer, Geometry, PrimKind, Topology};

const SHAPES: &[&str] = &[
    "tetrahedron",
    "cube",
    "octahedron",
    "dodecahedron",
    "icosahedron",
    "polygon",
];

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "msh.platonic".into(),
        family: OpFamily::Msh,
        label: "Platonic Solid".into(),
        summary: "A tetrahedron, cube, octahedron, dodecahedron or icosahedron centred on Centre, or a flat regular polygon facing +Z, as polygons wound counter-clockwise seen from outside. Each face has its own points, so it writes flat normals `N` and per-face texture coordinates `uv` (the polygon's span the unit square); optionally a colour per face (prim `Cd`).".into(),
        version: 1,
        params: vec![
            ParamSchema::menu("shape", "Shape", SHAPES, "icosahedron")
                .help("Which solid, or polygon: one flat face with Sides corners."),
            ParamSchema::float("radius", "Radius", 1.0, Some(0.0), None)
                .help("Distance from the centre to every corner."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0])
                .help("Where the shape is centred."),
            ParamSchema::int("sides", "Sides", 4, Some(3), Some(256))
                .help("Polygon: number of corners (4 is a square, edges along the axes)."),
            ParamSchema::toggle("face_colours", "Face Colours", false)
                .help("Give every face its own colour (prim `Cd`), evenly spread around the hue circle."),
        ],
        inputs: vec![],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Geo,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 100,
        time_dependent: false,
        sink: false,
    }
}

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}

/// Vertices on the unit sphere and faces as unordered vertex sets.
fn solid(shape: &str) -> (Vec<V3>, Vec<Vec<usize>>) {
    let phi = (1.0 + 5f64.sqrt()) / 2.0;
    // Faces of a triangle-faced solid: vertex triples at the edge length.
    let triples = |v: &[V3]| {
        let d = |a: usize, b: usize| dot(sub(v[a], v[b]), sub(v[a], v[b]));
        let edge = (1..v.len()).map(|b| d(0, b)).fold(f64::MAX, f64::min);
        let near = |a, b| (d(a, b) - edge).abs() < 1e-9;
        let mut faces = Vec::new();
        for a in 0..v.len() {
            for b in a + 1..v.len() {
                for c in b + 1..v.len() {
                    if near(a, b) && near(b, c) && near(a, c) {
                        faces.push(vec![a, b, c]);
                    }
                }
            }
        }
        faces
    };
    let icosahedron = || {
        let mut v = Vec::new();
        for s1 in [-1.0, 1.0] {
            for s2 in [-1.0, 1.0] {
                v.push([0.0, s1, s2 * phi]);
                v.push([s1, s2 * phi, 0.0]);
                v.push([s2 * phi, 0.0, s1]);
            }
        }
        v
    };
    let (verts, faces) = match shape {
        "tetrahedron" => {
            let v = vec![
                [1.0, 1.0, 1.0],
                [1.0, -1.0, -1.0],
                [-1.0, 1.0, -1.0],
                [-1.0, -1.0, 1.0],
            ];
            let f = triples(&v);
            (v, f)
        }
        "cube" => {
            let mut v = Vec::new();
            for x in [-1.0, 1.0] {
                for y in [-1.0, 1.0] {
                    for z in [-1.0, 1.0] {
                        v.push([x, y, z]);
                    }
                }
            }
            let mut f = Vec::new();
            for axis in [0usize, 1, 2] {
                for s in [-1.0, 1.0] {
                    f.push((0..8).filter(|&i| v[i][axis] == s).collect());
                }
            }
            (v, f)
        }
        "octahedron" => {
            let mut v = Vec::new();
            for axis in 0..3 {
                for s in [-1.0, 1.0] {
                    let mut p = [0.0; 3];
                    p[axis] = s;
                    v.push(p);
                }
            }
            let f = triples(&v);
            (v, f)
        }
        "dodecahedron" => {
            // The dual of the icosahedron: a vertex per icosahedron face, a
            // face per icosahedron vertex.
            let iv = icosahedron();
            let ifaces = triples(&iv);
            let v: Vec<V3> = ifaces
                .iter()
                .map(|f| {
                    let c = f.iter().fold([0.0; 3], |s, &i| {
                        [s[0] + iv[i][0], s[1] + iv[i][1], s[2] + iv[i][2]]
                    });
                    [c[0] / 3.0, c[1] / 3.0, c[2] / 3.0]
                })
                .collect();
            let f = (0..iv.len())
                .map(|i| {
                    (0..ifaces.len())
                        .filter(|&j| ifaces[j].contains(&i))
                        .collect()
                })
                .collect();
            (v, f)
        }
        _ => {
            let v = icosahedron();
            let f = triples(&v);
            (v, f)
        }
    };
    (verts.into_iter().map(norm).collect(), faces)
}

/// The face's corners ordered counter-clockwise around its outward normal
/// (the centroid's direction: every solid is convex and centred).
fn order(verts: &[V3], face: &[usize]) -> Vec<usize> {
    let c = face.iter().fold([0.0; 3], |s, &i| {
        [s[0] + verts[i][0], s[1] + verts[i][1], s[2] + verts[i][2]]
    });
    let n = norm(c);
    let c = c.map(|x| x / face.len() as f64);
    let u = norm(sub(verts[face[0]], c));
    let w = cross(n, u);
    let mut out = face.to_vec();
    out.sort_by(|&a, &b| {
        let angle = |i: usize| {
            let d = sub(verts[i], c);
            dot(d, w).atan2(dot(d, u))
        };
        angle(a).total_cmp(&angle(b))
    });
    out
}

/// A colour per face, spread around the hue circle by the golden angle.
fn face_colour(i: usize) -> [f32; 4] {
    let h = (i as f64 * 0.618_033_988_75).fract() * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let soft = |c: f64| (0.35 + 0.65 * c) as f32;
    [soft(r), soft(g), soft(b), 1.0]
}

/// Builds the geometry (CPU attributes) for the given parameters.
pub fn build(
    shape: &str,
    radius: f32,
    sides: u32,
    face_colours: bool,
    centre: [f32; 3],
) -> Geometry {
    let r = f64::from(radius.max(0.0));
    let c = centre.map(f64::from);
    let mut faces: Vec<Vec<V3>> = Vec::new();
    let mut normals: Vec<V3> = Vec::new();
    if shape == "polygon" {
        let n = sides.clamp(3, 256);
        let start = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI / f64::from(n);
        faces.push(
            (0..n)
                .map(|i| {
                    let a = start + std::f64::consts::TAU * f64::from(i) / f64::from(n);
                    [a.cos(), a.sin(), 0.0]
                })
                .collect(),
        );
        normals.push([0.0, 0.0, 1.0]);
    } else {
        let (verts, sets) = solid(shape);
        for set in &sets {
            let ordered = order(&verts, set);
            let pts: Vec<V3> = ordered.iter().map(|&i| verts[i]).collect();
            normals.push(norm(cross(sub(pts[1], pts[0]), sub(pts[2], pts[0]))));
            faces.push(pts);
        }
    }
    let mut p = Vec::new();
    let mut nrm = Vec::new();
    let mut uv = Vec::new();
    let mut corners: Vec<Vec<u32>> = Vec::new();
    for (face, n) in faces.iter().zip(&normals) {
        // Texture coordinates: the face's own plane, its bounds to [0, 1].
        let (u_axis, w_axis) = if n[2].abs() > 0.999 {
            ([1.0, 0.0, 0.0], [0.0, n[2].signum(), 0.0])
        } else {
            let u = norm(cross([0.0, 0.0, 1.0], *n));
            (u, cross(*n, u))
        };
        let local: Vec<(f64, f64)> = face
            .iter()
            .map(|q| (dot(*q, u_axis), dot(*q, w_axis)))
            .collect();
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        for &(a, b) in &local {
            lo = (lo.0.min(a), lo.1.min(b));
            hi = (hi.0.max(a), hi.1.max(b));
        }
        let mut ids = Vec::new();
        for (q, (a, b)) in face.iter().zip(&local) {
            ids.push(p.len() as u32);
            p.push([
                (c[0] + q[0] * r) as f32,
                (c[1] + q[1] * r) as f32,
                (c[2] + q[2] * r) as f32,
            ]);
            nrm.push([n[0] as f32, n[1] as f32, n[2] as f32]);
            uv.push([
                ((a - lo.0) / (hi.0 - lo.0).max(1e-12)) as f32,
                ((b - lo.1) / (hi.1 - lo.1).max(1e-12)) as f32,
            ]);
        }
        corners.push(ids);
    }
    let mut g = Geometry::new();
    g.set_points(p.len() as u32, None);
    let prims: Vec<(PrimKind, &[u32])> = corners
        .iter()
        .map(|c| (PrimKind::Polygon, c.as_slice()))
        .collect();
    g.set_topology(Some(Topology::from_prims(&prims)));
    g.set_point(Attribute::from_vec3("P", &p));
    g.set_point(Attribute::from_vec3("N", &nrm));
    let uv_bytes = uv.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    g.set_point(Attribute::cpu(
        "uv",
        atd_core::geo::AttrType::VEC2,
        uv_bytes,
    ));
    if face_colours {
        let cd: Vec<[f32; 4]> = (0..corners.len()).map(face_colour).collect();
        g.set(AttrClass::Prim, Attribute::from_vec4("Cd", &cd))
            .expect("one colour per face");
    }
    g
}

#[derive(Debug, Default)]
pub struct Platonic {
    key: Option<(String, u32, u32, bool, [u32; 3])>,
    built: Geometry,
    header: Option<(u32, Arc<GeoBuffer>)>,
}

pub fn create(_device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Platonic::default())
}

impl GeoOp for Platonic {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let shape = p.str("shape");
        let radius = p.f32("radius");
        let sides = p.i64("sides").clamp(3, 256) as u32;
        let colours = p.bool("face_colours");
        let c = p.vec4("centre");
        let centre = [c[0], c[1], c[2]];
        let key = (
            shape.to_owned(),
            radius.to_bits(),
            sides,
            colours,
            centre.map(f32::to_bits),
        );
        if self.key.as_ref() != Some(&key) {
            self.built = build(shape, radius, sides, colours, centre);
            self.key = Some(key);
        }
        let points = self.built.points();
        if self.header.as_ref().is_none_or(|(n, _)| *n != points) {
            self.header = Some((points, ctx.cpu_header(points)));
        }
        // A clone shares every attribute: nothing is copied.
        let mut out = self.built.clone();
        out.set_points(points, self.header.as_ref().map(|(_, h)| h.clone()));
        *ctx.output = out;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(a: &Attribute) -> Vec<u32> {
        a.cpu_bytes()
            .unwrap()
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn solids_have_the_right_faces_wound_outwards() {
        for (shape, faces, corners) in [
            ("tetrahedron", 4, 3),
            ("cube", 6, 4),
            ("octahedron", 8, 3),
            ("dodecahedron", 12, 5),
            ("icosahedron", 20, 3),
            ("polygon", 1, 4),
        ] {
            let g = build(shape, 2.0, 4, true, [0.0; 3]);
            assert_eq!(g.prims(), faces, "{shape}");
            assert_eq!(
                g.points(),
                faces * corners,
                "{shape}: points split per face"
            );
            let t = g.topology().unwrap();
            let start = words(&t.start);
            assert!(start.windows(2).all(|w| w[1] - w[0] == corners), "{shape}");
            let p: Vec<[f32; 4]> = g
                .point("P")
                .unwrap()
                .cpu_bytes()
                .unwrap()
                .chunks_exact(16)
                .map(|c| {
                    std::array::from_fn(|i| {
                        f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap())
                    })
                })
                .collect();
            for q in &p {
                let r = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
                assert!((r - 2.0).abs() < 1e-5, "{shape}: radius {r}");
            }
            // Counter-clockwise from outside: the first three corners' normal
            // points away from the centre (polygon: +Z).
            let c = words(&t.corners);
            for f in 0..faces as usize {
                let k = &c[f * corners as usize..];
                let (a, b, d) = (p[k[0] as usize], p[k[1] as usize], p[k[2] as usize]);
                let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let e2 = [d[0] - a[0], d[1] - a[1], d[2] - a[2]];
                let n = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let out = if shape == "polygon" {
                    n[2]
                } else {
                    n[0] * a[0] + n[1] * a[1] + n[2] * a[2]
                };
                assert!(out > 0.0, "{shape} face {f} winds inwards");
            }
            assert!(g.attr(AttrClass::Prim, "Cd").is_some());
        }
    }

    #[test]
    fn the_square_spans_the_unit_square_in_uv() {
        let g = build(
            "polygon",
            std::f32::consts::SQRT_2,
            4,
            false,
            [5.0, 0.0, 0.0],
        );
        let uv: Vec<f32> = g
            .point("uv")
            .unwrap()
            .cpu_bytes()
            .unwrap()
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let corners: Vec<(f32, f32)> = uv.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        for want in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            assert!(
                corners
                    .iter()
                    .any(|c| (c.0 - want.0).abs() < 1e-6 && (c.1 - want.1).abs() < 1e-6),
                "{corners:?}"
            );
        }
        assert!(g.attr(AttrClass::Prim, "Cd").is_none());
    }
}
