//! WGSL (PRD §6, IMG render & code): a full-screen shader written by the
//! user, inline or in a file that reloads when saved.
//!
//! Compilation (naga validation, then the driver) runs on a worker thread,
//! and a file is watched by its own thread, so neither touches the render
//! thread (rule 1). The last shader that compiled keeps running while a new
//! one compiles, and when it fails, so live coding never blanks the output.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;

use crate::stateful::clear;

const DEFAULT: &str = "return textureSample(in0, samp, uv);";

/// Names the user's code can call.
const HELPERS: &str = r#"
fn p1() -> vec4f { return params.v[0]; }
fn p2() -> vec4f { return params.v[1]; }
fn p3() -> vec4f { return params.v[2]; }
fn p4() -> vec4f { return params.v[3]; }
fn time() -> f32 { return params.v[4].x; }
fn resolution() -> vec2f { return params.v[4].yz; }
fn frame() -> f32 { return params.v[4].w; }
"#;

pub fn schema() -> OperatorSchema {
    let input = |name: &str| SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: "img.wgsl".into(),
        family: OpFamily::Img,
        label: "WGSL".into(),
        summary: "A full-screen shader you write in WGSL: the body of `fn shade(uv: vec2f) -> vec4f` (uv from the top-left). Inputs are `in0` and `in1` with sampler `samp`; `p1()`…`p4()` return the four parameters, and `time()`, `resolution()` and `frame()` are available. A file reloads whenever it is saved; the last working shader keeps running while you edit.".into(),
        version: 1,
        params: vec![
            ParamSchema::string("code", "Code", DEFAULT)
                .help("The shader body, used when File is empty.")
                .multiline(),
            ParamSchema::string("file", "File", "")
                .help("Path to a file holding the shader body; reloads when it changes."),
            ParamSchema::vec("p1", "P1", &[0.0, 0.0, 0.0, 0.0]).help("Returned by `p1()`."),
            ParamSchema::vec("p2", "P2", &[0.0, 0.0, 0.0, 0.0]).help("Returned by `p2()`."),
            ParamSchema::vec("p3", "P3", &[0.0, 0.0, 0.0, 0.0]).help("Returned by `p3()`."),
            ParamSchema::vec("p4", "P4", &[0.0, 0.0, 0.0, 0.0]).help("Returned by `p4()`."),
        ],
        inputs: vec![input("in0"), input("in1")],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 100,
        // Time-based shaders are common; the cost is one draw.
        time_dependent: true,
        sink: false,
    }
}

type Built = Result<FullscreenPass, String>;

fn compile(device: &wgpu::Device, format: wgpu::TextureFormat, body: &str) -> Built {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        FullscreenPass::try_new(device, "img.wgsl", format, 2, HELPERS, body)
    }))
    .unwrap_or_else(|_| Err("shader compilation panicked".into()))
}

/// Watches a file and compiles each new version.
struct Watcher {
    path: String,
    stop: Arc<AtomicBool>,
    rx: Receiver<Built>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch(device: wgpu::Device, format: wgpu::TextureFormat, path: String) -> Watcher {
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_path = path.clone();
    let spawned = std::thread::Builder::new()
        .name("atd-wgsl-watch".into())
        .spawn(move || {
            let mut last: Option<Result<String, String>> = None;
            while !thread_stop.load(Ordering::Relaxed) {
                let now = std::fs::read_to_string(&thread_path)
                    .map_err(|e| format!("{thread_path}: {e}"));
                if last.as_ref() != Some(&now) {
                    let built = match &now {
                        Ok(body) => compile(&device, format, body),
                        Err(e) => Err(e.clone()),
                    };
                    if tx.send(built).is_err() {
                        return;
                    }
                    last = Some(now);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
    if let Err(e) = spawned {
        let (tx2, rx2) = channel();
        let _ = tx2.send(Err(format!("cannot start file watcher: {e}")));
        return Watcher {
            path,
            stop,
            rx: rx2,
        };
    }
    Watcher { path, stop, rx }
}

pub struct Wgsl {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    /// The last shader that compiled.
    pass: Option<FullscreenPass>,
    error: Option<String>,
    /// Inline code compiled (or being compiled), and its result channel.
    code: Option<String>,
    building: Option<Receiver<Built>>,
    watcher: Option<Watcher>,
}

impl std::fmt::Debug for Wgsl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wgsl")
            .field("ready", &self.pass.is_some())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Wgsl {
        device: device.clone(),
        format,
        pass: None,
        error: None,
        code: None,
        building: None,
        watcher: None,
    })
}

impl Wgsl {
    fn take(&mut self, built: Built) {
        match built {
            Ok(pass) => {
                self.pass = Some(pass);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl GpuOp for Wgsl {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let file = p.str("file");
        if file.is_empty() {
            self.watcher = None;
            let code = p.str("code");
            if self.code.as_deref() != Some(code) {
                let (tx, rx) = channel();
                let (device, format, body) = (self.device.clone(), self.format, code.to_owned());
                std::thread::Builder::new()
                    .name("atd-wgsl-compile".into())
                    .spawn(move || {
                        let _ = tx.send(compile(&device, format, &body));
                    })
                    .map_err(|e| format!("cannot start compiler thread: {e}"))?;
                self.code = Some(code.to_owned());
                self.building = Some(rx);
            }
        } else {
            self.code = None;
            self.building = None;
            if self.watcher.as_ref().is_none_or(|w| w.path != file) {
                self.watcher = Some(watch(self.device.clone(), self.format, file.to_owned()));
            }
        }

        if let Some(rx) = &self.building {
            match rx.try_recv() {
                Ok(built) => {
                    self.building = None;
                    self.take(built);
                }
                Err(TryRecvError::Empty) => ctx.cook_again = true,
                Err(TryRecvError::Disconnected) => {
                    self.building = None;
                    self.error = Some("shader compiler stopped".into());
                }
            }
        }
        if let Some(w) = &self.watcher {
            let mut latest = None;
            while let Ok(built) = w.rx.try_recv() {
                latest = Some(built);
            }
            if let Some(built) = latest {
                self.take(built);
            }
            // Keep looking for saves.
            ctx.cook_again = true;
        }

        match &self.pass {
            Some(pass) => {
                let in0 = ctx
                    .inputs
                    .first()
                    .and_then(Option::as_ref)
                    .unwrap_or(ctx.black);
                let in1 = ctx
                    .inputs
                    .get(1)
                    .and_then(Option::as_ref)
                    .unwrap_or(ctx.black);
                let (w, h) = (ctx.output.width() as f32, ctx.output.height() as f32);
                pass.draw(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    &[in0, in1],
                    &[
                        p.vec4("p1"),
                        p.vec4("p2"),
                        p.vec4("p3"),
                        p.vec4("p4"),
                        [ctx.time as f32, w, h, ctx.frame as f32],
                    ],
                    ctx.output,
                );
            }
            None => clear(ctx.encoder, ctx.output),
        }
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}
