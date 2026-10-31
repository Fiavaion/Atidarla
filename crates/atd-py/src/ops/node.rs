//! The engine side of a Python operator node (`docs/python-ops.md`).
//!
//! On the cook thread a node only copies its parameter and input values
//! into its slot and the newest result out of it, in place (rule 2: no
//! allocation once warm, tested). Live it never waits (rule 1): it shows
//! the newest result that has arrived (a frame late) and hands over this
//! frame's job if the worker is not reading the slot. Offline it hands
//! over the job, waits for this frame's result (bounded) and uses it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use atd_core::registry::{OpFamily, ParamKind};
use atd_gpu::engine::{CookCtx, GpuOp, OpLibrary, Params, SigCtx, SigInputs, SigOp};
use serde_json::Value as Json;

use super::host::{GRACE, Job, Output, PyOpHost, Slot, Value};
use super::{NOT_ALLOWED, OpDecl};

/// What an offline cook waits beyond an operator's timeout and [`GRACE`]
/// (the supervisor's reaction time and a worker restart).
const MARGIN: Duration = Duration::from_secs(2);

/// Adds the engine implementation of Python operator `type_id`.
pub(crate) fn add_factory(
    library: &mut OpLibrary,
    type_id: &str,
    family: OpFamily,
    host: Option<PyOpHost>,
) {
    let id: Arc<str> = Arc::from(type_id);
    match family {
        OpFamily::Img => library.add(type_id, move |_, _| {
            Box::new(PyImgOp {
                core: Core::new(host.clone(), id.clone()),
                pixels: Vec::new(),
                texture: None,
                upload: false,
            })
        }),
        _ => library.add_sig(type_id, move || {
            Box::new(PySigOp {
                core: Core::new(host.clone(), id.clone()),
            })
        }),
    }
}

/// What both kinds of node share: the slot, the declaration, the error.
struct Core {
    host: Option<PyOpHost>,
    type_id: Arc<str>,
    slot: Option<Arc<Slot>>,
    decl: Option<Arc<OpDecl>>,
    decl_gen: u64,
    seq: u64,
    error: Option<String>,
    last_time: Option<f64>,
}

impl Core {
    fn new(host: Option<PyOpHost>, type_id: Arc<str>) -> Self {
        let (decl, decl_gen) = host.as_ref().map_or((None, 0), |h| {
            let generation = h.decl_gen();
            (h.try_decl(&type_id).ok().flatten(), generation)
        });
        Self {
            host,
            type_id,
            slot: None,
            decl,
            decl_gen,
            seq: 0,
            error: None,
            last_time: None,
        }
    }

    /// The host and the current declaration, or why the node cannot cook.
    fn prepare(&mut self) -> Result<Arc<OpDecl>, String> {
        let Some(host) = &self.host else {
            return Err("the Python operator worker could not start (see the log)".into());
        };
        if !host.is_allowed() {
            return Err(format!(
                "{NOT_ALLOWED}: allow them when the app asks, or pass --allow-python-ops \
                 (docs/python-ops.md)"
            ));
        }
        let generation = host.decl_gen();
        if generation != self.decl_gen
            && let Ok(decl) = host.try_decl(&self.type_id)
        {
            self.decl = decl;
            self.decl_gen = generation;
        }
        if self.slot.is_none() {
            self.slot = Some(host.new_slot(self.type_id.clone()));
        }
        self.decl.clone().ok_or_else(|| {
            format!(
                "`{}` is no longer declared in its file in ops/",
                self.type_id
            )
        })
    }

    /// The step since the last cook (offline: the fixed step).
    fn dt(&mut self, params: &Params<'_>, time: f64) -> f64 {
        let dt = params
            .mode()
            .step()
            .unwrap_or_else(|| self.last_time.map_or(0.0, |t| (time - t).max(0.0)));
        self.last_time = Some(time);
        dt
    }

    /// Hands this frame's job to the worker. Live it skips the frame when
    /// the worker is reading the slot. Returns the job's number.
    #[allow(clippy::too_many_arguments)]
    fn submit(
        &mut self,
        decl: &OpDecl,
        params: &Params<'_>,
        inputs: Option<&SigInputs<'_>>,
        time: f64,
        dt: f64,
        frame: u64,
        blocking: bool,
    ) -> Option<u64> {
        let (Some(host), Some(slot)) = (&self.host, &self.slot) else {
            return None;
        };
        let mut job = if blocking {
            Some(slot.job.lock().unwrap_or_else(|e| e.into_inner()))
        } else {
            slot.job.try_lock().ok()
        }?;
        self.seq += 1;
        fill(&mut job.0, decl, params, inputs, time, dt, frame);
        job.0.seq = self.seq;
        job.1 = true;
        drop(job);
        host.kick(blocking);
        Some(self.seq)
    }

    /// Waits (offline) for the result of job `seq`.
    fn wait(&self, seq: u64, decl: &OpDecl) -> Result<(), String> {
        let Some(slot) = &self.slot else {
            return Ok(());
        };
        let limit = decl.timeout + GRACE + MARGIN;
        if slot.wait(seq, Instant::now() + limit) {
            Ok(())
        } else {
            Err(format!(
                "no result from the Python worker within {:.1} s",
                limit.as_secs_f64()
            ))
        }
    }

    /// Takes a result that has arrived, if any: `f` sees it when it is not
    /// an error. Keeps the error (or clears it).
    fn take(&mut self, blocking: bool, f: impl FnOnce(&Output)) {
        let Some(slot) = &self.slot else { return };
        let out = if blocking {
            Some(slot.out.lock().unwrap_or_else(|e| e.into_inner()))
        } else {
            slot.out.try_lock().ok()
        };
        let Some(mut out) = out else { return };
        if !out.fresh {
            return;
        }
        out.fresh = false;
        if out.error.is_none() {
            f(&out);
        }
        self.error.clone_from(&out.error);
    }

    fn result(&self) -> Result<(), String> {
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

/// A number parameter: its live value, else its bound channel, else its
/// expression, else its value (as `Params::f32`, without rounding to `f32`).
fn number(params: &Params<'_>, name: &str) -> f64 {
    params
        .live(name)
        .and_then(Json::as_f64)
        .or_else(|| params.bound(name).map(f64::from))
        .or_else(|| params.expr(name))
        .unwrap_or_else(|| params.value(name).and_then(Json::as_f64).unwrap_or(0.0))
}

/// Writes this frame's values into `job`, reusing its storage.
fn fill(
    job: &mut Job,
    decl: &OpDecl,
    params: &Params<'_>,
    inputs: Option<&SigInputs<'_>>,
    time: f64,
    dt: f64,
    frame: u64,
) {
    job.time = time;
    job.dt = dt;
    job.frame = frame;
    job.width = decl.size.0;
    job.height = decl.size.1;
    job.timeout = decl.timeout;
    let n = decl.schema.params.len();
    job.params.truncate(n);
    for (i, p) in decl.schema.params.iter().enumerate() {
        let name = &decl.param_names[i];
        if job.params.len() == i {
            job.params.push((name.clone(), Value::Float(0.0)));
        }
        let (slot_name, value) = &mut job.params[i];
        if !Arc::ptr_eq(slot_name, name) {
            *slot_name = name.clone();
        }
        match &p.kind {
            ParamKind::Float { .. } => *value = Value::Float(number(params, &p.name)),
            ParamKind::Int { .. } => *value = Value::Int(params.i64(&p.name)),
            ParamKind::Toggle | ParamKind::Pulse => *value = Value::Bool(params.bool(&p.name)),
            ParamKind::String | ParamKind::Menu { .. } | ParamKind::Path => {
                let s = params.str(&p.name);
                match value {
                    Value::Str(old) => {
                        if old != s {
                            old.clear();
                            old.push_str(s);
                        }
                    }
                    other => *other = Value::Str(s.to_owned()),
                }
            }
            ParamKind::Colour | ParamKind::Vec { .. } => {
                // Each component as driven (binding, expression; A66).
                let len = params
                    .value(&p.name)
                    .and_then(Json::as_array)
                    .map_or(0, Vec::len);
                let nums = (0..len).map(|i| params.component(&p.name, i).unwrap_or(0.0));
                match value {
                    Value::Nums(v) => {
                        v.clear();
                        v.extend(nums);
                    }
                    other => *other = Value::Nums(nums.collect()),
                }
            }
        }
    }
    let n = decl.input_names.len();
    job.inputs.truncate(n);
    for (i, name) in decl.input_names.iter().enumerate() {
        if job.inputs.len() == i {
            job.inputs.push((name.clone(), Vec::new()));
        }
        let (slot_name, channels) = &mut job.inputs[i];
        if !Arc::ptr_eq(slot_name, name) {
            *slot_name = name.clone();
        }
        let Some(ch) = inputs.and_then(|s| s.get(i)) else {
            channels.clear();
            continue;
        };
        channels.truncate(ch.len());
        for c in 0..ch.len() {
            let (cname, v) = (ch.name(c), f64::from(ch.last(c)));
            if c < channels.len() {
                let entry = &mut channels[c];
                if entry.0 != cname {
                    entry.0.clear();
                    entry.0.push_str(cname);
                }
                entry.1 = v;
            } else {
                channels.push((cname.to_owned(), v));
            }
        }
    }
}

/// A SIG Python operator node.
struct PySigOp {
    core: Core,
}

impl SigOp for PySigOp {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let decl = self.core.prepare()?;
        let offline = ctx.params.mode().is_offline();
        let dt = self.core.dt(&ctx.params, ctx.time);
        if offline {
            let seq = self
                .core
                .submit(
                    &decl,
                    &ctx.params,
                    Some(&ctx.inputs),
                    ctx.time,
                    dt,
                    ctx.frame,
                    true,
                )
                .unwrap_or(0);
            self.core.wait(seq, &decl)?;
        }
        let output = &mut *ctx.output;
        self.core.take(offline, |o| {
            let mut w = output.writer();
            for (name, v) in &o.channels {
                w.push(name, *v);
            }
        });
        if !offline {
            self.core.submit(
                &decl,
                &ctx.params,
                Some(&ctx.inputs),
                ctx.time,
                dt,
                ctx.frame,
                false,
            );
        }
        self.core.result()
    }
}

/// An IMG Python operator node: its pixels go into a texture of its own,
/// which each cook copies into the output (after the engine's offline
/// clear, which a direct queue write would precede).
struct PyImgOp {
    core: Core,
    pixels: Vec<u8>,
    texture: Option<wgpu::Texture>,
    /// `pixels` changed since they were last uploaded.
    upload: bool,
}

impl GpuOp for PyImgOp {
    fn output_size(
        &self,
        _params: &Params<'_>,
        _input: Option<(u32, u32)>,
        _project: (u32, u32),
    ) -> Option<(u32, u32)> {
        self.core.decl.as_ref().map(|d| d.size)
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let decl = self.core.prepare()?;
        let (w, h) = decl.size;
        let offline = ctx.params.mode().is_offline();
        let dt = self.core.dt(&ctx.params, ctx.time);
        if offline {
            let seq = self
                .core
                .submit(&decl, &ctx.params, None, ctx.time, dt, ctx.frame, true)
                .unwrap_or(0);
            self.core.wait(seq, &decl)?;
        }
        let (pixels, upload) = (&mut self.pixels, &mut self.upload);
        let expected = (w as usize) * (h as usize) * 4;
        self.core.take(offline, |o| {
            if o.pixels.len() == expected {
                pixels.clear();
                pixels.extend_from_slice(&o.pixels);
                *upload = true;
            }
        });
        if !offline {
            self.core
                .submit(&decl, &ctx.params, None, ctx.time, dt, ctx.frame, false);
        }
        self.core.result()?;
        let format = ctx.output.format();
        if !matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb
        ) {
            return Err(format!(
                "Python IMG operators need an 8-bit RGBA engine format, not {format:?}"
            ));
        }
        let fits = self
            .texture
            .as_ref()
            .is_some_and(|t| t.width() == w && t.height() == h && t.format() == format);
        if !fits {
            self.texture = Some(ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("atd-python-img"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
            self.upload = !self.pixels.is_empty();
        }
        let texture = self.texture.as_ref().expect("created above");
        if self.upload && self.pixels.len() == expected {
            ctx.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            self.upload = false;
        }
        fn copy(t: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
            wgpu::TexelCopyTextureInfo {
                texture: t,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            }
        }
        ctx.encoder.copy_texture_to_texture(
            copy(texture),
            copy(ctx.output),
            wgpu::Extent3d {
                width: w.min(ctx.output.width()),
                height: h.min(ctx.output.height()),
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }
}
