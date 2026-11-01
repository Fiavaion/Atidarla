//! `img.render` (PRD §6 "Render (GEO + MAT + camera)"): draws up to four
//! geometries, each with its material, through one camera into an image,
//! with a depth buffer and 4× MSAA.
//!
//! - **Primitives** become draw records on the GPU ([`crate::triangulate`]);
//!   the vertex shader pulls `P`, `N`, `Cd` (point, else prim) and `uv` from
//!   the attribute buffers by record (no vertex buffers, as Render Points).
//!   Triangles are drawn non-indexed from the records, three vertices each;
//!   polylines as screen-space quads; a geometry with no primitives, or any
//!   geometry with a Point Sprite material, as camera-facing sprites.
//! - **Normals:** `N` when the geometry has it, else the triangle's own
//!   (faceted). Back faces seen through a cull mode of `none` are lit from
//!   their side.
//! - **Hostile geometry:** topology is validated in [`crate::triangulate`];
//!   a triangle with any non-finite position (or one that projects to
//!   infinity) is dropped whole in the vertex shader; segments and sprites
//!   behind the camera are dropped.
//! - **Determinism:** no atomics, opaque drawing with a depth test of
//!   "less", draws in slot order and records in primitive order, so equal
//!   depths keep the first-drawn fragment on every run; antialiasing is
//!   MSAA, with line, sprite-edge and alpha coverage turned into a fixed
//!   sample mask (no blending, so no order dependence; alpha below 1 is
//!   screen-door transparency).
//! - **Splats:** a geometry with a Splat material ([`crate::splat`]) is
//!   drawn after every opaque one, blended back to front in an order a
//!   stable GPU sort fixes (so it too repeats exactly), tested against the
//!   opaque depth without writing it. Splat slots are drawn in slot order.

use lsr_core::geo::{AttrClass, AttrType, header, names};
use lsr_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use lsr_gpu::engine::{CookCtx, GpuOp};
use lsr_gpu::geo::kernels::{BindGroupCache, Packer};
use lsr_gpu::mat::{Material, Model};

use crate::camera::{Camera, direction};
use crate::splat::SplatDraw;
use crate::triangulate::Triangulator;

/// Geometry and material pairs.
pub const SLOTS: usize = 4;
const SAMPLES: u32 = 4;
pub(crate) const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const GLOBALS: usize = 272;
const DRAW: usize = 144;

const PROJECTIONS: &[&str] = &["perspective", "orthographic"];
const CAMERAS: &[&str] = &["orbit", "look_at"];
const CULL: &[&str] = &["back", "none", "front"];

/// Socket names of slot `k`: (geometry, material).
fn slot_sockets(k: usize) -> (String, String) {
    if k == 0 {
        ("geo".into(), "mat".into())
    } else {
        (format!("geo{}", k + 1), format!("mat{}", k + 1))
    }
}

pub fn schema() -> OperatorSchema {
    let socket = |name: String, wire| SocketSchema {
        name,
        wire,
        optional: true,
        feedback: false,
    };
    let mut inputs = Vec::new();
    for k in 0..SLOTS {
        let (g, m) = slot_sockets(k);
        inputs.push(socket(g, WireType::Geo));
        inputs.push(socket(m, WireType::Mat));
    }
    OperatorSchema {
        type_id: "img.render".into(),
        family: OpFamily::Img,
        label: "Render".into(),
        summary: "Draws up to four geometries (`geo`, `geo2`…), each with the material wired beside it (`mat`, `mat2`…; a grey PBR surface coloured by `Cd` when none), through a perspective or orthographic camera, lit by one directional light and ambient light, with a depth buffer and antialiasing. Polygons are filled (normals from `N`, else faceted; texture coordinates from `uv`), polylines drawn as lines, and geometry without primitives as points.".into(),
        version: 1,
        params: vec![
            ParamSchema::menu("projection", "Projection", PROJECTIONS, "perspective")
                .help("perspective: Field of View; orthographic: Ortho Height, no foreshortening."),
            ParamSchema::menu("camera", "Camera", CAMERAS, "orbit")
                .help("orbit: the camera circles Target at Distance, placed by Azimuth and Elevation (easy to animate); look_at: the camera sits at Position looking at Target."),
            ParamSchema::vec("target", "Target", &[0.0, 0.0, 0.0])
                .help("The point the camera looks at."),
            ParamSchema::float("azimuth", "Azimuth", 30.0, None, None)
                .help("Orbit: degrees around the Y axis (0 looks from +Z, 90 from +X)."),
            ParamSchema::float("elevation", "Elevation", 20.0, Some(-90.0), Some(90.0))
                .help("Orbit: degrees above the XZ plane."),
            ParamSchema::float("distance", "Distance", 4.0, Some(0.0), None)
                .help("Orbit: distance from Target."),
            ParamSchema::vec("position", "Position", &[0.0, 0.0, 4.0])
                .help("Look at: the camera's position."),
            ParamSchema::float("fov", "Field of View", 45.0, Some(1.0), Some(179.0))
                .help("Perspective: vertical angle of view in degrees."),
            ParamSchema::float("ortho_height", "Ortho Height", 3.0, Some(1e-6), None)
                .help("Orthographic: world units from the bottom to the top of the image."),
            ParamSchema::float("near", "Near", 0.05, Some(1e-6), None)
                .help("Nearest distance drawn."),
            ParamSchema::float("far", "Far", 100.0, Some(1e-6), None)
                .help("Farthest distance drawn."),
            ParamSchema::float("light_azimuth", "Light Azimuth", 40.0, None, None)
                .help("Direction the light comes from: degrees around the Y axis."),
            ParamSchema::float("light_elevation", "Light Elevation", 50.0, Some(-90.0), Some(90.0))
                .help("Direction the light comes from: degrees above the XZ plane."),
            ParamSchema::colour("light_colour", "Light Colour", [1.0, 1.0, 1.0, 1.0])
                .help("Colour of the directional light."),
            ParamSchema::float("light_intensity", "Light Intensity", 1.0, Some(0.0), None)
                .help("Multiplies Light Colour."),
            ParamSchema::colour("ambient", "Ambient", [0.15, 0.15, 0.18, 1.0])
                .help("Light from every direction (added to lit materials)."),
            ParamSchema::menu("cull", "Cull", CULL, "back")
                .help("Which polygon sides are not drawn: back (seen from behind; polygons wind counter-clockwise seen from their front), none, or front."),
            ParamSchema::toggle("antialias", "Antialias", true)
                .help("4× multisampling, with antialiased lines and sprite edges; off: aliased edges."),
            ParamSchema::toggle("wireframe", "Wireframe", false)
                .help("Draw the polygon edges over every surface."),
            ParamSchema::colour("wire_colour", "Wire Colour", [1.0, 1.0, 1.0, 1.0])
                .help("Colour of the wireframe overlay (alpha: its opacity)."),
            ParamSchema::float("wire_width", "Wire Width", 1.0, Some(0.25), Some(32.0))
                .help("Wireframe overlay width in pixels."),
            ParamSchema::float("point_size", "Point Size", 3.0, Some(0.5), Some(256.0))
                .help("Pixel size of the points of a geometry without primitives (Point Sprite materials set their own)."),
            ParamSchema::colour("background", "Background", [0.0, 0.0, 0.0, 1.0])
                .help("Colour the image is cleared to."),
        ],
        inputs,
        outputs: vec![socket("out".into(), WireType::Img)],
        capabilities: vec![],
        cook_budget_us: 250,
        time_dependent: false,
        sink: false,
    }
}

/// The camera block every `img.render` shader (and the splat passes) reads.
pub(crate) const GLOBALS_WGSL: &str = r#"
struct Globals {
    view_proj: mat4x4f,
    view: mat4x4f,
    // xyz camera position; w 1 perspective, 0 orthographic
    eye: vec4f,
    forward: vec4f,
    // towards the light
    light_dir: vec4f,
    light: vec4f,
    ambient: vec4f,
    wire: vec4f,
    // x wire width px (0: off), y unused, z, w image size
    misc: vec4f,
    // projection scale of view x, y
    scale: vec4f,
    // x antialiasing on
    opts: vec4u,
};
"#;

const SOURCE: &str = r#"
struct Draw {
    m: Material,
    // has N, point Cd, prim Cd, uv
    has: vec4u,
    // x segment width px, y has pscale
    extra: vec4f,
    // size, size in pixels, square, sphere shading
    sprite: vec4f,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<uniform> d: Draw;
@group(0) @binding(2) var<storage, read> recs: array<vec4u>;
@group(0) @binding(3) var<storage, read> P: array<vec4f>;
@group(0) @binding(4) var<storage, read> N: array<vec4f>;
@group(0) @binding(5) var<storage, read> Cd: array<vec4f>;
@group(0) @binding(6) var<storage, read> primCd: array<vec4f>;
@group(0) @binding(7) var<storage, read> UV: array<vec2f>;
@group(0) @binding(8) var<storage, read> pscale: array<f32>;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

fn finite3(v: vec3f) -> bool {
    let e = bitcast<vec3u>(v) & vec3u(0x7f800000u);
    return all(e != vec3u(0x7f800000u));
}
fn finite4(v: vec4f) -> bool {
    let e = bitcast<vec4u>(v) & vec4u(0x7f800000u);
    return all(e != vec4u(0x7f800000u));
}
// Outside the clip volume: the primitive is not drawn.
const GONE: vec4f = vec4f(2.0, 2.0, 2.0, 1.0);

fn colour_of(i: u32, prim: u32) -> vec4f {
    if (d.has.y != 0u) { return Cd[i]; }
    if (d.has.z != 0u) { return primCd[prim]; }
    return vec4f(1.0);
}

fn view_dir(world: vec3f) -> vec3f {
    if (g.eye.w > 0.5) { return normalize(g.eye.xyz - world); }
    return -g.forward.xyz;
}

// Coverage (edge antialiasing, alpha) becomes a sample mask: the first
// round(4a) of 4 samples, or with 1 sample, all of it from a = 0.5. There
// is no blending, so the result never depends on draw order; the colour
// written is opaque.
struct FragOut {
    @location(0) colour: vec4f,
    @builtin(sample_mask) mask: u32,
};
fn coverage(a: f32) -> u32 {
    if (g.opts.x != 0u) {
        let n = u32(clamp(a, 0.0, 1.0) * 4.0 + 0.5);
        return (1u << n) - 1u;
    }
    return select(0u, 1u, a >= 0.5);
}
fn frag(rgb: vec3f, a: f32) -> FragOut {
    var o: FragOut;
    o.colour = vec4f(rgb, 1.0);
    o.mask = coverage(a);
    return o;
}

struct TriOut {
    @builtin(position) pos: vec4f,
    @location(0) world: vec3f,
    @location(1) n: vec3f,
    @location(2) cd: vec4f,
    @location(3) uv: vec2f,
    @location(4) bary: vec3f,
    @location(5) @interpolate(flat, either) edges: u32,
    @location(6) @interpolate(flat, either) face: vec3f,
};

@vertex
fn vs_tri(@builtin(vertex_index) vi: u32) -> TriOut {
    let t = vi / 3u;
    let j = vi - t * 3u;
    let r = recs[t];
    let p0 = P[r.x].xyz;
    let p1 = P[r.y].xyz;
    let p2 = P[r.z].xyz;
    let c0 = g.view_proj * vec4f(p0, 1.0);
    let c1 = g.view_proj * vec4f(p1, 1.0);
    let c2 = g.view_proj * vec4f(p2, 1.0);
    var o: TriOut;
    o.edges = r.w >> 29u;
    if (!(finite4(c0) && finite4(c1) && finite4(c2))) {
        o.pos = GONE;
        return o;
    }
    var idx = r.x;
    var p = p0;
    var c = c0;
    o.bary = vec3f(1.0, 0.0, 0.0);
    if (j == 1u) { idx = r.y; p = p1; c = c1; o.bary = vec3f(0.0, 1.0, 0.0); }
    if (j == 2u) { idx = r.z; p = p2; c = c2; o.bary = vec3f(0.0, 0.0, 1.0); }
    let f = cross(p1 - p0, p2 - p0);
    let fl = length(f);
    var face = vec3f(0.0, 0.0, 1.0);
    if (fl > 0.0 && finite3(f / fl)) { face = f / fl; }
    o.pos = c;
    o.world = p;
    o.face = face;
    o.n = face;
    if (d.has.x != 0u) { o.n = N[idx].xyz; }
    o.cd = colour_of(idx, r.w & 0x1fffffffu);
    o.uv = vec2f(0.0);
    if (d.has.w != 0u) { o.uv = UV[idx]; }
    return o;
}

@fragment
fn fs_tri(i: TriOut, @builtin(front_facing) front: bool) -> FragOut {
    let fw = fwidth(i.bary);
    let ln = length(i.n);
    var n = i.face;
    if (ln > 1e-20 && finite3(i.n / ln)) { n = i.n / ln; }
    if (!front) { n = -n; }
    let v = view_dir(i.world);
    let nv = (g.view * vec4f(n, 0.0)).xyz;
    let mc = vec2f(nv.x * 0.5 + 0.5, 0.5 - nv.y * 0.5);
    // `uv` has v up (0 at the bottom of the image); textures have v down.
    let suv = select(vec2f(i.uv.x, 1.0 - i.uv.y), mc, d.m.flags.x == MODEL_MATCAP);
    let sampled = textureSample(tex, samp, suv);
    let texel = select(vec4f(1.0), sampled, d.m.flags.z != 0u);
    var col = shade(d.m, n, v, g.light_dir.xyz, g.light.rgb, g.ambient.rgb, i.cd, texel);
    // Distance in pixels to the nearest side of the polygon.
    let px = i.bary / max(fw, vec3f(1e-6));
    var e = 1e9;
    if ((i.edges & 1u) != 0u) { e = min(e, px.x); }
    if ((i.edges & 2u) != 0u) { e = min(e, px.y); }
    if ((i.edges & 4u) != 0u) { e = min(e, px.z); }
    if (d.m.flags.x == MODEL_LINE) {
        let cov = clamp(d.m.params.x * 0.5 + 0.5 - e, 0.0, 1.0);
        let edge = mat_base(d.m, i.cd, texel);
        if (d.m.colour2.a <= 0.0) {
            let o = frag(edge.rgb, cov * edge.a);
            if (o.mask == 0u) { discard; }
            return o;
        }
        col = vec4f(mix(d.m.colour2.rgb, edge.rgb, cov), 1.0);
    }
    if (g.misc.x > 0.0) {
        let cov = clamp(g.misc.x * 0.5 + 0.5 - e, 0.0, 1.0) * g.wire.a;
        col = vec4f(mix(col.rgb, g.wire.rgb, cov), col.a);
    }
    let o = frag(col.rgb, col.a);
    if (o.mask == 0u) { discard; }
    return o;
}

struct SegOut {
    @builtin(position) pos: vec4f,
    @location(0) cd: vec4f,
    @location(1) @interpolate(linear) side: f32,
};

@vertex
fn vs_seg(@builtin(vertex_index) vi: u32) -> SegOut {
    let s = vi / 6u;
    let k = vi - s * 6u;
    let r = recs[s];
    var o: SegOut;
    let ca = g.view_proj * vec4f(P[r.x].xyz, 1.0);
    let cb = g.view_proj * vec4f(P[r.y].xyz, 1.0);
    if (!(finite4(ca) && finite4(cb)) || ca.w <= 1e-6 || cb.w <= 1e-6) {
        o.pos = GONE;
        return o;
    }
    // Quad corners as (end, side): (0,-) (1,-) (0,+) (0,+) (1,-) (1,+).
    var end = 0u;
    if (k == 1u || k == 4u || k == 5u) { end = 1u; }
    var side = -1.0;
    if (k == 2u || k == 3u || k == 5u) { side = 1.0; }
    let half_res = g.misc.zw * 0.5;
    let sa = ca.xy / ca.w * half_res;
    let sb = cb.xy / cb.w * half_res;
    var dir = sb - sa;
    let len = length(dir);
    if (len > 1e-6) { dir = dir / len; } else { dir = vec2f(1.0, 0.0); }
    let nrm = vec2f(-dir.y, dir.x);
    let half_w = d.extra.x * 0.5 + 1.0;
    var c = ca;
    var idx = r.x;
    if (end == 1u) { c = cb; idx = r.y; }
    o.pos = vec4f(c.xy + nrm * side * half_w / half_res * c.w, c.z, c.w);
    o.side = side * half_w;
    o.cd = colour_of(idx, r.z);
    return o;
}

@fragment
fn fs_seg(i: SegOut) -> FragOut {
    let cov = clamp(d.extra.x * 0.5 + 0.5 - abs(i.side), 0.0, 1.0);
    let base = mat_base(d.m, i.cd, vec4f(1.0));
    let o = frag(base.rgb + d.m.emission.rgb, cov * base.a);
    if (o.mask == 0u) { discard; }
    return o;
}

struct SpriteOut {
    @builtin(position) pos: vec4f,
    @location(0) cd: vec4f,
    @location(1) corner: vec2f,
};

@vertex
fn vs_sprite(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> SpriteOut {
    var o: SpriteOut;
    let c = g.view_proj * vec4f(P[i].xyz, 1.0);
    if (!finite4(c) || c.w <= 1e-6) {
        o.pos = GONE;
        return o;
    }
    let corner = vec2f(f32(v & 1u), f32(v >> 1u)) * 2.0 - 1.0;
    var size = d.sprite.x;
    if (d.extra.y != 0.0) { size = size * pscale[i]; }
    var off: vec2f;
    if (d.sprite.y != 0.0) {
        // Pixels, with one pixel of edge for antialiasing.
        let s = size * 0.5 + 1.0;
        off = corner * s * 2.0 / g.misc.zw * c.w;
        o.corner = corner * s / max(size * 0.5, 1e-6);
    } else {
        off = corner * size * 0.5 * g.scale.xy;
        o.corner = corner;
    }
    if (!finite3(vec3f(off, 0.0))) {
        o.pos = GONE;
        return o;
    }
    o.pos = vec4f(c.xy + off, c.z, c.w);
    o.cd = colour_of(i, 0u);
    return o;
}

@fragment
fn fs_sprite(i: SpriteOut) -> FragOut {
    let r = length(i.corner);
    let fw = max(fwidth(r), 1e-6);
    var cov = 1.0;
    if (d.sprite.z == 0.0) { cov = clamp((1.0 - r) / fw + 0.5, 0.0, 1.0); }
    else if (max(abs(i.corner.x), abs(i.corner.y)) > 1.0) { cov = 0.0; }
    let base = mat_base(d.m, i.cd, vec4f(1.0));
    var rgb = base.rgb;
    if (d.sprite.w != 0.0) {
        let right = vec3f(g.view[0].x, g.view[1].x, g.view[2].x);
        let up = vec3f(g.view[0].y, g.view[1].y, g.view[2].y);
        let back = vec3f(g.view[0].z, g.view[1].z, g.view[2].z);
        let z = sqrt(max(1.0 - dot(i.corner, i.corner), 0.0));
        let n = normalize(right * i.corner.x + up * i.corner.y + back * z);
        rgb = base.rgb * (g.ambient.rgb + g.light.rgb * max(dot(n, g.light_dir.xyz), 0.0));
    }
    let o = frag(rgb + d.m.emission.rgb, cov * base.a);
    if (o.mask == 0u) { discard; }
    return o;
}
"#;

/// Pipelines for one sample count.
#[derive(Debug)]
struct Pipelines {
    samples: u32,
    /// Cull back, none, front.
    tri: [wgpu::RenderPipeline; 3],
    seg: wgpu::RenderPipeline,
    sprite: wgpu::RenderPipeline,
}

/// Per-slot GPU state.
#[derive(Debug)]
struct Slot {
    draw: wgpu::Buffer,
    last: Option<[u8; DRAW]>,
    tri: Triangulator,
}

/// Targets of one output size.
#[derive(Debug)]
struct Targets {
    out: wgpu::Texture,
    out_view: wgpu::TextureView,
    samples: u32,
    msaa: Option<wgpu::TextureView>,
    depth: wgpu::TextureView,
}

pub struct Render {
    format: wgpu::TextureFormat,
    module: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipelines: Vec<Pipelines>,
    globals: wgpu::Buffer,
    last_globals: Option<[u8; GLOBALS]>,
    slots: Vec<Slot>,
    /// Bound for attributes a geometry does not have (never read then).
    dummy: wgpu::Buffer,
    binds: BindGroupCache,
    /// Texture bind groups by texture, most recently used last.
    tex_binds: Vec<(wgpu::Texture, wgpu::BindGroup)>,
    targets: Option<Targets>,
    msaa_ok: bool,
    status: Option<String>,
    /// Splat drawing per slot, made when a slot first has a Splat material.
    splats: Vec<Option<SplatDraw>>,
}

impl std::fmt::Debug for Render {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Render")
            .field("format", &self.format)
            .field("msaa_ok", &self.msaa_ok)
            .finish_non_exhaustive()
    }
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Render::new(device, format))
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl Render {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let source = format!(
            "{}{}{GLOBALS_WGSL}{SOURCE}",
            lsr_gpu::mat::WGSL,
            lsr_gpu::mat::SHADE
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("img.render"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let mut entries = vec![uniform_entry(0), uniform_entry(1)];
        entries.extend((2..9).map(storage_entry));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img.render"),
            entries: &entries,
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img.render.texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("img.render"),
            bind_group_layouts: &[Some(&layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("img.render"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let buffer = |label, size: usize, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage,
                mapped_at_creation: false,
            })
        };
        let uniform = wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST;
        let slots = (0..SLOTS)
            .map(|_| Slot {
                draw: buffer("img.render.draw", DRAW, uniform),
                last: None,
                tri: Triangulator::new(device),
            })
            .collect();
        let features = format.guaranteed_format_features(device.features());
        let depth = DEPTH.guaranteed_format_features(device.features());
        let msaa = wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4;
        let msaa_ok = features
            .flags
            .contains(msaa | wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE)
            && depth.flags.contains(msaa);
        Self {
            format,
            module,
            pipeline_layout,
            layout,
            tex_layout,
            sampler,
            pipelines: Vec::new(),
            globals: buffer("img.render.globals", GLOBALS, uniform),
            last_globals: None,
            slots,
            dummy: buffer("img.render.dummy", 16, wgpu::BufferUsages::STORAGE),
            binds: BindGroupCache::default(),
            tex_binds: Vec::new(),
            targets: None,
            msaa_ok,
            status: (!msaa_ok)
                .then(|| "antialiasing is not available for this image format".to_owned()),
            splats: (0..SLOTS).map(|_| None).collect(),
        }
    }

    fn pipelines(&mut self, device: &wgpu::Device, samples: u32) -> &Pipelines {
        if let Some(i) = self.pipelines.iter().position(|p| p.samples == samples) {
            return &self.pipelines[i];
        }
        let make = |vs: &str, fs: &str, topology, cull| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("img.render"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: cull,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(true),
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
            })
        };
        use wgpu::PrimitiveTopology::{TriangleList, TriangleStrip};
        let p = Pipelines {
            samples,
            tri: [
                make("vs_tri", "fs_tri", TriangleList, Some(wgpu::Face::Back)),
                make("vs_tri", "fs_tri", TriangleList, None),
                make("vs_tri", "fs_tri", TriangleList, Some(wgpu::Face::Front)),
            ],
            seg: make("vs_seg", "fs_seg", TriangleList, None),
            sprite: make("vs_sprite", "fs_sprite", TriangleStrip, None),
        };
        self.pipelines.push(p);
        self.pipelines.last().expect("pushed")
    }

    fn targets(&mut self, device: &wgpu::Device, out: &wgpu::Texture, samples: u32) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| &t.out == out && t.samples == samples)
        {
            return;
        }
        let size = out.size();
        let tex = |format, samples, label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        // Keep the attachments when only the pooled output changed.
        let (msaa, depth) = match self.targets.take() {
            Some(t) if t.out.size() == size && t.samples == samples => (t.msaa, t.depth),
            _ => (
                (samples > 1).then(|| tex(self.format, samples, "img.render.msaa")),
                tex(DEPTH, samples, "img.render.depth"),
            ),
        };
        self.targets = Some(Targets {
            out: out.clone(),
            out_view: out.create_view(&Default::default()),
            samples,
            msaa,
            depth,
        });
    }

    fn texture_bind(&mut self, device: &wgpu::Device, texture: &wgpu::Texture) -> wgpu::BindGroup {
        if let Some(i) = self.tex_binds.iter().position(|(t, _)| t == texture) {
            let hit = self.tex_binds.remove(i);
            let bind = hit.1.clone();
            self.tex_binds.push(hit);
            return bind;
        }
        let view = texture.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("img.render.texture"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        if self.tex_binds.len() == 16 {
            self.tex_binds.remove(0);
        }
        self.tex_binds.push((texture.clone(), bind.clone()));
        bind
    }
}

/// The camera, light and options of one cook, as the `Globals` block.
fn globals(p: &lsr_gpu::engine::Params<'_>, size: (u32, u32), msaa: bool) -> [u8; GLOBALS] {
    let (w, h) = (size.0 as f32, size.1 as f32);
    let v3 = |name| {
        let v = p.vec4(name);
        [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
    };
    let target = v3("target");
    let eye = if p.str("camera") == "look_at" {
        v3("position")
    } else {
        let d = direction(f64::from(p.f32("azimuth")), f64::from(p.f32("elevation")));
        let r = f64::from(p.f32("distance").max(0.0));
        [
            target[0] + d[0] * r,
            target[1] + d[1] * r,
            target[2] + d[2] * r,
        ]
    };
    let perspective = p.str("projection") != "orthographic";
    let cam = Camera {
        eye,
        target,
        fov: f64::from(p.f32("fov")),
        ortho_height: f64::from(p.f32("ortho_height")),
        near: f64::from(p.f32("near")),
        far: f64::from(p.f32("far")),
        perspective,
        aspect: f64::from(w / h),
    }
    .sanitised();
    let m = cam.matrices();
    let l = direction(
        f64::from(p.f32("light_azimuth")),
        f64::from(p.f32("light_elevation")),
    );
    let li = p.f32("light_intensity").max(0.0);
    let lc = p.vec4("light_colour");
    let wire = if p.bool("wireframe") {
        p.f32("wire_width").clamp(0.25, 32.0)
    } else {
        0.0
    };
    let mut u = Packer::<GLOBALS>::new();
    for col in m.view_proj.iter().chain(&m.view) {
        u.vec4(*col);
    }
    u.vec4([
        cam.eye[0] as f32,
        cam.eye[1] as f32,
        cam.eye[2] as f32,
        f32::from(u8::from(perspective)),
    ])
    .vec4([m.forward[0], m.forward[1], m.forward[2], 0.0])
    .vec4([l[0] as f32, l[1] as f32, l[2] as f32, 0.0])
    .vec4([lc[0] * li, lc[1] * li, lc[2] * li, 1.0])
    .vec4(p.vec4("ambient"))
    .vec4(p.vec4("wire_colour"))
    .vec4([wire, 0.0, w, h])
    .vec4([m.scale[0], m.scale[1], 0.0, 0.0])
    .u32(u32::from(msaa));
    u.bytes().try_into().expect("GLOBALS bytes")
}

/// What one slot draws, prepared before the render pass.
struct Prepared<'a> {
    points: bool,
    gpu_count: Option<&'a wgpu::Buffer>,
    count: u32,
    /// `P`, `N`, `Cd`, prim `Cd`, `uv`, `pscale` (`None`: not present).
    bufs: [Option<&'a wgpu::Buffer>; 6],
    tex_bind: wgpu::BindGroup,
}

/// The per-slot `Draw` block: the material, which attributes exist, the
/// segment width and the sprite settings.
fn draw_block(
    material: &Material,
    has_texture: bool,
    has: [bool; 5],
    point_size: f32,
) -> [u8; DRAW] {
    let sprite = if material.model == Model::Sprite {
        material.params
    } else {
        // Points of a geometry without primitives: discs in pixels.
        [point_size, 1.0, 0.0, 0.0]
    };
    let seg_width = if material.model == Model::Line {
        material.params[0]
    } else {
        1.0
    };
    let mut t = Packer::<48>::new();
    for h in &has[..4] {
        t.u32(u32::from(*h));
    }
    t.vec4([seg_width, f32::from(u8::from(has[4])), 0.0, 0.0])
        .vec4(sprite);
    let mut block = [0u8; DRAW];
    block[..lsr_gpu::mat::BYTES].copy_from_slice(&material.pack(has_texture));
    block[lsr_gpu::mat::BYTES..].copy_from_slice(t.bytes());
    block
}

impl GpuOp for Render {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let out = ctx.output;
        let samples = if p.bool("antialias") && self.msaa_ok {
            SAMPLES
        } else {
            1
        };
        self.targets(ctx.device, out, samples);
        self.pipelines(ctx.device, samples);
        let bytes = globals(&p, (out.width(), out.height()), samples > 1);
        if self.last_globals != Some(bytes) {
            ctx.queue.write_buffer(&self.globals, 0, &bytes);
            self.last_globals = Some(bytes);
        }
        let cull = match p.str("cull") {
            "none" => 1,
            "front" => 2,
            _ => 0,
        };
        let point_size = p.f32("point_size").clamp(0.5, 256.0);

        // Before the render pass: triangulation (compute), uniforms,
        // uploads of CPU-only attributes and texture bind groups.
        let mut prepared: [Option<Prepared<'_>>; SLOTS] = Default::default();
        let mut splat_slots = [false; SLOTS];
        let mut first_error = None;
        for (k, slot_out) in prepared.iter_mut().enumerate() {
            let geo_inputs = ctx.geo_inputs;
            let Some(geo) = geo_inputs.get(2 * k).filter(|g| g.points() > 0) else {
                continue;
            };
            let Some(pos) = geo.point_typed(names::P, AttrType::VEC3) else {
                first_error.get_or_insert_with(|| {
                    format!(
                        "the geometry on `{}` has no `P` (vec3f) attribute",
                        slot_sockets(k).0
                    )
                });
                continue;
            };
            let mat_inputs = ctx.mat_inputs;
            let material = mat_inputs.get(2 * k + 1).cloned().unwrap_or_default();
            if material.model == Model::Splat {
                let format = self.format;
                let draw = self.splats[k]
                    .get_or_insert_with(|| SplatDraw::new(ctx.device, format, GLOBALS_WGSL));
                match draw.prepare(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    ctx.buffers,
                    &self.globals,
                    geo,
                    &material,
                    samples,
                ) {
                    Ok(()) => splat_slots[k] = true,
                    Err(e) => {
                        first_error.get_or_insert_with(|| format!("`{}`: {e}", slot_sockets(k).0));
                    }
                }
                continue;
            }
            let points = material.model == Model::Sprite || geo.prims() == 0;
            if !points
                && let Err(e) =
                    self.slots[k]
                        .tri
                        .update(ctx.device, ctx.queue, ctx.buffers, ctx.encoder, geo)
            {
                first_error.get_or_insert_with(|| format!("`{}`: {e}", slot_sockets(k).0));
                continue;
            }
            let attrs = [
                Some(pos),
                geo.point_typed(names::N, AttrType::VEC3),
                geo.point_typed(names::CD, AttrType::VEC4),
                geo.attr(AttrClass::Prim, names::CD)
                    .filter(|a| a.ty() == AttrType::VEC4),
                geo.point_typed(names::UV, AttrType::VEC2),
                geo.point_typed(names::PSCALE, AttrType::F32),
            ];
            let texture = mat_inputs.texture(&material);
            let block = draw_block(
                &material,
                texture.is_some(),
                [
                    attrs[1].is_some(),
                    attrs[2].is_some(),
                    attrs[3].is_some(),
                    attrs[4].is_some(),
                    attrs[5].is_some(),
                ],
                point_size,
            );
            let slot = &mut self.slots[k];
            if slot.last != Some(block) {
                ctx.queue.write_buffer(&slot.draw, 0, &block);
                slot.last = Some(block);
            }
            // Values only on the CPU (an MSH result) are uploaded once.
            let bufs = attrs.map(|a| a.map(|a| a.ensure_gpu(ctx.queue, ctx.buffers)));
            let tex_bind = self.texture_bind(ctx.device, texture.unwrap_or(ctx.black));
            *slot_out = Some(Prepared {
                points,
                gpu_count: geo.header().filter(|_| geo.is_gpu_counted()),
                count: geo.points(),
                bufs,
                tex_bind,
            });
        }

        let Self {
            pipelines,
            slots,
            targets,
            globals,
            dummy,
            binds,
            layout,
            splats,
            ..
        } = self;
        let bg = p.vec4("background");
        let targets = targets.as_ref().expect("made above");
        let (view, resolve) = match &targets.msaa {
            Some(m) => (m, Some(&targets.out_view)),
            None => (&targets.out_view, None),
        };
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img.render"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: resolve,
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
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let pipes = pipelines
            .iter()
            .find(|q| q.samples == samples)
            .expect("made above");
        for (k, prep) in prepared.iter().enumerate() {
            let Some(prep) = prep else { continue };
            let slot = &slots[k];
            let b = prep.bufs.map(|b| b.unwrap_or(dummy));
            pass.set_bind_group(1, &prep.tex_bind, &[]);
            let mut bind = |records: &wgpu::Buffer| {
                binds.get(
                    ctx.device,
                    layout,
                    &[
                        globals, &slot.draw, records, b[0], b[1], b[2], b[3], b[4], b[5],
                    ],
                )
            };
            if prep.points {
                let group = bind(dummy);
                pass.set_pipeline(&pipes.sprite);
                pass.set_bind_group(0, &group, &[]);
                match prep.gpu_count {
                    Some(hdr) => pass.draw_indirect(hdr, header::DRAW_OFFSET),
                    None => pass.draw(0..header::SPRITE_VERTICES, 0..prep.count),
                }
                continue;
            }
            let Some(rec) = slot.tri.ready() else {
                continue;
            };
            if rec.any_tris {
                let group = bind(rec.tris);
                pass.set_pipeline(&pipes.tri[cull]);
                pass.set_bind_group(0, &group, &[]);
                pass.draw_indirect(rec.tri_header, header::DRAW_OFFSET);
            }
            if rec.any_segs {
                let group = bind(rec.segs);
                pass.set_pipeline(&pipes.seg);
                pass.set_bind_group(0, &group, &[]);
                pass.draw_indirect(rec.seg_header, header::DRAW_OFFSET);
            }
        }
        // Splats after every opaque geometry: they blend over it.
        for (draw, on) in splats.iter().zip(splat_slots) {
            if let (Some(draw), true) = (draw, on) {
                draw.draw(&mut pass, samples);
            }
        }
        drop(pass);
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }
}
