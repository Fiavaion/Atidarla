//! `atidarla render`: deterministic offline render of a project to a
//! numbered PNG sequence (FR-H1). Usage, what is and is not deterministic,
//! and the per-operator classification: `docs/render.md`.
//!
//! The engine cooks in [`CookMode::Offline`]: frame `i` is at `i / fps`
//! seconds, every frame advances exactly one step, operators wait for their
//! worker threads instead of using whatever has arrived, live inputs output
//! their idle value and sinks do not emit. Two renders of the same project
//! on the same machine (same adapter and backend) give byte-identical
//! frames.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use atd_core::graph::{Network, NodeId};
use atd_core::pack::Flat;
use atd_core::platform::PlatformInfo;
use atd_core::registry::{Availability, Registry, WireType};
use atd_gpu::engine::{CookMode, Engine, EngineConfig};

/// Frames per second when `--fps` is not given (project files have no
/// frame rate yet).
pub const DEFAULT_FPS: f64 = 60.0;

/// File extensions that name a movie or image file rather than a folder.
const FILE_EXTENSIONS: &[&str] = &[
    "mov", "mp4", "m4v", "mkv", "avi", "webm", "gif", "png", "jpg", "jpeg", "exr", "tif", "tiff",
];

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Frames to render: start inclusive, end exclusive.
    pub range: Range<u64>,
    pub fps: f64,
    /// The node to render, by name; `None` picks the output node.
    pub node: Option<String>,
    /// Overrides the project resolution (tests).
    pub size: Option<[u32; 2]>,
    /// Run the project's Python operators (`--allow-python-ops`).
    pub allow_python_ops: bool,
}

/// Parses `A-B` (start inclusive, end exclusive).
pub fn parse_range(s: &str) -> Result<Range<u64>, String> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| format!("`{s}`: expected START-END, e.g. 0-300"))?;
    let parse = |v: &str| {
        v.trim()
            .parse::<u64>()
            .map_err(|_| format!("`{s}`: `{v}` is not a frame number"))
    };
    let (a, b) = (parse(a)?, parse(b)?);
    if a >= b {
        return Err(format!(
            "`{s}`: the end is exclusive and must be greater than the start"
        ));
    }
    Ok(a..b)
}

/// Parses a frame rate: a positive number.
pub fn parse_fps(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(format!("`{s}`: the frame rate must be a positive number")),
    }
}

/// The file name of frame `i`.
pub fn frame_name(i: u64) -> String {
    format!("frame_{i:06}.png")
}

/// Renders `project` into the folder `out`.
pub fn run(project: &Path, out: &Path, opts: &RenderOptions) -> Result<()> {
    check_out(out)?;
    let gpu = atd_gpu::GpuContext::headless().context("no GPU adapter for rendering")?;
    let info = gpu.adapter.get_info();
    let mut r = Renderer::new(gpu.device.clone(), gpu.queue.clone(), gpu.platform_info())?;
    r.load(project, opts)?;
    for w in r.take_warnings() {
        eprintln!("warning: {w}");
    }
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let start = Instant::now();
    let mut size = (0, 0);
    for i in opts.range.clone() {
        let frame = r.frame(i)?;
        for w in r.take_warnings() {
            eprintln!("warning: {w}");
        }
        size = (frame.width, frame.height);
        frame.write_png(&out.join(frame_name(i)))?;
    }
    println!(
        "rendered frames {}-{} (end exclusive) of {} at {}x{}, {} fps, to {} in {:.1} s on {} ({:?})",
        opts.range.start,
        opts.range.end,
        r.output_name(),
        size.0,
        size.1,
        opts.fps,
        out.display(),
        start.elapsed().as_secs_f64(),
        info.name,
        info.backend,
    );
    Ok(())
}

/// `--out` must name a folder: movie output is not implemented yet.
fn check_out(out: &Path) -> Result<()> {
    if let Some(ext) = out.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if FILE_EXTENSIONS.contains(&ext.as_str()) {
            bail!(
                "--out {}: `atidarla render` writes a numbered PNG sequence ({}, {}, ...) into a \
                 folder; writing a .{ext} file is not implemented yet (movie encoding, HAP or \
                 FFmpeg, is a separate step of FR-H1). Pass a folder, e.g. --out {}",
                out.display(),
                frame_name(0),
                frame_name(1),
                out.with_extension("").display()
            );
        }
    }
    if out.is_file() {
        bail!("--out {}: is a file; pass a folder", out.display());
    }
    Ok(())
}

/// One rendered frame, tightly packed RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    pub fn write_png(&self, path: &Path) -> Result<()> {
        let file = std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?,
        );
        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()?
            .write_image_data(&self.rgba)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// An engine set up for offline rendering.
pub struct Renderer {
    engine: Engine,
    platform: PlatformInfo,
    output: NodeId,
    output_label: String,
    fps: f64,
    /// Nodes already reported as not available on this platform.
    unavailable: BTreeSet<NodeId>,
    warnings: Vec<String>,
    /// The loaded project's Python operators (their worker).
    python: Option<atd_py::ops::PythonOps>,
}

impl Renderer {
    /// An offline engine on `device` with every built-in operator and the
    /// Python worker for expressions.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, platform: PlatformInfo) -> Result<Self> {
        let (registry, library) = crate::project::library();
        let mut engine = Engine::new(device, queue, registry, library, EngineConfig::default());
        engine.set_python(Box::new(atd_py::expr::PyExprHost::start()?));
        Ok(Self {
            engine,
            platform,
            output: NodeId(0),
            output_label: String::new(),
            fps: DEFAULT_FPS,
            unavailable: BTreeSet::new(),
            warnings: Vec::new(),
            python: None,
        })
    }

    /// Loads the project at `dir` (replacing any loaded before; the texture
    /// pool stays warm) and picks the node to render.
    pub fn load(&mut self, dir: &Path, opts: &RenderOptions) -> Result<()> {
        let mut project = atd_format::load(dir)?;
        let net = project
            .networks
            .remove(atd_format::MAIN)
            .context("the project has no main network")?;
        let (_, failed) = self
            .engine
            .extend(|r, l| atd_ops_img::isf::register_project_shaders(dir, r, l));
        if let Some((name, e)) = failed.first() {
            bail!("shaders/{name}: {e}");
        }
        let mut python = self
            .engine
            .extend(|r, l| crate::project::python_ops(dir, r, l))?;
        if opts.allow_python_ops {
            python.allow();
        }
        // Pack instances are expanded into the nodes inside them
        // (`docs/packs.md`); the engine cooks the flat network.
        let registry = project
            .packs
            .register(self.engine.registry())
            .map_err(crate::project::pack_error)?;
        let net = net
            .validated(&registry)
            .with_context(|| format!("{}: networks/main.toml", dir.display()))?;
        let flat = project
            .packs
            .flatten(&net, &registry)
            .map_err(crate::project::pack_error)?;
        let (output, label) = output_node(&net, &registry, &flat, opts.node.as_deref())?;
        self.fps = opts.fps;
        self.engine.set_mode(CookMode::Offline { fps: opts.fps });
        self.engine.set_project_dir(Some(dir.to_owned()));
        self.engine
            .load(flat.network)
            .map_err(|e| match e {
                atd_gpu::engine::EngineError::Graph(g) => crate::project::explain(g, &python),
                other => other.into(),
            })
            .with_context(|| format!("{}", dir.display()))?;
        self.python = Some(python);
        let [w, h] = opts.size.unwrap_or(project.resolution);
        self.engine.set_resolution(w, h);
        self.output = output;
        self.output_label = label;
        self.unavailable.clear();
        let net = self.engine.network();
        for node in net.nodes() {
            match classify(&node.type_id) {
                Some(Class::LiveInput) => self.warnings.push(format!(
                    "{} ({}) is a live input: an offline render does not read it, so it outputs its idle value",
                    node.name, node.type_id
                )),
                Some(Class::Sink) => self.warnings.push(format!(
                    "{} ({}) has external side effects: it passes its input through and does not emit during an offline render",
                    node.name, node.type_id
                )),
                _ => {}
            }
        }
        Ok(())
    }

    /// Cooks frame `i` (at `i / fps` seconds) and reads back the output.
    /// Fails on a node error; operators this platform does not run
    /// (capability flags, PRD A19) are reported as warnings.
    pub fn frame(&mut self, i: u64) -> Result<Frame> {
        self.engine.set_frame(i);
        self.engine
            .cook(&[self.output], i as f64 / self.fps)
            .with_context(|| format!("frame {i}"))?;
        let mut errors = Vec::new();
        for n in self.engine.network().nodes() {
            let Some(e) = self.engine.error(n.id) else {
                continue;
            };
            let fallback = self.engine.registry().get(&n.type_id).is_some_and(|s| {
                matches!(
                    Registry::availability(s, &self.platform),
                    Availability::Fallback { .. }
                )
            });
            if fallback {
                if self.unavailable.insert(n.id) {
                    self.warnings.push(format!(
                        "{} ({}) is not available on this platform and outputs nothing: {e}",
                        n.name, n.type_id
                    ));
                }
            } else {
                errors.push(format!("{}: {e}", n.name));
            }
        }
        if !errors.is_empty() {
            bail!("frame {i}: node errors: {}", errors.join("; "));
        }
        let texture = self.engine.result(self.output).with_context(|| {
            format!(
                "frame {i}: {} has no image (is it an IMG node that cooked?)",
                self.output_name()
            )
        })?;
        let rgba =
            atd_interop::readback::read_rgba8(self.engine.device(), self.engine.queue(), texture)
                .with_context(|| format!("frame {i}: reading the image back"))?;
        Ok(Frame {
            width: texture.width(),
            height: texture.height(),
            rgba,
        })
    }

    /// Warnings since the last call.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// The rendered node's name in the project (`glow1` for a Pack
    /// instance, `glow1/blur1` for a node inside one).
    pub fn output_name(&self) -> &str {
        &self.output_label
    }
}

/// The node to render, as a node of the flat network and a name for
/// people: `name` if given (an IMG node of the project, a Pack instance
/// with an IMG output, or a node inside one as `glow1/blur1`), otherwise
/// the node the app's viewer shows first: the last IMG node nothing reads
/// from, else the last IMG node. (The viewer's rule, `atd-app/src/
/// project.rs`, limited to IMG nodes.)
fn output_node(
    net: &Network,
    registry: &Registry,
    flat: &Flat,
    name: Option<&str>,
) -> Result<(NodeId, String)> {
    let is_img = |net: &Network, id: NodeId| {
        net.node(id)
            .and_then(|n| registry.get(&n.type_id))
            .and_then(|s| s.outputs.first())
            .is_some_and(|o| o.wire == WireType::Img)
    };
    if let Some(name) = name {
        let found = net
            .nodes()
            .find(|n| n.name == name)
            .map(|n| (n, net))
            .or_else(|| {
                flat.network
                    .nodes()
                    .find(|n| n.name == name || flat.display(n.id) == name)
                    .map(|n| (n, &flat.network))
            });
        let Some((node, within)) = found else {
            let names: Vec<&str> = net
                .nodes()
                .filter(|n| is_img(net, n.id))
                .map(|n| n.name.as_str())
                .collect();
            bail!(
                "--node {name}: no such node (IMG nodes: {})",
                names.join(", ")
            );
        };
        if !is_img(within, node.id) {
            bail!(
                "--node {name}: {} is not an IMG node, so it has no image to render",
                node.type_id
            );
        }
        let id = flat
            .view(node.id)
            .with_context(|| format!("--node {name}: the Pack has no output to render"))?;
        return Ok((id, name.to_owned()));
    }
    let read: BTreeSet<NodeId> = net.wires().map(|w| w.from.node).collect();
    let ids: Vec<NodeId> = net
        .nodes()
        .map(|n| n.id)
        .filter(|&id| is_img(net, id))
        .collect();
    let id = ids
        .iter()
        .rev()
        .copied()
        .find(|id| !read.contains(id))
        .or_else(|| ids.last().copied())
        .context("the project has no IMG node to render; add one, or pick one with --node")?;
    let label = net.node(id).map(|n| n.name.clone()).unwrap_or_default();
    let flat_id = flat
        .view(id)
        .with_context(|| format!("{label}: the Pack has no output to render"))?;
    Ok((flat_id, label))
}

/// How an operator behaves in an offline render (`docs/render.md` has the
/// same table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A function of its parameters, inputs and the frame's time.
    Pure,
    /// Keeps state from frame to frame (integrators, history,
    /// simulations): identical across runs of the same range, but a range
    /// started part-way differs from the same frames of a longer render.
    Stateful,
    /// Works on a worker thread (decoding, compiling, rasterising);
    /// offline the cook waits for the result.
    Waits,
    /// A GPU readback: offline the values are exactly one frame late.
    OneFrameLate,
    /// Reads a device, the network or another application: offline it
    /// outputs its idle value, and `render` warns.
    LiveInput,
    /// Has external side effects: offline it passes through and does not
    /// emit.
    Sink,
}

/// The offline class of operator `type_id`; `None` for one not classified
/// yet (a test requires every registered operator to be).
pub fn classify(type_id: &str) -> Option<Class> {
    use Class::*;
    if type_id.starts_with("img.isf_") {
        // Project ISF shaders: compiled on a worker thread. Persistent
        // buffers also make one stateful.
        return Some(Waits);
    }
    if type_id.starts_with(atd_core::pack::TYPE_PREFIX) {
        // A Pack instance never reaches the engine: it is flattened into
        // the nodes inside it, and each of those is classified on its own.
        return Some(Pure);
    }
    if type_id.starts_with("msh.") {
        // MSH operators (`docs/design/msh-operators.md`): cooked on worker
        // threads; offline each frame waits for its own result.
        return Some(Waits);
    }
    if type_id.starts_with("py.") {
        // Python operators (`docs/python-ops.md`): cooked on the Python
        // operator worker; offline each frame waits for its own result.
        // Those with declared state are also stateful.
        return Some(Waits);
    }
    Some(match type_id {
        "img.blend" | "img.bloom" | "img.blur" | "img.channel_image" | "img.channel_mix"
        | "img.chroma_key" | "img.composite" | "img.constant" | "img.crop" | "img.displace"
        | "img.edge" | "img.hsv_adjust" | "img.kaleidoscope" | "img.level" | "img.lookup"
        | "img.luma_key" | "img.mirror" | "img.noise" | "img.normal_map" | "img.projection"
        | "img.ramp" | "img.remap" | "img.resolution" | "img.shape" | "img.slope"
        | "img.switch" | "img.threshold" | "img.tile" | "img.transform" => Pure,
        "sig.constant" | "sig.lfo" | "sig.limit" | "sig.math" | "sig.merge" | "sig.noise"
        | "sig.pattern" | "sig.select" => Pure,
        "img.cache"
        | "img.feedback"
        | "img.fluid"
        // Also waits for its pipelines and weights file.
        | "img.nca"
        | "img.reaction_diffusion"
        | "img.ripple"
        | "img.time_machine" => Stateful,
        "sig.beat" | "sig.count" | "sig.envelope" | "sig.filter" | "sig.lag" | "sig.onset"
        | "sig.spectrum" | "sig.speed" | "sig.timer" | "sig.trail" | "sig.trigger" => Stateful,
        // Geometry (M2): GPU work in the frame's encoder, nothing waits.
        "img.render_points" | "pnt.points" | "pnt.noise" => Pure,
        // The M2 PNT batch (`docs/milestones/m2/pnt-batch.md`).
        "pnt.age" | "pnt.attract" | "pnt.attribute" | "pnt.attribute_math" | "pnt.bend"
        | "pnt.collide" | "pnt.colour" | "pnt.connect" | "pnt.copy" | "pnt.delete" | "pnt.drag"
        | "pnt.gravity" | "pnt.group" | "pnt.integrate" | "pnt.merge" | "pnt.normals"
        | "pnt.piece" | "pnt.randomize" | "pnt.scatter" | "pnt.smooth" | "pnt.sort"
        | "pnt.subset" | "pnt.taper" | "pnt.transform" | "pnt.turbulence" | "pnt.twist"
        | "pnt.vortex" | "pnt.wave" | "pnt.wind" => Pure,
        "pnt.emitter" | "pnt.solver" => Stateful,
        "pnt.particles" => Stateful,
        "img.render" | "msh.platonic" => Pure,
        "mat.line" | "mat.matcap" | "mat.pbr" | "mat.point_sprite" | "mat.toon" | "mat.unlit" => {
            Pure
        }
        "img.movie_in" | "img.text" | "img.wgsl" | "sig.audio_file_in" => Waits,
        // Waits for its compile (and its file); its state lives on the wire.
        "pnt.kernel" => Waits,
        // Compile; persistent `state` images and buffers also make it stateful.
        "img.compute" => Waits,
        "img.analyze" => OneFrameLate,
        "img.share_in" | "sig.audio_in" | "sig.input" | "sig.midi_in" | "sig.osc_in"
        | "sig.timecode_in" => LiveInput,
        "img.share_out" | "sig.audio_out" | "sig.dmx_out" | "sig.midi_out" | "sig.osc_out" => Sink,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::graph::{ChannelRef, Edit, Endpoint, Network, Node, Param, Wire};
    use serde_json::{Value, json};

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue, PlatformInfo)> {
        let instance = atd_gpu::primary_instance();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            let required =
                std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
            );
            return None;
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: atd_gpu::optional_features(&adapter),
            required_limits: atd_gpu::required_limits(&adapter),
            ..Default::default()
        }))
        .ok()?;
        Some((device, queue, atd_gpu::platform_info(&adapter.get_info())))
    }

    fn renderer() -> Option<Renderer> {
        let (device, queue, platform) = gpu()?;
        Some(Renderer::new(device, queue, platform).unwrap())
    }

    fn opts(range: Range<u64>) -> RenderOptions {
        RenderOptions {
            range,
            fps: 60.0,
            node: None,
            size: Some([96, 54]),
            allow_python_ops: false,
        }
    }

    fn example(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples")
            .join(format!("{name}.atidarla"))
    }

    fn render(r: &mut Renderer, frames: impl IntoIterator<Item = u64>) -> Vec<(u64, Frame)> {
        frames
            .into_iter()
            .map(|i| (i, r.frame(i).unwrap_or_else(|e| panic!("{e:#}"))))
            .collect()
    }

    /// A project folder in the temp directory, removed when dropped.
    struct TempProject(std::path::PathBuf);

    impl Drop for TempProject {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Builds a network node by node.
    struct Builder {
        reg: Registry,
        net: Network,
    }

    impl Builder {
        fn new() -> Self {
            Self {
                reg: crate::builtin_registry(),
                net: Network::default(),
            }
        }

        fn edit(&mut self, edit: Edit) {
            self.net.apply(&self.reg, edit).unwrap();
        }

        fn node(&mut self, ty: &str, name: &str, params: &[(&str, Value)]) -> NodeId {
            let id = self.net.alloc_id();
            self.edit(Edit::AddNode {
                node: Node::new(id, ty, name),
            });
            for (p, v) in params {
                self.edit(Edit::SetParam {
                    node: id,
                    param: (*p).into(),
                    value: Some(Param::constant(v.clone())),
                });
            }
            id
        }

        fn wire(&mut self, from: NodeId, to: NodeId, socket: &str) {
            self.edit(Edit::Connect {
                wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
            });
        }

        fn param(&mut self, node: NodeId, param: &str, value: Param) {
            self.edit(Edit::SetParam {
                node,
                param: param.into(),
                value: Some(value),
            });
        }

        fn bind(&mut self, node: NodeId, param: &str, src: NodeId, channel: &str) {
            self.param(
                node,
                param,
                Param {
                    value: json!(0.0),
                    expr: None,
                    bind: Some(ChannelRef {
                        node: src,
                        channel: channel.into(),
                    }),
                },
            );
        }

        fn expr(&mut self, node: NodeId, param: &str, expr: &str) {
            self.param(
                node,
                param,
                Param {
                    value: json!(0.0),
                    expr: Some(expr.into()),
                    bind: None,
                },
            );
        }

        fn save(self, name: &str) -> TempProject {
            let dir = std::env::temp_dir()
                .join(format!("atd-render-{name}-{}.atidarla", std::process::id()));
            let mut project = atd_format::Project::new(name);
            project
                .networks
                .insert(atd_format::MAIN.to_owned(), self.net);
            atd_format::save(&dir, &project).unwrap();
            TempProject(dir)
        }
    }

    /// No state from frame to frame: every frame is a function of its
    /// time alone. Time through an LFO binding and native expressions
    /// (`time`, `frame`); Text (rasterised) and WGSL (compiled) on worker
    /// threads.
    fn stateless_project() -> TempProject {
        let mut b = Builder::new();
        let lfo = b.node("sig.lfo", "lfo1", &[("frequency", json!(0.7))]);
        let noise = b.node("img.noise", "noise1", &[("scale", json!(4.0))]);
        b.bind(noise, "z", lfo, "lfo");
        let kaleido = b.node("img.kaleidoscope", "kaleido1", &[]);
        b.wire(noise, kaleido, "in");
        b.expr(kaleido, "rotation", "time * 40");
        let shape = b.node("img.shape", "shape1", &[]);
        b.expr(shape, "rotate", "frame * 3");
        let text = b.node(
            "img.text",
            "text1",
            &[("text", json!("FR-H1")), ("size", json!(0.4))],
        );
        b.wire(shape, text, "in");
        let comp = b.node("img.composite", "comp1", &[("opacity", json!(0.6))]);
        b.wire(kaleido, comp, "a");
        b.wire(text, comp, "b");
        let level = b.node("img.level", "level1", &[]);
        b.wire(comp, level, "in");
        b.bind(level, "brightness", lfo, "lfo");
        let shader = b.node(
            "img.wgsl",
            "wgsl1",
            &[(
                "code",
                json!(
                    "let c = textureSample(in0, samp, uv);\n\
                     return vec4f(c.rgb * (0.75 + 0.25 * sin(time() * 3.0 + uv.x * 6.0)), 1.0);"
                ),
            )],
        );
        b.wire(level, shader, "in0");
        b.save("stateless")
    }

    #[test]
    fn every_operator_is_classified() {
        let registry = crate::builtin_registry();
        let missing: Vec<&str> = registry
            .iter()
            .map(|s| s.type_id.as_str())
            .filter(|t| classify(t).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "classify these for offline rendering (and add them to docs/render.md): {missing:?}"
        );
        // Every sink is classified as one, and nothing else is.
        for s in registry.iter() {
            assert_eq!(
                s.sink,
                classify(&s.type_id) == Some(Class::Sink),
                "{}",
                s.type_id
            );
        }
    }

    #[test]
    fn ranges_fps_and_outputs_are_checked() {
        assert_eq!(parse_range("0-300"), Ok(0..300));
        assert_eq!(parse_range(" 10 - 20 "), Ok(10..20));
        assert!(parse_range("20-10").is_err());
        assert!(parse_range("5-5").is_err());
        assert!(parse_range("300").is_err());
        assert!(parse_fps("0").is_err());
        assert_eq!(parse_fps("29.97"), Ok(29.97));
        let err = check_out(Path::new("renders/clip.mov"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not implemented yet"), "{err}");
        assert!(err.contains("renders/clip"), "{err}");
        assert!(check_out(Path::new("renders/clip")).is_ok());
        assert_eq!(frame_name(12), "frame_000012.png");
    }

    /// FR-H1 / M2 AC4: the AC1 example (Audio File In decoded on a worker
    /// thread → Spectrum → bound parameters, Timer, Feedback) renders to
    /// byte-identical frames twice.
    #[test]
    fn two_renders_of_an_example_are_identical() {
        let Some(mut a) = renderer() else { return };
        let mut b = renderer().unwrap();
        let dir = example("audio-reactive-feedback");
        a.load(&dir, &opts(0..24)).unwrap();
        b.load(&dir, &opts(0..24)).unwrap();
        let (fa, fb) = (render(&mut a, 0..24), render(&mut b, 0..24));
        assert_eq!(fa, fb);
        // Not trivially equal: the picture changes, and the audio drives it.
        assert_ne!(fa[1].1, fa[23].1);
        let spectrum = a
            .engine
            .network()
            .nodes()
            .find(|n| n.type_id == "sig.spectrum")
            .unwrap()
            .id;
        let level = a.engine.channels(spectrum).and_then(|c| c.value("level"));
        assert!(level.is_some_and(|l| l > 0.0), "{level:?}");
    }

    /// The Python operators example (`docs/python-ops.md`) renders to the
    /// same frames twice when allowed; without --allow-python-ops the render
    /// stops and says why.
    #[test]
    fn python_operators_render_deterministically_only_when_allowed() {
        let Some(mut a) = renderer() else { return };
        let mut b = renderer().unwrap();
        assert_eq!(classify("py.motion.counter"), Some(Class::Waits));
        let dir = example("python-ops");
        let allowed = RenderOptions {
            allow_python_ops: true,
            ..opts(0..8)
        };
        a.load(&dir, &allowed).unwrap();
        b.load(&dir, &allowed).unwrap();
        let (fa, fb) = (render(&mut a, 0..8), render(&mut b, 0..8));
        assert_eq!(fa, fb);
        assert_ne!(fa[0].1, fa[7].1, "the Python operators animate it");
        a.load(&dir, &opts(0..1)).unwrap();
        let e = format!("{:#}", a.frame(0).unwrap_err());
        assert!(e.contains(atd_py::ops::NOT_ALLOWED), "{e}");
    }

    /// Worker-thread results do not depend on thread scheduling: the same
    /// render with every core busy (the decode thread and the cook compete
    /// with spinning threads) gives the same bytes, and the audio is there
    /// from the first frame (the cook waited for the decode).
    #[test]
    fn worker_results_do_not_depend_on_scheduling() {
        let Some(mut calm) = renderer() else { return };
        let dir = example("audio-reactive-feedback");
        calm.load(&dir, &opts(0..12)).unwrap();
        let first = calm.frame(0).unwrap();
        let audio = calm
            .engine
            .network()
            .nodes()
            .find(|n| n.type_id == "sig.audio_file_in")
            .unwrap()
            .id;
        assert!(
            calm.engine.channels(audio).is_some_and(|c| !c.is_empty()),
            "the clip is decoded by the first frame"
        );
        let mut expected = vec![(0, first)];
        expected.extend(render(&mut calm, 1..12));

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let spinners: Vec<_> = (0..cores * 2)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 0u64;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                        std::hint::black_box(x);
                    }
                })
            })
            .collect();
        let mut busy = renderer().unwrap();
        busy.load(&dir, &opts(0..12)).unwrap();
        let got = render(&mut busy, 0..12);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for s in spinners {
            s.join().unwrap();
        }
        assert_eq!(got, expected);
    }

    /// Frames do not depend on what was rendered before them: frames 10-19
    /// of a stateless project rendered alone, backwards, on an engine whose
    /// texture pool is warm from another project, equal the same frames of
    /// a 0-20 render. (A stateful project's frames legitimately differ
    /// here, as its state starts with the render; those are compared only
    /// across runs of the same range.)
    #[test]
    fn frames_do_not_depend_on_render_history() {
        let Some(mut whole) = renderer() else { return };
        let project = stateless_project();
        whole.load(&project.0, &opts(0..20)).unwrap();
        let expected = render(&mut whole, 0..20);
        assert_ne!(expected[10].1, expected[19].1, "the picture changes");

        let mut part = renderer().unwrap();
        part.load(&example("audio-reactive-feedback"), &opts(0..5))
            .unwrap();
        render(&mut part, 0..5);
        part.load(&project.0, &opts(10..20)).unwrap();
        let mut got = render(&mut part, (10..20).rev());
        got.reverse();
        assert_eq!(got, expected[10..]);
    }

    /// Live inputs output their idle value without opening anything, and
    /// `render` names them.
    #[test]
    fn live_inputs_are_idle_and_reported() {
        let Some(mut r) = renderer() else { return };
        const PORT: u16 = 47_913;
        let mut b = Builder::new();
        let osc = b.node("sig.osc_in", "osc1", &[("port", json!(PORT))]);
        let midi = b.node("sig.midi_in", "midi1", &[]);
        let keys = b.node(
            "sig.input",
            "keys1",
            &[("device", json!("keyboard")), ("keys", json!("a space"))],
        );
        let out = b.node("sig.osc_out", "oscout1", &[("port", json!(PORT))]);
        b.wire(keys, out, "in");
        let noise = b.node("img.noise", "noise1", &[]);
        b.bind(noise, "z", osc, "");
        b.bind(noise, "amplitude", midi, "");
        b.bind(noise, "offset", out, "a");
        let project = b.save("live");
        r.load(&project.0, &opts(0..3)).unwrap();
        let warnings = r.take_warnings().join("\n");
        for name in ["osc1", "midi1", "keys1"] {
            assert!(
                warnings.contains(&format!("{name} (")) && warnings.contains("live input"),
                "{name}: {warnings}"
            );
        }
        assert!(warnings.contains("oscout1 (sig.osc_out)"), "{warnings}");
        let first = render(&mut r, 0..3);
        // OSC In did not take the port.
        std::net::UdpSocket::bind(("127.0.0.1", PORT)).expect("port still free");
        let channels = |name: &str| {
            let id = r
                .engine
                .network()
                .nodes()
                .find(|n| n.name == name)
                .unwrap()
                .id;
            r.engine.channels(id).cloned().unwrap_or_default()
        };
        assert!(channels("osc1").is_empty());
        assert!(channels("midi1").is_empty());
        let k = channels("keys1");
        assert_eq!((k.value("a"), k.value("space")), (Some(0.0), Some(0.0)));
        assert_eq!(channels("oscout1"), k, "OSC Out passes through");

        let mut again = renderer().unwrap();
        again.load(&project.0, &opts(0..3)).unwrap();
        assert_eq!(render(&mut again, 0..3), first);
    }

    /// Packs (FR-P4): projects with Pack instances render to identical
    /// frames twice; an expression inside a Pack (renamed for the instance)
    /// and an expression naming a Pack instance's channel both evaluate.
    #[test]
    fn projects_with_packs_render_identically_and_their_expressions_work() {
        let Some(mut a) = renderer() else { return };
        let mut b = renderer().unwrap();
        for name in [
            "pack-chromatic-aberration",
            "pack-beat-flash",
            "pack-audio-pulse",
            "pack-feedback-trails",
            "pack-spark-fountain",
            "pack-curl-swarm",
            "pack-spinning-solid",
            "pack-lfo-bank",
            "pack-film-grain",
            "pack-duotone",
            "pack-colour-grade",
            "pack-rgb-delay",
        ] {
            let dir = example(name);
            a.load(&dir, &opts(0..12)).unwrap();
            b.load(&dir, &opts(0..12)).unwrap();
            let (fa, fb) = (render(&mut a, 0..12), render(&mut b, 0..12));
            assert_eq!(fa, fb, "{name}");
            assert_ne!(fa[0].1, fa[11].1, "{name}: the picture changes");
        }
        a.load(&example("pack-chromatic-aberration"), &opts(0..3))
            .unwrap();
        assert_eq!(a.output_name(), "aberration1");
        render(&mut a, 0..3);
        let id = |r: &Renderer, n: &str| r.engine.network().node_by_name(n).unwrap().id;
        let blue = a
            .engine
            .expression_value(id(&a, "aberration1__blue_shift"), "amount")
            .expect("the expression inside the Pack evaluates");
        let red = a
            .engine
            .channels(id(&a, "wobble"))
            .and_then(|c| c.value("lfo"))
            .unwrap();
        assert!((blue + f64::from(red)).abs() < 1e-6, "{blue} vs {red}");
        a.load(&example("pack-beat-flash"), &opts(0..3)).unwrap();
        render(&mut a, 0..3);
        let kick = a.engine.expression_value(id(&a, "kick"), "rotate");
        assert!(kick.is_some(), "op('flash1')['trigger'] evaluates");
        // A node inside a Pack can be rendered by its path.
        let inner = RenderOptions {
            node: Some("flash1/flash".into()),
            ..opts(0..1)
        };
        a.load(&example("pack-beat-flash"), &inner).unwrap();
        assert_eq!(a.output_name(), "flash1/flash");
        render(&mut a, 0..1);
    }

    /// A Pack's "how much of the effect" parameter at 0 gives back the
    /// image wired into it (Composite's opacity mixes from input B, so the
    /// image must be on B): Vignette's Amount, Neon Edges' and Duotone's
    /// Mix, Colour Grade's Tint Amount, RGB Delay's Mix.
    #[test]
    fn pack_amount_zero_leaves_the_image_alone() {
        let Some(mut r) = renderer() else { return };
        for (name, instance, param, source) in [
            ("pack-vignette", "vignette1", "amount", "clouds"),
            ("pack-neon-edges", "neon1", "mix", "label"),
            ("pack-duotone", "duotone1", "mix", "title"),
            ("pack-colour-grade", "warm", "tint_amount", "source"),
            ("pack-rgb-delay", "delay1", "mix", "shapes"),
        ] {
            let mut project = atd_format::load(&example(name)).unwrap();
            let reg = project.packs.register(&crate::builtin_registry()).unwrap();
            let net = project.networks.get_mut(atd_format::MAIN).unwrap();
            let node = net.node_by_name(instance).unwrap().id;
            net.apply(
                &reg,
                Edit::SetParam {
                    node,
                    param: param.into(),
                    value: Some(Param::constant(json!(0.0))),
                },
            )
            .unwrap();
            // Colour Grade's other controls are not neutral in the example.
            if name == "pack-colour-grade" {
                for p in ["contrast", "gamma", "saturation"] {
                    net.apply(
                        &reg,
                        Edit::SetParam {
                            node,
                            param: p.into(),
                            value: None,
                        },
                    )
                    .unwrap();
                }
            }
            let dir = TempProject(std::env::temp_dir().join(format!(
                "atd-render-{name}-zero-{}.atidarla",
                std::process::id()
            )));
            atd_format::save(&dir.0, &project).unwrap();
            let mut frames = |node: &str| {
                let o = RenderOptions {
                    node: Some(node.into()),
                    ..opts(0..3)
                };
                r.load(&dir.0, &o).unwrap();
                render(&mut r, 0..3)
            };
            let (out, src) = (frames(instance), frames(source));
            for ((i, a), (_, b)) in out.iter().zip(&src) {
                let worst = a
                    .rgba
                    .iter()
                    .zip(&b.rgba)
                    .map(|(x, y)| x.abs_diff(*y))
                    .max()
                    .unwrap();
                assert!(
                    worst <= 2,
                    "{name}: `{param}` 0 changes frame {i} (largest difference {worst}/255)"
                );
            }
        }
    }
    /// Share Out publishes nothing in an offline render (Spout, Windows).
    #[cfg(windows)]
    #[test]
    fn share_out_does_not_publish_offline() {
        let Some(mut r) = renderer() else { return };
        // Render Share Out itself, so it cooks (the output node, tint,
        // does not read it).
        let opts = RenderOptions {
            node: Some("share_out1".into()),
            ..opts(0..4)
        };
        r.load(&example("texture-sharing"), &opts).unwrap();
        render(&mut r, 0..3);
        // Live, the sender would be up by now (it starts on a worker).
        std::thread::sleep(std::time::Duration::from_millis(500));
        render(&mut r, 3..4);
        let senders = atd_interop::spout::SpoutReceiver::list_senders();
        assert!(
            !senders.iter().any(|s| s.starts_with("Atidarla Example")),
            "{senders:?}"
        );
    }
}
