//! Remesh: isotropic triangle remeshing towards a target edge length
//! (Botsch and Kobbelt, "A Remeshing Approach to Multiresolution
//! Modeling", SGP 2004): each iteration splits long edges, collapses short
//! ones, flips edges towards valence six, relaxes points in their tangent
//! plane and projects them back onto the input surface. Borders and sharp
//! edges (feature angle) are kept as creases.

use std::collections::{HashMap, HashSet};

use lsr_core::geo::names;
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::triangulated;
use crate::mesh::{Column, Interp, Mesh, limits, v3};
use crate::normals::point_normals;
use crate::topo::key;

type V = [f64; 3];

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("edge_length", "Edge Length", 0.1, Some(1e-6), None)
            .help("The edge length the triangles aim for."),
        ParamSchema::int("iterations", "Iterations", 5, Some(1), Some(50))
            .help("Rounds of splitting, collapsing, flipping and relaxing."),
        ParamSchema::float("feature_angle", "Feature Angle", 45.0, Some(0.0), Some(180.0)).help(
            "Edges where the surface bends by more than this many degrees are kept as creases (180: none); open borders are always kept.",
        ),
        ParamSchema::toggle("project", "Project", true)
            .help("Moves the points back onto the input surface after each relaxation."),
    ]
}

/// The working triangle mesh.
struct Tris {
    pos: Vec<V>,
    tris: Vec<[u32; 3]>,
    alive: Vec<bool>,
    around: Vec<Vec<u32>>,
    locked: Vec<bool>,
    dead: Vec<bool>,
    /// Crease and border edges.
    feature: HashSet<(u32, u32)>,
}

impl Tris {
    fn on_edge(&self, a: u32, b: u32, out: &mut Vec<u32>) {
        out.clear();
        for &t in &self.around[a as usize] {
            if self.alive[t as usize] && self.tris[t as usize].contains(&b) {
                out.push(t);
            }
        }
    }

    fn neighbours(&self, v: u32, out: &mut Vec<u32>) {
        out.clear();
        for &t in &self.around[v as usize] {
            if self.alive[t as usize] {
                for &w in &self.tris[t as usize] {
                    if w != v && !out.contains(&w) {
                        out.push(w);
                    }
                }
            }
        }
    }

    fn cross(&self, t: [u32; 3]) -> V {
        let [a, b, c] = t.map(|i| self.pos[i as usize]);
        v3::cross(v3::sub(b, a), v3::sub(c, a))
    }

    fn len(&self, a: u32, b: u32) -> f64 {
        v3::len(v3::sub(self.pos[a as usize], self.pos[b as usize]))
    }

    /// Unique live edges, sorted.
    fn edges(&self) -> Vec<(u32, u32)> {
        let mut e: Vec<(u32, u32)> = Vec::with_capacity(self.tris.len() * 3 / 2);
        for (t, tri) in self.tris.iter().enumerate() {
            if self.alive[t] {
                for i in 0..3 {
                    e.push(key(tri[i], tri[(i + 1) % 3]));
                }
            }
        }
        e.sort_unstable();
        e.dedup();
        e
    }

    fn add_tri(&mut self, t: [u32; 3]) -> u32 {
        let id = self.tris.len() as u32;
        self.tris.push(t);
        self.alive.push(true);
        for v in t {
            self.around[v as usize].push(id);
        }
        id
    }

    fn split(&mut self, a: u32, b: u32, on: &[u32]) -> u32 {
        let mid = v3::scale(v3::add(self.pos[a as usize], self.pos[b as usize]), 0.5);
        let m = self.pos.len() as u32;
        self.pos.push(mid);
        self.around.push(Vec::new());
        self.dead.push(false);
        let crease = self.feature.remove(&key(a, b));
        self.locked.push(crease);
        if crease {
            self.feature.insert(key(a, m));
            self.feature.insert(key(m, b));
        }
        for &t in on {
            let tri = self.tris[t as usize];
            let i = (0..3)
                .find(|&i| {
                    let (x, y) = (tri[i], tri[(i + 1) % 3]);
                    (x == a && y == b) || (x == b && y == a)
                })
                .expect("the triangle has the edge");
            let (x, y, z) = (tri[i], tri[(i + 1) % 3], tri[(i + 2) % 3]);
            self.tris[t as usize] = [x, m, z];
            self.around[y as usize].retain(|&s| s != t);
            self.around[m as usize].push(t);
            self.add_tri([m, y, z]);
        }
        m
    }
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(input) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let m = triangulated(input, job)?;
    let len = s.f64("edge_length").max(1e-6);
    let (lo, hi) = (len * 4.0 / 5.0, len * 4.0 / 3.0);
    let pos: Vec<V> = m.p.iter().map(|&p| v3::of(p)).collect();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut src: Vec<u32> = Vec::new();
    let mut area = 0.0;
    for f in 0..m.prims() {
        let p = m.prim(f);
        if m.closed[f] && p.len() == 3 && p[0] != p[1] && p[1] != p[2] && p[0] != p[2] {
            let t = [p[0], p[1], p[2]];
            let [a, b, c] = t.map(|i| pos[i as usize]);
            area += v3::len(v3::cross(v3::sub(b, a), v3::sub(c, a))) / 2.0;
            tris.push(t);
            src.push(f as u32);
        }
    }
    if tris.is_empty() {
        return Ok(Mesh::new());
    }
    // An equilateral triangle of side L has area √3/4 L².
    let estimate = area / (3f64.sqrt() / 4.0 * lo * lo) * 1.5;
    if estimate > limits::PRIMS as f64 || estimate / 2.0 > limits::POINTS as f64 {
        return Err(format!(
            "an edge length of {len} would make about {:.0} triangles; the limit is {}",
            estimate,
            limits::PRIMS
        ));
    }
    let surface = Surface::new(&pos, &tris);
    let n = pos.len();
    let mut w = Tris {
        pos: pos.clone(),
        tris: tris.clone(),
        alive: vec![true; tris.len()],
        around: vec![Vec::new(); n],
        locked: vec![false; n],
        dead: vec![false; n],
        feature: HashSet::new(),
    };
    for (t, tri) in tris.iter().enumerate() {
        for &v in tri {
            w.around[v as usize].push(t as u32);
        }
    }
    // Creases: border, non-manifold and sharp edges.
    let cos_feature = s.f64("feature_angle").clamp(0.0, 180.0).to_radians().cos();
    let mut uses: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (t, tri) in tris.iter().enumerate() {
        for i in 0..3 {
            uses.entry(key(tri[i], tri[(i + 1) % 3]))
                .or_default()
                .push(t as u32);
        }
    }
    for (&e, ts) in &uses {
        let crease = match ts.as_slice() {
            [a, b] => {
                let (na, nb) = (
                    v3::norm(w.cross(tris[*a as usize])),
                    v3::norm(w.cross(tris[*b as usize])),
                );
                v3::dot(na, nb) < cos_feature - 1e-9
            }
            _ => true,
        };
        if crease {
            w.feature.insert(e);
            w.locked[e.0 as usize] = true;
            w.locked[e.1 as usize] = true;
        }
    }
    // Points no triangle uses take no part.
    for v in 0..n {
        if w.around[v].is_empty() {
            w.dead[v] = true;
        }
    }
    let project = s.bool("project");
    let mut on = Vec::new();
    let mut na = Vec::new();
    let mut nb = Vec::new();
    for _ in 0..s.i64("iterations").clamp(1, 50) {
        job.check()?;
        // Split long edges, longest first, one per triangle per pass.
        for _ in 0..32 {
            let mut long: Vec<(f64, u32, u32)> = w
                .edges()
                .into_iter()
                .map(|(a, b)| (w.len(a, b), a, b))
                .filter(|e| e.0 > hi)
                .collect();
            if long.is_empty() {
                break;
            }
            long.sort_by(|x, y| y.0.total_cmp(&x.0).then((x.1, x.2).cmp(&(y.1, y.2))));
            let mut busy = vec![false; w.tris.len()];
            for (i, &(_, a, b)) in long.iter().enumerate() {
                job.every(i)?;
                w.on_edge(a, b, &mut on);
                if on.iter().any(|&t| busy[t as usize]) {
                    continue;
                }
                for &t in &on {
                    busy[t as usize] = true;
                }
                w.split(a, b, &on);
                busy.resize(w.tris.len(), true);
            }
            limits::check(w.pos.len() as u64, w.tris.len() as u64, 0)?;
        }
        // Collapse short edges, shortest first.
        let mut short: Vec<(f64, u32, u32)> = w
            .edges()
            .into_iter()
            .map(|(a, b)| (w.len(a, b), a, b))
            .filter(|e| e.0 < lo)
            .collect();
        short.sort_by(|x, y| x.0.total_cmp(&y.0).then((x.1, x.2).cmp(&(y.1, y.2))));
        for (i, &(_, a, b)) in short.iter().enumerate() {
            job.every(i)?;
            if w.dead[a as usize] || w.dead[b as usize] || w.len(a, b) >= lo {
                continue;
            }
            let (keep, gone) = match (w.locked[a as usize], w.locked[b as usize]) {
                (true, true) => continue,
                (false, true) => (b, a),
                _ => (a, b),
            };
            w.on_edge(keep, gone, &mut on);
            if on.is_empty() || on.len() > 2 {
                continue;
            }
            let target = if w.locked[keep as usize] {
                w.pos[keep as usize]
            } else {
                v3::scale(v3::add(w.pos[a as usize], w.pos[b as usize]), 0.5)
            };
            w.neighbours(keep, &mut na);
            w.neighbours(gone, &mut nb);
            if na.iter().filter(|x| nb.contains(x)).count() != on.len() {
                continue;
            }
            // No new edge may be long, no triangle may flip.
            let too_long = na.iter().chain(nb.iter()).any(|&x| {
                x != keep && x != gone && v3::len(v3::sub(w.pos[x as usize], target)) > hi
            });
            if too_long {
                continue;
            }
            let mut ok = true;
            for &v in &[keep, gone] {
                for &t in &w.around[v as usize] {
                    let tri = w.tris[t as usize];
                    if !w.alive[t as usize] || (tri.contains(&keep) && tri.contains(&gone)) {
                        continue;
                    }
                    let before = w.cross(tri);
                    let [p0, p1, p2] = tri.map(|x| {
                        if x == keep || x == gone {
                            target
                        } else {
                            w.pos[x as usize]
                        }
                    });
                    let after = v3::cross(v3::sub(p1, p0), v3::sub(p2, p0));
                    if v3::dot(before, after) <= 0.1 * v3::len(before) * v3::len(after) {
                        ok = false;
                    }
                }
            }
            if !ok {
                continue;
            }
            w.pos[keep as usize] = target;
            let moved = std::mem::take(&mut w.around[gone as usize]);
            for t in moved {
                let ti = t as usize;
                if !w.alive[ti] {
                    continue;
                }
                if w.tris[ti].contains(&keep) {
                    w.alive[ti] = false;
                } else {
                    for x in &mut w.tris[ti] {
                        if *x == gone {
                            *x = keep;
                        }
                    }
                    w.around[keep as usize].push(t);
                }
            }
            let alive = &w.alive;
            w.around[keep as usize].retain(|&t| alive[t as usize]);
            w.dead[gone as usize] = true;
        }
        // Flip edges towards valence 6 (4 on borders).
        let valence = |w: &Tris, v: u32, buf: &mut Vec<u32>| {
            w.neighbours(v, buf);
            buf.len() as i64
        };
        let target = |w: &Tris, v: u32| if w.locked[v as usize] { 4 } else { 6 };
        for (i, (a, b)) in w.edges().into_iter().enumerate() {
            job.every(i)?;
            if w.feature.contains(&(a, b)) {
                continue;
            }
            w.on_edge(a, b, &mut on);
            if on.len() != 2 {
                continue;
            }
            let (mut t1, mut t2) = (on[0], on[1]);
            let has = |t: u32, x: u32, y: u32| {
                let tri = w.tris[t as usize];
                (0..3).any(|i| tri[i] == x && tri[(i + 1) % 3] == y)
            };
            if !has(t1, a, b) {
                std::mem::swap(&mut t1, &mut t2);
            }
            if !has(t1, a, b) || !has(t2, b, a) {
                continue;
            }
            let opp = |t: u32| {
                *w.tris[t as usize]
                    .iter()
                    .find(|&&x| x != a && x != b)
                    .expect("a third point")
            };
            let (c, d) = (opp(t1), opp(t2));
            if c == d {
                continue;
            }
            w.on_edge(c, d, &mut nb);
            if !nb.is_empty() {
                continue;
            }
            let vals = [a, b, c, d].map(|v| valence(&w, v, &mut na));
            let goal = [a, b, c, d].map(|v| target(&w, v));
            let dev = |delta: [i64; 4]| -> i64 {
                (0..4).map(|k| (vals[k] + delta[k] - goal[k]).abs()).sum()
            };
            if dev([-1, -1, 1, 1]) >= dev([0, 0, 0, 0]) {
                continue;
            }
            let n1 = [a, d, c];
            let n2 = [d, b, c];
            let old = v3::add(w.cross(w.tris[t1 as usize]), w.cross(w.tris[t2 as usize]));
            let (c1, c2) = (w.cross(n1), w.cross(n2));
            let lo_dot = |x: V| v3::dot(x, old) <= 0.2 * v3::len(x) * v3::len(old);
            if lo_dot(c1) || lo_dot(c2) {
                continue;
            }
            w.tris[t1 as usize] = n1;
            w.tris[t2 as usize] = n2;
            w.around[a as usize].retain(|&t| t != t2);
            w.around[b as usize].retain(|&t| t != t1);
            w.around[c as usize].push(t2);
            w.around[d as usize].push(t1);
        }
        // Tangential relaxation towards the area-weighted centroid.
        job.check()?;
        let mut normal = vec![[0.0f64; 3]; w.pos.len()];
        let mut centre = vec![([0.0f64; 3], 0.0f64); w.pos.len()];
        for (t, tri) in w.tris.iter().enumerate() {
            if !w.alive[t] {
                continue;
            }
            let cr = w.cross(*tri);
            let a = v3::len(cr);
            let c = v3::scale(
                v3::add(
                    v3::add(w.pos[tri[0] as usize], w.pos[tri[1] as usize]),
                    w.pos[tri[2] as usize],
                ),
                1.0 / 3.0,
            );
            for &v in tri {
                let v = v as usize;
                normal[v] = v3::add(normal[v], cr);
                centre[v].0 = v3::add(centre[v].0, v3::scale(c, a));
                centre[v].1 += a;
            }
        }
        for v in 0..w.pos.len() {
            if w.dead[v] || w.locked[v] || centre[v].1 <= 0.0 {
                continue;
            }
            let nn = v3::norm(normal[v]);
            let c = v3::scale(centre[v].0, 1.0 / centre[v].1);
            let d = v3::sub(c, w.pos[v]);
            let d = v3::sub(d, v3::scale(nn, v3::dot(d, nn)));
            w.pos[v] = v3::add(w.pos[v], v3::scale(d, 0.8));
            if project {
                w.pos[v] = surface.nearest(w.pos[v]).1;
            }
        }
    }
    // Compact, with attributes from the nearest input triangle.
    let mut out = Mesh {
        point_groups: m.point_groups.clone(),
        prim_groups: m.prim_groups.clone(),
        ..Mesh::new()
    };
    let mut map = vec![u32::MAX; w.pos.len()];
    let mut pts = Interp::new(&m.point_attrs, w.pos.len());
    let mut prims = Interp::new(&m.prim_attrs, w.tris.len());
    for (t, tri) in w.tris.iter().enumerate() {
        if !w.alive[t] {
            continue;
        }
        job.every(t)?;
        let ids = tri.map(|v| {
            if map[v as usize] == u32::MAX {
                let p = w.pos[v as usize];
                let (st, _, bary) = surface.nearest(p);
                let [i, j, k] = tris[st as usize];
                pts.blend(&[
                    (i, bary[0] as f32),
                    (j, bary[1] as f32),
                    (k, bary[2] as f32),
                ]);
                map[v as usize] = out.add_point(v3::f32(p));
            }
            map[v as usize]
        });
        out.add_polygon(&ids);
        let c = v3::scale(
            v3::add(
                v3::add(w.pos[tri[0] as usize], w.pos[tri[1] as usize]),
                w.pos[tri[2] as usize],
            ),
            1.0 / 3.0,
        );
        prims.copy(src[surface.nearest(c).0 as usize]);
    }
    out.point_attrs = pts.finish();
    out.prim_attrs = prims.finish();
    if m.point_attr(names::N).is_some() {
        let nn = point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}

/// Nearest-point queries on a triangle mesh through a uniform grid.
#[derive(Debug)]
pub struct Surface<'a> {
    pos: &'a [V],
    tris: &'a [[u32; 3]],
    cell: f64,
    grid: HashMap<[i64; 3], Vec<u32>>,
    reach: i64,
}

impl<'a> Surface<'a> {
    pub fn new(pos: &'a [V], tris: &'a [[u32; 3]]) -> Self {
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        let mut edge = 0.0;
        for t in tris {
            for i in 0..3 {
                let p = pos[t[i] as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
                edge += v3::len(v3::sub(p, pos[t[(i + 1) % 3] as usize]));
            }
        }
        let diag = v3::len(v3::sub(hi, lo)).max(1e-9);
        let mean_edge = edge / (3 * tris.len().max(1)) as f64;
        let cell = (diag / 64.0).max(mean_edge);
        let at = |x: f64| (x / cell).floor() as i64;
        let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
        for (t, tri) in tris.iter().enumerate() {
            let [a, b, c] = tri.map(|i| pos[i as usize]);
            let mn: [i64; 3] = std::array::from_fn(|k| at(a[k].min(b[k]).min(c[k])));
            let mx: [i64; 3] = std::array::from_fn(|k| at(a[k].max(b[k]).max(c[k])));
            for x in mn[0]..=mx[0] {
                for y in mn[1]..=mx[1] {
                    for z in mn[2]..=mx[2] {
                        grid.entry([x, y, z]).or_default().push(t as u32);
                    }
                }
            }
        }
        let reach = (diag / cell).ceil() as i64 + 2;
        Self {
            pos,
            tris,
            cell,
            grid,
            reach,
        }
    }

    /// The nearest triangle to `q`, the nearest point on it and that
    /// point's barycentric coordinates.
    pub fn nearest(&self, q: V) -> (u32, V, [f64; 3]) {
        let c = q.map(|x| (x / self.cell).floor() as i64);
        let mut best = (f64::MAX, 0u32, q, [1.0, 0.0, 0.0]);
        for r in 0..=self.reach {
            for x in -r..=r {
                for y in -r..=r {
                    for z in -r..=r {
                        if x.abs().max(y.abs()).max(z.abs()) != r {
                            continue;
                        }
                        let Some(list) = self.grid.get(&[c[0] + x, c[1] + y, c[2] + z]) else {
                            continue;
                        };
                        for &t in list {
                            let [a, b, cc] = self.tris[t as usize].map(|i| self.pos[i as usize]);
                            let (p, bary) = closest_on_triangle(q, a, b, cc);
                            let d = v3::len(v3::sub(p, q));
                            if d < best.0 || (d == best.0 && t < best.1) {
                                best = (d, t, p, bary);
                            }
                        }
                    }
                }
            }
            if best.0 <= r as f64 * self.cell {
                break;
            }
        }
        (best.1, best.2, best.3)
    }
}

/// The point of triangle `abc` nearest to `p`, with its barycentric
/// coordinates (Ericson, "Real-Time Collision Detection", 5.1.5).
fn closest_on_triangle(p: V, a: V, b: V, c: V) -> (V, [f64; 3]) {
    let ab = v3::sub(b, a);
    let ac = v3::sub(c, a);
    let ap = v3::sub(p, a);
    let d1 = v3::dot(ab, ap);
    let d2 = v3::dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (a, [1.0, 0.0, 0.0]);
    }
    let bp = v3::sub(p, b);
    let d3 = v3::dot(ab, bp);
    let d4 = v3::dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (b, [0.0, 1.0, 0.0]);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return (v3::add(a, v3::scale(ab, v)), [1.0 - v, v, 0.0]);
    }
    let cp = v3::sub(p, c);
    let d5 = v3::dot(ab, cp);
    let d6 = v3::dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (c, [0.0, 0.0, 1.0]);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return (v3::add(a, v3::scale(ac, w)), [1.0 - w, 0.0, w]);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return (v3::add(b, v3::scale(v3::sub(c, b), w)), [0.0, 1.0 - w, w]);
    }
    let denom = va + vb + vc;
    if denom.abs() < 1e-300 {
        return (a, [1.0, 0.0, 0.0]);
    }
    let v = vb / denom;
    let w = vc / denom;
    (
        v3::add(a, v3::add(v3::scale(ab, v), v3::scale(ac, w))),
        [1.0 - v - w, v, w],
    )
}
