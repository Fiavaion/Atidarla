//! Point File In: a point file (`.atdpc`, PLY, CSV, XYZ/PTS) or a numbered
//! sequence of them, read and uploaded on a worker thread (rule 1).
//!
//! [`Loader`] is the worker: one thread per node, started on first use,
//! that reads the newest requested file (older requests still queued are
//! skipped), uploads its attributes to the GPU from that thread and hands
//! back a GEO value. Live, the node keeps showing what it has until the
//! next file arrives and cooks again until it does; offline it waits, so
//! frame `i` shows file `i`. Sensor In plays its recordings through the
//! same loader.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{CookMode, GeoCtx, GeoOp};
use atd_gpu::geo::{BufferPool, Geometry};

use crate::cache;

struct Request {
    generation: u64,
    path: PathBuf,
    queue: wgpu::Queue,
    pool: BufferPool,
}

struct Reply {
    generation: u64,
    result: Result<Geometry, String>,
    status: String,
}

/// Reads point files on a thread of its own (see the module docs).
#[derive(Default)]
pub(crate) struct Loader {
    tx: Option<Sender<Request>>,
    rx: Option<Receiver<Reply>>,
    generation: u64,
    pending: bool,
    geometry: Geometry,
    error: Option<String>,
    status: String,
}

impl Loader {
    fn start(&mut self) {
        let (tx, requests) = mpsc::channel::<Request>();
        let (replies, rx) = mpsc::channel::<Reply>();
        std::thread::Builder::new()
            .name("atd-pnt-file".into())
            .spawn(move || {
                while let Ok(mut r) = requests.recv() {
                    // Only the newest request matters.
                    while let Ok(newer) = requests.try_recv() {
                        r = newer;
                    }
                    let name = r.path.file_name().map_or_else(
                        || r.path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    let result =
                        cache::read(&r.path).map(|c| c.to_geometry(Some((&r.queue, &r.pool))));
                    let status = match &result {
                        Ok(g) => format!("{} points from {name}", g.points()),
                        Err(_) => String::new(),
                    };
                    let reply = Reply {
                        generation: r.generation,
                        result,
                        status,
                    };
                    if replies.send(reply).is_err() {
                        break;
                    }
                }
            })
            .expect("starting a point-file thread");
        self.tx = Some(tx);
        self.rx = Some(rx);
    }

    /// Asks for `path` (replacing any request still on its way).
    pub fn request(&mut self, ctx: &GeoCtx<'_>, path: PathBuf) {
        if self.tx.is_none() {
            self.start();
        }
        self.generation += 1;
        self.pending = true;
        let request = Request {
            generation: self.generation,
            path,
            queue: ctx.queue.clone(),
            pool: ctx.buffers.clone(),
        };
        if self.tx.as_ref().is_none_or(|tx| tx.send(request).is_err()) {
            self.pending = false;
            self.error = Some("the point-file thread stopped".into());
        }
    }

    /// Takes the newest reply (offline: waits for it). True while a file is
    /// still on its way.
    pub fn poll(&mut self, mode: CookMode) -> bool {
        while self.pending {
            let Some(rx) = &self.rx else { break };
            match mode.receive(rx) {
                Ok(r) if r.generation == self.generation => {
                    self.pending = false;
                    match r.result {
                        Ok(g) => {
                            self.geometry = g;
                            self.error = None;
                            self.status = r.status;
                        }
                        Err(e) => {
                            self.geometry = Geometry::new();
                            self.error = Some(e);
                            self.status.clear();
                        }
                    }
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.pending = false;
                    self.error = Some("the point-file thread stopped".into());
                }
            }
        }
        self.pending
    }

    /// Forgets the file (empty geometry, no error).
    pub fn clear(&mut self) {
        self.generation += 1;
        self.pending = false;
        self.geometry = Geometry::new();
        self.error = None;
        self.status.clear();
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "file_in",
        "Point File In",
        "Reads a point cloud from a file: Atidarla's point cache (.atdpc), PLY (ASCII or binary; the vertex element), CSV with a header row, or XYZ/PTS text. `x y z` become `P`, `nx ny nz` `N`, colour channels `Cd` (0-1; 8- and 16-bit PLY colours are scaled), `u v` `uv`, `id` stays the point identity and every other column becomes a float attribute (at most 16). A `$F` in the file name (`$F4`: four digits, zero-padded) plays a numbered sequence by Frame. Files are read and uploaded on a worker thread; live, the last file stays until the next arrives. Convert other caches with `atidarla points convert` (docs/point-caches.md).",
        &[],
        vec![
            ParamSchema::path("file", "File").help("The point file, or a sequence with `$F` / `$F4` for the frame number (relative paths are relative to the project folder)."),
            ParamSchema::int("frame", "Frame", 1, None, None).help("The frame `$F` stands for; drive it with an expression such as `frame + 1` to play a sequence."),
        ],
        100,
    )
}

#[derive(Default)]
pub struct FileIn {
    loader: Loader,
    /// The parameters of the last request: file text and frame.
    file: String,
    frame: i64,
}

pub fn create(_device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(FileIn::default())
}

impl GeoOp for FileIn {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let file = p.str("file");
        let frame = p.i64("frame");
        let changed = file != self.file || (file.contains("$F") && frame != self.frame);
        if changed {
            self.file.clear();
            self.file.push_str(file);
            self.frame = frame;
            if file.is_empty() {
                self.loader.clear();
            } else {
                let path = p.resolve(&cache::frame_path(file, frame));
                self.loader.request(ctx, path);
            }
        }
        if self.loader.poll(p.mode()) {
            ctx.cook_again = true;
        }
        if let Some(e) = self.loader.error() {
            return Err(e.to_owned());
        }
        // A clone shares the file's attributes (no copy).
        *ctx.output = self.loader.geometry().clone();
        Ok(())
    }

    fn status(&self) -> Option<&str> {
        self.loader.status()
    }
}
