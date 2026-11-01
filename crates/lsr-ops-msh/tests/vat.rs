//! VAT In on small synthetic exports made here (no third-party sample
//! files): soft, rigid, fluid (with and without a lookup texture) and
//! sprite playback, 8- and 16-bit PNG and EXR textures, frame blending,
//! looping, data-file parsing, glTF `TEXCOORD_1` texels, and the example
//! project's files, which this generator wrote.

// Points and frames are walked by index across parallel arrays.
#![allow(clippy::needless_range_loop, clippy::type_complexity)]

mod common;

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};

use common::*;
use lsr_ops_msh::Mesh;
use lsr_ops_msh::vat::read_data;
use serde_json::{Value, json};

type Px = [f32; 4];

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lsr-vat-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png_bytes(w: usize, h: usize, px: &[Px], sixteen: bool) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w as u32, h as u32);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(if sixteen {
            png::BitDepth::Sixteen
        } else {
            png::BitDepth::Eight
        });
        let mut wr = e.write_header().unwrap();
        let data: Vec<u8> = px
            .iter()
            .flatten()
            .flat_map(|&v| {
                let v = v.clamp(0.0, 1.0);
                if sixteen {
                    ((v * 65535.0).round() as u16).to_be_bytes().to_vec()
                } else {
                    vec![(v * 255.0).round() as u8]
                }
            })
            .collect();
        wr.write_image_data(&data).unwrap();
    }
    out
}

fn write_exr(path: &Path, w: usize, h: usize, px: &[Px]) {
    use exr::prelude::{Image, SpecificChannels, Vec2, WritableImage};
    // One thread, so the file's bytes are the same every time.
    let channels = SpecificChannels::rgba(|Vec2(x, y): Vec2<usize>| {
        let p = px[y * w + x];
        (p[0], p[1], p[2], p[3])
    });
    Image::from_channels((w, h), channels)
        .write()
        .non_parallel()
        .to_file(path)
        .unwrap();
}

fn obj(points: &[[f32; 3]], faces: &[Vec<u32>]) -> String {
    let mut s = String::from("# Synthetic VAT mesh (Lasair tests)\n");
    for p in points {
        s += &format!("v {} {} {}\n", p[0], p[1], p[2]);
    }
    for f in faces {
        s += "f";
        for i in f {
            s += &format!(" {}", i + 1);
        }
        s += "\n";
    }
    s
}

fn data_json(frames: usize, bounds: Option<([f64; 3], [f64; 3])>) -> String {
    let mut s = format!("[\n  {{\n    \"_frameCount\": {frames},\n    \"_houdiniFPS\": 24");
    if let Some((lo, hi)) = bounds {
        for (k, a) in ["X", "Y", "Z"].iter().enumerate() {
            s += &format!(",\n    \"_boundingMin{a}\": {}", lo[k]);
        }
        for (k, a) in ["X", "Y", "Z"].iter().enumerate() {
            s += &format!(",\n    \"_boundingMax{a}\": {}", hi[k]);
        }
    }
    s + "\n  }\n]\n"
}

/// The soft-body ripple: an 8 × 8 grid, 32 texels a row (2 rows a frame),
/// 24 frames; positions in a 16-bit PNG across the data file's bounds.
/// Returns the files and the exact positions per frame.
pub fn ripple() -> (Vec<(&'static str, Vec<u8>)>, Vec<Vec<[f64; 3]>>) {
    let (n, w, rows, frames) = (8usize, 32usize, 2usize, 24usize);
    let rest: Vec<[f32; 3]> = (0..n * n)
        .map(|k| {
            let (i, j) = (k % n, k / n);
            [
                -1.0 + 2.0 * i as f32 / (n - 1) as f32,
                0.0,
                -1.0 + 2.0 * j as f32 / (n - 1) as f32,
            ]
        })
        .collect();
    let mut faces = Vec::new();
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            let k = (j * n + i) as u32;
            faces.push(vec![k, k + n as u32, k + n as u32 + 1, k + 1]);
        }
    }
    let at: Vec<Vec<[f64; 3]>> = (0..frames)
        .map(|f| {
            rest.iter()
                .map(|p| {
                    let (x, z) = (f64::from(p[0]), f64::from(p[2]));
                    let ph = TAU * f as f64 / frames as f64;
                    [x, 0.25 * (ph + 2.0 * x).sin() * (1.5 * z).cos(), z]
                })
                .collect()
        })
        .collect();
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in at.iter().flatten() {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let mut px = vec![[0.0f32; 4]; w * rows * frames];
    for (f, ps) in at.iter().enumerate() {
        for (i, p) in ps.iter().enumerate() {
            let v: [f32; 3] =
                std::array::from_fn(|k| ((p[k] - lo[k]) / (hi[k] - lo[k]).max(1e-12)) as f32);
            px[(f * rows + i / w) * w + i % w] = [v[0], v[1], v[2], 1.0];
        }
    }
    (
        vec![
            ("ripple_mesh.obj", obj(&rest, &faces).into_bytes()),
            ("ripple_pos.png", png_bytes(w, rows * frames, &px, true)),
            (
                "ripple_data.json",
                data_json(frames, Some((lo, hi))).into_bytes(),
            ),
        ],
        at,
    )
}

/// Quaternion (x, y, z, w) turning `a` radians about unit `axis`.
fn quat(axis: [f64; 3], a: f64) -> [f64; 4] {
    let s = (a / 2.0).sin();
    [axis[0] * s, axis[1] * s, axis[2] * s, (a / 2.0).cos()]
}

fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let u = [q[0], q[1], q[2]];
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let t = cross(u, v).map(|x| 2.0 * x);
    let c = cross(u, t);
    std::array::from_fn(|k| v[k] + q[3] * t[k] + c[k])
}

/// Rigid tumble: four cubes, one texel each, 24 frames; pivots and
/// rotations in EXR, colours in an 8-bit PNG. Returns the files, the
/// pivots and quaternions per frame and piece.
pub fn tumble(dir: &Path) -> (Vec<(&'static str, Vec<u8>)>, Vec<Vec<([f64; 3], [f64; 4])>>) {
    let (pieces, frames) = (4usize, 24usize);
    let mut pts = Vec::new();
    let mut faces = Vec::new();
    for k in 0..pieces {
        let cx = -0.9 + 0.6 * k as f32;
        let b = pts.len() as u32;
        for c in 0..8 {
            let s = |bit: usize| if c & bit == 0 { -0.15 } else { 0.15 };
            pts.push([cx + s(1), s(2), s(4)]);
        }
        for f in [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ] {
            faces.push(f.iter().map(|&i| b + i).collect());
        }
    }
    let motion: Vec<Vec<([f64; 3], [f64; 4])>> = (0..frames)
        .map(|f| {
            (0..pieces)
                .map(|k| {
                    let t = TAU * f as f64 / frames as f64;
                    let pivot = [-0.9 + 0.6 * k as f64, 0.3 * (t + k as f64).sin(), 0.0];
                    let axis = [
                        [1.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0],
                        [0.0, 0.0, 1.0],
                        [0.6, 0.8, 0.0],
                    ][k];
                    (pivot, quat(axis, t))
                })
                .collect()
        })
        .collect();
    let pos: Vec<Px> = motion
        .iter()
        .flatten()
        .map(|(p, _)| [p[0] as f32, p[1] as f32, p[2] as f32, 1.0])
        .collect();
    let rot: Vec<Px> = motion
        .iter()
        .flatten()
        .map(|(_, q)| q.map(|v| v as f32))
        .collect();
    let col: Vec<Px> = (0..frames * pieces)
        .map(|i| {
            let (f, k) = (i / pieces, i % pieces);
            let t = f as f32 / frames as f32;
            [0.25 * k as f32, t, 1.0 - t, 1.0]
        })
        .collect();
    write_exr(&dir.join("tumble_pos.exr"), pieces, frames, &pos);
    write_exr(&dir.join("tumble_rot.exr"), pieces, frames, &rot);
    let read = |n: &str| std::fs::read(dir.join(n)).unwrap();
    (
        vec![
            ("tumble_mesh.obj", obj(&pts, &faces).into_bytes()),
            ("tumble_pos.exr", read("tumble_pos.exr")),
            ("tumble_rot.exr", read("tumble_rot.exr")),
            ("tumble_col.png", png_bytes(pieces, frames, &col, false)),
            ("tumble_data.json", data_json(frames, None).into_bytes()),
        ],
        motion,
    )
}

fn save(dir: &Path, files: &[(&str, Vec<u8>)]) {
    for (n, b) in files {
        std::fs::write(dir.join(n), b).unwrap();
    }
}

fn vat(d: &Path, mode: &str, base: &str, extra: &[(&str, Value)]) -> Mesh {
    let p = |s: &str| json!(d.join(s).to_string_lossy());
    // The first value of a name wins: the extras go first.
    let mut params: Vec<(&str, Value)> = extra.to_vec();
    params.extend([
        ("mode", json!(mode)),
        ("mesh", p(&format!("{base}_mesh.obj"))),
        (
            "position",
            p(&format!(
                "{base}_pos.{}",
                if base == "ripple" { "png" } else { "exr" }
            )),
        ),
        ("data", p(&format!("{base}_data.json"))),
    ]);
    run("msh.vat_in", &params, &[])
}

fn near(a: [f32; 3], b: [f64; 3], tol: f64) -> bool {
    (0..3).all(|k| (f64::from(a[k]) - b[k]).abs() <= tol)
}

#[test]
fn soft_body_plays_blends_and_loops() {
    let d = dir("soft");
    let (files, at) = ripple();
    save(&d, &files);
    // 16-bit across a 0.5-wide range: within 1e-5.
    let tol = 2e-5;
    for f in [0usize, 7, 23] {
        let m = vat(&d, "soft", "ripple", &[("frame", json!(f))]);
        assert_eq!(m.points(), 64);
        assert_eq!(m.prims(), 49);
        for i in 0..64 {
            assert!(near(m.p[i], at[f][i], tol), "frame {f} point {i}");
        }
        assert!(unit_normals(&m, 1e-4));
    }
    // Between frames 3 and 4.
    let m = vat(&d, "soft", "ripple", &[("frame", json!(3.25))]);
    for i in 0..64 {
        let want: [f64; 3] = std::array::from_fn(|k| at[3][i][k] * 0.75 + at[4][i][k] * 0.25);
        assert!(near(m.p[i], want, tol), "blend at point {i}");
    }
    // Looping: frame 24 is frame 0; 23.5 blends 23 and 0. Without it,
    // frames past the end hold the last.
    assert_eq!(
        vat(&d, "soft", "ripple", &[("frame", json!(24.0))]).p,
        vat(&d, "soft", "ripple", &[("frame", json!(0.0))]).p
    );
    let m = vat(&d, "soft", "ripple", &[("frame", json!(23.5))]);
    let want: [f64; 3] = std::array::from_fn(|k| (at[23][5][k] + at[0][5][k]) / 2.0);
    assert!(near(m.p[5], want, tol));
    let held = vat(
        &d,
        "soft",
        "ripple",
        &[("frame", json!(40.0)), ("loop", json!(false))],
    );
    assert!(near(held.p[9], at[23][9], tol));
    // Without interpolation the frame is floored.
    let step = vat(
        &d,
        "soft",
        "ripple",
        &[("frame", json!(3.9)), ("interpolate", json!(false))],
    );
    assert!(near(step.p[9], at[3][9], tol));
    // Offsets: the same texture added to the rest grid (rest y is 0, so
    // only x and z double).
    let off = vat(&d, "soft", "ripple", &[("positions", json!("offset"))]);
    assert!(near(
        off.p[9],
        [at[0][9][0] * 2.0, at[0][9][1], at[0][9][2] * 2.0],
        tol
    ));
}

#[test]
fn rigid_pieces_move_and_turn_as_wholes() {
    let d = dir("rigid");
    let (files, motion) = tumble(&d);
    save(&d, &files);
    let rest = vat(&d, "rigid", "tumble", &[("position", json!(""))]);
    assert_eq!((rest.points(), rest.prims()), (32, 24));
    for f in [0usize, 5, 17] {
        let m = vat(
            &d,
            "rigid",
            "tumble",
            &[
                ("frame", json!(f)),
                (
                    "rotation",
                    json!(d.join("tumble_rot.exr").to_string_lossy()),
                ),
                ("colour", json!(d.join("tumble_col.png").to_string_lossy())),
            ],
        );
        for i in 0..32 {
            let k = i / 8;
            let (pivot, q) = motion[f][k];
            let centre = [-0.9 + 0.6 * k as f64, 0.0, 0.0];
            let local: [f64; 3] = std::array::from_fn(|c| f64::from(rest.p[i][c]) - centre[c]);
            let r = rotate(q, local);
            let want: [f64; 3] = std::array::from_fn(|c| pivot[c] + r[c]);
            assert!(
                near(m.p[i], want, 1e-5),
                "frame {f} point {i}: {:?} vs {want:?}",
                m.p[i]
            );
        }
        // Colour per piece and frame from the 8-bit PNG.
        let cd = m.point_attr("Cd").unwrap().floats().unwrap();
        let t = f as f32 / 24.0;
        assert_eq!(m.point_attr("Cd").unwrap().comps, 4);
        assert!((cd[4 * 8 * 2] - 0.5).abs() < 0.003, "piece 2 red");
        assert!((cd[1] - t).abs() < 0.003);
        assert_eq!(cd[3], 1.0, "alpha");
        // Pieces stay rigid: a cube's diagonal keeps its length.
        let diag = |m: &Mesh| {
            (0..3)
                .map(|c| (m.p[7][c] - m.p[0][c]).powi(2))
                .sum::<f32>()
                .sqrt()
        };
        assert!((diag(&m) - diag(&rest)).abs() < 1e-5);
    }
    // Without a rotation texture, pieces only move.
    let m = vat(&d, "rigid", "tumble", &[("frame", json!(6))]);
    let (pivot, _) = motion[6][1];
    assert!(near(
        m.p[8],
        [pivot[0] - 0.15, pivot[1] - 0.15, -0.15],
        1e-5
    ));
}

#[test]
fn fluid_topology_changes_per_frame() {
    let d = dir("fluid");
    // A pool of two triangles; frame 1 collapses the second to a point.
    let rest = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [2.0, 0.0, 0.0],
        [3.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
    ];
    std::fs::write(
        d.join("pool_mesh.obj"),
        obj(&rest, &[vec![0, 1, 2], vec![3, 4, 5]]),
    )
    .unwrap();
    let mut px: Vec<Px> = rest.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    px.extend(
        [
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [0.0, 2.0, 1.0],
            [5.0; 3],
            [5.0; 3],
            [5.0; 3],
        ]
        .iter()
        .map(|p| [p[0], p[1], p[2], 1.0]),
    );
    write_exr(&d.join("pool_pos.exr"), 6, 2, &px);
    std::fs::write(d.join("pool_data.json"), data_json(2, None)).unwrap();
    let f0 = vat(&d, "fluid", "pool", &[("frame", json!(0))]);
    assert_eq!((f0.points(), f0.prims()), (6, 2));
    let f1 = vat(&d, "fluid", "pool", &[("frame", json!(1.5))]);
    assert_eq!(
        (f1.points(), f1.prims()),
        (3, 1),
        "collapsed triangle gone, frames not blended"
    );
    assert_eq!(f1.p[2], [0.0, 2.0, 1.0]);
    // With a lookup texture: frame 0 shows the second triangle's texels for
    // the first triangle's points; the second's points are unused.
    let w = 6.0;
    let at = |x: f32, y: f32| [(x + 0.5) / w, 1.0 - (y + 0.5) / 2.0, 0.0, 1.0];
    let mut lk: Vec<Px> = (3..6).map(|x| at(x as f32, 0.0)).collect();
    lk.extend([[0.0; 4]; 3]);
    lk.extend((0..6).map(|x| at(x as f32, 1.0)));
    write_exr(&d.join("pool_lookup.exr"), 6, 2, &lk);
    let l0 = vat(
        &d,
        "fluid",
        "pool",
        &[("lookup", json!(d.join("pool_lookup.exr").to_string_lossy()))],
    );
    assert_eq!(l0.prims(), 1);
    assert_eq!(l0.p, [rest[3], rest[4], rest[5]]);
}

#[test]
fn sprites_follow_their_particles() {
    let d = dir("sprite");
    let q = |cx: f32| {
        [
            [cx - 0.1, -0.1, 0.0],
            [cx + 0.1, -0.1, 0.0],
            [cx + 0.1, 0.1, 0.0],
            [cx - 0.1, 0.1, 0.0],
        ]
    };
    let mut pts = q(0.0).to_vec();
    pts.extend(q(1.0));
    std::fs::write(
        d.join("dots_mesh.obj"),
        obj(&pts, &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]]),
    )
    .unwrap();
    let px: Vec<Px> = vec![[0.0, 1.0, 0.0, 1.0], [2.0, 3.0, 0.0, 1.0]];
    write_exr(&d.join("dots_pos.exr"), 2, 1, &px);
    std::fs::write(d.join("dots_data.json"), data_json(1, None)).unwrap();
    let m = vat(&d, "sprite", "dots", &[]);
    assert!(near(m.p[0], [-0.1, 0.9, 0.0], 1e-6));
    assert!(near(m.p[6], [2.1, 3.1, 0.0], 1e-6));
}

#[test]
fn gltf_texel_coordinates_choose_the_texels() {
    let d = dir("uv2");
    let (files, at) = ripple();
    save(&d, &files);
    // The grid with its texels given by uv2, in reverse order of points.
    let mut g = run(
        "msh.vat_in",
        &[("mesh", json!(d.join("ripple_mesh.obj").to_string_lossy()))],
        &[],
    );
    let (w, h) = (32.0f32, 48.0f32);
    let uv: Vec<f32> = (0..64)
        .flat_map(|i| {
            let t = 63 - i;
            [
                ((t % 32) as f32 + 0.5) / w,
                1.0 - ((t / 32) as f32 + 0.5) / h,
            ]
        })
        .collect();
    g.set_point_attr(lsr_ops_msh::Column::f32("uv", 2, uv));
    let bytes = lsr_ops_msh::io::write(&g, lsr_ops_msh::io::Format::Gltf).unwrap();
    let text = String::from_utf8(bytes)
        .unwrap()
        .replace("TEXCOORD_0", "TEXCOORD_1");
    std::fs::write(d.join("ripple_mesh.gltf"), text).unwrap();
    let m = run(
        "msh.vat_in",
        &[
            ("mesh", json!(d.join("ripple_mesh.gltf").to_string_lossy())),
            (
                "position",
                json!(d.join("ripple_pos.png").to_string_lossy()),
            ),
            ("data", json!(d.join("ripple_data.json").to_string_lossy())),
            ("frame", json!(2)),
        ],
        &[],
    );
    assert!(m.point_attr("uv2").is_some(), "TEXCOORD_1 read as uv2");
    for i in 0..64 {
        assert!(near(m.p[i], at[2][63 - i], 2e-5), "point {i}");
    }
}

#[test]
fn data_files_and_errors() {
    let a = read_data(r#"[{"_frameCount": 12, "_boundingMinX": -1, "_boundingMinY": -2, "_boundingMinZ": -3, "_boundingMaxX": 1, "_boundingMaxY": 2, "_boundingMaxZ": 3, "_houdiniFPS": 30}]"#).unwrap();
    assert_eq!(a.frames, Some(12));
    assert_eq!(a.min, Some([-1.0, -2.0, -3.0]));
    assert_eq!(a.max, Some([1.0, 2.0, 3.0]));
    assert_eq!(a.fps, Some(30.0));
    let b =
        read_data(r#"{"Frame Count": 5, "Bound Min": [0, 0, 0], "Bound Max": [1, 1, 1]}"#).unwrap();
    assert_eq!((b.frames, b.max), (Some(5), Some([1.0; 3])));
    assert!(read_data("[1, 2]").is_err());
    assert!(read_data("{").is_err());
    let d = dir("errors");
    let (files, _) = ripple();
    save(&d, &files);
    let err = |extra: &[(&str, Value)]| {
        let p = |s: &str| json!(d.join(s).to_string_lossy());
        // The first value of a name wins: the extras go first.
        let mut params: Vec<(&str, Value)> = extra.to_vec();
        params.extend([
            ("mesh", p("ripple_mesh.obj")),
            ("position", p("ripple_pos.png")),
        ]);
        lsr_ops_msh::run("msh.vat_in", &params, &[]).unwrap_err()
    };
    assert!(err(&[]).contains("bounds"), "a PNG needs the bounds");
    let e = err(&[
        ("data", json!(d.join("ripple_data.json").to_string_lossy())),
        ("frames", json!(30)),
    ]);
    assert!(e.contains("rows"), "{e}");
    let e = err(&[(
        "position",
        json!(d.join("ripple_mesh.obj").to_string_lossy()),
    )]);
    assert!(e.contains(".exr or .png"), "{e}");
    // No mesh: nothing; no position texture: the rest mesh.
    assert_eq!(run("msh.vat_in", &[], &[]).points(), 0);
}

/// `examples/mesh-vat.lasair/vat/` holds exactly what these generators
/// write (set `LSR_WRITE_VAT_EXAMPLE=1` to rewrite it).
#[test]
fn example_files_are_the_generators_output() {
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/mesh-vat.lasair/vat");
    let tmp = dir("example");
    let (mut files, _) = ripple();
    files.extend(tumble(&tmp).0);
    if std::env::var_os("LSR_WRITE_VAT_EXAMPLE").is_some() {
        std::fs::create_dir_all(&ex).unwrap();
        save(&ex, &files);
    }
    for (name, bytes) in &files {
        let have = std::fs::read(ex.join(name)).unwrap_or_default();
        assert!(&have == bytes, "{name} differs from the generator's output");
    }
}
