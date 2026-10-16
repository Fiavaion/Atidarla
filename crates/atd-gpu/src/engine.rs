//! The engine: a network plus its undo history, scheduler, operator
//! instances and the latest result of every cooked node (PRD §5.3).
//!
//! Each frame [`Engine::cook`] asks the scheduler which nodes must cook to
//! produce the requested outputs. SIG operators cook on the CPU into reused
//! channel buffers; IMG operators record into one command encoder, which is
//! submitted once, after which replaced textures return to the pool.
//! Feedback inputs see the source's result from the previous frame.
//!
//! Parameters bound to a channel (FR-P2 channel export) read that channel's
//! current value when the node cooks; the scheduler orders and dirties
//! through bindings, so a time-dependent LFO animates whatever it drives.
//!
//! **Status (M1 pre-work):** IMG and SIG wires. GEO/DAT results arrive with
//! their families. Operators are created per node from an [`OpLibrary`] of
//! factories registered by the `atd-ops-*` crates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use atd_core::cook::{CookStat, PlanError, Scheduler};
use atd_core::graph::{Edit, GraphError, Network, NodeId};
use atd_core::history::History;
use atd_core::registry::{Registry, WireType};
use serde_json::Value;

use crate::pool::{TexKey, TexturePool};

/// Usage of every engine texture: render target, sampled, copyable.
pub const ENGINE_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

/// SIG data: named channels, each with `samples` values (1 at control rate).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Channels {
    names: Vec<String>,
    samples: usize,
    /// Samples per second for audio-rate blocks; 0 at control rate.
    rate: f32,
    /// Channel-major: channel `c` is `data[c * samples..(c + 1) * samples]`.
    data: Vec<f32>,
}

impl Channels {
    /// Builds channels with one sample each.
    pub fn single(names: &[&str], values: &[f32]) -> Self {
        let mut c = Self::default();
        c.set_single(names, values);
        c
    }

    /// Replaces the contents with one sample per channel, reusing storage
    /// (no allocation when the channel names are unchanged).
    pub fn set_single(&mut self, names: &[&str], values: &[f32]) {
        assert_eq!(names.len(), values.len(), "one value per channel");
        if self.names.len() != names.len() || self.names.iter().zip(names).any(|(a, b)| a != b) {
            self.names.clear();
            self.names.extend(names.iter().map(|n| (*n).to_owned()));
        }
        self.samples = 1;
        self.rate = 0.0;
        self.data.clear();
        self.data.extend_from_slice(values);
    }

    /// Shapes the buffer as an audio-rate block of `samples` per channel at
    /// `rate` Hz (zeroed), reusing storage; fill it with [`Self::channel_mut`].
    pub fn set_block(&mut self, names: &[&str], rate: f32, samples: usize) {
        if self.names.len() != names.len() || self.names.iter().zip(names).any(|(a, b)| a != b) {
            self.names.clear();
            self.names.extend(names.iter().map(|n| (*n).to_owned()));
        }
        self.samples = samples;
        self.rate = rate;
        self.data.clear();
        self.data.resize(names.len() * samples, 0.0);
    }

    /// Like [`Self::set_block`], taking the channel names from `like`
    /// (no allocation when they already match).
    pub fn set_block_like(&mut self, like: &Channels, rate: f32, samples: usize) {
        if self.names != like.names {
            self.names.clone_from(&like.names);
        }
        self.samples = samples;
        self.rate = rate;
        self.data.clear();
        self.data.resize(self.names.len() * samples, 0.0);
    }

    /// Reserves room for `values` samples in total, so growing blocks up to
    /// that size does not reallocate.
    pub fn reserve(&mut self, values: usize) {
        self.data.reserve(values.saturating_sub(self.data.len()));
    }

    /// Samples per second of an audio-rate block; 0 at control rate.
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Rebuilds one-sample channels in place: [`ChannelWriter::push`] each
    /// channel; the writer trims leftovers when dropped. Name strings are
    /// reused, so a steady set of channels does not allocate.
    pub fn writer(&mut self) -> ChannelWriter<'_> {
        if self.samples != 1 {
            // Previous contents were a block; shrink to one sample per channel.
            self.data.truncate(self.names.len().min(self.data.len()));
            self.samples = 1;
        }
        ChannelWriter { ch: self, n: 0 }
    }

    /// Replaces the contents with a copy of `other`, reusing storage.
    pub fn copy_from(&mut self, other: &Channels) {
        if self.names != other.names {
            self.names.clone_from(&other.names);
        }
        self.samples = other.samples;
        self.rate = other.rate;
        self.data.clear();
        self.data.extend_from_slice(&other.data);
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    pub fn name(&self, c: usize) -> &str {
        &self.names[c]
    }

    pub fn samples(&self) -> usize {
        self.samples
    }

    /// All samples of channel `c`.
    pub fn channel(&self, c: usize) -> &[f32] {
        &self.data[c * self.samples..(c + 1) * self.samples]
    }

    /// Mutable samples of channel `c`.
    pub fn channel_mut(&mut self, c: usize) -> &mut [f32] {
        let s = self.samples;
        &mut self.data[c * s..(c + 1) * s]
    }

    /// Index of a channel by name; an empty name means the first channel.
    pub fn index(&self, name: &str) -> Option<usize> {
        if name.is_empty() {
            return (!self.names.is_empty()).then_some(0);
        }
        self.names.iter().position(|n| n == name)
    }

    /// The latest sample of channel `name` (empty: the first channel).
    pub fn value(&self, name: &str) -> Option<f32> {
        let c = self.index(name)?;
        self.channel(c).last().copied()
    }

    /// The latest sample of channel `c`.
    pub fn last(&self, c: usize) -> f32 {
        self.channel(c).last().copied().unwrap_or(0.0)
    }
}

/// Builds [`Channels`] in place; see [`Channels::writer`].
#[derive(Debug)]
pub struct ChannelWriter<'a> {
    ch: &'a mut Channels,
    n: usize,
}

impl ChannelWriter<'_> {
    pub fn push(&mut self, name: &str, value: f32) {
        let ch = &mut *self.ch;
        if self.n < ch.names.len() {
            if ch.names[self.n] != name {
                ch.names[self.n].clear();
                ch.names[self.n].push_str(name);
            }
        } else {
            ch.names.push(name.to_owned());
        }
        if self.n < ch.data.len() {
            ch.data[self.n] = value;
        } else {
            ch.data.push(value);
        }
        self.n += 1;
    }
}

impl Drop for ChannelWriter<'_> {
    fn drop(&mut self) {
        self.ch.names.truncate(self.n);
        self.ch.data.truncate(self.n);
        self.ch.samples = 1;
        self.ch.rate = 0.0;
    }
}

/// Read access to a node's parameters, with schema defaults and channel
/// bindings resolved.
#[derive(Debug, Clone, Copy)]
pub struct Params<'a> {
    net: &'a Network,
    registry: &'a Registry,
    sig: &'a HashMap<NodeId, Channels>,
    node: NodeId,
    /// Project folder that relative paths are relative to.
    project_dir: Option<&'a Path>,
}

impl<'a> Params<'a> {
    /// Parameters of `node`, resolving bindings against `sig` (for tests and
    /// tools; the engine builds these itself when cooking).
    pub fn new(
        net: &'a Network,
        registry: &'a Registry,
        sig: &'a HashMap<NodeId, Channels>,
        node: NodeId,
    ) -> Self {
        Self {
            net,
            registry,
            sig,
            node,
            project_dir: None,
        }
    }

    /// The same parameters with relative paths resolved against `dir`.
    pub fn with_project_dir(mut self, dir: Option<&'a Path>) -> Self {
        self.project_dir = dir;
        self
    }

    /// A path parameter's text as a file path: absolute paths as they are,
    /// relative ones relative to the project folder (PRD §5.5). This
    /// allocates, so operators call it when the text changes, not per frame.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        match self.project_dir {
            Some(dir) if p.is_relative() && !path.is_empty() => dir.join(p),
            _ => p.to_owned(),
        }
    }

    /// The stored value (or schema default), ignoring bindings.
    pub fn value(&self, name: &str) -> Option<&'a Value> {
        self.net.param_value(self.registry, self.node, name)
    }

    /// The bound channel's current value, if the parameter is bound and the
    /// channel exists.
    pub fn bound(&self, name: &str) -> Option<f32> {
        let bind = self.net.node(self.node)?.params.get(name)?.bind.as_ref()?;
        self.sig.get(&bind.node)?.value(&bind.channel)
    }

    pub fn f32(&self, name: &str) -> f32 {
        self.bound(name)
            .unwrap_or_else(|| self.value(name).and_then(Value::as_f64).unwrap_or(0.0) as f32)
    }

    pub fn i64(&self, name: &str) -> i64 {
        match self.bound(name) {
            Some(v) => v.round() as i64,
            None => self.value(name).and_then(Value::as_i64).unwrap_or(0),
        }
    }

    pub fn bool(&self, name: &str) -> bool {
        match self.bound(name) {
            Some(v) => v > 0.5,
            None => self.value(name).and_then(Value::as_bool).unwrap_or(false),
        }
    }

    pub fn str(&self, name: &str) -> &'a str {
        self.value(name).and_then(Value::as_str).unwrap_or("")
    }

    /// A `Vec`/`Colour` parameter as 4 floats (missing components are 0,
    /// except alpha, which defaults to 1 for 3-component values).
    pub fn vec4(&self, name: &str) -> [f32; 4] {
        let mut out = [0.0, 0.0, 0.0, 1.0];
        if let Some(a) = self.value(name).and_then(Value::as_array) {
            for (o, v) in out.iter_mut().zip(a) {
                *o = v.as_f64().unwrap_or(0.0) as f32;
            }
        }
        out
    }
}

/// Everything an IMG operator needs to cook one frame.
#[derive(Debug)]
pub struct CookCtx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// One entry per input socket in schema order; `None` if unconnected,
    /// not an IMG input, or the source has no result.
    pub inputs: &'a [Option<wgpu::Texture>],
    /// SIG inputs by input index (`None` for IMG sockets), for operators
    /// that turn channels into images.
    pub sig_inputs: SigInputs<'a>,
    /// A 1×1 transparent black texture, for unconnected inputs.
    pub black: &'a wgpu::Texture,
    pub output: &'a wgpu::Texture,
    pub params: Params<'a>,
    /// Seconds since the engine's time origin.
    pub time: f64,
    pub frame: u64,
    /// Set by an operator whose result is provisional (e.g. a shader still
    /// compiling on a worker thread): the engine cooks it again next frame
    /// even if nothing upstream changed.
    pub cook_again: bool,
}

impl CookCtx<'_> {
    /// Input `i`, or the black texture when it is missing.
    pub fn input_or_black(&self, i: usize) -> &wgpu::Texture {
        self.inputs
            .get(i)
            .and_then(Option::as_ref)
            .unwrap_or(self.black)
    }
}

/// A live IMG operator instance (one per node).
pub trait GpuOp: Send {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String>;

    /// The output size this operator wants, given its first IMG input's
    /// size (if connected) and the project size. `None` (the default)
    /// follows the input, or the project size for generators.
    fn output_size(
        &self,
        _params: &Params<'_>,
        _input: Option<(u32, u32)>,
        _project: (u32, u32),
    ) -> Option<(u32, u32)> {
        None
    }
}

/// SIG inputs of a node, by input index. Does not allocate.
#[derive(Debug, Clone, Copy)]
pub struct SigInputs<'a> {
    sources: &'a [Option<NodeId>],
    sig: &'a HashMap<NodeId, Channels>,
}

impl<'a> SigInputs<'a> {
    /// Inputs given by source node per input index (for tests and tools).
    pub fn new(sources: &'a [Option<NodeId>], sig: &'a HashMap<NodeId, Channels>) -> Self {
        Self { sources, sig }
    }

    /// Channels on input `i`, if connected and cooked.
    pub fn get(&self, i: usize) -> Option<&'a Channels> {
        self.sources
            .get(i)
            .copied()
            .flatten()
            .and_then(|n| self.sig.get(&n))
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// Everything a SIG operator needs to cook one frame.
#[derive(Debug)]
pub struct SigCtx<'a> {
    pub inputs: SigInputs<'a>,
    /// This node's channels from its previous cook, to overwrite in place.
    pub output: &'a mut Channels,
    pub params: Params<'a>,
    /// Seconds since the engine's time origin.
    pub time: f64,
    /// Seconds since the previous engine frame (0 on the first).
    pub dt: f64,
    pub frame: u64,
}

/// A live SIG operator instance (one per node). May keep state between
/// cooks (lag, integrators, history).
pub trait SigOp: Send {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String>;
}

/// Context for an IMG→SIG operator (image analysis): GPU access to its IMG
/// inputs, recording into the frame's command encoder, and channels out.
#[derive(Debug)]
pub struct ImgToSigCtx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// One entry per input socket; `None` if unconnected or not IMG.
    pub inputs: &'a [Option<wgpu::Texture>],
    /// An IMG input's source was cooked earlier this frame (its image may
    /// have changed). False when the node is only cooking again to collect
    /// results.
    pub inputs_cooked: bool,
    pub output: &'a mut Channels,
    pub params: Params<'a>,
    pub time: f64,
    pub frame: u64,
    /// Set while results are still on their way back from the GPU: the
    /// engine cooks the node again next frame.
    pub cook_again: bool,
}

/// An operator that reads images on the GPU and outputs channels. Results
/// come back asynchronously: [`Self::after_submit`] runs once the frame's
/// commands are submitted, where readback buffers may be mapped.
pub trait ImgToSigOp: Send {
    fn cook(&mut self, ctx: &mut ImgToSigCtx<'_>) -> Result<(), String>;
    fn after_submit(&mut self) {}
}

/// Creates an IMG→SIG operator instance.
pub type ImgToSigFactory = Box<dyn Fn(&wgpu::Device) -> Box<dyn ImgToSigOp> + Send + Sync>;

/// Creates an IMG operator instance for the engine's device and output format.
pub type OpFactory =
    Box<dyn Fn(&wgpu::Device, wgpu::TextureFormat) -> Box<dyn GpuOp> + Send + Sync>;

/// Creates a SIG operator instance.
pub type SigFactory = Box<dyn Fn() -> Box<dyn SigOp> + Send + Sync>;

/// Factories for every operator type the engine can run.
#[derive(Default)]
pub struct OpLibrary {
    factories: HashMap<String, OpFactory>,
    sig_factories: HashMap<String, SigFactory>,
    analysis_factories: HashMap<String, ImgToSigFactory>,
}

impl std::fmt::Debug for OpLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ids: Vec<_> = self
            .factories
            .keys()
            .chain(self.sig_factories.keys())
            .chain(self.analysis_factories.keys())
            .collect();
        ids.sort();
        f.debug_struct("OpLibrary").field("types", &ids).finish()
    }
}

impl OpLibrary {
    /// Registers an IMG (GPU) operator implementation.
    pub fn add(
        &mut self,
        type_id: impl Into<String>,
        factory: impl Fn(&wgpu::Device, wgpu::TextureFormat) -> Box<dyn GpuOp> + Send + Sync + 'static,
    ) {
        self.factories.insert(type_id.into(), Box::new(factory));
    }

    /// Registers a SIG (CPU channel) operator implementation.
    pub fn add_sig(
        &mut self,
        type_id: impl Into<String>,
        factory: impl Fn() -> Box<dyn SigOp> + Send + Sync + 'static,
    ) {
        self.sig_factories.insert(type_id.into(), Box::new(factory));
    }

    /// Registers an IMG→SIG (image analysis) operator implementation.
    pub fn add_img_to_sig(
        &mut self,
        type_id: impl Into<String>,
        factory: impl Fn(&wgpu::Device) -> Box<dyn ImgToSigOp> + Send + Sync + 'static,
    ) {
        self.analysis_factories
            .insert(type_id.into(), Box::new(factory));
    }

    pub fn contains(&self, type_id: &str) -> bool {
        self.factories.contains_key(type_id)
            || self.sig_factories.contains_key(type_id)
            || self.analysis_factories.contains_key(type_id)
    }
}

/// Output resolution and format of the engine's textures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            format: wgpu::TextureFormat::Rgba8Unorm,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error(transparent)]
    Plan(#[from] PlanError),
}

pub struct Engine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    registry: Registry,
    library: OpLibrary,
    config: EngineConfig,
    net: Network,
    history: History,
    scheduler: Scheduler,
    gpu_ops: HashMap<NodeId, Box<dyn GpuOp>>,
    sig_ops: HashMap<NodeId, Box<dyn SigOp>>,
    analysis_ops: HashMap<NodeId, Box<dyn ImgToSigOp>>,
    results: HashMap<NodeId, wgpu::Texture>,
    sig: HashMap<NodeId, Channels>,
    errors: HashMap<NodeId, String>,
    pool: TexturePool,
    black: wgpu::Texture,
    frame: u64,
    last_time: Option<f64>,
    // Per-frame scratch, reused.
    plan: Vec<NodeId>,
    requested: Vec<NodeId>,
    inputs: Vec<Option<wgpu::Texture>>,
    sig_sources: Vec<Option<NodeId>>,
    /// Nodes that asked to cook again next frame ([`CookCtx::cook_again`]).
    again: Vec<NodeId>,
    released: Vec<wgpu::Texture>,
    /// Folder of the open project: relative paths are relative to it.
    project_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("nodes", &self.net.len())
            .field("frame", &self.frame)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Engine {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        registry: Registry,
        library: OpLibrary,
        config: EngineConfig,
    ) -> Self {
        let black = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atd-black"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let pool = TexturePool::new(device.clone());
        Self {
            device,
            queue,
            registry,
            library,
            config,
            net: Network::default(),
            history: History::default(),
            scheduler: Scheduler::default(),
            gpu_ops: HashMap::new(),
            sig_ops: HashMap::new(),
            analysis_ops: HashMap::new(),
            results: HashMap::new(),
            sig: HashMap::new(),
            errors: HashMap::new(),
            pool,
            black,
            frame: 0,
            last_time: None,
            plan: Vec::new(),
            requested: Vec::new(),
            inputs: Vec::new(),
            sig_sources: Vec::new(),
            again: Vec::new(),
            released: Vec::new(),
            project_dir: None,
        }
    }

    /// Sets the folder relative paths in parameters are relative to (the
    /// open project's). Set it before [`Engine::load`]: operators resolve a
    /// path when it changes.
    pub fn set_project_dir(&mut self, dir: Option<PathBuf>) {
        self.project_dir = dir;
    }

    pub fn project_dir(&self) -> Option<&Path> {
        self.project_dir.as_deref()
    }

    /// Changes the project resolution (generators' output size); every
    /// node cooks again.
    pub fn set_resolution(&mut self, width: u32, height: u32) {
        let max = self.device.limits().max_texture_dimension_2d;
        let (width, height) = (width.clamp(1, max), height.clamp(1, max));
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        let all: Vec<NodeId> = self.net.nodes().map(|n| n.id).collect();
        self.scheduler.mark_dirty(&self.net, all);
    }

    pub fn network(&self) -> &Network {
        &self.net
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn pool(&self) -> &TexturePool {
        &self.pool
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Reserves an ID for a node about to be added with [`Edit::AddNode`].
    pub fn alloc_id(&mut self) -> NodeId {
        self.net.alloc_id()
    }

    /// Reserves an ID for a new comment box.
    pub fn alloc_comment_id(&mut self) -> atd_core::graph::CommentId {
        self.net.alloc_comment_id()
    }

    /// Applies `edits` as one undo step.
    pub fn edit(
        &mut self,
        label: &str,
        edits: impl IntoIterator<Item = Edit>,
    ) -> Result<(), EngineError> {
        let committed = self
            .history
            .commit(&mut self.net, &self.registry, label, edits)?;
        self.after_change(committed.dirty);
        Ok(())
    }

    /// Applies `edits`, merging with the previous step while `key` repeats
    /// and the history is not sealed (one undo step per gesture).
    pub fn edit_coalesced(
        &mut self,
        label: &str,
        key: &str,
        edits: impl IntoIterator<Item = Edit>,
    ) -> Result<(), EngineError> {
        let committed =
            self.history
                .commit_coalesced(&mut self.net, &self.registry, label, key, edits)?;
        self.after_change(committed.dirty);
        Ok(())
    }

    /// Ends the current gesture (see [`Self::edit_coalesced`]).
    pub fn seal_history(&mut self) {
        self.history.seal();
    }

    pub fn undo(&mut self) -> Result<bool, EngineError> {
        let Some(c) = self.history.undo(&mut self.net, &self.registry)? else {
            return Ok(false);
        };
        self.after_change(c.dirty);
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, EngineError> {
        let Some(c) = self.history.redo(&mut self.net, &self.registry)? else {
            return Ok(false);
        };
        self.after_change(c.dirty);
        Ok(true)
    }

    fn after_change(&mut self, dirty: Vec<NodeId>) {
        let alive = |id: &NodeId| self.net.node(*id).is_some();
        let removed: Vec<NodeId> = self
            .gpu_ops
            .keys()
            .chain(self.sig_ops.keys())
            .chain(self.analysis_ops.keys())
            .chain(self.results.keys())
            .chain(self.sig.keys())
            .copied()
            .filter(|id| !alive(id))
            .collect();
        for id in removed {
            self.gpu_ops.remove(&id);
            self.sig_ops.remove(&id);
            self.analysis_ops.remove(&id);
            self.sig.remove(&id);
            self.errors.remove(&id);
            self.scheduler.forget(id);
            if let Some(t) = self.results.remove(&id) {
                // Not yet safe to reuse if a submitted frame still reads it;
                // it joins the next frame's release list.
                self.released.push(t);
            }
        }
        let dirty: Vec<NodeId> = dirty
            .into_iter()
            .filter(|id| self.net.node(*id).is_some())
            .collect();
        self.scheduler.mark_dirty(&self.net, dirty);
    }

    /// Cooks what `requested` needs at `time` (seconds) and submits it.
    /// Returns how many nodes cooked (0 when everything was cached).
    pub fn cook(&mut self, requested: &[NodeId], time: f64) -> Result<usize, EngineError> {
        if !self.again.is_empty() {
            let net = &self.net;
            let again = self
                .again
                .iter()
                .copied()
                .filter(|id| net.node(*id).is_some());
            self.scheduler.mark_dirty(net, again);
            self.again.clear();
        }
        let Self {
            device,
            queue,
            registry,
            library,
            config,
            net,
            scheduler,
            gpu_ops,
            sig_ops,
            analysis_ops,
            results,
            sig,
            errors,
            pool,
            black,
            frame,
            last_time,
            plan,
            inputs,
            sig_sources,
            released,
            again,
            project_dir,
            ..
        } = self;
        scheduler.plan(net, registry, requested, plan)?;
        let dt = last_time.map_or(0.0, |t| (time - t).max(0.0));
        *last_time = Some(time);
        let key = TexKey {
            width: config.width,
            height: config.height,
            format: config.format,
            usage: ENGINE_USAGE,
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("atd-cook"),
        });

        for &id in plan.iter() {
            let start = Instant::now();
            let node = net.node(id).expect("planned nodes exist");
            let Some(schema) = registry.get(&node.type_id) else {
                continue;
            };
            let is_sig = schema
                .outputs
                .first()
                .is_some_and(|o| o.wire == WireType::Sig);

            if is_sig && let Some(factory) = library.analysis_factories.get(&node.type_id) {
                let op = analysis_ops.entry(id).or_insert_with(|| factory(device));
                inputs.clear();
                let mut inputs_cooked = false;
                for socket in &schema.inputs {
                    let src = net
                        .inputs_of(id)
                        .find(|(name, _)| *name == socket.name)
                        .map(|(_, src)| src.node)
                        .filter(|_| socket.wire == WireType::Img);
                    // The plan is in dependency order: sources come first.
                    inputs_cooked |= src.is_some_and(|s| plan.contains(&s));
                    inputs.push(src.and_then(|s| results.get(&s)).cloned());
                }
                let mut out = sig.remove(&id).unwrap_or_default();
                let mut ctx = ImgToSigCtx {
                    device,
                    queue,
                    encoder: &mut encoder,
                    inputs,
                    inputs_cooked,
                    output: &mut out,
                    params: Params {
                        net,
                        registry,
                        sig,
                        node: id,
                        project_dir: project_dir.as_deref(),
                    },
                    time,
                    frame: *frame,
                    cook_again: false,
                };
                let result = op.cook(&mut ctx);
                if ctx.cook_again {
                    again.push(id);
                }
                match result {
                    Ok(()) => {
                        errors.remove(&id);
                    }
                    Err(e) => {
                        errors.insert(id, e);
                    }
                }
                sig.insert(id, out);
                scheduler.record(id, start.elapsed());
                continue;
            }

            if is_sig {
                if let std::collections::hash_map::Entry::Vacant(slot) = sig_ops.entry(id) {
                    match library.sig_factories.get(&node.type_id) {
                        Some(f) => {
                            slot.insert(f());
                        }
                        None => {
                            errors.insert(id, format!("no implementation for `{}`", node.type_id));
                            sig.remove(&id);
                            scheduler.record(id, start.elapsed());
                            continue;
                        }
                    }
                }
                sig_sources.clear();
                for socket in &schema.inputs {
                    let src = net
                        .inputs_of(id)
                        .find(|(name, _)| *name == socket.name)
                        .map(|(_, src)| src.node);
                    sig_sources.push(src.filter(|_| socket.wire == WireType::Sig));
                }
                let mut out = sig.remove(&id).unwrap_or_default();
                let mut ctx = SigCtx {
                    inputs: SigInputs {
                        sources: sig_sources,
                        sig,
                    },
                    output: &mut out,
                    params: Params {
                        net,
                        registry,
                        sig,
                        node: id,
                        project_dir: project_dir.as_deref(),
                    },
                    time,
                    dt,
                    frame: *frame,
                };
                let op = sig_ops.get_mut(&id).expect("instance created above");
                match op.cook(&mut ctx) {
                    Ok(()) => {
                        errors.remove(&id);
                    }
                    Err(e) => {
                        errors.insert(id, e);
                    }
                }
                sig.insert(id, out);
                scheduler.record(id, start.elapsed());
                continue;
            }

            if let std::collections::hash_map::Entry::Vacant(slot) = gpu_ops.entry(id) {
                match library.factories.get(&node.type_id) {
                    Some(f) => {
                        slot.insert(f(device, config.format));
                    }
                    None => {
                        errors.insert(id, format!("no GPU implementation for `{}`", node.type_id));
                        if let Some(old) = results.remove(&id) {
                            released.push(old);
                        }
                        scheduler.record(id, start.elapsed());
                        continue;
                    }
                }
            }

            inputs.clear();
            sig_sources.clear();
            // Size of the first connected IMG input (feedback inputs carry
            // last frame's image and do not set the size).
            let mut input_size = None;
            for socket in &schema.inputs {
                let src = net
                    .inputs_of(id)
                    .find(|(name, _)| *name == socket.name)
                    .map(|(_, src)| src.node);
                let tex = src.and_then(|s| results.get(&s)).cloned();
                if input_size.is_none()
                    && socket.wire == WireType::Img
                    && !socket.feedback
                    && let Some(t) = &tex
                {
                    input_size = Some((t.width(), t.height()));
                }
                inputs.push(tex);
                sig_sources.push(src.filter(|_| socket.wire == WireType::Sig));
            }

            let project = (config.width, config.height);
            let wanted = gpu_ops
                .get(&id)
                .and_then(|op| {
                    op.output_size(
                        &Params {
                            net,
                            registry,
                            sig,
                            node: id,
                            project_dir: project_dir.as_deref(),
                        },
                        input_size,
                        project,
                    )
                })
                .or(input_size)
                .unwrap_or(project);
            let max = device.limits().max_texture_dimension_2d;
            let output = pool.acquire(TexKey {
                width: wanted.0.clamp(1, max),
                height: wanted.1.clamp(1, max),
                ..key
            });
            let mut ctx = CookCtx {
                device,
                queue,
                encoder: &mut encoder,
                inputs,
                sig_inputs: SigInputs {
                    sources: sig_sources,
                    sig,
                },
                black,
                output: &output,
                params: Params {
                    net,
                    registry,
                    sig,
                    node: id,
                    project_dir: project_dir.as_deref(),
                },
                time,
                frame: *frame,
                cook_again: false,
            };
            let op = gpu_ops.get_mut(&id).expect("instance created above");
            let result = op.cook(&mut ctx);
            if ctx.cook_again {
                again.push(id);
            }
            match result {
                Ok(()) => {
                    errors.remove(&id);
                    if let Some(old) = results.insert(id, output) {
                        released.push(old);
                    }
                }
                Err(e) => {
                    errors.insert(id, e);
                    released.push(output);
                }
            }
            scheduler.record(id, start.elapsed());
        }
        inputs.clear();

        queue.submit([encoder.finish()]);
        for id in plan.iter() {
            if let Some(op) = analysis_ops.get_mut(id) {
                op.after_submit();
            }
        }
        for t in released.drain(..) {
            pool.release(t);
        }
        pool.end_frame();
        *frame += 1;
        Ok(plan.len())
    }

    /// Replaces the network (e.g. a loaded project) after validating it
    /// against the registry. Undo history, operator instances and cached
    /// results start afresh; on error the current network is kept.
    pub fn load(&mut self, net: Network) -> Result<(), EngineError> {
        let net = net.validated(&self.registry)?;
        self.net = net;
        self.history = History::default();
        self.scheduler = Scheduler::default();
        self.gpu_ops.clear();
        self.sig_ops.clear();
        self.analysis_ops.clear();
        self.sig.clear();
        self.errors.clear();
        self.again.clear();
        self.last_time = None;
        // Textures may still be read by a submitted frame: release them
        // with the next frame's.
        self.released.extend(self.results.drain().map(|(_, t)| t));
        Ok(())
    }

    /// True when an operator's last result was provisional (for example a
    /// shader still compiling) and it will cook again on the next frame.
    pub fn pending(&self) -> bool {
        !self.again.is_empty()
    }

    /// Cooks one frame of the running patch: the viewed node (if any) plus
    /// every sink (operators with external side effects, such as OSC Out),
    /// which must run even when nothing downstream is viewed.
    pub fn cook_frame(&mut self, viewed: Option<NodeId>, time: f64) -> Result<usize, EngineError> {
        let mut requested = std::mem::take(&mut self.requested);
        requested.clear();
        requested.extend(viewed);
        for node in self.net.nodes() {
            if self.registry.get(&node.type_id).is_some_and(|s| s.sink) && Some(node.id) != viewed {
                requested.push(node.id);
            }
        }
        let result = self.cook(&requested, time);
        self.requested = requested;
        result
    }

    /// The latest image of an IMG node, if it has cooked successfully.
    pub fn result(&self, node: NodeId) -> Option<&wgpu::Texture> {
        self.results.get(&node)
    }

    /// The latest channels of a SIG node.
    pub fn channels(&self, node: NodeId) -> Option<&Channels> {
        self.sig.get(&node)
    }

    /// The last cook error of `node`, if any.
    pub fn error(&self, node: NodeId) -> Option<&str> {
        self.errors.get(&node).map(String::as_str)
    }

    pub fn stat(&self, node: NodeId) -> Option<&CookStat> {
        self.scheduler.stat(node)
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_reuse_storage_and_resolve_names() {
        let mut c = Channels::single(&["x", "y"], &[1.0, 2.0]);
        assert_eq!(c.value("y"), Some(2.0));
        assert_eq!(c.value(""), Some(1.0), "empty name is the first channel");
        assert_eq!(c.value("z"), None);
        let names_ptr = c.names.as_ptr();
        let data_ptr = c.data.as_ptr();
        c.set_single(&["x", "y"], &[3.0, 4.0]);
        assert_eq!(c.names.as_ptr(), names_ptr, "names reused");
        assert_eq!(c.data.as_ptr(), data_ptr, "data reused");
        assert_eq!((c.last(0), c.last(1)), (3.0, 4.0));
        let mut d = Channels::default();
        d.copy_from(&c);
        assert_eq!(d, c);

        // The writer reuses name strings and trims leftovers.
        let mut w = c.writer();
        w.push("x", 9.0);
        drop(w);
        assert_eq!(c, Channels::single(&["x"], &[9.0]));
    }
}
