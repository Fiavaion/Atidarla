//! The AI operator: sends its input to an AI Host through a [`Link`] and
//! shows results through the Output Governor (FR-A6).
//!
//! Each cook (render thread) it scales the input to the model size into a
//! small RGBA8 texture and records an asynchronous copy of it; finished
//! copies go to the link's mailbox; a newer result is uploaded into one of
//! two textures (the other keeps the previous result); and the governor
//! pass draws the output at the project frame rate from the input, the
//! previous and the newest result. None of it waits (rule 1) and, once the
//! textures exist, none of it allocates (rule 2).
//!
//! Offline (`atidarla render`), frame `i` shows the result for frame
//! `i - 1`'s input, waited for, like Analyze: deterministic for a
//! deterministic model.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use atd_ai::link::{FrameParams, Link, LinkConfig, LinkState, ModelSpec, PVal};
use atd_ai::router::Route;
use atd_ai::supervisor::Supervisor;
use atd_gpu::capture::Capture;
use atd_gpu::engine::{CookCtx, GpuOp, Params};
use atd_gpu::fullscreen::FullscreenPass;

use crate::schemas::{Kind, PROMPTS};

/// How often the status line is refreshed.
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// A result older than this many AI intervals is stale (`passthrough`).
const STALE_INTERVALS: f64 = 3.0;
/// Offline: longest wait for a result.
const OFFLINE_WAIT: Duration = Duration::from_secs(120);
/// Motion field resolution for `flow`.
const FLOW_SIZE: (u32, u32) = (64, 36);

const PREP: &str = "return textureSample(in0, samp, uv);";

const GOVERN: &str = r#"
    let mode = params.v[0].x;
    let t = params.v[0].y;
    let has = params.v[0].z;
    let has_prev = params.v[0].w;
    let effect = params.v[1].x;
    let fresh = params.v[1].y;
    let src = textureSample(in0, samp, uv);
    let cur = textureSample(in2, samp, uv);
    let prev = textureSample(in1, samp, uv);
    let f = textureSample(in3, samp, uv).xy;
    if (has < 0.5 || (mode > 2.5 && fresh < 0.5)) {
        let l = dot(src.rgb, vec3f(0.299, 0.587, 0.114));
        return vec4f(mix(src.rgb, vec3f(l * 0.8, l * 0.9, l * 1.1), effect), src.a);
    }
    if (mode < 0.5 || mode > 2.5 || has_prev < 0.5) {
        return cur;
    }
    if (mode < 1.5) {
        return mix(prev, cur, t);
    }
    let a = textureSample(in1, samp, uv - t * f);
    let b = textureSample(in2, samp, uv + (1.0 - t) * f);
    return mix(a, b, t);
"#;

/// Block matching between the previous (in0) and newest (in1) result: for
/// each texel, the displacement `d` such that content at `uv` in the newest
/// was at `uv - d` in the previous one.
const FLOW: &str = r#"
    let step = params.v[0].x;
    let px = params.v[0].yz;
    var best = 1e9;
    var bd = vec2f(0.0);
    for (var j = -6; j <= 6; j++) {
        for (var i = -6; i <= 6; i++) {
            let d = vec2f(f32(i), f32(j)) * step;
            var sad = 0.0;
            for (var y = -1; y <= 1; y++) {
                for (var x = -1; x <= 1; x++) {
                    let o = vec2f(f32(x), f32(y)) * px;
                    let p = textureSampleLevel(in0, samp, uv + o - d, 0.0).rgb;
                    let q = textureSampleLevel(in1, samp, uv + o, 0.0).rgb;
                    sad += dot(abs(p - q), vec3f(1.0));
                }
            }
            sad += length(d) * 0.5;
            if (sad < best) {
                best = sad;
                bd = d;
            }
        }
    }
    return vec4f(bd, 0.0, 1.0);
"#;

const PROMPT_TEXT: [&str; PROMPTS] = [
    "prompts.0.text",
    "prompts.1.text",
    "prompts.2.text",
    "prompts.3.text",
];
const PROMPT_WEIGHT: [&str; PROMPTS] = [
    "prompts.0.weight",
    "prompts.1.weight",
    "prompts.2.weight",
    "prompts.3.weight",
];
const PROMPT_PARAM: [(&str, &str); PROMPTS] = [
    ("prompt1", "weight1"),
    ("prompt2", "weight2"),
    ("prompt3", "weight3"),
    ("prompt4", "weight4"),
];
/// String parameters sent as they are, by kind.
const DIFFUSION_STRINGS: [(&str, &str); 3] =
    [("negative", "negative"), ("rcfg", "rcfg"), ("lora", "lora")];

/// The parameters that decide the link's configuration, as last applied.
#[derive(Debug, Default, PartialEq)]
struct Applied {
    route: String,
    model: String,
    adapter: String,
    task: String,
    size: (u32, u32),
    in_flight: u32,
    budget_ms: f64,
}

pub struct AiOp {
    kind: Kind,
    supervisor: &'static Supervisor,
    link: Option<Link>,
    applied: Applied,
    error: Option<String>,
    prep: FullscreenPass,
    govern: FullscreenPass,
    flow: FullscreenPass,
    flow_tex: Option<wgpu::Texture>,
    send: Option<wgpu::Texture>,
    capture: Option<Capture>,
    /// Two result textures: `results[cur]` is the newest.
    results: [Option<wgpu::Texture>; 2],
    cur: usize,
    have: u32,
    seen: u64,
    latest_at: Option<Instant>,
    params: FrameParams,
    texts: [Arc<str>; PROMPTS],
    strings: [Arc<str>; 3],
    status: String,
    status_at: Option<Instant>,
    /// Offline: the sequence number of the frame sent last.
    offline_sent: Option<u64>,
}

impl std::fmt::Debug for AiOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiOp")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

pub fn create(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    kind: Kind,
    supervisor: &'static Supervisor,
) -> Box<dyn GpuOp> {
    let empty: Arc<str> = Arc::from("");
    Box::new(AiOp {
        kind,
        supervisor,
        link: None,
        applied: Applied::default(),
        error: None,
        prep: FullscreenPass::new(
            device,
            "ai.prep",
            wgpu::TextureFormat::Rgba8Unorm,
            1,
            "",
            PREP,
        ),
        govern: FullscreenPass::new(device, "ai.govern", format, 4, "", GOVERN),
        flow: FullscreenPass::new(
            device,
            "ai.flow",
            wgpu::TextureFormat::Rgba16Float,
            2,
            "",
            FLOW,
        ),
        flow_tex: None,
        send: None,
        capture: None,
        results: [None, None],
        cur: 0,
        have: 0,
        seen: 0,
        latest_at: None,
        params: FrameParams::with_capacity(32),
        texts: std::array::from_fn(|_| empty.clone()),
        strings: std::array::from_fn(|_| empty.clone()),
        status: String::with_capacity(160),
        status_at: None,
        offline_sent: None,
    })
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    (w, h): (u32, u32),
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

/// A model given as an id resolves to its folder in the model cache; one
/// given as a path (or a built-in) is used as it is.
fn model_path(adapter: &str, model: &str) -> Option<String> {
    if adapter == "builtin" || model.is_empty() {
        return None;
    }
    let p = std::path::Path::new(model);
    if p.is_absolute() || model.contains('/') || model.contains('\\') {
        return Some(model.to_owned());
    }
    let dir = atd_ai::models::model_dir(model);
    // An ONNX model folder holds `model.onnx`.
    let onnx = dir.join("model.onnx");
    Some(
        if adapter == "onnx" && onnx.exists() {
            onnx
        } else {
            dir
        }
        .to_string_lossy()
        .into_owned(),
    )
}

impl AiOp {
    fn adapter_and_task<'a>(&self, p: &Params<'a>) -> (&'a str, &'a str) {
        match self.kind {
            Kind::Diffusion => ("diffusion.sd", ""),
            Kind::Model => (p.str("adapter"), p.str("task")),
            Kind::Vision(task) => ("onnx", task),
            Kind::Video => ("video.v2v", ""),
        }
    }

    /// Re-configures the link when a parameter that decides it changed.
    /// Compares in place, so a steady frame allocates nothing.
    fn configure(&mut self, p: &Params<'_>) -> Result<(), String> {
        let (adapter, task) = self.adapter_and_task(p);
        let size = (
            p.i64("width").clamp(8, 2048) as u32,
            p.i64("height").clamp(8, 2048) as u32,
        );
        let in_flight = p.i64("in_flight").clamp(1, 4) as u32;
        let budget = f64::from(p.f32("budget_ms"));
        let a = &self.applied;
        if self.link.is_some()
            && a.route == p.str("route")
            && a.model == p.str("model")
            && a.adapter == adapter
            && a.task == task
            && a.size == size
            && a.in_flight == in_flight
            && a.budget_ms == budget
        {
            return Ok(());
        }
        let route = Route::parse(p.str("route"))?;
        let model = p.str("model");
        let mut options = serde_json::Map::new();
        if !task.is_empty() {
            options.insert("task".into(), task.into());
        }
        if self.kind == Kind::Vision("generic") {
            options.insert(
                "input".into(),
                serde_json::json!({
                    "name": p.str("input_name"),
                    "layout": p.str("layout"),
                }),
            );
        }
        let config = LinkConfig {
            route,
            model: ModelSpec {
                model_id: model.to_owned(),
                adapter: adapter.to_owned(),
                path: model_path(adapter, model),
                options: serde_json::Value::Object(options),
            },
            realtime_vendor: matches!(self.kind, Kind::Diffusion | Kind::Video)
                .then(|| "nvidia".to_owned()),
            in_flight,
            budget_ms: budget,
            max_size: size,
        };
        match &self.link {
            Some(link) => link.set_config(config),
            None => self.link = Some(Link::new(self.supervisor, config)),
        }
        self.applied = Applied {
            route: p.str("route").to_owned(),
            model: model.to_owned(),
            adapter: adapter.to_owned(),
            task: task.to_owned(),
            size,
            in_flight,
            budget_ms: budget,
        };
        Ok(())
    }

    /// Fills the per-frame parameters in place.
    fn frame_params(&mut self, p: &Params<'_>) {
        let set_text = |slot: &mut Arc<str>, text: &str| {
            if &**slot != text {
                *slot = Arc::from(text);
            }
        };
        match self.kind {
            Kind::Diffusion | Kind::Video => {
                for (i, (prompt, weight)) in PROMPT_PARAM.iter().enumerate() {
                    set_text(&mut self.texts[i], p.str(prompt));
                    self.params
                        .set(PROMPT_TEXT[i], PVal::S(self.texts[i].clone()));
                    self.params
                        .set(PROMPT_WEIGHT[i], PVal::F(f64::from(p.f32(weight))));
                }
                self.params.set("seed", PVal::I(p.i64("seed")));
            }
            Kind::Model | Kind::Vision(_) => {}
        }
        if self.kind == Kind::Diffusion {
            for (i, (param, key)) in DIFFUSION_STRINGS.iter().enumerate() {
                set_text(&mut self.strings[i], p.str(param));
                self.params.set(key, PVal::S(self.strings[i].clone()));
            }
            for key in ["strength", "guidance", "delta", "similarity", "lora_weight"] {
                self.params.set(key, PVal::F(f64::from(p.f32(key))));
            }
            self.params.set("steps", PVal::I(p.i64("steps")));
        }
        if self.kind == Kind::Vision("generic") {
            let (m, s) = (p.vec4("mean"), p.vec4("std"));
            for (i, key) in ["mean.r", "mean.g", "mean.b"].iter().enumerate() {
                self.params.set(key, PVal::F(f64::from(m[i])));
            }
            for (i, key) in ["std.r", "std.g", "std.b"].iter().enumerate() {
                self.params.set(key, PVal::F(f64::from(s[i])));
            }
        }
    }

    fn refresh_status(&mut self, now: Instant) {
        if self
            .status_at
            .is_some_and(|t| now.duration_since(t) < STATUS_EVERY)
        {
            return;
        }
        self.status_at = Some(now);
        let Some(link) = &self.link else { return };
        let s = &mut self.status;
        s.clear();
        link.with_status(|st| {
            let host = st.target.as_ref().map(atd_ai::router::Target::host_name);
            let _ = match &st.state {
                LinkState::Running => write!(
                    s,
                    "{} · {:.1} AI fps · {:.0} ms{}{}",
                    host.as_deref().unwrap_or("?"),
                    st.ai_fps,
                    st.latency_ms,
                    if st.realtime { "" } else { " · interactive" },
                    st.vram_mb.map(|v| format!(" · {v} MB")).unwrap_or_default(),
                ),
                LinkState::Routing => write!(s, "routing…"),
                LinkState::Loading => {
                    write!(s, "{}: loading the model…", host.as_deref().unwrap_or("?"))
                }
                LinkState::Waiting(why) => write!(s, "waiting: {why}"),
            };
            if st.cost_usd > 0.0 {
                let _ = write!(s, " · ${:.2}", st.cost_usd);
            }
        });
    }
}

impl GpuOp for AiOp {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let offline = ctx.params.mode().is_offline();
        if let Err(e) = self.configure(&ctx.params) {
            self.error = Some(e.clone());
            return Err(e);
        }
        self.frame_params(&ctx.params);
        let size = self.applied.size;
        if self
            .send
            .as_ref()
            .is_none_or(|t| (t.width(), t.height()) != size)
        {
            self.send = Some(texture(
                ctx.device,
                "ai.send",
                size,
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            ));
            self.capture = Some(Capture::new(ctx.device, size.0, size.1, 3));
        }
        let link = self.link.as_ref().expect("configured");
        let capture = self.capture.as_mut().expect("capture");

        // Hand finished copies of the input to the link.
        if offline && capture.busy() {
            let _ = ctx.device.poll(wgpu::PollType::wait_indefinitely());
        }
        if capture.ready() {
            let (w, h) = capture.size();
            let row = w as usize * 4;
            let ts = (ctx.time * 1e6) as u64;
            let mut fill = |px: &mut [u8]| {
                capture.poll(ctx.device, |r, y| {
                    px[y as usize * row..][..row].copy_from_slice(r);
                });
            };
            if offline {
                self.offline_sent = Some(link.offer_blocking(w, h, ts, &self.params, &mut fill));
            } else {
                // Busy mailbox: this frame is dropped, the newest goes next.
                let _ = link.offer(w, h, ts, &self.params, &mut fill);
            }
        }

        // Newest result, uploaded into the texture that is not the newest.
        let mut got: Option<Instant> = None;
        {
            let (device, queue) = (ctx.device, ctx.queue);
            let (cur, results, have) = (&mut self.cur, &mut self.results, &mut self.have);
            let mut up = |out: &atd_ai::link::OutSlot| {
                let (w, h) = (out.width, out.height);
                let next = 1 - *cur;
                if results[next]
                    .as_ref()
                    .is_none_or(|t| (t.width(), t.height()) != (w, h))
                {
                    results[next] = Some(texture(
                        device,
                        "ai.result",
                        (w, h),
                        wgpu::TextureFormat::Rgba8Unorm,
                        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    ));
                }
                let tex = results[next].as_ref().expect("result texture");
                queue.write_texture(
                    tex.as_image_copy(),
                    &out.pixels,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: None,
                    },
                    tex.size(),
                );
                *cur = next;
                *have = (*have + 1).min(2);
                got = Some(out.at.unwrap_or_else(Instant::now));
            };
            match (offline, self.offline_sent) {
                (true, Some(seq)) => {
                    if !link.wait_result(seq, OFFLINE_WAIT, &mut up) {
                        let why = link.with_status(|s| match &s.state {
                            LinkState::Waiting(w) => w.clone(),
                            _ => "no result".into(),
                        });
                        return Err(format!("the AI host gave no result: {why}"));
                    }
                    self.seen = link.result_seq();
                }
                (true, None) => {}
                (false, _) => {
                    link.take(&mut self.seen, &mut up);
                }
            }
        }
        if got.is_some() {
            self.latest_at = got;
        }

        // Copy this frame's input for the host.
        let input = ctx.input_or_black(0).clone();
        let send = self.send.as_ref().expect("send texture");
        self.prep
            .draw(ctx.device, ctx.queue, ctx.encoder, &[&input], &[], send);
        self.capture
            .as_mut()
            .expect("capture")
            .record(ctx.encoder, send);

        // Govern.
        let now = Instant::now();
        let mode = match ctx.params.str("policy") {
            "hold" => 0.0,
            "flow" => 2.0,
            "passthrough" => 3.0,
            _ => 1.0,
        };
        let fps = link.with_status(|s| s.ai_fps);
        let interval = if fps > 0.0 { 1.0 / fps } else { 0.1 };
        let age = self
            .latest_at
            .map_or(f64::INFINITY, |t| now.duration_since(t).as_secs_f64());
        let t = if offline {
            1.0
        } else {
            (age / interval).clamp(0.0, 1.0) as f32
        };
        let fresh = age < interval * STALE_INTERVALS;
        if mode == 2.0 && got.is_some() && self.have == 2 {
            let flow = self.flow_tex.get_or_insert_with(|| {
                texture(
                    ctx.device,
                    "ai.flow",
                    FLOW_SIZE,
                    wgpu::TextureFormat::Rgba16Float,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                )
            });
            let (prev, cur) = (
                self.results[1 - self.cur].as_ref().expect("previous"),
                self.results[self.cur].as_ref().expect("newest"),
            );
            let px = [1.0 / cur.width() as f32, 1.0 / cur.height() as f32];
            self.flow.draw(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &[prev, cur],
                &[[0.01, px[0] * 2.0, px[1] * 2.0, 0.0]],
                flow,
            );
        }
        let black = ctx.black;
        let newest = self.results[self.cur].as_ref().unwrap_or(black);
        let previous = if self.have == 2 {
            self.results[1 - self.cur].as_ref().unwrap_or(black)
        } else {
            black
        };
        let flow = if mode == 2.0 {
            self.flow_tex.as_ref().unwrap_or(black)
        } else {
            black
        };
        self.govern.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&input, previous, newest, flow],
            &[
                [mode, t, f32::from(self.have > 0), f32::from(self.have == 2)],
                [ctx.params.f32("effect"), f32::from(fresh), 0.0, 0.0],
            ],
            ctx.output,
        );
        if !offline {
            self.refresh_status(now);
        }
        self.error = None;
        Ok(())
    }

    fn after_submit(&mut self) {
        if let Some(c) = &mut self.capture {
            c.after_submit();
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}
