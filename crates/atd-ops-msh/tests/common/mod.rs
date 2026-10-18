//! Mesh checks shared by the CPU tests.
#![allow(dead_code)]

use std::collections::HashMap;

use atd_ops_msh::Mesh;
use serde_json::Value;

pub fn run(ty: &str, params: &[(&str, Value)], inputs: &[Option<Mesh>]) -> Mesh {
    atd_ops_msh::run(ty, params, inputs).unwrap_or_else(|e| panic!("{ty}: {e}"))
}

pub fn make(ty: &str, params: &[(&str, Value)]) -> Mesh {
    run(ty, params, &[])
}

pub fn on(ty: &str, params: &[(&str, Value)], input: &Mesh) -> Mesh {
    run(ty, params, &[Some(input.clone())])
}

/// Every edge of every polygon is used exactly twice, once in each
/// direction (closed, consistently oriented, manifold edges).
pub fn watertight(m: &Mesh) -> bool {
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for f in 0..m.prims() {
        if !m.closed[f] {
            continue;
        }
        let p = m.prim(f);
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    !edges.is_empty()
        && edges
            .iter()
            .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1))
}

/// Signed volume (positive when polygons face outward).
pub fn volume(m: &Mesh) -> f64 {
    let mut v = 0.0;
    for f in 0..m.prims() {
        if !m.closed[f] {
            continue;
        }
        let p = m.prim(f);
        let a = m.p[p[0] as usize].map(f64::from);
        for i in 1..p.len() - 1 {
            let b = m.p[p[i] as usize].map(f64::from);
            let c = m.p[p[i + 1] as usize].map(f64::from);
            v += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]);
        }
    }
    v / 6.0
}

pub fn area(m: &Mesh) -> f64 {
    (0..m.prims())
        .filter(|&f| m.closed[f])
        .map(|f| {
            let n = m.prim_normal(f);
            (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() / 2.0
        })
        .sum()
}

/// Every normal has unit length (within `tol`).
pub fn unit_normals(m: &Mesh, tol: f32) -> bool {
    let n = m.vec3("N").expect("N");
    n.iter()
        .all(|v| ((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() - 1.0).abs() <= tol)
}

pub fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1e-12)
}
