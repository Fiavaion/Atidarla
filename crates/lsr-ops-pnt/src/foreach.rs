//! For-Each blocks (`docs/design/solver-foreach.md`): `pnt.foreach` (Begin)
//! and `pnt.foreach_end` (End). The body between them is ordinary
//! operators; the engine cooks it once per pass (`GeoOp::repeat`, a loop
//! closed by End's output wired back into Begin's `end` feedback input).
//!
//! Begin gives pass `k` either the whole input (count) or the points whose
//! piece attribute is `k` (pieces), with `iteration = k` on every point and
//! as a detail value. It writes into buffers of its own, re-made only when
//! the input's attributes or size change, so the body can work in place and
//! keeps the same bind groups from pass to pass.
//!
//! End copies each pass's result after the previous ones (buffer copies, no
//! bind groups) and, after the last pass, outputs them merged: directly when
//! every pass had a CPU-known count, else compacted by the live counts it
//! copied from each pass's header (the counts never leave the GPU).

use std::sync::Arc;

use lsr_core::geo::{AttrClass, AttrType, WORKGROUP, buffer_bytes, header};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp, MAX_REPEAT, Params, Repeat};
use lsr_gpu::geo::kernels::{Bind, Compactor, Gather, GatherMap, Kernel, Uniforms, Work, pass};
use lsr_gpu::geo::{ATTR_USAGE, Attribute, DetailValue, GeoBuffer, Geometry};

use crate::kit::{self, PointKernel, desc, menu_index, typed_access};
use crate::select::{Compact, MAX_ATTRS, drop_prims};

/// Most passes of a For-Each block per frame.
pub const MAX_ITERATIONS: u32 = MAX_REPEAT;

const MODES: &[&str] = &["count", "pieces"];

/// The attribute (and detail value) Begin writes.
const ITERATION: &str = "iteration";

// ------------------------------------------------------------------- Begin

pub fn begin_schema() -> OperatorSchema {
    crate::pnt(
        "foreach",
        "For-Each",
        "For-Each Begin: the operators between it and a For-Each End run once per pass, and End merges the passes' results. Count: every pass gets the whole input; pieces: pass k gets the points whose piece attribute is k (from Piece). Each pass's points carry `iteration` (its number, also a detail value), so operators keyed by an attribute (Randomize, Attribute Math, Colour by Attribute) differ per pass. Wire End's output into `end` to close the block.",
        &[("in", false), ("end", true)],
        vec![
            ParamSchema::menu("mode", "Mode", MODES, "count").help(
                "count: every pass gets the whole input; pieces: pass k gets the points whose Piece Attribute is k (pieces numbered from Iterations up are left out).",
            ),
            ParamSchema::int("iterations", "Iterations", 4, Some(1), Some(i64::from(MAX_ITERATIONS)))
                .help("How many passes (count), or pieces numbered 0 to this − 1 (pieces). The body costs this many times its single cost."),
            ParamSchema::string("attribute", "Piece Attribute", "piece")
                .help("pieces: the one-component point attribute that numbers the pieces (Piece writes `piece`; chunks or attribute buckets number them from 0)."),
        ],
        60,
    )
}

const FLAGS: &str = "struct Params {
    d: vec4u,
    k: u32,
    cap: u32,
    pad0: u32,
    pad1: u32,
};
@group(0) @binding(2) var<storage, read_write> flags: array<u32>;
@group(0) @binding(3) var<storage, read> X: array<u32>;
";

const FILL: &str = "struct Params { k: vec4u };
@group(0) @binding(2) var<storage, read_write> it: array<u32>;
fn point(i: u32) { it[i] = params.k.x; }
";

/// One of Begin's own attribute buffers.
#[derive(Debug)]
struct Own {
    name: Arc<str>,
    ty: AttrType,
    len: u32,
    buffer: wgpu::Buffer,
}

pub struct Begin {
    /// Copies of the input's point attributes, then `iteration`.
    own: Vec<Own>,
    fill: PointKernel,
    flags: PointKernel,
    /// Pieces: the compaction over the input's capacity.
    sized: Option<(u32, Compactor, wgpu::Buffer)>,
    gather: Gather,
    /// The compaction's header (bound by its kernels) and the output's.
    work: wgpu::Buffer,
    header: wgpu::Buffer,
    name: Arc<str>,
}

pub fn create_begin(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Begin {
        own: Vec::new(),
        fill: PointKernel::new(device, "pnt.foreach.iteration", FILL, &[ReadWrite], 16),
        flags: PointKernel::with_main(
            device,
            "pnt.foreach.flags",
            &format!("{FLAGS}{}", typed_access("X", "params.d", true)),
            &[ReadWrite, Read],
            32,
            "if (i < params.cap) {
        var keep = 0u;
        if (i < hdr.count && ld_X(i).x == f32(params.k)) { keep = 1u; }
        flags[i] = keep;
    }",
        ),
        sized: None,
        gather: Gather::new(device),
        work: kit::work_header(device),
        header: kit::work_header(device),
        name: Arc::from(ITERATION),
    })
}

impl Begin {
    /// Makes the own buffers match `g`'s point attributes at `len` elements
    /// (re-made only when they change: an edit, not a frame).
    fn sync(&mut self, device: &wgpu::Device, g: &Geometry, len: u32) {
        let attrs = g.attrs(AttrClass::Point);
        let wanted = attrs.iter().filter(|a| a.name() != ITERATION);
        let same = self.own.len() == wanted.clone().count() + 1
            && wanted
                .clone()
                .zip(&self.own)
                .all(|(a, o)| a.name() == &*o.name && a.ty() == o.ty && o.len == len)
            && self
                .own
                .last()
                .is_some_and(|o| &*o.name == ITERATION && o.len == len);
        if same {
            return;
        }
        let make = |name: Arc<str>, ty: AttrType| Own {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pnt.foreach"),
                size: buffer_bytes(len, ty),
                usage: ATTR_USAGE,
                mapped_at_creation: false,
            }),
            name,
            ty,
            len,
        };
        self.own = wanted
            .map(|a| make(a.name_arc().clone(), a.ty()))
            .chain(std::iter::once(make(self.name.clone(), AttrType::U32)))
            .collect();
    }

    fn compaction(&mut self, device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
        if self.sized.as_ref().is_none_or(|s| s.0 != capacity) {
            let flags = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pnt.foreach.flags"),
                size: lsr_core::geo::align_up(u64::from(capacity) * 4),
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.sized = Some((capacity, Compactor::new(device, capacity), flags));
        }
        self.sized.as_ref().expect("sized above").2.clone()
    }
}

impl GeoOp for Begin {
    fn repeat(&self, params: &Params<'_>) -> Repeat {
        Repeat::Iterations(params.i64("iterations").clamp(1, i64::from(MAX_ITERATIONS)) as u32)
    }

    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(g) = ctx.input(0).cloned() else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        let n = g.attrs(AttrClass::Point).len();
        if n >= MAX_ATTRS {
            return Err(format!(
                "{n} point attributes; For-Each carries at most {}",
                MAX_ATTRS - 1
            ));
        }
        let p = ctx.params;
        let pieces = menu_index(MODES, p.str("mode")) == 1;
        let cap = g.points();
        let k = ctx.iteration;
        let device = ctx.device;
        self.sync(device, &g, cap);
        let mut out = g.clone();
        let hdr = if pieces {
            let name = p.str("attribute");
            let (x, ty) = kit::readable_any(ctx, &g, name)?;
            if ty.comps() != 1 {
                return Err(format!(
                    "the piece attribute `{name}` must have one component"
                ));
            }
            let src_hdr = kit::header_of(&g)?;
            let flags = self.compaction(device, cap);
            let mut u = lsr_gpu::geo::kernels::Packer::<32>::new();
            for w in desc(ty) {
                u.u32(w);
            }
            u.u32(k).u32(cap);
            self.flags.write(ctx.queue, u.bytes());
            let (_, compactor, _) = self.sized.as_ref().expect("sized above");
            let mut ps = pass(ctx.encoder);
            self.flags.dispatch_in(
                device,
                &mut ps,
                &src_hdr,
                &[&flags, &x],
                Work::Elements(cap),
            );
            compactor.record(device, ctx.queue, &mut ps, &flags, 0, &self.work);
            let map = GatherMap {
                header: &self.work,
                map: compactor.map(),
            };
            for own in &self.own[..self.own.len() - 1] {
                let src = g
                    .point_typed(&own.name, own.ty)
                    .expect("synced to the input")
                    .ensure_gpu(ctx.queue, ctx.buffers);
                self.gather
                    .record(device, &mut ps, map, own.ty.stride(), src, &own.buffer);
            }
            drop(ps);
            ctx.encoder
                .copy_buffer_to_buffer(&self.work, 0, &self.header, 0, header::BYTES);
            out.set_gpu_counted(cap, Arc::new(GeoBuffer::unpooled(self.header.clone())));
            drop_prims(&mut out);
            self.header.clone()
        } else {
            for own in &self.own[..self.own.len() - 1] {
                let a = g
                    .point_typed(&own.name, own.ty)
                    .expect("synced to the input");
                let bytes = a.bytes();
                let src = a.ensure_gpu(ctx.queue, ctx.buffers);
                ctx.encoder
                    .copy_buffer_to_buffer(src, 0, &own.buffer, 0, bytes);
            }
            kit::header_of(&g)?
        };
        for own in &self.own {
            out.set_point(Attribute::gpu(
                own.name.clone(),
                own.ty,
                own.len,
                GeoBuffer::unpooled(own.buffer.clone()),
            ));
        }
        let it = &self.own.last().expect("synced").buffer;
        self.fill.run(
            ctx,
            &hdr,
            &[it],
            &lsr_gpu::geo::kernels::words([k, 0, 0, 0]),
        );
        out.set_detail(self.name.clone(), DetailValue::U32(u64::from(k)));
        *ctx.output = out;
        Ok(())
    }
}

// --------------------------------------------------------------------- End

pub fn end_schema() -> OperatorSchema {
    crate::pnt(
        "foreach_end",
        "For-Each End",
        "For-Each End: merges the results of a For-Each block's passes into one geometry, pass 0's points first. Attributes are those of the first pass with points (zero where a later pass lacks one); primitives and groups are not carried. Wire its output back into the For-Each's `end` input to close the block; outside a block it passes its input through.",
        &[("in", false)],
        vec![],
        150,
    )
}

const END_FLAGS: &str = "struct Params { n: u32, total: u32, pad0: u32, pad1: u32 };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> starts: array<u32>;
@group(0) @binding(2) var<storage, read> counts: array<u32>;
@group(0) @binding(3) var<storage, read_write> flags: array<u32>;
@compute @workgroup_size(WG)
fn main(@builtin(global_invocation_id) gid: vec3u) {
    let i = gid.x;
    if (i >= params.total) { return; }
    var k = 0u;
    for (var j = 1u; j < params.n; j = j + 1u) {
        if (i >= starts[j]) { k = j; }
    }
    var keep = 0u;
    if (i - starts[k] < counts[k]) { keep = 1u; }
    flags[i] = keep;
}
";

pub struct End {
    /// This frame's merged attributes (pooled), `room` elements each.
    acc: Vec<(Arc<str>, AttrType, GeoBuffer)>,
    room: u32,
    /// Elements used: the passes' capacities so far.
    total: u32,
    gpu_counted: bool,
    starts: [u32; MAX_REPEAT as usize],
    starts_buf: wgpu::Buffer,
    /// Each pass's live count, copied from its header.
    counts: wgpu::Buffer,
    flags: Kernel,
    flags_params: Uniforms,
    compact: Compact,
    cpu_header: Option<(u32, Arc<GeoBuffer>)>,
}

pub fn create_end(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    let table = |label: &str| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: u64::from(MAX_REPEAT) * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    Box::new(End {
        acc: Vec::new(),
        room: 0,
        total: 0,
        gpu_counted: false,
        starts: [0; MAX_REPEAT as usize],
        starts_buf: table("pnt.foreach_end.starts"),
        counts: table("pnt.foreach_end.counts"),
        flags: Kernel::new(
            device,
            "pnt.foreach_end.flags",
            &END_FLAGS.replace("WG", &WORKGROUP.to_string()),
            "main",
            &[Uniform, Read, Read, ReadWrite],
        ),
        flags_params: Uniforms::new(device, "pnt.foreach_end.flags", 16),
        compact: Compact::new(device),
        cpu_header: None,
    })
}

impl End {
    /// A pooled buffer for `len` elements of `ty` (cleared offline, so no
    /// frame depends on the pool's history).
    fn buffer(ctx: &mut GeoCtx<'_>, ty: AttrType, len: u32) -> GeoBuffer {
        let b = ctx.buffers.attr(buffer_bytes(len, ty));
        if ctx.params.mode().is_offline() {
            ctx.encoder.clear_buffer(&b, 0, None);
        }
        b
    }

    /// Appends pass `k`'s result `g` after the passes before it.
    fn append(&mut self, ctx: &mut GeoCtx<'_>, g: &Geometry, k: u32) -> Result<(), String> {
        let cap = g.points();
        self.starts[k as usize] = self.total;
        if cap == 0 {
            ctx.encoder
                .clear_buffer(&self.counts, u64::from(k) * 4, Some(4));
            return Ok(());
        }
        if self.acc.is_empty() {
            // The first pass with points sets the attributes; room for the
            // remaining passes at its size.
            let n = g.attrs(AttrClass::Point).len();
            if n > MAX_ATTRS {
                return Err(format!(
                    "{n} point attributes; For-Each End merges at most {MAX_ATTRS}"
                ));
            }
            let room = u64::from(cap) * u64::from(ctx.iterations - k);
            self.room = room_for(ctx, room, self.total)?;
            for i in 0..n {
                let a = &g.attrs(AttrClass::Point)[i];
                let (name, ty) = (a.name_arc().clone(), a.ty());
                let b = Self::buffer(ctx, ty, self.room);
                self.acc.push((name, ty, b));
            }
        }
        let need = u64::from(self.total) + u64::from(cap);
        if need > u64::from(self.room) {
            let room = room_for(ctx, need.max(u64::from(self.room) * 2), 0)?;
            for slot in &mut self.acc {
                let grown = Self::buffer(ctx, slot.1, room);
                let used = u64::from(self.total) * u64::from(slot.1.stride());
                if used > 0 {
                    ctx.encoder
                        .copy_buffer_to_buffer(&slot.2, 0, &grown, 0, used);
                }
                slot.2 = grown;
            }
            self.room = room;
        }
        for (name, ty, dst) in &self.acc {
            let stride = u64::from(ty.stride());
            let at = u64::from(self.total) * stride;
            let bytes = u64::from(cap) * stride;
            match g.point_typed(name, *ty) {
                Some(a) => {
                    let src = a.ensure_gpu(ctx.queue, ctx.buffers);
                    ctx.encoder.copy_buffer_to_buffer(src, 0, dst, at, bytes);
                }
                None => ctx.encoder.clear_buffer(dst, at, Some(bytes)),
            }
        }
        let hdr = kit::header_of(g)?;
        ctx.encoder
            .copy_buffer_to_buffer(&hdr, 0, &self.counts, u64::from(k) * 4, 4);
        self.total += cap;
        self.gpu_counted |= g.is_gpu_counted();
        Ok(())
    }

    /// After the last pass: the merged geometry.
    fn finish(&mut self, ctx: &mut GeoCtx<'_>) -> Result<Geometry, String> {
        let mut out = Geometry::new();
        if self.acc.is_empty() {
            return Ok(out);
        }
        let total = self.total;
        out.set_points(total, None);
        for (name, ty, b) in self.acc.drain(..) {
            out.set_point(Attribute::gpu(name, ty, total, b));
        }
        if !self.gpu_counted {
            let header = match &self.cpu_header {
                Some((n, h)) if *n == total => h.clone(),
                _ => {
                    let h = ctx.cpu_header(total);
                    self.cpu_header = Some((total, h.clone()));
                    h
                }
            };
            out.set_points(total, Some(header));
            return Ok(out);
        }
        // Keep each pass's live points: element i of pass k is kept when
        // i − start_k < count_k.
        let n = ctx.iterations;
        let mut table = [0u8; MAX_REPEAT as usize * 4];
        for (chunk, s) in table.chunks_exact_mut(4).zip(&self.starts[..n as usize]) {
            chunk.copy_from_slice(&s.to_le_bytes());
        }
        ctx.queue
            .write_buffer(&self.starts_buf, 0, &table[..n as usize * 4]);
        self.flags_params
            .write(ctx.queue, &lsr_gpu::geo::kernels::words([n, total, 0, 0]));
        let flags = self.compact.flags(ctx.device, total);
        self.flags.dispatch(
            ctx.device,
            ctx.encoder,
            &[
                self.flags_params.buffer(),
                &self.starts_buf,
                &self.counts,
                &flags,
            ],
            Work::Elements(total),
        );
        self.compact.apply(ctx, &mut out)?;
        Ok(out)
    }
}

/// `room` elements as a buffer length, after `used`, within the limits.
fn room_for(ctx: &GeoCtx<'_>, room: u64, used: u32) -> Result<u32, String> {
    let max =
        (ctx.device.limits().max_storage_buffer_binding_size / 16).min(crate::sources::MAX_POINTS);
    let room = room.max(u64::from(used)).max(1);
    if room > max {
        return Err(format!(
            "the passes give {room} points, more than this GPU can hold here ({max})"
        ));
    }
    Ok(room as u32)
}

impl GeoOp for End {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let k = ctx.iteration;
        if k == 0 {
            self.acc.clear();
            self.room = 0;
            self.total = 0;
            self.gpu_counted = false;
        }
        let g = ctx.input(0).cloned().unwrap_or_default();
        self.append(ctx, &g, k)?;
        if k + 1 == ctx.iterations {
            *ctx.output = self.finish(ctx)?;
        }
        Ok(())
    }
}
