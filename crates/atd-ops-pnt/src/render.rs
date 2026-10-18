//! `img.render_points`: draws a geometry's points as sprites (design §4.4).
//!
//! WebGPU points are always one pixel, so each point is an instanced quad
//! (a 4-vertex strip) whose vertex shader pulls `P` and `Cd` from storage
//! buffers by instance index. A geometry whose count is on the GPU (a
//! simulation) is drawn with `draw_indirect` from its header, so the count
//! never comes back to the CPU. The camera is orthographic in the XY plane
//! (Z is ignored) to start with.
//!
//! Polyline primitives (Connect's lines) can be drawn too, as a line list
//! over the corner array: vertex `2c` and `2c + 1` are corners `c` and
//! `c + 1`, and a segment that crosses into the next primitive is moved off
//! screen (the primitive is found by a binary search of the start offsets).

use atd_core::geo::{AttrType, header, names};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::geo::kernels::{Bind, BindGroupCache, Packer, layout_entries};

const BLENDS: &[&str] = &["add", "over"];
const COLOURS: &[&str] = &["attribute", "constant"];
const DRAWS: &[&str] = &["points", "lines", "both"];

pub fn schema() -> OperatorSchema {
    let socket = |name: &str, wire| SocketSchema {
        name: name.into(),
        wire,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: "img.render_points".into(),
        family: OpFamily::Img,
        label: "Render Points".into(),
        summary: "Draws the points of a geometry as square sprites over a background, looking down the Z axis (orthographic), and optionally its polyline primitives as one-pixel lines (such as Connect's). Colour from the `Cd` attribute or a constant; additive blending makes dense regions glow.".into(),
        version: 1,
        params: vec![
            ParamSchema::menu("draw", "Draw", DRAWS, "points").help(
                "points: sprites; lines: the polyline primitives' segments, one pixel wide, coloured from their points (polygons are left to the mesh renderer); both: lines under sprites.",
            ),
            ParamSchema::float("size", "Size", 2.0, Some(0.5), Some(64.0))
                .help("Sprite size in pixels."),
            ParamSchema::menu("colour_from", "Colour From", COLOURS, "attribute").help(
                "attribute: each point's `Cd` times Colour (Colour alone when `Cd` is missing); constant: Colour.",
            ),
            ParamSchema::colour("colour", "Colour", [1.0, 1.0, 1.0, 1.0])
                .help("Sprite colour, or the multiplier of `Cd`."),
            ParamSchema::float("intensity", "Intensity", 1.0, Some(0.0), None)
                .help("Multiplies the sprite colour (useful with additive blending)."),
            ParamSchema::menu("blend", "Blend", BLENDS, "add")
                .help("add: sprites add up (glow, order-independent); over: alpha blending in point order."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0])
                .help("The point of the XY plane at the image centre."),
            ParamSchema::float("height", "Height", 2.0, Some(1e-6), None)
                .help("World units from the bottom to the top of the image (X follows the aspect ratio)."),
            ParamSchema::colour("background", "Background", [0.0, 0.0, 0.0, 1.0])
                .help("Colour the image is cleared to."),
        ],
        inputs: vec![socket("geo", WireType::Geo)],
        outputs: vec![socket("out", WireType::Img)],
        capabilities: vec![],
        cook_budget_us: 150,
        time_dependent: false,
        sink: false,
    }
}

const SOURCE: &str = r#"
struct Params {
    colour: vec4f,
    // centre x, y; scale to NDC x, y
    view: vec4f,
    // sprite size in NDC x, y; use Cd; unused
    sprite: vec4f,
    // primitives, unused...
    lines: vec4u,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> P: array<vec4f>;
@group(0) @binding(2) var<storage, read> Cd: array<vec4f>;

struct VsOut {
    @builtin(position) pos: vec4f,
    @location(0) colour: vec4f,
};

fn colour_of(i: u32) -> vec4f {
    var c = params.colour;
    if (params.sprite.z > 0.5) { c = c * Cd[i]; }
    return c;
}

@vertex
fn vs(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> VsOut {
    let p = P[i].xyz;
    let ndc = (p.xy - params.view.xy) * params.view.zw;
    // Strip corners: (-1,-1) (1,-1) (-1,1) (1,1).
    let corner = vec2f(f32(v & 1u), f32(v >> 1u)) * 2.0 - 1.0;
    var out: VsOut;
    out.pos = vec4f(ndc + corner * params.sprite.xy * 0.5, 0.0, 1.0);
    out.colour = colour_of(i);
    return out;
}

@fragment
fn fs(v: VsOut) -> @location(0) vec4f {
    return v.colour;
}
"#;

/// Lines: vertices `2c` and `2c + 1` are the segment from corner `c` to
/// `c + 1`; segments that cross from one primitive to the next, belong to a
/// polygon or join a point to itself are moved off screen.
const LINES: &str = r#"
@group(0) @binding(3) var<storage, read> corners: array<u32>;
@group(0) @binding(4) var<storage, read> starts: array<u32>;
@group(0) @binding(5) var<storage, read> kinds: array<u32>;

@vertex
fn vs_line(@builtin(vertex_index) v: u32) -> VsOut {
    let c = v / 2u;
    // The primitive holding corner c: the last start at or before it.
    var lo = 0u;
    var hi = params.lines.x;
    while (lo + 1u < hi) {
        let mid = (lo + hi) / 2u;
        if (starts[mid] <= c) { lo = mid; } else { hi = mid; }
    }
    let a = corners[c];
    let b = corners[c + 1u];
    var out: VsOut;
    if (c + 1u >= starts[lo + 1u] || kinds[lo] != 1u || a == b) {
        out.pos = vec4f(2.0, 2.0, 0.0, 1.0);
        out.colour = vec4f(0.0);
        return out;
    }
    let i = select(a, b, (v & 1u) == 1u);
    let ndc = (P[i].xyz.xy - params.view.xy) * params.view.zw;
    out.pos = vec4f(ndc, 0.0, 1.0);
    out.colour = colour_of(i);
    return out;
}
"#;

pub struct RenderPoints {
    pipelines: [wgpu::RenderPipeline; 2],
    line_pipelines: [wgpu::RenderPipeline; 2],
    layout: wgpu::BindGroupLayout,
    line_layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    last: Option<[u8; 64]>,
    /// Bound as `Cd` when the geometry has none.
    white: wgpu::Buffer,
    binds: BindGroupCache,
    line_binds: BindGroupCache,
    target: Option<(wgpu::Texture, wgpu::TextureView)>,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(RenderPoints::new(device, format))
}

impl RenderPoints {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        use Bind::*;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("img.render_points"),
            source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
        });
        let line_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("img.render_points.lines"),
            source: wgpu::ShaderSource::Wgsl(format!("{SOURCE}{LINES}").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img.render_points"),
            entries: &layout_entries(&[Uniform, Read, Read], wgpu::ShaderStages::VERTEX),
        });
        let line_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img.render_points.lines"),
            entries: &layout_entries(
                &[Uniform, Read, Read, Read, Read, Read],
                wgpu::ShaderStages::VERTEX,
            ),
        });
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let blends = [
            wgpu::BlendState {
                color: add,
                alpha: add,
            },
            wgpu::BlendState::ALPHA_BLENDING,
        ];
        let pipelines = |layout: &wgpu::BindGroupLayout,
                         module: &wgpu::ShaderModule,
                         entry: &str,
                         topology: wgpu::PrimitiveTopology| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("img.render_points"),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            blends.map(|blend| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("img.render_points"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: Some(blend),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology,
                        ..Default::default()
                    },
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            })
        };
        let sprite_pipelines = pipelines(
            &layout,
            &module,
            "vs",
            wgpu::PrimitiveTopology::TriangleStrip,
        );
        let line_pipelines = pipelines(
            &line_layout,
            &line_module,
            "vs_line",
            wgpu::PrimitiveTopology::LineList,
        );
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img.render_points"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let white = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img.render_points.white"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        Self {
            pipelines: sprite_pipelines,
            line_pipelines,
            layout,
            line_layout,
            uniforms,
            last: None,
            white,
            binds: BindGroupCache::default(),
            line_binds: BindGroupCache::default(),
            target: None,
        }
    }
}

impl GpuOp for RenderPoints {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let out = ctx.output;
        if self.target.as_ref().is_none_or(|(t, _)| t != out) {
            self.target = Some((out.clone(), out.create_view(&Default::default())));
        }
        let view = &self.target.as_ref().expect("set above").1;
        let geo = ctx.geo_inputs.get(0);
        let bg = p.vec4("background");
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img.render_points"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(bg[0]),
                        g: f64::from(bg[1]),
                        b: f64::from(bg[2]),
                        a: f64::from(bg[3]),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let Some(geo) = geo.filter(|g| g.points() > 0) else {
            return Ok(());
        };
        let Some(pos) = geo.point_typed(names::P, AttrType::VEC3) else {
            return Err("the geometry has no `P` (vec3f) attribute".into());
        };
        let cd = geo
            .point_typed(names::CD, AttrType::VEC4)
            .filter(|_| p.str("colour_from") == "attribute");
        let draw = p.str("draw");
        let lines = geo
            .topology()
            .filter(|t| draw != "points" && !t.is_empty() && t.corners.len() >= 2);
        let (w, h) = (out.width() as f32, out.height() as f32);
        let height = p.f32("height").max(1e-6);
        let centre = p.vec4("centre");
        let size = p.f32("size").clamp(0.5, 64.0);
        let colour = p.vec4("colour").map(|c| c * p.f32("intensity"));
        let mut u = Packer::<64>::new();
        u.vec4(colour)
            .vec4([centre[0], centre[1], 2.0 / (height * w / h), 2.0 / height])
            .vec4([
                2.0 * size / w,
                2.0 * size / h,
                f32::from(u8::from(cd.is_some())),
                0.0,
            ])
            .u32(lines.map_or(0, |t| t.len()));
        let bytes: [u8; 64] = u.bytes().try_into().expect("64 bytes");
        if self.last != Some(bytes) {
            ctx.queue.write_buffer(&self.uniforms, 0, &bytes);
            self.last = Some(bytes);
        }
        // Values only on the CPU (an MSH result) are uploaded once.
        let pos = pos.ensure_gpu(ctx.queue, ctx.buffers);
        let cd = match cd {
            Some(a) => a.ensure_gpu(ctx.queue, ctx.buffers),
            None => &self.white,
        };
        let blend = usize::from(p.str("blend") == "over");
        if let Some(t) = lines {
            let corners = t.corners.ensure_gpu(ctx.queue, ctx.buffers);
            let starts = t.start.ensure_gpu(ctx.queue, ctx.buffers);
            let kinds = t.kinds.ensure_gpu(ctx.queue, ctx.buffers);
            let bind = self.line_binds.get(
                ctx.device,
                &self.line_layout,
                &[&self.uniforms, pos, cd, corners, starts, kinds],
            );
            pass.set_pipeline(&self.line_pipelines[blend]);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..2 * (t.corners.len() - 1), 0..1);
        }
        if draw == "lines" {
            return Ok(());
        }
        let bind = self
            .binds
            .get(ctx.device, &self.layout, &[&self.uniforms, pos, cd]);
        pass.set_pipeline(&self.pipelines[blend]);
        pass.set_bind_group(0, &bind, &[]);
        match geo.header().filter(|_| geo.is_gpu_counted()) {
            Some(hdr) => pass.draw_indirect(hdr, header::DRAW_OFFSET),
            None => pass.draw(0..header::SPRITE_VERTICES, 0..geo.points()),
        }
        Ok(())
    }
}
