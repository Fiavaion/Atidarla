//! `pnt.kernel`: a per-point kernel written in the Kernel DSL (A12,
//! `docs/kernel-dsl.md`), inline or in a `.atk` file that reloads when saved;
//! and the nodes of a project's `kernels/<name>.atk` operators, which run the
//! same [`PointsRunner`] with their parameters as the `ch()` values.
//!
//! Compiling (the `atd-kernel` front end, naga, then the driver) runs on a
//! worker thread and a file is read by its own thread, so neither touches the
//! render thread (rule 1). The last kernel that compiled keeps running while a
//! new one compiles and when it fails. Simulation state lives on the GEO wire
//! (a Solver loop), not in the operator, so swapping the kernel keeps it
//! (FR-K4): an attribute keeps its buffer and values when the new kernel
//! binds it with the same type, and one bound with a new type starts at zero.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use atd_core::geo::{AttrClass, AttrType, Scalar, header};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::Geometry;
use atd_gpu::geo::kernels::BindGroupCache;
use atd_kernel::{Access, AttrSchema, CompileOptions, Compiled, FrameGlobals, Target};
use atd_ops_img::kernel::{
    ChValues, KernelSource, ProjectSource, SharedSource, Text, defaults, fill_values, render_errors,
};

use crate::kit::{self, Missing};

const DEFAULT: &str = "// Runs once for every point. Read and write attributes with @name.
@P.y += sin(@P.x * 3.0 + @Time * 2.0) * 0.5 * @TimeInc;
";

/// Most attribute buffers a kernel binds (with its snapshots).
const MAX_ATTRS: usize = 30;

pub fn schema() -> OperatorSchema {
    let mut s = crate::pnt(
        "kernel",
        "Kernel",
        "A point kernel in the Kernel DSL (VEX-flavoured, see the Kernel DSL reference): runs once for every point, reading and writing attributes as `@name` (`v@v`, `f@age`, …; missing ones are created at zero), with `@Time`, `@TimeInc`, `@Frame`, noise, `point()` and `nearpoints()`. `ch(\"name\", default)` parameters take their defaults here; a kernel file in the project's `kernels/` folder becomes an operator with controls for them. Put it inside a Solver loop and its attributes persist from frame to frame; editing the code keeps the simulation running. The last kernel that compiled keeps running while you edit; errors name the line and column.",
        &[("in", false)],
        vec![
            ParamSchema::string("code", "Code", DEFAULT)
                .help("The kernel source, used when File is empty.")
                .multiline(),
            ParamSchema::path("file", "File")
                .help("Path to a `.atk` file holding the kernel; reloads when it is saved."),
        ],
        150,
    );
    s.time_dependent = true;
    s
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Kernel {
        source: KernelSource::default(),
        runner: PointsRunner::new(device),
    })
}

/// A node of a project Points kernel operator reading `shared`.
pub fn create_project(device: &wgpu::Device, shared: Arc<SharedSource>) -> Box<dyn GeoOp> {
    Box::new(ProjectKernel {
        source: ProjectSource::new(shared),
        runner: PointsRunner::new(device),
    })
}

/// A compiled kernel with its pipeline and the buffers it owns.
struct Program {
    compiled: Compiled,
    pipeline: wgpu::ComputePipeline,
    layouts: [wgpu::BindGroupLayout; 2],
    uniforms: wgpu::Buffer,
    ubytes: Vec<u8>,
    /// `ch()` values by slot.
    values: Vec<[f32; 4]>,
    groups: [BindGroupCache; 2],
    /// Per attribute binding: the operator's copy of a snapshot's input.
    snapshots: Vec<Option<wgpu::Buffer>>,
    /// The source this was compiled from.
    source: Arc<str>,
    /// The source and input schema it was compiled for ([`key_of`]).
    key: u64,
}

type Built = Result<Program, String>;

/// The engine's type for a kernel attribute type.
fn core_type(t: atd_kernel::AttrType) -> AttrType {
    let scalar = match t.scalar {
        atd_kernel::Scalar::F32 => Scalar::F32,
        atd_kernel::Scalar::I32 => Scalar::I32,
        atd_kernel::Scalar::U32 => Scalar::U32,
    };
    AttrType::new(scalar, t.components)
}

fn kernel_type(t: AttrType) -> atd_kernel::AttrType {
    let scalar = match t.scalar {
        Scalar::F32 => atd_kernel::Scalar::F32,
        Scalar::I32 => atd_kernel::Scalar::I32,
        Scalar::U32 => atd_kernel::Scalar::U32,
    };
    atd_kernel::AttrType::new(scalar, t.comps())
}

/// Identifies a compile: the source and the input's point attributes (names
/// and types). Hashing allocates nothing.
fn key_of(source: &str, g: &Geometry) -> u64 {
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    for a in g.attrs(AttrClass::Point) {
        a.name().hash(&mut h);
        a.ty().hash(&mut h);
    }
    h.finish()
}

/// Whether `p` can run on `g` as it is: compiled for exactly this input, or
/// every attribute it binds is there with its type or missing and created by
/// the kernel. (Compiled for this input, a written attribute of another type
/// is one the kernel redefines: it starts again at zero.)
fn fits(p: &Program, g: &Geometry, key: u64) -> bool {
    p.key == key
        || p.compiled
            .bindings
            .attributes
            .iter()
            .all(|a| match g.point(&a.name) {
                Some(x) => x.ty() == core_type(a.ty),
                None => a.created_if_missing || a.snapshot,
            })
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Compiles `source` for points with `schema` and builds its pipeline (on a
/// worker thread).
fn build(device: &wgpu::Device, source: Arc<str>, schema: Vec<AttrSchema>, key: u64) -> Built {
    let mut opts = CompileOptions::new(Target::Points);
    opts.input_schema = Some(schema);
    opts.max_storage_buffers = device.limits().max_storage_buffers_per_shader_stage;
    let compiled = atd_kernel::compile(&source, &opts).map_err(|ds| render_errors(&ds, &source))?;
    let b = &compiled.bindings;
    if !b.images.is_empty() {
        return Err("`texture()` needs image inputs, which Kernel does not have yet".into());
    }
    if b.attributes.len() > MAX_ATTRS {
        return Err(format!(
            "the kernel binds {} attributes; at most {MAX_ATTRS}",
            b.attributes.len()
        ));
    }
    // The bind group caches bind buffers to bindings 0, 1, … in order.
    let h = b.header.as_ref().ok_or("a points kernel has a header")?;
    let in_order = h.binding == 0
        && b.uniforms.binding == 1
        && b.attributes
            .iter()
            .enumerate()
            .all(|(i, a)| a.group == 1 && a.binding == i as u32);
    if !in_order {
        return Err("internal: unexpected kernel binding layout".into());
    }
    // An error scope, not a panic: the app logs uncaptured errors instead
    // of panicking, so a pipeline the driver rejects must be caught here.
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let program = {
        let b = &compiled.bindings;
        let g0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pnt.kernel g0"),
            entries: &[
                storage_entry(0, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let entries: Vec<_> = b
            .attributes
            .iter()
            .map(|a| storage_entry(a.binding, a.access == Access::Read))
            .collect();
        let g1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pnt.kernel g1"),
            entries: &entries,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pnt.kernel"),
            bind_group_layouts: &[Some(&g0), Some(&g1)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pnt.kernel"),
            source: wgpu::ShaderSource::Wgsl(compiled.wgsl.as_str().into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pnt.kernel"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some(&b.entry_point),
            compilation_options: Default::default(),
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.kernel uniforms"),
            size: b.uniform_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let values = defaults(&compiled);
        Program {
            ubytes: vec![0; b.uniform_size as usize],
            snapshots: vec![None; b.attributes.len()],
            pipeline,
            layouts: [g0, g1],
            uniforms,
            values,
            groups: Default::default(),
            source,
            key,
            compiled,
        }
    };
    match pollster::block_on(scope.pop()) {
        Some(e) => Err(format!("the GPU rejected the kernel: {e}")),
        None => Ok(program),
    }
}

/// Runs Points kernels: compiles each new source (and input schema) on a
/// worker thread and dispatches the last one that compiled.
pub struct PointsRunner {
    device: wgpu::Device,
    /// The last kernel that compiled.
    program: Option<Program>,
    error: Option<String>,
    /// Key of the compile last started, and its result channel.
    requested: Option<u64>,
    building: Option<Receiver<Built>>,
    /// This frame's attribute buffers, in binding order (reused).
    bufs: Vec<wgpu::Buffer>,
}

impl PointsRunner {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            device: device.clone(),
            program: None,
            error: None,
            requested: None,
            building: None,
            bufs: Vec::with_capacity(MAX_ATTRS),
        }
    }

    fn run(
        &mut self,
        ctx: &mut GeoCtx<'_>,
        g: &mut Geometry,
        values: ChValues<'_>,
    ) -> Result<(), String> {
        let Some(p) = self.program.as_mut() else {
            return Ok(());
        };
        let attrs = &p.compiled.bindings.attributes;
        self.bufs.clear();
        for (i, a) in attrs.iter().enumerate() {
            let ty = core_type(a.ty);
            let buf = if a.snapshot {
                // This frame's input of the attribute bound just before.
                let src = self.bufs[i - 1].clone();
                let snap = match &p.snapshots[i] {
                    Some(s) if s.size() >= src.size() => s.clone(),
                    _ => {
                        let s = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("pnt.kernel snapshot"),
                            size: src.size(),
                            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        p.snapshots[i] = Some(s.clone());
                        s
                    }
                };
                ctx.encoder
                    .copy_buffer_to_buffer(&src, 0, &snap, 0, src.size());
                snap
            } else if a.access == Access::ReadWrite || g.point_typed(&a.name, ty).is_none() {
                kit::writable(ctx, g, &a.name, ty, Missing::Zero)?
            } else {
                kit::readable(ctx, g, &a.name, ty)?
            };
            self.bufs.push(buf);
        }
        let hdr = kit::header_of(g)?;
        let globals = FrameGlobals {
            time: ctx.time as f32,
            time_inc: ctx.dt as f32,
            frame: ctx.frame as f32,
            width: 0,
            height: 0,
        };
        fill_values(&p.compiled, values, &mut p.values);
        p.compiled
            .write_uniforms(&globals, &p.values, &mut p.ubytes);
        ctx.queue.write_buffer(&p.uniforms, 0, &p.ubytes);
        let mut refs: [&wgpu::Buffer; MAX_ATTRS] = [&hdr; MAX_ATTRS];
        for (r, b) in refs.iter_mut().zip(&self.bufs) {
            *r = b;
        }
        let [c0, c1] = &mut p.groups;
        let bind0 = c0.get(ctx.device, &p.layouts[0], &[&hdr, &p.uniforms]);
        let bind1 = c1.get(ctx.device, &p.layouts[1], &refs[..self.bufs.len()]);
        let mut pass = atd_gpu::geo::kernels::pass(ctx.encoder);
        pass.set_pipeline(&p.pipeline);
        pass.set_bind_group(0, &bind0, &[]);
        pass.set_bind_group(1, &bind1, &[]);
        pass.dispatch_workgroups_indirect(&hdr, header::DISPATCH_OFFSET);
        Ok(())
    }

    /// Runs `source` (compiling it first when it or the input's attributes
    /// are new) over the input, with `values` as its `ch()` values.
    pub fn cook(
        &mut self,
        ctx: &mut GeoCtx<'_>,
        source: Text,
        values: ChValues<'_>,
    ) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        // A file that cannot be read (an editor's save in progress, say)
        // leaves the last kernel running.
        let mut file_error = None;
        let source = match source {
            Ok(s) => s,
            Err(e) => match &self.program {
                Some(p) => {
                    file_error = Some(e);
                    p.source.clone()
                }
                None => {
                    *ctx.output = g;
                    return Err(e);
                }
            },
        };
        // Compile when the source changed, or when the input's attributes no
        // longer suit the kernel (their types decide how `@name` reads); each
        // source and input schema once, so a failing kernel is not retried.
        let key = key_of(&source, &g);
        let wanted = self
            .program
            .as_ref()
            .is_none_or(|p| p.source != source || !fits(p, &g, key));
        if !wanted && self.building.is_none() {
            // Back to the kernel that runs (after a failed edit): forget the
            // failed compile, so the same edit again compiles and fails again.
            self.error = None;
            self.requested = None;
        }
        if wanted && self.requested != Some(key) {
            let schema: Vec<AttrSchema> = g
                .attrs(AttrClass::Point)
                .iter()
                .map(|a| AttrSchema {
                    name: a.name().to_owned(),
                    ty: kernel_type(a.ty()),
                })
                .collect();
            let (tx, rx) = channel();
            let device = self.device.clone();
            let spawned = std::thread::Builder::new()
                .name("atd-kernel-compile".into())
                .spawn(move || {
                    let _ = tx.send(build(&device, source, schema, key));
                });
            self.requested = Some(key);
            match spawned {
                Ok(_) => self.building = Some(rx),
                Err(e) => self.error = Some(format!("cannot start the kernel compiler: {e}")),
            }
        }
        if let Some(rx) = &self.building {
            match ctx.params.mode().receive(rx) {
                Ok(Ok(program)) => {
                    self.building = None;
                    self.program = Some(program);
                    self.error = None;
                }
                Ok(Err(e)) => {
                    self.building = None;
                    self.error = Some(e);
                }
                Err(TryRecvError::Empty) => ctx.cook_again = true,
                Err(TryRecvError::Disconnected) => {
                    self.building = None;
                    self.error = Some("the kernel compiler stopped".into());
                }
            }
        }
        let ran = if g.points() > 0 && self.program.as_ref().is_some_and(|p| fits(p, &g, key)) {
            self.run(ctx, &mut g, values)
        } else {
            Ok(())
        };
        *ctx.output = g;
        ran?;
        match file_error.as_ref().or(self.error.as_ref()) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// Whether a compile is running, for [`GeoOp::status`].
    pub fn status(&self) -> Option<&str> {
        match (&self.building, &self.program) {
            (Some(_), _) => Some("compiling"),
            (None, Some(_)) if self.error.is_some() => {
                Some("running the last kernel that compiled")
            }
            _ => None,
        }
    }
}

/// `pnt.kernel`: an inline or file Points kernel.
struct Kernel {
    source: KernelSource,
    runner: PointsRunner,
}

impl GeoOp for Kernel {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let text = self.source.get(ctx.params, &mut ctx.cook_again);
        self.runner.cook(ctx, text, ChValues::Defaults)
    }

    fn keeps_output_on_error(&self) -> bool {
        // A failed edit reports its error while the last kernel runs on.
        true
    }

    fn status(&self) -> Option<&str> {
        self.runner.status()
    }
}

/// A project's `kernels/<name>.atk` operator: the file's kernel with the
/// node's parameters as its `ch()` values.
struct ProjectKernel {
    source: ProjectSource,
    runner: PointsRunner,
}

impl GeoOp for ProjectKernel {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let text = self.source.get();
        let params = ctx.params;
        self.runner.cook(ctx, text, ChValues::Params(params))
    }

    fn keeps_output_on_error(&self) -> bool {
        true
    }

    fn status(&self) -> Option<&str> {
        self.runner.status()
    }
}
