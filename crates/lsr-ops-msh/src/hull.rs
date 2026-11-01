//! Convex Hull: the smallest convex mesh around the input's points
//! (quickhull, Barber, Dobkin & Huhdanpaa, "The Quickhull Algorithm for
//! Convex Hulls", ACM TOMS 1996), in `f64` with a distance tolerance
//! scaled to the bounds. Coplanar inputs give one convex polygon, collinear
//! inputs a line.

use std::collections::HashMap;

use lsr_core::geo::{AttrClass, names};
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Interp, Mesh, v3};

type V = [f64; 3];

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::string("group", "Group", "")
            .help("Only the points of this point group (empty: every point)."),
    ]
}

#[derive(Debug, Clone)]
struct Face {
    v: [usize; 3],
    n: V,
    d: f64,
    outside: Vec<usize>,
    alive: bool,
}

impl Face {
    fn new(p: &[V], v: [usize; 3]) -> Face {
        let n = v3::norm(v3::cross(
            v3::sub(p[v[1]], p[v[0]]),
            v3::sub(p[v[2]], p[v[0]]),
        ));
        Face {
            v,
            n,
            d: v3::dot(n, p[v[0]]),
            outside: Vec::new(),
            alive: true,
        }
    }

    fn dist(&self, q: V) -> f64 {
        v3::dot(self.n, q) - self.d
    }
}

/// The hull of `p` as outward triangles over indices into `p` (empty when
/// the points span no volume).
pub fn hull3(p: &[V], eps: f64, job: &Job) -> Result<Vec<[usize; 3]>, String> {
    let n = p.len();
    if n < 4 {
        return Ok(Vec::new());
    }
    // A starting tetrahedron from extreme points.
    let mut ext = [0usize; 6];
    for (i, q) in p.iter().enumerate() {
        for k in 0..3 {
            if q[k] < p[ext[2 * k]][k] {
                ext[2 * k] = i;
            }
            if q[k] > p[ext[2 * k + 1]][k] {
                ext[2 * k + 1] = i;
            }
        }
    }
    let (mut a, mut b, mut best) = (0, 0, -1.0);
    for &i in &ext {
        for &j in &ext {
            let d = v3::len(v3::sub(p[i], p[j]));
            if d > best {
                (a, b, best) = (i, j, d);
            }
        }
    }
    if best <= eps {
        return Ok(Vec::new());
    }
    let ab = v3::sub(p[b], p[a]);
    let (mut c, mut best) = (0, -1.0);
    for (i, q) in p.iter().enumerate() {
        let d = v3::len(v3::cross(ab, v3::sub(*q, p[a]))) / v3::len(ab);
        if d > best {
            (c, best) = (i, d);
        }
    }
    if best <= eps {
        return Ok(Vec::new());
    }
    let nrm = v3::norm(v3::cross(ab, v3::sub(p[c], p[a])));
    let (mut d, mut best) = (0, -1.0);
    for (i, q) in p.iter().enumerate() {
        let h = v3::dot(nrm, v3::sub(*q, p[a])).abs();
        if h > best {
            (d, best) = (i, h);
        }
    }
    if best <= eps {
        return Ok(Vec::new());
    }
    let mut faces: Vec<Face> = Vec::new();
    // Orient the base away from d.
    let base = if v3::dot(nrm, v3::sub(p[d], p[a])) > 0.0 {
        [a, c, b]
    } else {
        [a, b, c]
    };
    let [x, y, z] = base;
    for v in [[x, y, z], [x, d, y], [y, d, z], [z, d, x]] {
        faces.push(Face::new(p, v));
    }
    // Edge (directed, as it appears in a face) -> face.
    let mut edges: HashMap<(usize, usize), usize> = HashMap::new();
    for (f, face) in faces.iter().enumerate() {
        for k in 0..3 {
            edges.insert((face.v[k], face.v[(k + 1) % 3]), f);
        }
    }
    let used = [a, b, c, d];
    for i in 0..n {
        job.every(i)?;
        if used.contains(&i) {
            continue;
        }
        assign(&mut faces, 0..4, i, p, eps);
    }
    let mut stack: Vec<usize> = (0..4).collect();
    let mut visible: Vec<usize> = Vec::new();
    let mut horizon: Vec<(usize, usize)> = Vec::new();
    let mut steps = 0usize;
    while let Some(f) = stack.pop() {
        steps += 1;
        job.every(steps)?;
        if !faces[f].alive || faces[f].outside.is_empty() {
            continue;
        }
        // The farthest point of this face.
        let face = &faces[f];
        let &q = face
            .outside
            .iter()
            .max_by(|&&i, &&j| face.dist(p[i]).total_cmp(&face.dist(p[j])))
            .expect("non-empty");
        // Faces seen from q: a connected region around f.
        visible.clear();
        visible.push(f);
        faces[f].alive = false;
        let mut k = 0;
        while k < visible.len() {
            let g = visible[k];
            k += 1;
            let v = faces[g].v;
            for e in 0..3 {
                let (s, t) = (v[e], v[(e + 1) % 3]);
                if let Some(&h) = edges.get(&(t, s))
                    && faces[h].alive
                    && faces[h].dist(p[q]) > eps
                {
                    faces[h].alive = false;
                    visible.push(h);
                }
            }
        }
        // The horizon: edges of visible faces whose twin is not visible.
        horizon.clear();
        for &g in &visible {
            let v = faces[g].v;
            for e in 0..3 {
                let (s, t) = (v[e], v[(e + 1) % 3]);
                edges.remove(&(s, t));
                if let Some(&h) = edges.get(&(t, s))
                    && faces[h].alive
                {
                    horizon.push((s, t));
                }
            }
        }
        let first = faces.len();
        for &(s, t) in &horizon {
            let nf = faces.len();
            faces.push(Face::new(p, [s, t, q]));
            for (u, w) in [(s, t), (t, q), (q, s)] {
                edges.insert((u, w), nf);
            }
        }
        let orphans: Vec<usize> = visible
            .iter()
            .flat_map(|&g| std::mem::take(&mut faces[g].outside))
            .filter(|&i| i != q)
            .collect();
        let end = faces.len();
        for i in orphans {
            assign(&mut faces, first..end, i, p, eps);
        }
        stack.extend(first..faces.len());
    }
    Ok(faces.iter().filter(|f| f.alive).map(|f| f.v).collect())
}

/// Puts point `i` in the outside set of the first face of `range` it is
/// above (points inside the hull are dropped).
fn assign(faces: &mut [Face], range: std::ops::Range<usize>, i: usize, p: &[V], eps: f64) {
    for f in range {
        if faces[f].alive && faces[f].dist(p[i]) > eps {
            faces[f].outside.push(i);
            return;
        }
    }
}

/// The 2D convex hull of `q` (monotone chain), counter-clockwise, as
/// indices; points within `tol` (twice the triangle area) of a straight
/// run are left out.
pub fn hull2(q: &[[f64; 2]], tol: f64) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..q.len()).collect();
    idx.sort_by(|&a, &b| {
        q[a][0]
            .total_cmp(&q[b][0])
            .then(q[a][1].total_cmp(&q[b][1]))
    });
    idx.dedup_by(|a, b| q[*a] == q[*b]);
    if idx.len() < 3 {
        return idx;
    }
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut h: Vec<usize> = Vec::with_capacity(2 * idx.len());
    for pass in 0..2 {
        let start = h.len();
        let iter: Box<dyn Iterator<Item = &usize>> = if pass == 0 {
            Box::new(idx.iter())
        } else {
            Box::new(idx.iter().rev())
        };
        for &i in iter {
            while h.len() >= start + 2 && cross(q[h[h.len() - 2]], q[h[h.len() - 1]], q[i]) <= tol {
                h.pop();
            }
            h.push(i);
        }
        h.pop();
    }
    h
}

/// Hull of the input's points: triangles facing outward, the input's point
/// attributes kept on the hull points (normals dropped: hulls are faceted).
pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let group = s.str("group");
    let ids: Vec<u32> = if group.is_empty() {
        (0..m.points() as u32).collect()
    } else {
        let g = m
            .group_members(AttrClass::Point, group)
            .ok_or_else(|| format!("the input has no point group `{group}`"))?;
        (0..m.points() as u32).filter(|&i| g[i as usize]).collect()
    };
    let p: Vec<V> = ids.iter().map(|&i| v3::of(m.p[i as usize])).collect();
    let Some((lo, hi)) = m.bounds() else {
        return Ok(Mesh::new());
    };
    let diag = v3::len(v3::sub(v3::of(hi), v3::of(lo)));
    let eps = 1e-9 * diag.max(1e-9);
    let tris = hull3(&p, eps, job)?;
    // Hull points in input order, then the primitives.
    let mut out = Mesh::new();
    let mut map: HashMap<usize, u32> = HashMap::new();
    let mut order: Vec<usize> = tris.iter().flatten().copied().collect();
    let polys: Vec<Vec<usize>> = if tris.is_empty() {
        flat_hull(&p, eps)
    } else {
        tris.iter().map(|t| t.to_vec()).collect()
    };
    if tris.is_empty() {
        order = polys.iter().flatten().copied().collect();
    }
    order.sort_unstable();
    order.dedup();
    let mut interp = Interp::new(&m.point_attrs, order.len());
    for &i in &order {
        map.insert(i, out.add_point(m.p[ids[i] as usize]));
        interp.copy(ids[i]);
    }
    out.point_attrs = interp.finish();
    out.point_attrs
        .retain(|c| c.name != names::N && c.name != names::GROUP);
    for poly in &polys {
        let c: Vec<u32> = poly.iter().map(|i| map[i]).collect();
        out.add_prim(&c, c.len() >= 3);
    }
    Ok(out)
}

/// The hull of points spanning no volume: a convex polygon in their plane,
/// a segment, or nothing.
fn flat_hull(p: &[V], eps: f64) -> Vec<Vec<usize>> {
    if p.is_empty() {
        return Vec::new();
    }
    let (mut a, mut b, mut best) = (0, 0, 0.0);
    for i in 0..p.len() {
        let d = v3::len(v3::sub(p[i], p[0]));
        if d > best {
            (a, best) = (i, d);
        }
    }
    best = 0.0;
    for i in 0..p.len() {
        let d = v3::len(v3::sub(p[i], p[a]));
        if d > best {
            (b, best) = (i, d);
        }
    }
    if best <= eps {
        return Vec::new();
    }
    let u = v3::norm(v3::sub(p[b], p[a]));
    let (mut c, mut far) = (a, 0.0);
    for i in 0..p.len() {
        let d = v3::len(v3::cross(u, v3::sub(p[i], p[a])));
        if d > far {
            (c, far) = (i, d);
        }
    }
    if far <= eps {
        return vec![vec![a, b]];
    }
    let n = v3::norm(v3::cross(u, v3::sub(p[c], p[a])));
    let w = v3::cross(n, u);
    let q: Vec<[f64; 2]> = p
        .iter()
        .map(|x| {
            let d = v3::sub(*x, p[a]);
            [v3::dot(d, u), v3::dot(d, w)]
        })
        .collect();
    vec![hull2(&q, eps * best)]
}
