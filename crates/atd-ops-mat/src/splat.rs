//! Splat MAT (`mat.splat`, FR-C1) and how `img.render` draws it: Gaussian
//! splats after Kerbl et al., "3D Gaussian Splatting for Real-Time
//! Radiance Field Rendering" (SIGGRAPH 2023), from the paper's equations
//! (EWA splatting of each 3D Gaussian to a 2D one, sorted alpha blending).
//! No code from the INRIA reference renderer (non-commercial licence).
//!
//! Per frame, in the render's command encoder, all on the GPU:
//!
//! 1. **colour** (degree ≥ 1): the spherical harmonics up to the material's
//!    degree, evaluated towards the camera and added to `Cd` (one pass per
//!    16 coefficients floats, to stay within 8 storage buffers);
//! 2. **project:** each splat's covariance `R S Sᵀ Rᵀ` (from `scale`,
//!    `orient`) through the view and the projection's Jacobian to a 2D
//!    covariance in pixels (plus the paper's 0.3 px² low-pass), whose
//!    eigenvectors give an oriented quad out to where the Gaussian falls
//!    below 1/255; splats behind the camera, off screen or invisible are
//!    culled; a depth key and index per splat, and a count of the visible;
//! 3. **sort** the keys far to near ([`crate::splat_sort`], a stable radix
//!    sort, so the order and the image are the same every run);
//! 4. **draw** the visible splats, in order, as instanced quads blended
//!    with premultiplied "over", depth-tested against the opaque geometry
//!    of the same render (drawn first) without writing depth.
//!
//! Buffers grow with the largest cloud seen and are reused: a steady frame
//! allocates nothing (rule 2).

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{MatCtx, MatOp};
use atd_gpu::geo::kernels::{self, Bind, BindGroupCache, Kernel, Packer, Uniforms, Work};
use atd_gpu::geo::{BufferPool, DetailValue, Geometry};
use atd_gpu::mat::{Material, Model};

use crate::splat_sort::RadixSort;

/// Splats per draw (the per-dispatch limit of the projection pass).
pub const MAX_SPLATS: u32 = 65_535 * 256;
/// World size of splats of a geometry without `scale`.
const DEFAULT_SCALE: f32 = 0.01;
/// Bits of the depth sort key (three radix passes).
const KEY_BITS: u32 = 24;
/// Bytes of the per-splat projected record (three `vec4f`).
const RECORD: u64 = 48;
const PARAMS: usize = 48;

pub fn schema() -> OperatorSchema {
    let mut s = crate::materials::mat(
        "splat",
        "Splat",
        "Draws the geometry's points as Gaussian splats (3D Gaussian Splatting), as Splat In reads them: each an ellipsoid from `scale` and `orient`, coloured by `Cd` and, up to SH Degree, the view-dependent spherical harmonics `sh0`…, sorted on the GPU every frame and blended back to front over the render's other geometry. Points without `scale` are drawn as round splats.",
        None,
        params(),
    );
    // Filling a struct; the GPU work is `img.render`'s.
    s.cook_budget_us = 20;
    s
}

fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::colour("tint", "Tint", [1.0, 1.0, 1.0, 1.0])
            .help("Multiplies every splat's colour (alpha: its opacity)."),
        ParamSchema::float("opacity", "Opacity", 1.0, Some(0.0), Some(4.0))
            .help("Multiplies every splat's opacity (above 1 thickens a sparse capture)."),
        ParamSchema::float("size", "Size", 1.0, Some(0.0), Some(16.0))
            .help("Multiplies every splat's size (below 1 shows the capture's structure)."),
        ParamSchema::int("sh_degree", "SH Degree", 3, Some(0), Some(3)).help(
            "Highest spherical-harmonic band used for view-dependent colour (up to what the geometry has); 0: the base colour only, the fastest.",
        ),
    ]
}

/// The Splat MAT operator.
struct SplatMat;

impl MatOp for SplatMat {
    fn cook(&mut self, ctx: &mut MatCtx<'_>) -> Result<(), String> {
        let mut m = Material {
            use_cd: true,
            ..Material::default()
        };
        fill(&ctx.params, &mut m);
        *ctx.output = m;
        Ok(())
    }
}

pub fn create() -> Box<dyn MatOp> {
    Box::new(SplatMat)
}

fn fill(p: &atd_gpu::engine::Params<'_>, m: &mut Material) {
    m.model = Model::Splat;
    m.colour = p.vec4("tint");
    m.params = [
        p.f32("size").max(0.0),
        p.f32("opacity").max(0.0),
        p.i64("sh_degree").clamp(0, 3) as f32,
        0.0,
    ];
}

const COMMON: &str = r#"
struct SplatParams {
    tint: vec4f,
    // x size multiplier, y opacity multiplier, z default scale
    look: vec4f,
    // x count, y SH floats per splat, z first float of this pass, w flags
    counts: vec4u,
};
const HAS_CD: u32 = 1u;
const HAS_SCALE: u32 = 2u;
const HAS_ORIENT: u32 = 4u;
const GPU_COUNT: u32 = 8u;
const SH_COLOUR: u32 = 16u;
"#;

const SH: &str = r#"
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<uniform> s: SplatParams;
@group(0) @binding(2) var<storage, read> P: array<vec4f>;
@group(0) @binding(3) var<storage, read> Cd: array<vec4f>;
@group(0) @binding(4) var<storage, read> S0: array<vec4f>;
@group(0) @binding(5) var<storage, read> S1: array<vec4f>;
@group(0) @binding(6) var<storage, read> S2: array<vec4f>;
@group(0) @binding(7) var<storage, read> S3: array<vec4f>;
@group(0) @binding(8) var<storage, read_write> colour: array<vec4f>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= s.counts.x) { return; }
    var c = vec4f(1.0);
    if (s.counts.z > 0u) {
        c = colour[i];
    } else if ((s.counts.w & HAS_CD) != 0u) {
        c = Cd[i];
    }
    // From the camera towards the splat, as the coefficients were fitted.
    var d = g.forward.xyz;
    if (g.eye.w > 0.5) {
        let v = P[i].xyz - g.eye.xyz;
        let l = length(v);
        d = select(g.forward.xyz, v / l, l > 1e-20);
    }
    let x = d.x;
    let y = d.y;
    let z = d.z;
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    // Real spherical harmonics, bands 1-3, in the 3DGS coefficient order.
    var b: array<f32, 15>;
    b[0] = -0.4886025119 * y;
    b[1] = 0.4886025119 * z;
    b[2] = -0.4886025119 * x;
    b[3] = 1.0925484306 * x * y;
    b[4] = -1.0925484306 * y * z;
    b[5] = 0.3153915653 * (2.0 * zz - xx - yy);
    b[6] = -1.0925484306 * x * z;
    b[7] = 0.5462742153 * (xx - yy);
    b[8] = -0.5900435899 * y * (3.0 * xx - yy);
    b[9] = 2.8906114426 * x * y * z;
    b[10] = -0.4570457995 * y * (4.0 * zz - xx - yy);
    b[11] = 0.3731763326 * z * (2.0 * zz - 3.0 * xx - 3.0 * yy);
    b[12] = -0.4570457995 * x * (4.0 * zz - xx - yy);
    b[13] = 1.4453057213 * z * (xx - yy);
    b[14] = -0.5900435899 * x * (xx - 3.0 * yy);
    var rgb = c.rgb;
    for (var q = 0u; q < 4u; q++) {
        var v: vec4f;
        switch (q) {
            case 0u: { v = S0[i]; }
            case 1u: { v = S1[i]; }
            case 2u: { v = S2[i]; }
            default: { v = S3[i]; }
        }
        for (var k = 0u; k < 4u; k++) {
            let f = s.counts.z + q * 4u + k;
            if (f < s.counts.y) {
                rgb[f % 3u] += b[f / 3u] * v[k];
            }
        }
    }
    colour[i] = vec4f(rgb, c.a);
}
"#;

const PROJECT: &str = r#"
struct Splats {
    visible: atomic<u32>,
    pad0: u32,
    pad1: u32,
    pad2: u32,
    data: array<vec4f>,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<uniform> s: SplatParams;
@group(0) @binding(2) var<storage, read> P: array<vec4f>;
@group(0) @binding(3) var<storage, read> colour: array<vec4f>;
@group(0) @binding(4) var<storage, read> scale: array<vec4f>;
@group(0) @binding(5) var<storage, read> orient: array<vec4f>;
@group(0) @binding(6) var<storage, read> header: array<u32>;
@group(0) @binding(7) var<storage, read_write> rec: Splats;
@group(0) @binding(8) var<storage, read_write> keys: array<u32>;
@group(0) @binding(9) var<storage, read_write> vals: array<u32>;

fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= s.counts.x) { return; }
    keys[i] = 0xffffffu;
    vals[i] = i;
    rec.data[i * 3u] = vec4f(2.0, 2.0, 2.0, 0.0);
    if ((s.counts.w & GPU_COUNT) != 0u && i >= header[0]) { return; }
    let p = P[i].xyz;
    var c = vec4f(1.0);
    if ((s.counts.w & (HAS_CD | SH_COLOUR)) != 0u) { c = colour[i]; }
    let alpha = clamp(c.a * s.look.y * s.tint.a, 0.0, 1.0);
    if (!(alpha >= 1.0 / 255.0)) { return; }
    let clip = g.view_proj * vec4f(p, 1.0);
    if (!(clip.w > 1e-6) || !finite(clip.x) || !finite(clip.y)) { return; }
    let ndc = clip.xyz / clip.w;
    if (!(ndc.z >= 0.0 && ndc.z <= 1.0)) { return; }
    let t = (g.view * vec4f(p, 1.0)).xyz;
    let depth = -t.z;

    // 3D covariance R S Sᵀ Rᵀ.
    var sc = vec3f(s.look.z);
    if ((s.counts.w & HAS_SCALE) != 0u) { sc = scale[i].xyz; }
    sc = abs(sc) * s.look.x;
    var q = vec4f(0.0, 0.0, 0.0, 1.0);
    if ((s.counts.w & HAS_ORIENT) != 0u) {
        let o = orient[i];
        let l = length(o);
        if (l > 1e-12 && finite(l)) { q = o / l; }
    }
    let x = q.x;
    let y = q.y;
    let z = q.z;
    let w = q.w;
    let r = mat3x3f(
        vec3f(1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y)),
        vec3f(2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)),
        vec3f(2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)),
    );
    let m = mat3x3f(r[0] * sc.x, r[1] * sc.y, r[2] * sc.z);
    let wv = mat3x3f(g.view[0].xyz, g.view[1].xyz, g.view[2].xyz);
    let mv = wv * m;
    // View-space covariance.
    let v = mv * transpose(mv);

    // Jacobian of the projection to pixels (x right, y up).
    let hs = g.misc.zw * 0.5;
    let f = g.scale.xy * hs;
    var j0 = vec3f(f.x, 0.0, 0.0);
    var j1 = vec3f(0.0, f.y, 0.0);
    if (g.eye.w > 0.5) {
        let lim = 1.3 / g.scale.xy;
        let tx = clamp(t.x / depth, -lim.x, lim.x) * depth;
        let ty = clamp(t.y / depth, -lim.y, lim.y) * depth;
        j0 = vec3f(f.x / depth, 0.0, f.x * tx / (depth * depth));
        j1 = vec3f(0.0, f.y / depth, f.y * ty / (depth * depth));
    }
    let a = dot(j0, v * j0) + 0.3;
    let b = dot(j0, v * j1);
    let cc = dot(j1, v * j1) + 0.3;
    let mid = 0.5 * (a + cc);
    let disc = sqrt(max(0.25 * (a - cc) * (a - cc) + b * b, 0.0));
    let l1 = mid + disc;
    let l2 = max(mid - disc, 0.01);
    var axis = vec2f(1.0, 0.0);
    if (abs(b) > 1e-12) {
        axis = normalize(vec2f(b, l1 - a));
    } else if (cc > a) {
        axis = vec2f(0.0, 1.0);
    }
    // Out to where alpha · exp(-r²/2) falls below 1/255, at most 3σ.
    let k = min(sqrt(2.0 * log(255.0 * alpha)), 3.0);
    let cap = 2.0 * max(hs.x, hs.y);
    let r1 = min(k * sqrt(l1), cap);
    let r2 = min(k * sqrt(l2), cap);
    if (!(k > 0.0) || !finite(r1) || !finite(r2) || !finite(axis.x)) { return; }
    let centre = ndc.xy * hs;
    if (abs(centre.x) - r1 > hs.x || abs(centre.y) - r1 > hs.y) { return; }

    rec.data[i * 3u] = vec4f(ndc, k);
    rec.data[i * 3u + 1u] = vec4f(axis, r1, r2);
    rec.data[i * 3u + 2u] = vec4f(max(c.rgb, vec3f(0.0)) * s.tint.rgb, alpha);
    // Far first: positive depths order like their bits; 24 bits keep a
    // relative depth step of 2^-15 (ties keep the input order).
    keys[i] = (0xffffffffu - bitcast<u32>(max(depth, 1e-30))) >> 8u;
    atomicAdd(&rec.visible, 1u);
}
"#;

const DRAW: &str = r#"
struct Splats {
    visible: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
    data: array<vec4f>,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<storage, read> splats: Splats;
@group(0) @binding(2) var<storage, read> order: array<u32>;

struct Out {
    @builtin(position) pos: vec4f,
    // Position in standard deviations from the centre.
    @location(0) local: vec2f,
    @location(1) colour: vec4f,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) n: u32) -> Out {
    let i = order[n];
    let a = splats.data[i * 3u];
    let b = splats.data[i * 3u + 1u];
    let corner = vec2f(f32(vi & 1u), f32(vi >> 1u)) * 2.0 - 1.0;
    let hs = g.misc.zw * 0.5;
    let px = a.xy * hs + corner.x * b.z * b.xy + corner.y * b.w * vec2f(-b.y, b.x);
    var o: Out;
    o.pos = vec4f(px / hs, a.z, 1.0);
    o.local = corner * a.w;
    o.colour = splats.data[i * 3u + 2u];
    return o;
}

@fragment
fn fs(i: Out) -> @location(0) vec4f {
    let alpha = min(i.colour.a * exp(-0.5 * dot(i.local, i.local)), 0.99);
    if (alpha < 1.0 / 255.0) { discard; }
    return vec4f(i.colour.rgb * alpha, alpha);
}
"#;

/// Buffers of one capacity.
#[derive(Debug)]
struct Buffers {
    capacity: u32,
    colour: wgpu::Buffer,
    splats: wgpu::Buffer,
    keys: wgpu::Buffer,
    vals: wgpu::Buffer,
}

/// What `img.render` keeps per geometry slot to draw splats.
#[derive(Debug)]
pub struct SplatDraw {
    format: wgpu::TextureFormat,
    sh: Kernel,
    project: Kernel,
    sort: RadixSort,
    /// The three colour passes and the projection.
    uniforms: [Uniforms; 4],
    module: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    /// Pipelines by sample count.
    pipelines: Vec<(u32, wgpu::RenderPipeline)>,
    binds: BindGroupCache,
    indirect: wgpu::Buffer,
    dummy: wgpu::Buffer,
    buffers: Option<Buffers>,
    /// The draw of this frame, set by [`Self::prepare`].
    ready: Option<wgpu::BindGroup>,
}

/// The highest SH degree whose attributes `geo` has, up to `max`.
fn degree(geo: &Geometry, max: u32) -> u32 {
    let declared = match geo.detail(names::SH_DEGREE) {
        Some(DetailValue::I32(d)) => (*d).clamp(0, 3) as u32,
        Some(DetailValue::U32(d)) => (*d).min(3) as u32,
        _ => 3,
    };
    (0..=declared.min(max))
        .rev()
        .find(|&d| {
            let attrs = (3 * coefs(d)).div_ceil(4) as usize;
            names::SH[..attrs]
                .iter()
                .all(|n| geo.point_typed(n, AttrType::VEC4).is_some())
        })
        .unwrap_or(0)
}

const fn coefs(degree: u32) -> u32 {
    match degree {
        0 => 0,
        1 => 3,
        2 => 8,
        _ => 15,
    }
}

impl SplatDraw {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, globals_wgsl: &str) -> Self {
        use Bind::{Read, ReadWrite, Uniform};
        let sh = Kernel::new(
            device,
            "splat.colour",
            &format!("{globals_wgsl}{COMMON}{SH}"),
            "main",
            &[
                Uniform, Uniform, Read, Read, Read, Read, Read, Read, ReadWrite,
            ],
        );
        let project = Kernel::new(
            device,
            "splat.project",
            &format!("{globals_wgsl}{COMMON}{PROJECT}"),
            "main",
            &[
                Uniform, Uniform, Read, Read, Read, Read, Read, ReadWrite, ReadWrite, ReadWrite,
            ],
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("splat.draw"),
            source: wgpu::ShaderSource::Wgsl(format!("{globals_wgsl}{DRAW}").into()),
        });
        let entries = kernels::layout_entries(&[Uniform, Read, Read], wgpu::ShaderStages::VERTEX);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("splat.draw"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("splat.draw"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("splat.indirect"),
            size: 16,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        // 4 vertices per quad; the instance count is copied in each frame.
        indirect
            .slice(..)
            .get_mapped_range_mut()
            .expect("mapped at creation")
            .copy_from_slice(&kernels::words([4, 0, 0, 0]));
        indirect.unmap();
        Self {
            format,
            sh,
            project,
            sort: RadixSort::new(device),
            uniforms: std::array::from_fn(|_| Uniforms::new(device, "splat.params", PARAMS as u64)),
            module,
            layout,
            pipeline_layout,
            pipelines: Vec::new(),
            binds: BindGroupCache::default(),
            indirect,
            dummy: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("splat.dummy"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }),
            buffers: None,
            ready: None,
        }
    }

    fn pipeline(&mut self, device: &wgpu::Device, samples: u32) {
        if self.pipelines.iter().any(|(s, _)| *s == samples) {
            return;
        }
        let over = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("splat.draw"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: Some(wgpu::BlendState {
                        color: over,
                        alpha: over,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: crate::render::DEPTH,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push((samples, pipeline));
    }

    fn buffers(&mut self, device: &wgpu::Device, n: u32) {
        if self.buffers.as_ref().is_some_and(|b| b.capacity >= n) {
            return;
        }
        let capacity = n.next_power_of_two().max(1024);
        let buffer = |label, bytes: u64, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | usage,
                mapped_at_creation: false,
            })
        };
        let none = wgpu::BufferUsages::empty();
        let n = u64::from(capacity);
        self.buffers = Some(Buffers {
            capacity,
            colour: buffer("splat.colour", n * 16, none),
            splats: buffer(
                "splat.records",
                16 + n * RECORD,
                wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            ),
            keys: buffer("splat.keys", n * 4, none),
            vals: buffer("splat.vals", n * 4, none),
        });
    }

    /// Records the colour, projection and sort passes for `geo` drawn with
    /// `material` (before the render pass), ready for [`Self::draw`].
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        pool: &BufferPool,
        globals: &wgpu::Buffer,
        geo: &Geometry,
        material: &Material,
        samples: u32,
    ) -> Result<(), String> {
        self.ready = None;
        let n = geo.points();
        if n > MAX_SPLATS {
            return Err(format!("{n} splats; Splat MAT draws at most {MAX_SPLATS}"));
        }
        let pos = geo
            .point_typed(names::P, AttrType::VEC3)
            .ok_or("the geometry has no `P`")?;
        self.pipeline(device, samples);
        self.buffers(device, n);
        let cd = geo.point_typed(names::CD, AttrType::VEC4);
        let scale = geo.point_typed(names::SCALE, AttrType::VEC3);
        let orient = geo.point_typed(names::ORIENT, AttrType::VEC4);
        let header = geo.header().filter(|_| geo.is_gpu_counted());
        let deg = degree(geo, material.params[2].clamp(0.0, 3.0) as u32);
        let floats = 3 * coefs(deg);
        let flags = u32::from(cd.is_some())
            | u32::from(scale.is_some()) << 1
            | u32::from(orient.is_some()) << 2
            | u32::from(header.is_some()) << 3
            | u32::from(floats > 0) << 4;
        let block = |first: u32| {
            let mut u = Packer::<PARAMS>::new();
            u.vec4(material.colour)
                .vec4([material.params[0], material.params[1], DEFAULT_SCALE, 0.0])
                .u32(n)
                .u32(floats)
                .u32(first)
                .u32(flags);
            let mut out = [0u8; PARAMS];
            out.copy_from_slice(u.bytes());
            out
        };
        let passes = floats.div_ceil(16) as usize;
        for (j, u) in self.uniforms[..passes].iter().enumerate() {
            u.write(queue, &block(16 * j as u32));
        }
        self.uniforms[3].write(queue, &block(0));

        let p = pos.ensure_gpu(queue, pool);
        let cd_buf = cd.map(|a| a.ensure_gpu(queue, pool));
        let scale_buf = scale.map(|a| a.ensure_gpu(queue, pool));
        let orient_buf = orient.map(|a| a.ensure_gpu(queue, pool));
        let sh_bufs: [Option<&wgpu::Buffer>; 12] = std::array::from_fn(|k| {
            names::SH
                .get(k)
                .filter(|_| (k as u32) < floats.div_ceil(4))
                .and_then(|name| geo.point_typed(name, AttrType::VEC4))
                .map(|a| a.ensure_gpu(queue, pool))
        });
        let b = self.buffers.as_ref().expect("made above");
        let dummy = &self.dummy;
        encoder.clear_buffer(&b.splats, 0, Some(16));
        {
            let mut pass = kernels::pass(encoder);
            for (j, u) in self.uniforms[..passes].iter().enumerate() {
                let sh = |q: usize| sh_bufs[4 * j + q].unwrap_or(dummy);
                self.sh.dispatch_in(
                    device,
                    &mut pass,
                    &[
                        globals,
                        u.buffer(),
                        p,
                        cd_buf.unwrap_or(dummy),
                        sh(0),
                        sh(1),
                        sh(2),
                        sh(3),
                        &b.colour,
                    ],
                    Work::Elements(n),
                );
            }
            let colour = if passes > 0 {
                &b.colour
            } else {
                cd_buf.unwrap_or(dummy)
            };
            self.project.dispatch_in(
                device,
                &mut pass,
                &[
                    globals,
                    self.uniforms[3].buffer(),
                    p,
                    colour,
                    scale_buf.unwrap_or(dummy),
                    orient_buf.unwrap_or(dummy),
                    header.unwrap_or(dummy),
                    &b.splats,
                    &b.keys,
                    &b.vals,
                ],
                Work::Elements(n),
            );
        }
        let order = self
            .sort
            .sort(device, queue, encoder, &b.keys, &b.vals, n, KEY_BITS)?;
        encoder.copy_buffer_to_buffer(&b.splats, 0, &self.indirect, 4, 4);
        self.ready = Some(
            self.binds
                .get(device, &self.layout, &[globals, &b.splats, order]),
        );
        Ok(())
    }

    /// Draws what [`Self::prepare`] recorded this frame.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, samples: u32) {
        let (Some(bind), Some((_, pipeline))) = (
            &self.ready,
            self.pipelines.iter().find(|(s, _)| *s == samples),
        ) else {
            return;
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw_indirect(&self.indirect, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_degree_follows_the_attributes_the_detail_and_the_material() {
        use atd_gpu::geo::Attribute;
        let mut g = Geometry::new();
        g.set_points(1, None);
        g.set_point(Attribute::from_vec3(names::P, &[[0.0; 3]]));
        assert_eq!(degree(&g, 3), 0, "no SH attributes");
        for name in &names::SH[..6] {
            g.set_point(Attribute::from_vec4(*name, &[[0.0; 4]]));
        }
        assert_eq!(degree(&g, 3), 2, "24 floats: degree 2");
        assert_eq!(degree(&g, 1), 1, "the material caps it");
        g.set_detail(names::SH_DEGREE, DetailValue::I32(1));
        assert_eq!(degree(&g, 3), 1, "the detail caps it");
    }
}
