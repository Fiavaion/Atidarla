//! Clean: welds duplicate points, removes degenerate and duplicate
//! primitives and unused points, and makes polygon winding consistent
//! (closed parts facing outward).

use std::collections::{HashSet, VecDeque};

use atd_core::geo::names;
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::fused;
use crate::mesh::{Column, Interp, Mesh, v3};
use crate::normals::point_normals;
use crate::topo::Edges;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::toggle("fuse", "Fuse Points", true)
            .help("Welds points closer than Distance into one (the lowest-numbered keeps its attributes)."),
        ParamSchema::float("distance", "Distance", 1e-5, Some(0.0), None)
            .help("Fuse Points: points closer than this are welded (0: only exact duplicates)."),
        ParamSchema::toggle("degenerate", "Remove Degenerate", true).help(
            "Removes repeated corners, polygons with fewer than three distinct points or no area (see Min Area), and polylines with fewer than two points.",
        ),
        ParamSchema::float("min_area", "Min Area", 0.0, Some(0.0), None)
            .help("Remove Degenerate: polygons with at most this area count as degenerate."),
        ParamSchema::toggle("duplicates", "Remove Duplicates", true).help(
            "Removes primitives using the same points as an earlier one (in either direction); the first is kept.",
        ),
        ParamSchema::toggle("winding", "Fix Winding", true).help(
            "Flips polygons so neighbours agree on their direction; closed parts are turned to face outward, open parts follow the direction most of their polygons had.",
        ),
        ParamSchema::toggle("unused", "Remove Unused Points", true)
            .help("Removes points no primitive uses (when there are primitives)."),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(input) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let had = input.point_attr(names::N).is_some();
    let mut m = if s.bool("fuse") {
        fused(input, s.f64("distance").max(0.0), job)?
    } else {
        input.clone()
    };
    job.check()?;
    // Primitives to keep, with their (possibly shortened) corners.
    let mut keep: Vec<(usize, Vec<u32>)> = Vec::with_capacity(m.prims());
    let degenerate = s.bool("degenerate");
    let min_area = s.f64("min_area").max(0.0);
    for f in 0..m.prims() {
        job.every(f)?;
        let mut ids: Vec<u32> = m.prim(f).to_vec();
        if degenerate {
            ids.dedup();
            if m.closed[f] {
                while ids.len() > 1 && ids.first() == ids.last() {
                    ids.pop();
                }
                let mut distinct = ids.clone();
                distinct.sort_unstable();
                distinct.dedup();
                let area = v3::len(crate::mesh::newell(ids.iter().map(|&i| m.p[i as usize]))) / 2.0;
                if distinct.len() < 3 || area <= min_area {
                    continue;
                }
            } else if ids.len() < 2 {
                continue;
            }
        }
        keep.push((f, ids));
    }
    if s.bool("duplicates") {
        let mut seen: HashSet<(bool, Vec<u32>)> = HashSet::new();
        keep.retain(|(f, ids)| seen.insert((m.closed[*f], canonical(ids, m.closed[*f]))));
    }
    let mut out = Mesh {
        p: std::mem::take(&mut m.p),
        point_attrs: std::mem::take(&mut m.point_attrs),
        point_groups: m.point_groups.clone(),
        prim_groups: m.prim_groups.clone(),
        ..Mesh::new()
    };
    let mut prims = Interp::new(&m.prim_attrs, keep.len());
    for (f, ids) in &keep {
        out.add_prim(ids, m.closed[*f]);
        prims.copy(*f as u32);
    }
    out.prim_attrs = prims.finish();
    if s.bool("winding") {
        orient(&mut out, job)?;
    }
    if s.bool("unused") && out.prims() > 0 {
        out.remove_unused_points();
    }
    if had {
        let nn = point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}

/// The point list of a primitive up to its start and (for polygons)
/// direction, for duplicate detection.
fn canonical(ids: &[u32], closed: bool) -> Vec<u32> {
    if !closed {
        let rev: Vec<u32> = ids.iter().rev().copied().collect();
        return if rev.as_slice() < ids {
            rev
        } else {
            ids.to_vec()
        };
    }
    let rotate = |v: &[u32]| -> Vec<u32> {
        let k = (0..v.len()).min_by_key(|&i| v[i]).unwrap_or(0);
        v[k..].iter().chain(&v[..k]).copied().collect()
    };
    let fwd = rotate(ids);
    let rev: Vec<u32> = ids.iter().rev().copied().collect();
    let rev = rotate(&rev);
    fwd.min(rev)
}

/// Makes neighbouring polygons agree on their direction (across edges two
/// polygons share), then turns each closed part to face outward (positive
/// volume) and each open part to the direction most of it had.
pub fn orient(m: &mut Mesh, job: &Job) -> Result<(), String> {
    let edges = Edges::new(m);
    let n = m.prims();
    let mut flip = vec![false; n];
    let mut seen = vec![false; n];
    let mut queue = VecDeque::new();
    let mut part: Vec<usize> = Vec::new();
    for seed in 0..n {
        if seen[seed] || !m.closed[seed] {
            continue;
        }
        job.check()?;
        seen[seed] = true;
        queue.push_back(seed);
        part.clear();
        let mut closed_part = true;
        while let Some(f) = queue.pop_front() {
            part.push(f);
            let p = m.prim(f);
            let k = p.len();
            for i in 0..k {
                let (a, b) = (p[i], p[(i + 1) % k]);
                let uses = edges.of(a, b);
                if uses.len() != 2 {
                    closed_part = false;
                    continue;
                }
                for &(g, c) in uses {
                    let g = g as usize;
                    if g == f || seen[g] {
                        continue;
                    }
                    // Same direction along the shared edge: one must flip.
                    let same = m.prim(g)[c as usize] == a;
                    flip[g] = flip[f] ^ same;
                    seen[g] = true;
                    queue.push_back(g);
                }
            }
        }
        let turn = if closed_part {
            let mut vol = 0.0;
            for &f in &part {
                vol += signed_volume(m, f) * if flip[f] { -1.0 } else { 1.0 };
            }
            vol < 0.0
        } else {
            part.iter().filter(|&&f| flip[f]).count() * 2 > part.len()
        };
        if turn {
            for &f in &part {
                flip[f] = !flip[f];
            }
        }
    }
    for f in 0..n {
        if flip[f] {
            let (a, b) = (m.start[f] as usize, m.start[f + 1] as usize);
            m.corners[a..b].reverse();
        }
    }
    Ok(())
}

/// Six times the signed volume polygon `f` adds (a fan from its first
/// point to the origin).
fn signed_volume(m: &Mesh, f: usize) -> f64 {
    let p = m.prim(f);
    let a = v3::of(m.p[p[0] as usize]);
    let mut v = 0.0;
    for i in 1..p.len().saturating_sub(1) {
        let b = v3::of(m.p[p[i] as usize]);
        let c = v3::of(m.p[p[i + 1] as usize]);
        v += v3::dot(a, v3::cross(b, c));
    }
    v
}
