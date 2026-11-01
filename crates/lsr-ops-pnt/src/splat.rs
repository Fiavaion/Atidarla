//! `pnt.splat_in` (FR-C1): Gaussian splats from a PLY, SPZ or SOG file
//! ([`crate::splat_file`]) as a GEO value, for Splat MAT (`mat.splat`) and
//! `img.render`.
//!
//! The file is read, decoded and uploaded on a worker thread; the render
//! thread only swaps the finished value in (rule 1). Live, the previous
//! cloud stays on the wire while a new file loads; offline (`lasair
//! render`), the cook waits so every frame has the file.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp};
use lsr_gpu::geo::{BufferPool, Geometry};

use crate::splat_file;

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "splat_in",
        "Splat In",
        "Reads Gaussian splats (a 3D Gaussian Splatting capture) from a .ply in the standard 3DGS layout, an .spz, or a .sog archive or SOG folder (its meta.json), on a worker thread. Each splat is a point with `P`, `Cd` (base colour, opacity in alpha), `scale` (per-axis size), `orient` (rotation) and, up to degree 3, the spherical-harmonic coefficients `sh0`… that give view-dependent colour. Draw it with Splat MAT in Render.",
        &[],
        vec![
            ParamSchema::path("file", "File").help(
                "A .ply, .spz or .sog file, or a SOG folder's meta.json (relative paths are relative to the project folder).",
            ),
            ParamSchema::toggle("y_down", "Y Down Source", true).help(
                "PLY and SOG captures are stored in the trainer's camera frame (Y down, Z forward): turn them a half turn about X so they stand upright here. SPZ files are stored Y up already and are never turned. Off: PLY and SOG coordinates as stored.",
            ),
        ],
        // The render thread only swaps in a finished load.
        50,
    )
}

pub fn create(_: &wgpu::Device) -> Box<dyn GeoOp> {
    let (tx, rx) = mpsc::channel();
    Box::new(SplatIn {
        key: None,
        generation: 0,
        running: None,
        tx,
        rx,
        upload: None,
        error: None,
        status: String::new(),
    })
}

/// A finished load.
struct Done {
    generation: u64,
    result: Result<Geometry, String>,
    ms: f64,
    count: usize,
    degree: u8,
}

struct SplatIn {
    /// The file text and flip of the newest load.
    key: Option<(String, bool)>,
    generation: u64,
    /// The generation loading.
    running: Option<u64>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    upload: Option<(wgpu::Queue, BufferPool)>,
    error: Option<String>,
    status: String,
}

impl SplatIn {
    fn start(&mut self, path: PathBuf, flip: bool) {
        let Some((queue, pool)) = self.upload.clone() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.running = Some(generation);
        self.status = "loading…".into();
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new()
            .name("lsr-splat-in".into())
            .spawn(move || {
                let t0 = Instant::now();
                let (mut count, mut degree) = (0, 0);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut cloud = splat_file::read(&path)?;
                    let format = splat_file::Format::from_path(&path);
                    if flip && format != Some(splat_file::Format::Spz) {
                        cloud.flip_y_down();
                    }
                    count = cloud.len();
                    degree = cloud.sh_degree;
                    Ok(cloud.to_geometry(Some((&queue, &pool))))
                }))
                .unwrap_or_else(|_| {
                    Err("reading the splat file failed unexpectedly (a bug)".into())
                });
                let _ = tx.send(Done {
                    generation,
                    result,
                    ms: t0.elapsed().as_secs_f64() * 1e3,
                    count,
                    degree,
                });
            });
        if let Err(e) = spawned {
            self.running = None;
            self.error = Some(format!("starting the loader: {e}"));
        }
    }

    fn handle(&mut self, done: Done, out: &mut Geometry) {
        if self.running == Some(done.generation) {
            self.running = None;
        }
        // Only the newest load is shown.
        if done.generation != self.generation {
            return;
        }
        match done.result {
            Ok(g) => {
                *out = g;
                self.error = None;
                self.status = format!(
                    "{} splats, SH degree {} (loaded in {:.0} ms)",
                    done.count, done.degree, done.ms
                );
            }
            Err(e) => {
                out.clear();
                self.error = Some(e);
                self.status.clear();
            }
        }
    }
}

impl GeoOp for SplatIn {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let mode = ctx.params.mode();
        if self.upload.is_none() {
            self.upload = Some((ctx.queue.clone(), ctx.buffers.clone()));
        }
        let file = ctx.params.str("file");
        let flip = ctx.params.bool("y_down");
        let changed = self
            .key
            .as_ref()
            .is_none_or(|(f, y)| f != file || *y != flip);
        if changed {
            self.key = Some((file.to_owned(), flip));
            if file.is_empty() {
                self.generation += 1;
                self.running = None;
                self.error = None;
                self.status.clear();
                ctx.output.clear();
            } else {
                let path = ctx.params.resolve(file);
                self.start(path, flip);
            }
        }
        while self.running.is_some() {
            match mode.receive(&self.rx) {
                Ok(done) => self.handle(done, ctx.output),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("the splat loader stopped".into());
                }
            }
        }
        ctx.cook_again = self.running.is_some();
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}
