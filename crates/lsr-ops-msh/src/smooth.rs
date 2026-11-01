//! Smooth: Laplacian (umbrella) smoothing, and Taubin's λ|μ smoothing,
//! which alternates a shrinking and an inflating step so the shape keeps
//! its size (G. Taubin, "A Signal Processing Approach to Fair Surface
//! Design", SIGGRAPH 1995). Topology is unchanged.

use lsr_core::geo::{AttrClass, names};
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Mesh, v3};
use crate::normals::point_normals;
use crate::topo::Edges;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("method", "Method", &["taubin", "laplacian"], "taubin").help(
            "taubin: alternates smoothing and inflating steps, so noise goes but the shape keeps its size; laplacian: moves each point towards the average of its neighbours (shrinks the shape as it smooths).",
        ),
        ParamSchema::int("iterations", "Iterations", 10, Some(0), Some(1000))
            .help("Smoothing steps (taubin: each step is one smoothing and one inflating pass)."),
        ParamSchema::float("strength", "Strength", 0.5, Some(0.0), Some(1.0))
            .help("Share of the way each point moves towards the average of its neighbours per step."),
        ParamSchema::float("pass_band", "Pass Band", 0.1, Some(0.01), Some(0.5)).help(
            "taubin: the frequency below which shapes are kept (smaller keeps only broader shapes, smoothing more).",
        ),
        ParamSchema::toggle("keep_boundary", "Keep Boundary", true).help(
            "Points on open borders (and the ends of polylines) stay put; off, border points are smoothed along the border only.",
        ),
        ParamSchema::string("group", "Group", "")
            .help("Only the points of this point group move (empty: all points)."),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let n = m.points();
    let name = s.str("group");
    let movable: Vec<bool> = if name.is_empty() {
        vec![true; n]
    } else {
        m.group_members(AttrClass::Point, name)
            .ok_or_else(|| format!("the input has no point group `{name}`"))?
    };
    let edges = Edges::new(m);
    // Neighbours per point (CSR): border points see only border
    // neighbours, so a border slides along itself instead of caving in.
    let border = edges.border_points(m);
    let keep_boundary = s.bool("keep_boundary");
    let mut start = vec![0u32; n + 1];
    let mut list: Vec<u32> = Vec::new();
    let mut fixed = vec![false; n];
    for p in 0..n {
        job.every(p)?;
        let around = edges.neighbours(p as u32);
        let on_border = border[p];
        if on_border && keep_boundary {
            fixed[p] = true;
        }
        for &(q, is_border) in around {
            if !on_border || is_border {
                list.push(q);
            }
        }
        start[p + 1] = list.len() as u32;
        // A polyline end (one neighbour) or a lone point stays put.
        if start[p + 1] - start[p] < 2 && (on_border || around.len() < 2) {
            fixed[p] = true;
        }
    }
    let lambda = s.f64("strength").clamp(0.0, 1.0);
    let taubin = s.str("method") != "laplacian";
    // 1/λ + 1/μ = k_PB (Taubin's pass-band frequency), μ < -λ.
    let kpb = s.f64("pass_band").clamp(0.01, 0.5);
    let mu = if lambda > 0.0 {
        1.0 / (kpb - 1.0 / lambda)
    } else {
        0.0
    };
    let mut pos: Vec<[f64; 3]> = m.p.iter().map(|&p| v3::of(p)).collect();
    let mut next = pos.clone();
    let step = |pos: &mut Vec<[f64; 3]>, next: &mut Vec<[f64; 3]>, w: f64| {
        for p in 0..n {
            let (a, b) = (start[p] as usize, start[p + 1] as usize);
            if fixed[p] || !movable[p] || a == b {
                next[p] = pos[p];
                continue;
            }
            let mut c = [0.0; 3];
            for &q in &list[a..b] {
                c = v3::add(c, pos[q as usize]);
            }
            let c = v3::scale(c, 1.0 / (b - a) as f64);
            next[p] = v3::add(pos[p], v3::scale(v3::sub(c, pos[p]), w));
        }
        std::mem::swap(pos, next);
    };
    for _ in 0..s.i64("iterations").clamp(0, 1000) {
        job.check()?;
        step(&mut pos, &mut next, lambda);
        if taubin {
            step(&mut pos, &mut next, mu);
        }
    }
    let mut out = m.clone();
    out.p = pos.into_iter().map(v3::f32).collect();
    if out.point_attr(names::N).is_some() {
        let nn = point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}
