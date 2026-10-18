//! `pnt.points`: a point source (grid, random box, line or circle), made on
//! the GPU. It cooks only when a parameter changes, and allocates only then.

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::kernels::{Bind, Kernel, Packer, Uniforms, Work};
use atd_gpu::geo::wgsl;

/// Most points: a 1-D dispatch reaches 65,535 × 256 elements.
pub const MAX_POINTS: i64 = 16_000_000;

const SHAPES: &[&str] = &["grid", "random", "line", "circle"];

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "points",
        "Points",
        "A point source: a grid, random points in a box, a line or a circle, made on the GPU. Writes `P`, `Cd` (colour) and `id` (0, 1, 2, …).",
        &[],
        vec![
            ParamSchema::menu("shape", "Shape", SHAPES, "grid").help(
                "grid: rows and columns in the XY plane; random: uniform in a box; line: along X; circle: in the XY plane.",
            ),
            ParamSchema::int("count", "Count", 10_000, Some(0), Some(MAX_POINTS))
                .help("Number of points (a grid uses the nearest rows × columns at or above it, trimmed to the count)."),
            ParamSchema::vec("size", "Size", &[2.0, 2.0, 2.0])
                .help("Extent along X, Y and Z (a circle's diameters along X and Y)."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("Centre of the shape."),
            ParamSchema::int("seed", "Seed", 1, Some(0), None)
                .help("Random seed (random shape and random colours); the same seed gives the same points."),
            ParamSchema::colour("colour", "Colour", [1.0, 1.0, 1.0, 1.0])
                .help("Colour written to `Cd`."),
            ParamSchema::toggle("random_colour", "Random Colour", false)
                .help("Multiplies the colour by a random hue per point."),
        ],
        150,
    )
}

const SOURCE: &str = r#"
struct Params {
    centre: vec4f,
    size: vec4f,
    colour: vec4f,
    count: u32,
    shape: u32,
    seed: u32,
    random_colour: u32,
    cols: u32,
    rows: u32,
    pad0: u32,
    pad1: u32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(2) var<storage, read_write> Cd: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> id: array<u32>;

fn hue(h: f32) -> vec3f {
    let k = abs(fract(vec3f(h) + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0;
    return clamp(k, vec3f(0.0), vec3f(1.0));
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3u) {
    let i = gid.x;
    if (i >= params.count) { return; }
    let s = params.size.xyz;
    var p = vec3f(0.0);
    switch params.shape {
        case 0u: {
            let c = i % params.cols;
            let r = i / params.cols;
            let fx = select(0.5, f32(c) / f32(max(params.cols, 2u) - 1u), params.cols > 1u);
            let fy = select(0.5, f32(r) / f32(max(params.rows, 2u) - 1u), params.rows > 1u);
            p = vec3f((fx - 0.5) * s.x, (fy - 0.5) * s.y, 0.0);
        }
        case 1u: {
            p = (atd_rand3(params.seed, i) - vec3f(0.5)) * s;
        }
        case 2u: {
            let f = select(0.5, f32(i) / f32(max(params.count, 2u) - 1u), params.count > 1u);
            p = vec3f((f - 0.5) * s.x, 0.0, 0.0);
        }
        default: {
            let a = f32(i) / f32(max(params.count, 1u)) * 6.28318530718;
            p = vec3f(cos(a) * s.x * 0.5, sin(a) * s.y * 0.5, 0.0);
        }
    }
    P[i] = vec4f(p + params.centre.xyz, 0.0);
    var c = params.colour;
    if (params.random_colour != 0u) {
        c = vec4f(hue(atd_rand(params.seed ^ 0x51ed270bu, i)) * c.rgb, c.a);
    }
    Cd[i] = c;
    id[i] = i;
}
"#;

pub struct Points {
    kernel: Kernel,
    params: Uniforms,
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Points {
        kernel: Kernel::new(
            device,
            "pnt.points",
            &format!("{}{SOURCE}", wgsl::HASH),
            "main",
            &[Uniform, ReadWrite, ReadWrite, ReadWrite],
        ),
        params: Uniforms::new(device, "pnt.points", 80),
    })
}

impl GeoOp for Points {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let count = p.i64("count").clamp(0, MAX_POINTS) as u32;
        let max = ctx.device.limits().max_storage_buffer_binding_size / 16;
        if u64::from(count) > max {
            ctx.output.clear();
            return Err(format!(
                "{count} points need more than this GPU's {} MiB storage binding; use at most {max}",
                ctx.device.limits().max_storage_buffer_binding_size >> 20
            ));
        }
        let shape = SHAPES
            .iter()
            .position(|s| *s == p.str("shape"))
            .unwrap_or(0) as u32;
        let cols = ((count as f64).sqrt().ceil() as u32).max(1);
        let rows = count.div_ceil(cols).max(1);
        let mut u = Packer::<80>::new();
        u.vec4(p.vec4("centre"))
            .vec4(p.vec4("size"))
            .vec4(p.vec4("colour"))
            .u32(count)
            .u32(shape)
            .u32(p.i64("seed").max(0) as u32)
            .u32(u32::from(p.bool("random_colour")))
            .u32(cols)
            .u32(rows);
        self.params.write(ctx.queue, u.bytes());
        let pos = ctx.alloc(names::P, AttrType::VEC3, count);
        let cd = ctx.alloc(names::CD, AttrType::VEC4, count);
        let id = ctx.alloc(names::ID, AttrType::U32, count);
        if count > 0 {
            let buffers = [&pos, &cd, &id].map(|a| a.gpu_buffer().expect("on the GPU").clone());
            self.kernel.dispatch(
                ctx.device,
                ctx.encoder,
                &[self.params.buffer(), &buffers[0], &buffers[1], &buffers[2]],
                Work::Elements(count),
            );
        }
        let header = ctx.cpu_header(count);
        let out = &mut *ctx.output;
        out.clear();
        out.set_points(count, Some(header));
        out.set_point(pos);
        out.set_point(cd);
        out.set_point(id);
        Ok(())
    }
}
