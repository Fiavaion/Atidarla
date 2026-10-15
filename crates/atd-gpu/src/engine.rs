//! The GPU engine: a network plus its undo history, scheduler, operator
//! instances and the latest result of every cooked node (PRD §5.3).
//!
//! Each frame [`Engine::cook`] asks the scheduler which nodes must cook to
//! produce the requested outputs, records every operator into one command
//! encoder, submits it once, and only then returns replaced textures to the
//! pool. Feedback inputs see the source's result from the previous frame.
//!
//! **Status (M1 pre-work):** IMG (texture) wires only; SIG/GEO/DAT results
//! arrive with their families. Operators are created per node from an
//! [`OpLibrary`] of factories registered by the `atd-ops-*` crates.

use std::collections::HashMap;
use std::time::Instant;

use atd_core::cook::{CookStat, PlanError, Scheduler};
use atd_core::graph::{Edit, GraphError, Network, NodeId};
use atd_core::history::History;
use atd_core::registry::Registry;
use serde_json::Value;

use crate::pool::{TexKey, TexturePool};

/// Usage of every engine texture: render target, sampled, copyable.
pub const ENGINE_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

/// Read access to a node's parameters, with schema defaults.
#[derive(Debug, Clone, Copy)]
pub struct Params<'a> {
    net: &'a Network,
    registry: &'a Registry,
    node: NodeId,
}

impl<'a> Params<'a> {
    pub fn value(&self, name: &str) -> Option<&'a Value> {
        self.net.param_value(self.registry, self.node, name)
    }

    pub fn f32(&self, name: &str) -> f32 {
        self.value(name).and_then(Value::as_f64).unwrap_or(0.0) as f32
    }

    pub fn i64(&self, name: &str) -> i64 {
        self.value(name).and_then(Value::as_i64).unwrap_or(0)
    }

    pub fn bool(&self, name: &str) -> bool {
        self.value(name).and_then(Value::as_bool).unwrap_or(false)
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

/// Everything an operator needs to cook one frame.
#[derive(Debug)]
pub struct CookCtx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// One entry per input socket in schema order; `None` if unconnected or
    /// the source has no result.
    pub inputs: &'a [Option<wgpu::Texture>],
    /// A 1×1 transparent black texture, for unconnected inputs.
    pub black: &'a wgpu::Texture,
    pub output: &'a wgpu::Texture,
    pub params: Params<'a>,
    /// Seconds since the engine's time origin.
    pub time: f64,
    pub frame: u64,
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

/// A live operator instance (one per node).
pub trait GpuOp: Send {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String>;
}

/// Creates an operator instance for the engine's device and output format.
pub type OpFactory =
    Box<dyn Fn(&wgpu::Device, wgpu::TextureFormat) -> Box<dyn GpuOp> + Send + Sync>;

/// Factories for every operator type the engine can run.
#[derive(Default)]
pub struct OpLibrary {
    factories: HashMap<String, OpFactory>,
}

impl std::fmt::Debug for OpLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ids: Vec<_> = self.factories.keys().collect();
        ids.sort();
        f.debug_struct("OpLibrary").field("types", &ids).finish()
    }
}

impl OpLibrary {
    pub fn add(
        &mut self,
        type_id: impl Into<String>,
        factory: impl Fn(&wgpu::Device, wgpu::TextureFormat) -> Box<dyn GpuOp> + Send + Sync + 'static,
    ) {
        self.factories.insert(type_id.into(), Box::new(factory));
    }

    pub fn contains(&self, type_id: &str) -> bool {
        self.factories.contains_key(type_id)
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
    ops: HashMap<NodeId, Box<dyn GpuOp>>,
    results: HashMap<NodeId, wgpu::Texture>,
    errors: HashMap<NodeId, String>,
    pool: TexturePool,
    black: wgpu::Texture,
    frame: u64,
    // Per-frame scratch, reused.
    plan: Vec<NodeId>,
    inputs: Vec<Option<wgpu::Texture>>,
    released: Vec<wgpu::Texture>,
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
            ops: HashMap::new(),
            results: HashMap::new(),
            errors: HashMap::new(),
            pool,
            black,
            frame: 0,
            plan: Vec::new(),
            inputs: Vec::new(),
            released: Vec::new(),
        }
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
            .ops
            .keys()
            .chain(self.results.keys())
            .copied()
            .filter(|id| !alive(id))
            .collect();
        for id in removed {
            self.ops.remove(&id);
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
        let Self {
            device,
            queue,
            registry,
            library,
            config,
            net,
            scheduler,
            ops,
            results,
            errors,
            pool,
            black,
            frame,
            plan,
            inputs,
            released,
            ..
        } = self;
        scheduler.plan(net, registry, requested, plan)?;
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

            if let std::collections::hash_map::Entry::Vacant(slot) = ops.entry(id) {
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
            for socket in &schema.inputs {
                let src = net
                    .inputs_of(id)
                    .find(|(name, _)| *name == socket.name)
                    .map(|(_, src)| src.node);
                inputs.push(src.and_then(|s| results.get(&s)).cloned());
            }

            let output = pool.acquire(key);
            let mut ctx = CookCtx {
                device,
                queue,
                encoder: &mut encoder,
                inputs,
                black,
                output: &output,
                params: Params {
                    net,
                    registry,
                    node: id,
                },
                time,
                frame: *frame,
            };
            let op = ops.get_mut(&id).expect("instance created above");
            match op.cook(&mut ctx) {
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
        for t in released.drain(..) {
            pool.release(t);
        }
        *frame += 1;
        Ok(plan.len())
    }

    /// The latest result of `node`, if it has cooked successfully.
    pub fn result(&self, node: NodeId) -> Option<&wgpu::Texture> {
        self.results.get(&node)
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

impl Engine {
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
}
