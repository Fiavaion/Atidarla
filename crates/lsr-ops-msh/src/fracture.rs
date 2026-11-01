//! Fracture: breaks a closed mesh into Voronoi pieces. Each seed point's
//! cell (the space nearer to it than to any other seed) is built as a
//! convex polyhedron by cutting a box with the bisector planes of the
//! other seeds, then intersected with the input by the BSP Boolean. Heavy:
//! it cooks on a worker like every MSH operator, and is usually baked.

use lsr_core::geo::{AttrClass, names};
use lsr_core::registry::ParamSchema;
use serde_json::json;

use crate::cook::{Job, Snap};
use crate::edit::{merged, triangulated};
use crate::mesh::{Column, Mesh, v3};
use crate::points::unit;

type V = [f64; 3];

/// Most pieces.
pub const MAX_PIECES: usize = 512;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::int("count", "Pieces", 12, Some(1), Some(MAX_PIECES as i64)).help(
            "Without a `points` input: the number of seeded random seed points inside the mesh.",
        ),
        ParamSchema::int("seed", "Seed", 1, Some(0), None)
            .help("Random seed for the seed points: the same seed gives the same pieces."),
        ParamSchema::float("gap", "Gap", 0.0, Some(0.0), None)
            .help("Space left between neighbouring pieces (each cut moves back by half of it)."),
    ]
}

/// A convex polyhedron: polygons facing outward.
type Cell = Vec<Vec<V>>;

fn cube(lo: V, hi: V) -> Cell {
    let c = |i: usize| {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ]
    .iter()
    .map(|f| f.iter().map(|&i| c(i)).collect())
    .collect()
}

/// Keeps the part of `cell` where `n·x <= d`, closing the cut.
fn clip(cell: &Cell, n: V, d: f64, eps: f64) -> Cell {
    let mut out: Cell = Vec::with_capacity(cell.len() + 1);
    let mut cut: Vec<V> = Vec::new();
    for poly in cell {
        let mut next = Vec::with_capacity(poly.len() + 1);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (da, db) = (v3::dot(n, a) - d, v3::dot(n, b) - d);
            if da <= eps {
                next.push(a);
                if da.abs() <= eps {
                    cut.push(a);
                }
            }
            if (da < -eps && db > eps) || (da > eps && db < -eps) {
                let x = v3::lerp(a, b, da / (da - db));
                next.push(x);
                cut.push(x);
            }
        }
        if next.len() >= 3 {
            out.push(next);
        }
    }
    // The cap: the cut points around their centre, facing +n.
    if cut.len() >= 3 {
        let c = v3::scale(
            cut.iter().fold([0.0; 3], |s, p| v3::add(s, *p)),
            1.0 / cut.len() as f64,
        );
        let u = v3::norm(v3::sub(cut[0], c));
        let w = v3::cross(n, u);
        let mut ang: Vec<(f64, V)> = cut
            .iter()
            .map(|p| {
                let r = v3::sub(*p, c);
                (v3::dot(r, w).atan2(v3::dot(r, u)), *p)
            })
            .collect();
        ang.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cap: Vec<V> = Vec::with_capacity(ang.len());
        for (_, p) in ang {
            if cap.last().is_none_or(|q| v3::len(v3::sub(p, *q)) > eps) {
                cap.push(p);
            }
        }
        while cap.len() > 1 && v3::len(v3::sub(cap[0], cap[cap.len() - 1])) <= eps {
            cap.pop();
        }
        if cap.len() >= 3 {
            out.push(cap);
        }
    }
    out
}

/// Whether `q` is inside the closed triangle mesh `tris` (parity of
/// crossings of a ray in a fixed, unaligned direction).
fn inside(tris: &[[V; 3]], q: V) -> bool {
    let dir = v3::norm([0.5773, 0.5783, 0.5763]);
    let mut n = 0;
    for t in tris {
        // Möller-Trumbore.
        let (e1, e2) = (v3::sub(t[1], t[0]), v3::sub(t[2], t[0]));
        let h = v3::cross(dir, e2);
        let a = v3::dot(e1, h);
        if a.abs() < 1e-18 {
            continue;
        }
        let f = 1.0 / a;
        let s = v3::sub(q, t[0]);
        let u = f * v3::dot(s, h);
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let qv = v3::cross(s, e1);
        let v = f * v3::dot(dir, qv);
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        if f * v3::dot(e2, qv) > 0.0 {
            n += 1;
        }
    }
    n % 2 == 1
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(src) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let Some((lo, hi)) = src.bounds() else {
        return Ok(Mesh::new());
    };
    let (lo, hi) = (v3::of(lo), v3::of(hi));
    let diag = v3::len(v3::sub(hi, lo)).max(1e-9);
    let eps = 1e-9 * diag;
    let tri = triangulated(src, job)?;
    let tris: Vec<[V; 3]> = (0..tri.prims())
        .filter(|&f| tri.closed[f] && tri.prim(f).len() == 3)
        .map(|f| {
            let p = tri.prim(f);
            [0, 1, 2].map(|k| v3::of(tri.p[p[k] as usize]))
        })
        .collect();
    // Seed points: the `points` input, else random points inside.
    let seeds: Vec<V> = match inputs.get(1).and_then(Option::as_ref) {
        Some(pts) if pts.points() > 0 => {
            if pts.points() > MAX_PIECES {
                return Err(format!(
                    "{} seed points; Fracture makes at most {MAX_PIECES} pieces",
                    pts.points()
                ));
            }
            pts.p.iter().map(|&p| v3::of(p)).collect()
        }
        _ => {
            let count = s.i64("count").clamp(1, MAX_PIECES as i64) as usize;
            let seed = s.i64("seed").max(0) as u64;
            let mut out = Vec::with_capacity(count);
            let mut i = 0u64;
            while out.len() < count && i < 10_000 * count as u64 {
                job.every(i as usize)?;
                let q = [0, 1, 2].map(|k| lo[k] + (hi[k] - lo[k]) * unit(seed, i, k as u64));
                i += 1;
                if inside(&tris, q) {
                    out.push(q);
                }
            }
            out
        }
    };
    let half_gap = s.f64("gap").max(0.0) / 2.0;
    let pad = [0.01 * diag; 3];
    let (blo, bhi) = (v3::sub(lo, pad), v3::add(hi, pad));
    let mut pieces: Vec<Mesh> = Vec::with_capacity(seeds.len());
    for (i, &si) in seeds.iter().enumerate() {
        job.check()?;
        let mut cell = cube(blo, bhi);
        let mut others: Vec<(f64, V)> = seeds
            .iter()
            .enumerate()
            .filter(|&(j, _)| j != i)
            .map(|(_, &sj)| (v3::len(v3::sub(sj, si)), sj))
            .collect();
        others.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (dist, sj) in others {
            if dist <= eps {
                continue;
            }
            // Farther seeds cannot cut once the cell lies within half
            // their distance.
            let reach = cell
                .iter()
                .flatten()
                .map(|p| v3::len(v3::sub(*p, si)))
                .fold(0.0, f64::max);
            if dist / 2.0 > reach {
                break;
            }
            let n = v3::scale(v3::sub(sj, si), 1.0 / dist);
            let d = v3::dot(n, v3::lerp(si, sj, 0.5)) - half_gap;
            cell = clip(&cell, n, d, eps);
            if cell.len() < 4 {
                break;
            }
        }
        if cell.len() < 4 {
            continue;
        }
        let mut cm = Mesh::new();
        for poly in &cell {
            let ids: Vec<u32> = poly.iter().map(|&p| cm.add_point(v3::f32(p))).collect();
            cm.add_polygon(&ids);
        }
        let mut piece = crate::run(
            "msh.boolean",
            &[("operation", json!("intersect"))],
            &[Some(src.clone()), Some(cm)],
        )?;
        if piece.prims() == 0 {
            continue;
        }
        for g in &mut piece.prim_groups {
            if g == "from_a" {
                *g = "outside".into();
            } else if g == "from_b" {
                *g = "inside".into();
            }
        }
        let id = pieces.len() as u32;
        piece.set_prim_attr(Column::u32("piece", vec![id; piece.prims()]));
        piece.set_point_attr(Column::u32("piece", vec![id; piece.points()]));
        piece.point_attrs.retain(|c| c.name != names::N);
        pieces.push(piece);
    }
    let refs: Vec<&Mesh> = pieces.iter().collect();
    let mut out = merged(&refs, job)?;
    // Every piece had both groups, but make sure they exist when empty.
    for g in ["inside", "outside"] {
        if out.group_members(AttrClass::Prim, g).is_none() {
            out.set_group(AttrClass::Prim, g, &vec![false; out.prims()])?;
        }
    }
    Ok(out)
}
