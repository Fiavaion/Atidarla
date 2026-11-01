//! Deformer fusion (design: `docs/design/geometry-attributes.md` §3.2): the
//! engine hands a straight run of per-point stages (the field operators and
//! Transform, [`GeoOp::point_stage`](lsr_gpu::engine::GeoOp::point_stage))
//! to a [`Fuser`], which runs them as one kernel: each point is loaded,
//! passed through every stage's function in order, and stored once.
//!
//! Each stage is a function of the point's local `P`, `v` and `N` and its
//! uniforms ([`stage_function`]), and the unfused kernels call the same
//! function between a load and a store, so a fused run computes what the
//! chain computes (the equivalence tests compare the buffers byte for
//! byte). The uniforms are the unfused ones, one 128-byte block per
//! stage. Pipelines are built on a worker thread when a run's stage kinds
//! change (live: the run cooks unfused until it is ready; offline: the cook
//! waits for it).

use std::sync::mpsc;

use lsr_core::geo::{AttrType, names};
use lsr_gpu::engine::{GeoCtx, MAX_FUSED, PointFuser, PointStage, STAGE_BYTES};
use lsr_gpu::geo::kernels::{Bind, Kernel, Uniforms, Work};

use crate::kit::{self, Missing};
use crate::{field, transform};

/// `lsr_opaque(x, z)`: `x` passed through an exclusive-or with `z`, a
/// uniform that is always 0. Every stage's inputs pass through it, in the
/// unfused kernels and between fused stages alike: the compiler cannot see
/// through it, so it cannot fold one stage's arithmetic into the next (for
/// example `(a + r) - a` into `r` under fast math) or treat a stage
/// differently by where its inputs came from, and a fused run rounds as the
/// chain does.
pub(crate) const OPAQUE: &str = "fn lsr_opaque(x: vec4f, z: u32) -> vec4f {
    return bitcast<vec4f>(bitcast<vec4u>(x) ^ vec4u(z));
}
";

/// The WGSL function name of a stage kind.
pub(crate) fn function(kind: &str) -> String {
    format!("stage_{kind}")
}

/// A stage's `fn point(i: u32)` body (over `P[i]`, `v[i]`, `N[i]` and
/// `params`) as a function of local values: `fn stage_<kind>(lsr_p, lsr_v,
/// lsr_n, params: <ty>)`, pointers to the point's `P`, `v` and `N`.
pub(crate) fn stage_function(kind: &str, body: &str, ty: &str) -> String {
    let head = "fn point(i: u32) {";
    let rest = body
        .strip_prefix(head)
        .unwrap_or_else(|| panic!("stage `{kind}` does not start with `{head}`"));
    let rest = rest
        .replace("P[i]", "(*lsr_p)")
        .replace("v[i]", "(*lsr_v)")
        .replace("N[i]", "(*lsr_n)");
    assert!(
        !rest.contains("[i]"),
        "stage `{kind}` reads another per-point array"
    );
    format!(
        "fn {}(lsr_p: ptr<function, vec4f>, lsr_v: ptr<function, vec4f>, lsr_n: ptr<function, vec4f>, params: {ty}) {{{rest}\n",
        function(kind)
    )
}

/// The body of stage `kind` and its uniform struct and converter.
fn stage(kind: &str) -> Result<(&'static str, &'static str, &'static str), String> {
    if kind == "transform" {
        return Ok((transform::BODY, "TransformParams", "transform_params"));
    }
    let body = field::body(kind).ok_or_else(|| format!("no stage `{kind}`"))?;
    Ok((body, "FieldParams", "field_params"))
}

/// The fused kernel for `kinds`, in order.
fn source(kinds: &[&'static str]) -> Result<String, String> {
    let mut src = kit::prelude();
    src.push_str(&format!(
        "struct Block {{ w: array<vec4u, {words}> }};
@group(0) @binding(0) var<uniform> blocks: array<Block, {MAX_FUSED}>;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> v: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> N: array<vec4f>;
{field}
fn field_params(b: Block) -> FieldParams {{
    return FieldParams(
        bitcast<vec4f>(b.w[0]),
        bitcast<vec4f>(b.w[1]),
        bitcast<vec4f>(b.w[2]),
        bitcast<vec4f>(b.w[3]),
        bitcast<f32>(b.w[4].x),
        b.w[4].y,
        b.w[4].z,
        b.w[4].w,
        b.w[5],
    );
}}
{transform}
fn transform_params(b: Block) -> TransformParams {{
    return TransformParams(
        bitcast<vec4f>(b.w[0]),
        bitcast<vec4f>(b.w[1]),
        bitcast<vec4f>(b.w[2]),
        bitcast<vec4f>(b.w[3]),
        bitcast<vec4f>(b.w[4]),
        bitcast<vec4f>(b.w[5]),
        b.w[6],
    );
}}
{OPAQUE}",
        words = STAGE_BYTES / 16,
        field = field::DECL.replace("struct Params", "struct FieldParams"),
        transform = transform::DECL.replace("struct Params", "struct TransformParams"),
    ));
    let mut calls = String::new();
    let mut defined: Vec<&str> = Vec::new();
    for (k, kind) in kinds.iter().enumerate() {
        let (body, ty, conv) = stage(kind)?;
        if !defined.contains(kind) {
            defined.push(kind);
            src.push_str(&stage_function(kind, body, ty));
        }
        if k > 0 {
            calls.push_str(
                "        p = lsr_opaque(p, z);
        w = lsr_opaque(w, z);
        n = lsr_opaque(n, z);
",
            );
        }
        calls.push_str(&format!(
            "        {}(&p, &w, &n, {conv}(blocks[{k}]));\n",
            function(kind)
        ));
    }
    src.push_str(&format!(
        "@compute @workgroup_size({})
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let i = gid.x;
    // The last two words of block 0: which of `v` (bit 0) and `N` (bit 1)
    // are bound, and the zero for `lsr_opaque` (no stage uses them).
    let real = blocks[0].w[{last}].z;
    let z = blocks[0].w[{last}].w;
    if (i < hdr.count) {{
        var p = lsr_opaque(P[i], z);
        var w = vec4f(0.0);
        var n = vec4f(0.0);
        if ((real & 1u) != 0u) {{ w = lsr_opaque(v[i], z); }}
        if ((real & 2u) != 0u) {{ n = lsr_opaque(N[i], z); }}
{calls}        P[i] = p;
        if ((real & 1u) != 0u) {{ v[i] = w; }}
        if ((real & 2u) != 0u) {{ N[i] = n; }}
    }}
}}
",
        lsr_core::geo::WORKGROUP,
        last = STAGE_BYTES / 16 - 1,
    ));
    Ok(src)
}

fn build(device: &wgpu::Device, kinds: &[&'static str]) -> Result<Kernel, String> {
    use Bind::*;
    let src = source(kinds)?;
    Ok(Kernel::new(
        device,
        "pnt.fused",
        &src,
        "main",
        &[Uniform, Read, ReadWrite, ReadWrite, ReadWrite],
    ))
}

/// A pipeline being built on a worker thread.
struct Building {
    kinds: Vec<&'static str>,
    rx: mpsc::Receiver<Result<Kernel, String>>,
}

pub struct Fuser {
    device: wgpu::Device,
    /// The stage kinds `kernel` runs.
    kinds: Vec<&'static str>,
    kernel: Option<Kernel>,
    building: Option<Building>,
    blocks: Uniforms,
    data: Box<[u8]>,
    /// Bound for a `v` or `N` no stage touches.
    unused: [wgpu::Buffer; 2],
}

pub fn create(device: &wgpu::Device) -> Box<dyn PointFuser> {
    let unused = || {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.fused.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        })
    };
    Box::new(Fuser {
        device: device.clone(),
        kinds: Vec::new(),
        kernel: None,
        building: None,
        blocks: Uniforms::new(device, "pnt.fused", (MAX_FUSED * STAGE_BYTES) as u64),
        data: vec![0u8; MAX_FUSED * STAGE_BYTES].into_boxed_slice(),
        unused: [unused(), unused()],
    })
}

fn same(a: &[&'static str], stages: &[PointStage]) -> bool {
    a.len() == stages.len() && a.iter().zip(stages).all(|(k, s)| *k == s.kind)
}

impl Fuser {
    /// Whether the kernel for `stages` is ready; starts building it if not
    /// (offline: builds it now).
    fn ready(&mut self, stages: &[PointStage], offline: bool) -> bool {
        if self.kernel.is_some() && same(&self.kinds, stages) {
            return true;
        }
        if self
            .building
            .as_ref()
            .is_none_or(|b| !same(&b.kinds, stages))
        {
            let kinds: Vec<&'static str> = stages.iter().map(|s| s.kind).collect();
            let (tx, rx) = mpsc::channel();
            if offline {
                let _ = tx.send(build(&self.device, &kinds));
            } else {
                let device = self.device.clone();
                let job = kinds.clone();
                std::thread::Builder::new()
                    .name("lsr-fuse".into())
                    .spawn(move || {
                        let _ = tx.send(build(&device, &job));
                    })
                    .expect("spawning the fusion compile thread");
            }
            self.building = Some(Building { kinds, rx });
        }
        let building = self.building.as_ref().expect("set above");
        let got = if offline {
            Some(building.rx.recv().unwrap_or_else(|e| Err(e.to_string())))
        } else {
            match building.rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(e) => Some(Err(e.to_string())),
            }
        };
        match got {
            Some(Ok(kernel)) => {
                let b = self.building.take().expect("set above");
                self.kinds = b.kinds;
                self.kernel = Some(kernel);
                true
            }
            Some(Err(_)) => {
                // Not fusable after all: stay unfused for this run.
                self.building = None;
                false
            }
            None => false,
        }
    }
}

impl PointFuser for Fuser {
    fn busy(&self) -> bool {
        self.building.is_some()
    }

    fn cook(&mut self, ctx: &mut GeoCtx<'_>, stages: &[PointStage]) -> bool {
        if stages.len() > MAX_FUSED || !self.ready(stages, ctx.params.mode().is_offline()) {
            return false;
        }
        // Whatever the run needs is checked before anything is taken.
        let Some(input) = ctx.input(0) else {
            return false;
        };
        if input.points() == 0
            || input.point_typed(names::P, AttrType::VEC3).is_none()
            || input.header().is_none()
        {
            return false;
        }
        let mut has_v = input.point_typed(names::V, AttrType::VEC3).is_some();
        let has_n = input.point_typed(names::N, AttrType::VEC3).is_some();
        let turns = stages.iter().any(|s| s.kind == "transform");
        let writes_v = stages.iter().any(|s| s.velocity);
        // Per stage: its uniforms, and for Transform which of `v` and `N`
        // exist when it runs (a force before it may have made `v`).
        for (k, s) in stages.iter().enumerate() {
            let block = &mut self.data[k * STAGE_BYTES..(k + 1) * STAGE_BYTES];
            block.copy_from_slice(&s.uniforms);
            if s.kind == "transform" {
                let has = u32::from(has_v) | (u32::from(has_n) << 1);
                block[transform::HAS_OFFSET..transform::HAS_OFFSET + 4]
                    .copy_from_slice(&has.to_le_bytes());
            }
            has_v |= s.velocity;
        }
        let Some(mut g) = ctx.take_input(0) else {
            return false;
        };
        let hdr = kit::header_of(&g).expect("checked above");
        let had_v = g.point_typed(names::V, AttrType::VEC3).is_some();
        let (Ok(pos), Ok(vel), Ok(nrm)) = (
            kit::writable(ctx, &mut g, names::P, AttrType::VEC3, Missing::Error),
            if writes_v || (turns && had_v) {
                kit::writable(ctx, &mut g, names::V, AttrType::VEC3, Missing::Zero)
            } else {
                Ok(self.unused[0].clone())
            },
            if turns && has_n {
                kit::writable(ctx, &mut g, names::N, AttrType::VEC3, Missing::Error)
            } else {
                Ok(self.unused[1].clone())
            },
        ) else {
            unreachable!("P, v and N were checked above")
        };
        // Block 0's last two words: which of `v` and `N` are bound, and the
        // zero `settle` hides from the compiler (no stage uses them).
        let real = u32::from(writes_v || (turns && had_v)) | (u32::from(turns && has_n) << 1);
        let tail = STAGE_BYTES - 8;
        self.data[tail..tail + 4].copy_from_slice(&real.to_le_bytes());
        self.data[tail + 4..STAGE_BYTES].copy_from_slice(&0u32.to_le_bytes());
        self.blocks
            .write(ctx.queue, &self.data[..stages.len() * STAGE_BYTES]);
        let kernel = self.kernel.as_ref().expect("ready");
        kernel.dispatch(
            ctx.device,
            ctx.encoder,
            &[self.blocks.buffer(), &hdr, &pos, &vel, &nrm],
            Work::Header(&hdr),
        );
        *ctx.output = g;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fused_kernels_are_valid_wgsl() {
        let mut kinds: Vec<&'static str> = field::ids().collect();
        kinds.push("transform");
        kinds.extend(field::ids());
        let src = source(&kinds).unwrap();
        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .unwrap();
        assert!(source(&["nope"]).is_err());
    }
}
