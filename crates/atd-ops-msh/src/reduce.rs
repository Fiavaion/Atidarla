//! PolyReduce: quadric error metric edge collapse (Garland & Heckbert,
//! "Surface Simplification Using Quadric Error Metrics", SIGGRAPH 1997).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use atd_core::geo::names;
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::triangulated;
use crate::mesh::{Column, Interp, Mesh, v3};
use crate::normals::point_normals;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("percent", "Keep (%)", 25.0, Some(0.0), Some(100.0))
            .help("Share of the triangles to keep."),
        ParamSchema::float("tolerance", "Tolerance", 0.0, Some(0.0), None).help(
            "Stops earlier once the cheapest collapse would move the surface by more than about this distance (0: no limit).",
        ),
        ParamSchema::toggle("keep_boundary", "Keep Boundary", true)
            .help("Open edges (holes and borders) do not move."),
    ]
}

/// A symmetric 4×4 quadric: xx xy xz xw yy yz yw zz zw ww.
type Quadric = [f64; 10];

fn plane_quadric(n: [f64; 3], d: f64, w: f64) -> Quadric {
    let [a, b, c] = n;
    [
        a * a,
        a * b,
        a * c,
        a * d,
        b * b,
        b * c,
        b * d,
        c * c,
        c * d,
        d * d,
    ]
    .map(|x| x * w)
}

fn add_q(a: &Quadric, b: &Quadric) -> Quadric {
    std::array::from_fn(|i| a[i] + b[i])
}

fn error(q: &Quadric, v: [f64; 3]) -> f64 {
    let [x, y, z] = v;
    q[0] * x * x
        + 2.0 * q[1] * x * y
        + 2.0 * q[2] * x * z
        + 2.0 * q[3] * x
        + q[4] * y * y
        + 2.0 * q[5] * y * z
        + 2.0 * q[6] * y
        + q[7] * z * z
        + 2.0 * q[8] * z
        + q[9]
}

/// The position minimising `q`, if the 3×3 system is well conditioned.
fn optimum(q: &Quadric) -> Option<[f64; 3]> {
    let m = [[q[0], q[1], q[2]], [q[1], q[4], q[5]], [q[2], q[5], q[7]]];
    let r = [-q[3], -q[6], -q[8]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let scale = m.iter().flatten().fold(0.0f64, |a, &b| a.max(b.abs()));
    if det.abs() <= 1e-10 * scale.powi(3).max(1e-300) {
        return None;
    }
    let col = |k: usize| {
        let mut a = m;
        for i in 0..3 {
            a[i][k] = r[i];
        }
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    Some([col(0) / det, col(1) / det, col(2) / det])
}

#[derive(Debug, PartialEq)]
struct Entry {
    cost: f64,
    a: u32,
    b: u32,
    va: u32,
    vb: u32,
}

impl Eq for Entry {}

impl Ord for Entry {
    fn cmp(&self, o: &Self) -> Ordering {
        // A min-heap on cost, ties broken by the edge (deterministic).
        o.cost
            .total_cmp(&self.cost)
            .then_with(|| o.a.cmp(&self.a))
            .then_with(|| o.b.cmp(&self.b))
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// The points sharing a live triangle with `v`.
fn neighbours(v: u32, around: &[Vec<u32>], alive: &[bool], tris: &[[u32; 3]], out: &mut Vec<u32>) {
    out.clear();
    for &t in &around[v as usize] {
        if alive[t as usize] {
            for &w in &tris[t as usize] {
                if w != v && !out.contains(&w) {
                    out.push(w);
                }
            }
        }
    }
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(input) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let m = triangulated(input, job)?;
    let had = m.point_attr(names::N).is_some();
    let n = m.points();
    let mut pos: Vec<[f64; 3]> = m.p.iter().map(|&p| v3::of(p)).collect();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut src: Vec<u32> = Vec::new();
    let mut lines: Vec<usize> = Vec::new();
    for f in 0..m.prims() {
        let p = m.prim(f);
        if m.closed[f] && p.len() == 3 {
            tris.push([p[0], p[1], p[2]]);
            src.push(f as u32);
        } else if !m.closed[f] {
            lines.push(f);
        }
    }
    let mut alive = vec![true; tris.len()];
    let mut live = tris.len();
    let target =
        ((tris.len() as f64) * s.f64("percent").clamp(0.0, 100.0) / 100.0).round() as usize;
    let tol = s.f64("tolerance").max(0.0);
    let mut around: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (t, tri) in tris.iter().enumerate() {
        for &v in tri {
            around[v as usize].push(t as u32);
        }
    }
    // Quadrics from the planes of each point's triangles (unweighted, so
    // the cost is a sum of squared distances and the tolerance a distance).
    let mut quad = vec![[0.0f64; 10]; n];
    let tri_normal = |pos: &[[f64; 3]], t: [u32; 3]| {
        let [a, b, c] = t.map(|i| pos[i as usize]);
        v3::cross(v3::sub(b, a), v3::sub(c, a))
    };
    for tri in &tris {
        let nn = v3::norm(tri_normal(&pos, *tri));
        let d = -v3::dot(nn, pos[tri[0] as usize]);
        let q = plane_quadric(nn, d, 1.0);
        for &v in tri {
            quad[v as usize] = add_q(&quad[v as usize], &q);
        }
    }
    // Boundary edges (used by one triangle).
    let mut edge_count: std::collections::HashMap<(u32, u32), (u32, u32)> =
        std::collections::HashMap::new();
    for (t, tri) in tris.iter().enumerate() {
        for i in 0..3 {
            let (a, b) = (tri[i], tri[(i + 1) % 3]);
            let e = edge_count
                .entry((a.min(b), a.max(b)))
                .or_insert((0, t as u32));
            e.0 += 1;
        }
    }
    let mut locked = vec![false; n];
    for &f in &lines {
        for &c in m.prim(f) {
            locked[c as usize] = true;
        }
    }
    let keep_boundary = s.bool("keep_boundary");
    let mut boundary_edges: Vec<((u32, u32), u32)> = edge_count
        .iter()
        .filter(|(_, v)| v.0 == 1)
        .map(|(k, v)| (*k, v.1))
        .collect();
    boundary_edges.sort_unstable();
    for ((a, b), t) in boundary_edges {
        if keep_boundary {
            locked[a as usize] = true;
            locked[b as usize] = true;
        } else {
            // A plane through the edge, perpendicular to its triangle,
            // weighted heavily: the border keeps its shape.
            let nn = v3::norm(tri_normal(&pos, tris[t as usize]));
            let e = v3::sub(pos[b as usize], pos[a as usize]);
            let side = v3::norm(v3::cross(e, nn));
            let d = -v3::dot(side, pos[a as usize]);
            let q = plane_quadric(side, d, 100.0);
            quad[a as usize] = add_q(&quad[a as usize], &q);
            quad[b as usize] = add_q(&quad[b as usize], &q);
        }
    }
    let mut version = vec![0u32; n];
    let mut dead = vec![false; n];
    let plan = |a: u32,
                b: u32,
                pos: &[[f64; 3]],
                quad: &[Quadric],
                locked: &[bool]|
     -> Option<(f64, [f64; 3])> {
        let (la, lb) = (locked[a as usize], locked[b as usize]);
        let q = add_q(&quad[a as usize], &quad[b as usize]);
        let (pa, pb) = (pos[a as usize], pos[b as usize]);
        let target = match (la, lb) {
            (true, true) => return None,
            (true, false) => pa,
            (false, true) => pb,
            _ => optimum(&q).unwrap_or_else(|| {
                let mid = v3::scale(v3::add(pa, pb), 0.5);
                *[pa, pb, mid]
                    .iter()
                    .min_by(|x, y| error(&q, **x).total_cmp(&error(&q, **y)))
                    .expect("three candidates")
            }),
        };
        Some((error(&q, target).max(0.0), target))
    };
    let mut heap = BinaryHeap::new();
    let mut sorted: Vec<(u32, u32)> = edge_count.keys().copied().collect();
    sorted.sort_unstable();
    for (a, b) in sorted {
        if let Some((cost, _)) = plan(a, b, &pos, &quad, &locked) {
            heap.push(Entry {
                cost,
                a,
                b,
                va: 0,
                vb: 0,
            });
        }
    }
    let mut steps = 0usize;
    let mut na: Vec<u32> = Vec::new();
    let mut nb: Vec<u32> = Vec::new();
    while live > target {
        let Some(e) = heap.pop() else { break };
        steps += 1;
        job.every(steps)?;
        let (a, b) = (e.a, e.b);
        if dead[a as usize]
            || dead[b as usize]
            || version[a as usize] != e.va
            || version[b as usize] != e.vb
        {
            continue;
        }
        if tol > 0.0 && e.cost > tol * tol {
            break;
        }
        let Some((_, target_pos)) = plan(a, b, &pos, &quad, &locked) else {
            continue;
        };
        // Link condition: the shared neighbours are exactly the opposite
        // corners of the triangles on the edge.
        neighbours(a, &around, &alive, &tris, &mut na);
        neighbours(b, &around, &alive, &tris, &mut nb);
        let shared = na.iter().filter(|w| nb.contains(w)).count();
        let on_edge = around[a as usize]
            .iter()
            .filter(|&&t| alive[t as usize] && tris[t as usize].contains(&b))
            .count();
        if shared != on_edge || on_edge == 0 {
            continue;
        }
        // No triangle may flip or collapse.
        let mut ok = true;
        for &v in &[a, b] {
            for &t in &around[v as usize] {
                let t = t as usize;
                if !alive[t] || (tris[t].contains(&a) && tris[t].contains(&b)) {
                    continue;
                }
                let before = tri_normal(&pos, tris[t]);
                let moved = tris[t].map(|w| if w == a || w == b { u32::MAX } else { w });
                let corner = |w: u32| {
                    if w == u32::MAX {
                        target_pos
                    } else {
                        pos[w as usize]
                    }
                };
                let [p0, p1, p2] = moved.map(corner);
                let after = v3::cross(v3::sub(p1, p0), v3::sub(p2, p0));
                let (lb, la) = (v3::len(before), v3::len(after));
                if la <= 1e-12 * lb.max(1e-30) || v3::dot(before, after) < 0.2 * lb * la {
                    ok = false;
                    break;
                }
            }
            if !ok {
                break;
            }
        }
        if !ok {
            continue;
        }
        // Collapse b into a.
        pos[a as usize] = target_pos;
        quad[a as usize] = add_q(&quad[a as usize], &quad[b as usize]);
        locked[a as usize] |= locked[b as usize];
        let moved_tris = std::mem::take(&mut around[b as usize]);
        for t in moved_tris {
            let ti = t as usize;
            if !alive[ti] {
                continue;
            }
            if tris[ti].contains(&a) {
                alive[ti] = false;
                live -= 1;
            } else {
                for w in &mut tris[ti] {
                    if *w == b {
                        *w = a;
                    }
                }
                around[a as usize].push(t);
            }
        }
        around[a as usize].retain(|&t| alive[t as usize]);
        dead[b as usize] = true;
        version[a as usize] += 1;
        neighbours(a, &around, &alive, &tris, &mut na);
        for &w in &na {
            let (x, y) = (a.min(w), a.max(w));
            if let Some((cost, _)) = plan(x, y, &pos, &quad, &locked) {
                heap.push(Entry {
                    cost,
                    a: x,
                    b: y,
                    va: version[x as usize],
                    vb: version[y as usize],
                });
            }
        }
    }
    // Rebuild: surviving triangles and polylines, compacted points.
    let mut out = Mesh::new();
    let mut map = vec![u32::MAX; n];
    let mut pts = Interp::new(&m.point_attrs, n);
    let mut id = |v: u32, out: &mut Mesh, pts: &mut Interp| -> u32 {
        if map[v as usize] == u32::MAX {
            map[v as usize] = out.add_point(v3::f32(pos[v as usize]));
            pts.copy(v);
        }
        map[v as usize]
    };
    let mut prims = Interp::new(&m.prim_attrs, live + lines.len());
    for (t, tri) in tris.iter().enumerate() {
        if alive[t] {
            let ids = tri.map(|v| id(v, &mut out, &mut pts));
            out.add_polygon(&ids);
            prims.copy(src[t]);
        }
    }
    for &f in &lines {
        let ids: Vec<u32> = m
            .prim(f)
            .iter()
            .map(|&v| id(v, &mut out, &mut pts))
            .collect();
        out.add_prim(&ids, false);
        prims.copy(f as u32);
    }
    out.point_attrs = pts.finish();
    out.prim_attrs = prims.finish();
    out.point_groups = m.point_groups.clone();
    out.prim_groups = m.prim_groups.clone();
    if had {
        let nn = point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}
