//! Mesh files: round trips through OBJ, PLY and glTF keep positions,
//! normals, texture coordinates and colours; writers are deterministic;
//! readers handle the formats' features and refuse bad indices. Files go
//! to Cargo's per-target temporary folder (inside `target/`).

mod common;

use std::path::PathBuf;

use atd_ops_msh::io::{self, Format, ObjPoints};
use atd_ops_msh::{Column, Mesh};
use common::*;
use serde_json::json;

fn dir(tag: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("msh-files-{tag}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A sphere with N, uv and Cd, and a polyline.
fn sample() -> Mesh {
    let s = make("msh.sphere", &[("rows", json!(8)), ("columns", json!(12))]);
    let mut s = on(
        "msh.uv_project",
        &[("projection", json!("spherical")), ("axis", json!("y"))],
        &s,
    );
    let cd: Vec<f32> = (0..s.points())
        .flat_map(|i| [i as f32 / 100.0, 0.5, 1.0, 1.0])
        .collect();
    s.set_point_attr(Column::f32("Cd", 4, cd));
    s
}

fn attr(m: &Mesh, name: &str) -> Vec<f32> {
    m.point_attr(name)
        .unwrap_or_else(|| panic!("no {name}"))
        .floats()
        .unwrap()
        .to_vec()
}

fn near(a: &[f32], b: &[f32], tol: f32) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
}

#[test]
fn round_trips_keep_attributes() {
    let m = sample();
    let flat = |m: &Mesh| m.p.iter().flatten().copied().collect::<Vec<f32>>();
    for format in [Format::Obj, Format::Ply, Format::Gltf, Format::Glb] {
        let bytes = io::write(&m, format).unwrap();
        assert_eq!(
            io::write(&m, format).unwrap(),
            bytes,
            "{format:?} is deterministic"
        );
        let back = match format {
            Format::Obj => io::read_obj(&bytes, ObjPoints::Split).unwrap(),
            Format::Ply => io::read_ply(&bytes).unwrap(),
            _ => io::read_gltf(&bytes, None).unwrap(),
        };
        back.validate().unwrap();
        assert_eq!(back.points(), m.points(), "{format:?}");
        assert!(near(&flat(&back), &flat(&m), 0.0), "{format:?}: P is exact");
        assert!(
            near(&attr(&back, "N"), &attr(&m, "N"), 0.0),
            "{format:?}: N"
        );
        assert!(
            near(&attr(&back, "uv"), &attr(&m, "uv"), 0.0),
            "{format:?}: uv"
        );
        // PLY stores colours in 8 bits; OBJ has no alpha.
        let tol = if format == Format::Ply {
            0.51 / 255.0
        } else {
            0.0
        };
        assert!(
            near(&attr(&back, "Cd"), &attr(&m, "Cd"), tol),
            "{format:?}: Cd"
        );
        // glTF is triangulated; the others keep polygons.
        if matches!(format, Format::Obj | Format::Ply) {
            assert_eq!(back.prims(), m.prims(), "{format:?}");
            assert!(watertight(&back));
        } else {
            assert!(watertight(&back));
            assert!(close(volume(&back), volume(&m), 1e-6));
        }
    }
}

#[test]
fn obj_features() {
    let text = "# a quad, a triangle with negative indices, a line, groups\n\
        v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 2 0 0 1 0 0\n\
        vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\n\
        g base\nf 1/1 2/2 3/3 4/4\n\
        g extra base\nf -4/-3 5/2 -3/-2\n\
        l 1 5\n";
    let split = io::read_obj(text.as_bytes(), ObjPoints::Split).unwrap();
    assert_eq!(split.prims(), 3);
    assert!(!split.closed[2]);
    // (2, vt 2) is shared; point 5 with vt 2 differs from point 2's use.
    assert_eq!(split.prim(1), [1, 4, 2]);
    assert_eq!(split.prim_groups, ["base", "extra"]);
    assert_eq!(
        atd_ops_msh::io::prim_group(&split, "extra"),
        [false, true, true]
    );
    let cd = attr(&split, "Cd");
    assert_eq!(
        &cd[4 * 4..4 * 4 + 4],
        [1.0, 0.0, 0.0, 1.0],
        "vertex colours"
    );
    assert_eq!(&cd[..4], [1.0, 1.0, 1.0, 1.0], "white without a colour");
    let welded = io::read_obj(text.as_bytes(), ObjPoints::Position).unwrap();
    assert_eq!(welded.points(), 5);
    for bad in [
        "f 1 2 9\nv 0 0 0",
        "v 0 0 0\nf 0 1 1",
        "v 1 2\n",
        "v 0 0 0\nf 1/x 1 1",
    ] {
        assert!(
            io::read_obj(bad.as_bytes(), ObjPoints::Split).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn ply_ascii_and_big_endian() {
    let ascii = "ply\nformat ascii 1.0\ncomment x\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\n\
        property uchar red\nproperty uchar green\nproperty uchar blue\n\
        element face 1\nproperty list uchar int vertex_indices\nelement extra 1\nproperty int k\nend_header\n\
        0 0 0 255 0 0\n1 0 0 0 255 0\n1 1 0 0 0 255\n0 1 0 255 255 255\n4 0 1 2 3\n42\n";
    let m = io::read_ply(ascii.as_bytes()).unwrap();
    assert_eq!((m.points(), m.prims()), (4, 1));
    assert_eq!(&attr(&m, "Cd")[..4], [1.0, 0.0, 0.0, 1.0]);
    let mut be = b"ply\nformat binary_big_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar uint vertex_indices\nend_header\n".to_vec();
    for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
        for c in v {
            be.extend(c.to_be_bytes());
        }
    }
    be.push(3);
    for i in [0u32, 1, 2] {
        be.extend(i.to_be_bytes());
    }
    let t = io::read_ply(&be).unwrap();
    assert_eq!(t.p[1], [1.0, 0.0, 0.0]);
    assert_eq!(t.prim(0), [0, 1, 2]);
    let huge =
        "ply\nformat ascii 1.0\nelement vertex 4000000000\nproperty float x\nend_header\n1\n";
    assert!(
        io::read_ply(huge.as_bytes())
            .unwrap_err()
            .contains("too short")
    );
    let bad = "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n3 0 0 7\n";
    assert!(
        io::read_ply(bad.as_bytes())
            .unwrap_err()
            .contains("out of range")
    );
}

#[test]
fn gltf_node_transforms_and_external_buffers() {
    // A triangle in an external .bin, placed by a node translation and a
    // 90° rotation about Z, plus a child scaled by 2.
    let d = dir("gltf");
    let mut bin = Vec::new();
    for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
        for c in v {
            bin.extend(c.to_le_bytes());
        }
    }
    std::fs::write(d.join("tri.bin"), &bin).unwrap();
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let doc = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [
            {"mesh": 0, "translation": [10.0, 0.0, 0.0], "rotation": [0.0, 0.0, s, s], "children": [1]},
            {"mesh": 0, "scale": [2.0, 2.0, 2.0]}
        ],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}],
        "buffers": [{"byteLength": 36, "uri": "tri.bin"}]
    });
    let path = d.join("scene.gltf");
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let m = run(
        "msh.file_in",
        &[("file", json!(path.to_string_lossy()))],
        &[],
    );
    assert_eq!((m.points(), m.prims()), (6, 2));
    let p = |i: usize| m.p[i].map(|x| (x * 1e4).round() / 1e4);
    assert_eq!(p(1), [10.0, 1.0, 0.0], "rotated then moved");
    assert_eq!(p(4), [10.0, 2.0, 0.0], "the child inherits and scales");
    assert!(m.point_attr("N").is_some(), "normals added");
    // Absolute and URL buffers are refused.
    let mut evil = doc.clone();
    evil["buffers"][0]["uri"] = json!("C:/Windows/win.ini");
    assert!(io::read_gltf(&serde_json::to_vec(&evil).unwrap(), Some(&d)).is_err());
    let mut draco = doc.clone();
    draco["extensionsRequired"] = json!(["KHR_draco_mesh_compression"]);
    assert!(
        io::read_gltf(&serde_json::to_vec(&draco).unwrap(), Some(&d))
            .unwrap_err()
            .contains("draco")
    );
}

#[test]
fn file_out_writes_and_file_in_reads_back() {
    let d = dir("ops");
    let m = sample();
    for ext in ["obj", "ply", "gltf", "glb"] {
        let path = d.join(format!("sphere.{ext}"));
        let _ = std::fs::remove_file(&path);
        let passed = run(
            "msh.file_out",
            &[("file", json!(path.to_string_lossy()))],
            &[Some(m.clone())],
        );
        assert_eq!(passed, m, "passes its input through");
        let first = std::fs::read(&path).unwrap();
        run(
            "msh.file_out",
            &[("file", json!(path.to_string_lossy()))],
            &[Some(m.clone())],
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            first,
            "{ext}: byte-identical"
        );
        let back = run(
            "msh.file_in",
            &[("file", json!(path.to_string_lossy()))],
            &[],
        );
        assert_eq!(back.points(), m.points(), "{ext}");
    }
    let err = atd_ops_msh::run(
        "msh.file_in",
        &[("file", json!(d.join("missing.obj").to_string_lossy()))],
        &[],
    );
    assert!(err.is_err());
    let err = atd_ops_msh::run(
        "msh.file_out",
        &[("file", json!(d.join("x.stl").to_string_lossy()))],
        &[Some(m)],
    );
    assert!(err.unwrap_err().contains(".obj"));
}

/// The chain of `examples/mesh-boolean-gltf.atidarla`: Boolean (sphere minus box), Fuse,
/// UV Project, Mesh File Out as GLB. The file is the one checked in Blender
/// (`bench/check-gltf-blender.sh`); here it must be valid, deterministic and read back equal.
#[test]
fn boolean_uv_project_file_out_glb() {
    let ball = make(
        "msh.sphere",
        &[
            ("type", json!("ico")),
            ("level", json!(3)),
            ("radius", json!(0.6)),
        ],
    );
    let cutter = make(
        "msh.box",
        &[
            ("centre", json!([0.35, 0.35, 0.0])),
            ("size", json!([0.7, 0.7, 1.6])),
        ],
    );
    let cut = run(
        "msh.boolean",
        &[("operation", json!("subtract"))],
        &[Some(ball), Some(cutter)],
    );
    let welded = on("msh.fuse", &[], &cut);
    let wrapped = on(
        "msh.uv_project",
        &[("projection", json!("spherical")), ("axis", json!("y"))],
        &welded,
    );
    let path = dir("boolean-glb").join("boolean.glb");
    let _ = std::fs::remove_file(&path);
    let file = [("file", json!(path.to_string_lossy()))];
    run("msh.file_out", &file, &[Some(wrapped.clone())]);
    let first = std::fs::read(&path).unwrap();
    assert_eq!(&first[..4], b"glTF", "binary glTF header");
    run("msh.file_out", &file, &[Some(wrapped.clone())]);
    assert_eq!(std::fs::read(&path).unwrap(), first, "byte-identical");

    let triangles = |m: &Mesh| {
        (0..m.prims())
            .map(|i| m.prim(i).len().saturating_sub(2))
            .sum::<usize>()
    };
    let back = run("msh.file_in", &file, &[]);
    assert!(back.prims() > 0, "has faces");
    assert_eq!(back.prims(), triangles(&back), "read back as triangles");
    assert_eq!(back.prims(), triangles(&wrapped), "no faces lost");
    let uv = attr(&back, "uv");
    assert_eq!(uv.len(), back.points() * 2, "one uv per point");
    assert!(uv.iter().all(|&x| (-1e-6..=1.0 + 1e-6).contains(&x)));
}
