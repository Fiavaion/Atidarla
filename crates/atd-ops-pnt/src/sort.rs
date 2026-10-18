//! Sorting on the GPU and what is built on it: the Sort operator and the
//! grid hash the neighbour operators search (Normals, Smooth, Connect).
//!
//! [`Sorter`] is a bitonic sort of (key, index) pairs padded to a power of
//! two. Pairs compare by key, then by index, so equal keys keep their
//! order: the result is unique, the same on every run and every backend (no
//! atomics, design §4.3). Each of its log²(n)/2 stages is one dispatch;
//! the stages' parameters sit in one uniform buffer read at a dynamic
//! offset, so a frame binds no new groups (rule 2). At 2M points that is
//! 231 dispatches, a few milliseconds: Sort is for changes of order, not a
//! per-frame step at that size.

use atd_core::geo::{AttrClass, AttrType, header, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::kernels::{Bind, Gather, GatherMap, Kernel, Packer, Work, pass};

use crate::kit::{self, PointKernel, menu_index, typed_access, unit, vec3};
use crate::select::{MAX_ATTRS, drop_prims};

/// Most points a sort handles: `2^23` (its dispatches cover the padded
/// size in one dimension).
pub const MAX_SORT: u32 = 1 << 23;

const STAGE: &str = "struct Stage { j: u32, k: u32, n: u32, pad: u32 };
@group(0) @binding(0) var<storage, read_write> keys: array<u32>;
@group(0) @binding(1) var<storage, read_write> vals: array<u32>;
@group(1) @binding(0) var<uniform> st: Stage;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3u) {
    let i = gid.x;
    if (i >= st.n) { return; }
    let l = i ^ st.j;
    if (l <= i) { return; }
    let ki = keys[i];
    let kl = keys[l];
    let vi = vals[i];
    let vl = vals[l];
    let up = (i & st.k) == 0u;
    let greater = ki > kl || (ki == kl && vi > vl);
    if (greater == up) {
        keys[i] = kl;
        keys[l] = ki;
        vals[i] = vl;
        vals[l] = vi;
    }
}
";

/// Buffers and bind groups for one padded size.
struct Sized {
    n2: u32,
    keys: wgpu::Buffer,
    vals: wgpu::Buffer,
    bind: wgpu::BindGroup,
    stage_bind: wgpu::BindGroup,
    stages: u32,
    stride: u32,
}

/// A bitonic sort of `u32` keys with their indices (see the module docs).
pub struct Sorter {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    stage_layout: wgpu::BindGroupLayout,
    sized: Option<Sized>,
}

impl Sorter {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pnt.sort.stage"),
            source: wgpu::ShaderSource::Wgsl(STAGE.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pnt.sort.data"),
            entries: &atd_gpu::geo::kernels::layout_entries(
                &[Bind::ReadWrite, Bind::ReadWrite],
                wgpu::ShaderStages::COMPUTE,
            ),
        });
        let stage_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pnt.sort.stage"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: std::num::NonZeroU64::new(16),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pnt.sort"),
            bind_group_layouts: &[Some(&layout), Some(&stage_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pnt.sort"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            layout,
            stage_layout,
            sized: None,
        }
    }

    /// The padded size for `n` elements.
    pub fn padded(n: u32) -> u32 {
        n.max(2).next_power_of_two()
    }

    /// Makes the buffers for `n` elements (when the padded size changes, an
    /// edit) and returns the key and index buffers (`Self::padded(n)` each).
    pub fn size(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        n: u32,
    ) -> (wgpu::Buffer, wgpu::Buffer) {
        let n2 = Self::padded(n);
        if self.sized.as_ref().is_none_or(|s| s.n2 != n2) {
            let buffer = |label: &str| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: u64::from(n2) * 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            };
            let keys = buffer("pnt.sort.keys");
            let vals = buffer("pnt.sort.vals");
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pnt.sort.data"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: keys.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: vals.as_entire_binding(),
                    },
                ],
            });
            // Every (j, k) stage at its own aligned offset.
            let stride = device.limits().min_uniform_buffer_offset_alignment.max(16);
            let mut data = Vec::new();
            let mut stages = 0u32;
            let mut k = 2u32;
            while k <= n2 {
                let mut j = k >> 1;
                while j > 0 {
                    let mut block = vec![0u8; stride as usize];
                    block[..16].copy_from_slice(&atd_gpu::geo::kernels::words([j, k, n2, 0]));
                    data.extend_from_slice(&block);
                    stages += 1;
                    j >>= 1;
                }
                k <<= 1;
            }
            let stage_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pnt.sort.stages"),
                size: data.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&stage_buffer, 0, &data);
            let stage_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pnt.sort.stage"),
                layout: &self.stage_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &stage_buffer,
                        offset: 0,
                        size: std::num::NonZeroU64::new(16),
                    }),
                }],
            });
            self.sized = Some(Sized {
                n2,
                keys,
                vals,
                bind,
                stage_bind,
                stages,
                stride,
            });
        }
        let s = self.sized.as_ref().expect("sized above");
        (s.keys.clone(), s.vals.clone())
    }

    /// Records the sort (after the keys and indices were written).
    pub fn record(&self, pass: &mut wgpu::ComputePass<'_>) {
        let s = self.sized.as_ref().expect("size() first");
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &s.bind, &[]);
        let groups = s.n2.div_ceil(256).max(1);
        for k in 0..s.stages {
            pass.set_bind_group(1, &s.stage_bind, &[k * s.stride]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
    }

    /// The padded size the buffers have.
    pub fn n2(&self) -> u32 {
        self.sized.as_ref().map_or(0, |s| s.n2)
    }
}

/// WGSL: an order-preserving map from `f32` to `u32`.
pub const FKEY: &str = "fn atd_fkey(f: f32) -> u32 {
    let b = bitcast<u32>(f);
    if ((b & 0x80000000u) != 0u) { return ~b; }
    return b | 0x80000000u;
}
";

/// Copies a header with `mapped = count` (for a gather of every point).
const MAPPED: &str = "@group(0) @binding(0) var<storage, read> src: GeoHeader;
@group(0) @binding(1) var<storage, read_write> hdr: GeoHeader;
@compute @workgroup_size(1)
fn main() {
    hdr = src;
    hdr.mapped = src.count;
}
";

/// A one-thread kernel giving a copy of a header whose `mapped` is its
/// count.
pub fn mapped_kernel(device: &wgpu::Device) -> Kernel {
    Kernel::new(
        device,
        "pnt.mapped_header",
        &format!("{}{MAPPED}", header::WGSL),
        "main",
        &[Bind::Read, Bind::ReadWrite],
    )
}

// --------------------------------------------------------------------- Sort

const KEYS: &[&str] = &["axis", "distance", "attribute", "random", "reverse"];

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "sort",
        "Sort",
        "Reorders points: along an axis, by distance from a point (far first suits Render Points' `over` blending), by an attribute, randomly (seeded) or reversed. Equal keys keep their order, so the result is the same on every run. Every attribute follows; a few milliseconds for millions of points (not a per-frame step at that size).",
        &[("in", false)],
        vec![
            ParamSchema::menu("key", "Key", KEYS, "axis").help(
                "axis: position along Axis; distance: from Point; attribute: a component of Attribute; random: a seeded shuffle (by `id` when the points have one); reverse: the order backwards.",
            ),
            ParamSchema::vec("axis", "Axis", &[0.0, 0.0, 1.0]).help("axis: the direction sorted along."),
            ParamSchema::vec("point", "Point", &[0.0, 0.0, 5.0]).help("distance: the point distances are measured from."),
            ParamSchema::string("attribute", "Attribute", "age").help("attribute: the point attribute."),
            ParamSchema::int("component", "Component", 0, Some(0), Some(3)).help("attribute: which component."),
            ParamSchema::toggle("descending", "Descending", false).help("Largest first (for distance: farthest first)."),
            ParamSchema::int("seed", "Seed", 0, Some(0), None).help("random: selects a different order."),
        ],
        2000,
    )
}

const SORT_KEYS: &str = "struct Params {
    axis: vec4f,
    point: vec4f,
    d: vec4u,
    key: u32,
    comp: u32,
    seed: u32,
    // bit 0: descending; 1: an `id` is bound
    flags: u32,
    n2: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};
@group(0) @binding(2) var<storage, read_write> keys: array<u32>;
@group(0) @binding(3) var<storage, read_write> vals: array<u32>;
@group(0) @binding(4) var<storage, read> P: array<vec4f>;
@group(0) @binding(5) var<storage, read> X: array<u32>;
@group(0) @binding(6) var<storage, read> ID: array<u32>;
";

const SORT_MAIN: &str = "if (i < params.n2) {
        var k = 0xffffffffu;
        if (i < hdr.count) {
            switch params.key {
                case 0u: { k = atd_fkey(dot(P[i].xyz, params.axis.xyz)); }
                case 1u: { k = atd_fkey(length(P[i].xyz - params.point.xyz)); }
                case 2u: { k = atd_fkey(ld_X(i)[min(params.comp, 3u)]); }
                case 3u: {
                    var s = i;
                    if ((params.flags & 2u) != 0u) { s = ID[i]; }
                    k = atd_hash(s ^ atd_hash(params.seed));
                }
                default: { k = ~i; }
            }
            if ((params.flags & 1u) != 0u) { k = ~k; }
        }
        keys[i] = k;
        vals[i] = i;
    }";

pub struct Sort {
    keys: PointKernel,
    sorter: Sorter,
    mapped: Kernel,
    gather: Gather,
    dummy: wgpu::Buffer,
    /// Bound by the kernels; copied into the pooled output header.
    work: wgpu::Buffer,
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Sort {
        keys: PointKernel::with_main(
            device,
            "pnt.sort.keys",
            &format!("{SORT_KEYS}{FKEY}{}", typed_access("X", "params.d", true)),
            &[ReadWrite, ReadWrite, Read, Read, Read],
            80,
            SORT_MAIN,
        ),
        sorter: Sorter::new(device),
        mapped: mapped_kernel(device),
        gather: Gather::new(device),
        dummy: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.sort.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }),
        work: kit::work_header(device),
    })
}

impl GeoOp for Sort {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        if g.points() > MAX_SORT {
            return Err(format!("Sort handles at most {MAX_SORT} points"));
        }
        let p = ctx.params;
        let key = menu_index(KEYS, p.str("key"));
        let hdr_in = kit::header_of(&g)?;
        let pos = if key <= 1 {
            kit::readable(ctx, &g, names::P, AttrType::VEC3)?
        } else {
            self.dummy.clone()
        };
        let (x, d) = if key == 2 {
            let (b, ty) = kit::readable_any(ctx, &g, p.str("attribute"))?;
            (b, kit::desc(ty))
        } else {
            (self.dummy.clone(), [1, 1, 0, 0])
        };
        let id = g
            .point_typed(names::ID, AttrType::U32)
            .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone());
        let n = g.points();
        let (keys, vals) = self.sorter.size(ctx.device, ctx.queue, n);
        let mut u = Packer::<80>::new();
        u.vec4(unit(&p, "axis")).vec4(vec3(&p, "point"));
        for w in d {
            u.u32(w);
        }
        let flags = u32::from(p.bool("descending")) | (u32::from(id.is_some()) << 1);
        u.u32(key)
            .u32(p.i64("component").clamp(0, 3) as u32)
            .u32(kit::seed(&p))
            .u32(flags)
            .u32(self.sorter.n2());
        self.keys.write(ctx.queue, u.bytes());
        // The reordered attributes.
        let count = g.attrs(AttrClass::Point).len();
        if count > MAX_ATTRS {
            return Err(format!("more than {MAX_ATTRS} point attributes"));
        }
        let mut moves: [Option<(wgpu::Buffer, wgpu::Buffer, u32)>; MAX_ATTRS] = Default::default();
        for (k, slot) in moves.iter_mut().enumerate().take(count) {
            let a = g.attrs(AttrClass::Point)[k].clone();
            let src = a.ensure_gpu(ctx.queue, ctx.buffers).clone();
            let out = ctx.alloc(a.name_arc().clone(), a.ty(), n);
            *slot = Some((
                src,
                out.gpu_buffer().expect("on the GPU").clone(),
                a.ty().stride(),
            ));
            g.set_point(out);
        }
        let header = kit::new_header(ctx);
        let hdr = self.work.clone();
        let d = &self.dummy;
        let id_b = id.as_ref().unwrap_or(d);
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.keys.dispatch_in(
            device,
            &mut ps,
            &hdr_in,
            &[&keys, &vals, &pos, &x, id_b],
            Work::Elements(self.sorter.n2()),
        );
        self.sorter.record(&mut ps);
        self.mapped
            .dispatch_in(device, &mut ps, &[&hdr_in, &hdr], Work::Groups(1));
        let m = GatherMap {
            header: &hdr,
            map: &vals,
        };
        for (src, dst, stride) in moves.iter().flatten() {
            self.gather.record(device, &mut ps, m, *stride, src, dst);
        }
        drop(ps);
        ctx.encoder
            .copy_buffer_to_buffer(&hdr, 0, header.buffer(), 0, header::BYTES);
        if g.is_gpu_counted() {
            g.set_gpu_counted(n, header);
        } else {
            g.set_points(n, Some(header));
        }
        drop_prims(&mut g);
        *ctx.output = g;
        Ok(())
    }
}
