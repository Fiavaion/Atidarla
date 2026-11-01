//! Attribute Promote: a point attribute reduced over the whole geometry
//! (minimum, maximum, sum or average, component by component) and written
//! back to every point, so per-point operators can use the geometry's
//! extent, total or centre (normalising by the maximum, centring on the
//! average).
//!
//! The reduction runs in one workgroup: each thread folds a fixed stride of
//! points, then a tree in workgroup memory folds the threads, so the order
//! of every addition is fixed and the result repeats exactly. Detail
//! attributes live on the CPU and would need a readback (one frame late),
//! so the value stays on the GPU as a point attribute.

use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp};
use lsr_gpu::geo::kernels::{Bind, Packer, Work, pass};

use crate::kit::{self, Missing, PointKernel, desc, menu_index, typed_access};

const OPS: &[&str] = &["min", "max", "sum", "average"];

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "attribute_promote",
        "Attribute Promote",
        "Reduces a point attribute over all the points (min, max, sum or average, component by component) and writes the result to every point as Result, with the source's type (average and sum of integers are rounded). Use it to normalise by the largest value, centre on the average position, or scale by a total. Works on GPU-counted geometry (only live points count).",
        &[("in", false)],
        vec![
            ParamSchema::string("name", "Name", "P").help("The point attribute reduced."),
            ParamSchema::menu("operation", "Operation", OPS, "average").help("min, max: the smallest, largest value of each component; sum: their total; average: the total over the live point count."),
            ParamSchema::string("result", "Result", "promoted").help("The attribute written on every point (not Name)."),
        ],
        300,
    )
}

const REDUCE: &str = "struct Params {
    d: vec4u,
    // operation, unused, unused, unused
    op: vec4u,
};
@group(0) @binding(2) var<storage, read> A: array<u32>;
@group(0) @binding(3) var<storage, read_write> R: array<vec4f>;
var<workgroup> acc: array<vec4f, 256>;
fn fold(a: vec4f, b: vec4f) -> vec4f {
    switch params.op.x {
        case 0u: { return min(a, b); }
        case 1u: { return max(a, b); }
        default: { return a + b; }
    }
}
fn start() -> vec4f {
    switch params.op.x {
        case 0u: { return vec4f(3.4e38); }
        case 1u: { return vec4f(-3.4e38); }
        default: { return vec4f(0.0); }
    }
}
";

const REDUCE_MAIN: &str = "var v = start();
    for (var k = i; k < hdr.count; k = k + 256u) {
        v = fold(v, ld_A(k));
    }
    acc[i] = v;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (i < s) { acc[i] = fold(acc[i], acc[i + s]); }
        workgroupBarrier();
    }
    if (i == 0u) {
        var r = acc[0];
        if (params.op.x == 3u) { r = r / f32(max(hdr.count, 1u)); }
        if (hdr.count == 0u) { r = vec4f(0.0); }
        R[0] = r;
    }";

const BROADCAST: &str = "struct Params { d: vec4u };
@group(0) @binding(2) var<storage, read> R: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> D: array<u32>;
fn point(i: u32) { st_D(i, R[0]); }
";

pub struct Promote {
    reduce: PointKernel,
    broadcast: PointKernel,
    result: wgpu::Buffer,
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Promote {
        reduce: PointKernel::with_main(
            device,
            "pnt.attribute_promote.reduce",
            &format!("{REDUCE}{}", typed_access("A", "params.d", true)),
            &[Read, ReadWrite],
            32,
            REDUCE_MAIN,
        ),
        broadcast: PointKernel::new(
            device,
            "pnt.attribute_promote",
            &format!("{BROADCAST}{}", typed_access("D", "params.d", false)),
            &[Read, ReadWrite],
            16,
        ),
        result: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.attribute_promote.result"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }),
    })
}

impl GeoOp for Promote {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        let p = ctx.params;
        let (name, result) = (p.str("name"), p.str("result"));
        if !lsr_core::geo::valid_name(result) {
            return Err(format!("`{result}` is not a valid attribute name"));
        }
        if name == result {
            return Err("write the result to another attribute than Name".into());
        }
        let (a, ty) = kit::readable_any(ctx, &g, name)?;
        let hdr = kit::header_of(&g)?;
        let d = kit::writable(ctx, &mut g, result, ty, Missing::Create)?;
        let mut u = Packer::<32>::new();
        for w in desc(ty) {
            u.u32(w);
        }
        u.u32(menu_index(OPS, p.str("operation")));
        self.reduce.write(ctx.queue, u.bytes());
        self.broadcast.write(ctx.queue, &u.bytes()[..16]);
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.reduce
            .dispatch_in(device, &mut ps, &hdr, &[&a, &self.result], Work::Groups(1));
        self.broadcast
            .run_in(device, &mut ps, &hdr, &[&self.result, &d]);
        drop(ps);
        *ctx.output = g;
        Ok(())
    }
}
