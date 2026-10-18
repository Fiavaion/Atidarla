//! Worker-side cook times of typical MSH jobs (not a pass/fail test):
//! `cargo test -p atd-ops-msh --release --test timings -- --ignored --nocapture`.

use std::time::Instant;

use atd_ops_msh::{Mesh, run};
use serde_json::{Value, json};

fn time(label: &str, ty: &str, params: &[(&str, Value)], inputs: &[Option<Mesh>]) -> Mesh {
    let mut best = f64::MAX;
    let mut out = Mesh::new();
    for _ in 0..3 {
        let t = Instant::now();
        out = run(ty, params, inputs).unwrap();
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!(
        "{label:<46} {best:>9.2} ms  ({} points, {} prims)",
        out.points(),
        out.prims()
    );
    out
}

#[test]
#[ignore = "timings: run with --release --ignored --nocapture"]
fn worker_cook_times() {
    let sphere = time(
        "sphere polar 256 x 512",
        "msh.sphere",
        &[("rows", json!(256)), ("columns", json!(512))],
        &[],
    );
    let boxed = run("msh.box", &[], &[]).unwrap();
    time(
        "subdivide box, 5 levels",
        "msh.subdivide",
        &[("levels", json!(5))],
        &[Some(boxed)],
    );
    time(
        "normals, cusp 30, of the sphere",
        "msh.normals",
        &[("cusp", json!(30.0))],
        &[Some(sphere.clone())],
    );
    time(
        "polyreduce sphere to 10%",
        "msh.polyreduce",
        &[("percent", json!(10.0))],
        &[Some(sphere.clone())],
    );
    let ico = run(
        "msh.sphere",
        &[("type", json!("ico")), ("level", json!(4))],
        &[],
    )
    .unwrap();
    let cutter = run("msh.box", &[("centre", json!([0.3, 0.3, 0.3]))], &[]).unwrap();
    time(
        "boolean subtract, ico level 4 (5120 tris) - box",
        "msh.boolean",
        &[("operation", json!("subtract"))],
        &[Some(ico), Some(cutter)],
    );
    let pts = time(
        "scatter 1M points on the sphere",
        "msh.scatter",
        &[("count", json!(1_000_000))],
        &[Some(sphere.clone())],
    );
    let few = run(
        "msh.scatter",
        &[("count", json!(2000))],
        &[Some(sphere.clone())],
    )
    .unwrap();
    time(
        "iso surface, 2000 points, resolution 128",
        "msh.iso_surface",
        &[("radius", json!(0.05)), ("resolution", json!(128))],
        &[Some(few)],
    );
    time(
        "extrude every polygon of the sphere",
        "msh.extrude",
        &[],
        &[Some(sphere.clone())],
    );
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let glb = dir.join("timing.glb");
    time(
        "file out .glb (131k points)",
        "msh.file_out",
        &[("file", json!(glb.to_string_lossy()))],
        &[Some(sphere.clone())],
    );
    time(
        "file in .glb",
        "msh.file_in",
        &[("file", json!(glb.to_string_lossy()))],
        &[],
    );
    let t = Instant::now();
    let g = pts.to_geometry(None);
    eprintln!(
        "{:<46} {:>9.2} ms",
        "Mesh -> GEO conversion, 1M points",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let back = Mesh::from_geometry(&g, None).unwrap();
    eprintln!(
        "{:<46} {:>9.2} ms",
        "GEO -> Mesh conversion, 1M points",
        t.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(back.points(), pts.points());
}
