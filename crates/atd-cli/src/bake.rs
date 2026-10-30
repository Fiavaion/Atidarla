//! `atidarla bake`: write one node's output to disk with the offline render
//! engine (M2). A GEO/MSH node becomes mesh files, an IMG node a PNG
//! sequence; the difference from `render` is in `docs/render.md`.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use atd_core::registry::WireType;
use atd_ops_msh::io::{self, Format};

use crate::render::{RenderOptions, Renderer, frame_name};

/// Options of `atidarla bake`.
#[derive(Debug, Clone)]
pub struct BakeOptions {
    pub range: Range<u64>,
    pub fps: f64,
    pub node: Option<String>,
    /// Mesh file format; default PLY (keeps every attribute).
    pub format: Format,
    /// Always write `mesh_000000.ply`, ... even when every frame is the same.
    pub sequence: bool,
    pub allow_python_ops: bool,
}

/// `mesh.ply`, or `mesh_000012.ply` for frame 12.
fn mesh_name(i: Option<u64>, format: Format) -> String {
    let ext = match format {
        Format::Obj => "obj",
        Format::Ply => "ply",
        Format::Gltf => "gltf",
        Format::Glb => "glb",
    };
    match i {
        Some(i) => format!("mesh_{i:06}.{ext}"),
        None => format!("mesh.{ext}"),
    }
}

pub fn run(project: &Path, out: &Path, opts: &BakeOptions) -> Result<()> {
    let gpu = atd_gpu::GpuContext::headless().context("no GPU adapter for baking")?;
    let mut r = Renderer::new(gpu.device.clone(), gpu.queue.clone(), gpu.platform_info())?;
    let render_opts = RenderOptions {
        range: opts.range.clone(),
        fps: opts.fps,
        node: opts.node.clone(),
        size: None,
        allow_python_ops: opts.allow_python_ops,
    };
    r.load_as(project, &render_opts, &[WireType::Geo, WireType::Img])?;
    for w in r.take_warnings() {
        eprintln!("warning: {w}");
    }
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let start = Instant::now();
    let summary = match r.output_kind() {
        WireType::Img => {
            for i in opts.range.clone() {
                let frame = r.frame(i)?;
                frame.write_png(&out.join(frame_name(i)))?;
            }
            format!("{} PNG frames", opts.range.end - opts.range.start)
        }
        _ => bake_meshes(&mut r, out, opts)?,
    };
    for w in r.take_warnings() {
        eprintln!("warning: {w}");
    }
    println!(
        "baked {} frames {}-{} (end exclusive): {summary}, to {} in {:.1} s",
        r.output_name(),
        opts.range.start,
        opts.range.end,
        out.display(),
        start.elapsed().as_secs_f64(),
    );
    Ok(())
}

/// Cooks each frame and writes its mesh. If every frame of the range gives
/// the same bytes (a static network) one file is written instead of one per
/// frame, unless `--sequence`.
fn bake_meshes(r: &mut Renderer, out: &Path, opts: &BakeOptions) -> Result<String> {
    let write = |i: Option<u64>, bytes: &[u8]| -> Result<PathBuf> {
        let path = out.join(mesh_name(i, opts.format));
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    };
    let mut first: Option<Vec<u8>> = None;
    let mut same = !opts.sequence;
    let mut files = 0u64;
    for i in opts.range.clone() {
        let mesh = r.mesh(i)?;
        let bytes = io::write(&mesh, opts.format).map_err(|e| anyhow::anyhow!("frame {i}: {e}"))?;
        if same {
            match &first {
                None => {
                    first = Some(bytes);
                    continue;
                }
                Some(f) if *f == bytes => continue,
                Some(f) => {
                    // The first frame that differs: the earlier ones, all
                    // equal to the first, go to their own files now.
                    same = false;
                    for j in opts.range.start..i {
                        write(Some(j), f)?;
                        files += 1;
                    }
                }
            }
        }
        write(Some(i), &bytes)?;
        files += 1;
    }
    match first {
        Some(bytes) if same => {
            write(None, &bytes)?;
            Ok(format!(
                "static: one mesh file {}",
                mesh_name(None, opts.format)
            ))
        }
        _ if files == 0 => bail!("the range is empty: nothing to bake"),
        _ => Ok(format!("{files} mesh files")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples")
            .join(name)
    }

    fn out_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atd-bake-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn opts(range: Range<u64>, node: Option<&str>) -> BakeOptions {
        BakeOptions {
            range,
            fps: 60.0,
            node: node.map(str::to_owned),
            format: Format::Ply,
            sequence: false,
            allow_python_ops: false,
        }
    }

    #[test]
    fn names() {
        assert_eq!(mesh_name(Some(12), Format::Obj), "mesh_000012.obj");
        assert_eq!(mesh_name(None, Format::Glb), "mesh.glb");
    }

    /// A static mesh network bakes to one file the mesh reader reads back,
    /// and `--sequence` writes one per frame with the same content.
    #[test]
    fn static_mesh_network_bakes_to_one_readable_file() {
        if atd_gpu::GpuContext::headless().is_err() {
            eprintln!("no GPU adapter: skipped");
            return;
        }
        let out = out_dir("static");
        let project = example("mesh-primitives.atidarla");
        // `ball` is a sphere with fixed parameters.
        run(&project, &out, &opts(0..3, Some("ball"))).unwrap();
        let one = out.join("mesh.ply");
        assert!(
            one.is_file(),
            "{:?}",
            std::fs::read_dir(&out).unwrap().collect::<Vec<_>>()
        );
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
        let mesh = io::read(&one, io::ObjPoints::Split).unwrap();
        assert!(mesh.p.len() > 100 && !mesh.start.is_empty());

        let seq = out_dir("seq");
        let mut o = opts(0..3, Some("ball"));
        o.sequence = true;
        run(&project, &seq, &o).unwrap();
        for i in 0..3 {
            let m = io::read(
                &seq.join(mesh_name(Some(i), Format::Ply)),
                io::ObjPoints::Split,
            )
            .unwrap();
            assert_eq!(m.p, mesh.p);
        }
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::remove_dir_all(&seq);
    }

    /// An animated mesh network bakes to one file per frame, and they differ.
    #[test]
    fn animated_mesh_network_bakes_one_file_per_frame() {
        if atd_gpu::GpuContext::headless().is_err() {
            eprintln!("no GPU adapter: skipped");
            return;
        }
        let out = out_dir("anim");
        run(
            &example("mesh-primitives.atidarla"),
            &out,
            &opts(0..3, Some("scene")),
        )
        .unwrap();
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3);
        let read = |i| {
            io::read(
                &out.join(mesh_name(Some(i), Format::Ply)),
                io::ObjPoints::Split,
            )
            .unwrap()
        };
        assert_ne!(read(0).p, read(2).p);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// An IMG node bakes to a PNG sequence like `render`.
    #[test]
    fn image_node_bakes_png_frames() {
        if atd_gpu::GpuContext::headless().is_err() {
            eprintln!("no GPU adapter: skipped");
            return;
        }
        let out = out_dir("img");
        run(
            &example("image-processing.atidarla"),
            &out,
            &opts(2..4, None),
        )
        .unwrap();
        assert!(out.join("frame_000002.png").is_file());
        assert!(out.join("frame_000003.png").is_file());
        let _ = std::fs::remove_dir_all(&out);
    }
}
