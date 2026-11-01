//! Simulation loops and pieces (`docs/design/solver-foreach.md`):
//! `pnt.solver`, the entry of a loop whose state lives on a feedback wire,
//! and `pnt.piece`, For-Each Begin: a `piece` number per point that
//! operators keyed by an attribute act on piece by piece.

use std::sync::Arc;

use lsr_core::geo::{AttrType, names};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp, Params, Repeat};
use lsr_gpu::geo::kernels::{Bind, Packer};

use crate::kit::{self, PointKernel, desc, menu_index, typed_access};

// ------------------------------------------------------------------ Solver

pub fn solver_schema() -> OperatorSchema {
    let mut s = crate::pnt(
        "solver",
        "Solver",
        "The entry of a simulation loop: outputs `init` at the start (first frame, Reset, until Start Frame, or while `prev` has no geometry), then the previous frame's state from `prev`. Wire the end of the body (for example Gravity → Collide → Integrate → Age) into `prev`; the body may sit in a Pack (a Solver Block). The state is updated in place when nothing else reads it. Sub-steps run the body several times per frame with a smaller step.",
        &[("init", false), ("prev", true)],
        vec![
            ParamSchema::toggle("reset", "Reset", false)
                .help("While on, outputs `init` every frame (the simulation restarts when it is turned off)."),
            ParamSchema::int("start_frame", "Start Frame", 0, Some(0), None)
                .help("Up to this frame the output is `init`; the simulation runs from the next."),
            ParamSchema::int("substeps", "Sub-steps", 1, Some(1), Some(i64::from(MAX_SUBSTEPS)))
                .help("Runs the loop's body this many times per frame, each with the frame's step divided among them (a finer, steadier simulation at the same frame rate; the body costs that many times more)."),
        ],
        20,
    );
    s.time_dependent = true;
    s
}

pub struct Solver {
    started: bool,
}

pub fn create_solver(_device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Solver { started: false })
}

/// Most sub-steps per frame.
pub const MAX_SUBSTEPS: u32 = 64;

impl GeoOp for Solver {
    fn repeat(&self, params: &Params<'_>) -> Repeat {
        Repeat::SubSteps(params.i64("substeps").clamp(1, i64::from(MAX_SUBSTEPS)) as u32)
    }

    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let start = p.i64("start_frame").max(0) as u64;
        let has_prev = ctx.input(1).is_some_and(|g| g.points() > 0);
        // A frame's later sub-steps continue from the one before (`prev` is
        // the body's end as the last sub-step left it).
        let first = ctx.iteration == 0;
        let restart =
            !has_prev || first && (!self.started || p.bool("reset") || ctx.frame <= start);
        self.started = true;
        let out = if !restart {
            ctx.take_input(1)
        } else if first {
            ctx.take_input(0)
        } else {
            // `init` may have been handed over to the first sub-step.
            ctx.input(0).cloned()
        };
        *ctx.output = out.unwrap_or_default();
        Ok(())
    }
}

// ------------------------------------------------------------------- Piece

const RULES: &[&str] = &["chunks", "attribute", "grid"];

pub fn piece_schema() -> OperatorSchema {
    crate::pnt(
        "piece",
        "Piece",
        "For-Each Begin: numbers points by piece into a `piece` attribute, so the operators after it act per piece (Randomize keyed by `piece` gives every piece its own value; Group, Delete, Sort and Colour by Attribute read it too): chunks of Size consecutive points, buckets of an attribute's value (for example `copynum` from Copy to Points), or cells of a spatial grid.",
        &[("in", false)],
        vec![
            ParamSchema::menu("rule", "Rule", RULES, "chunks").help(
                "chunks: points 0 to Size − 1 are piece 0, and so on; attribute: floor((value − Low) / Width); grid: one number per cell of side Cell (not consecutive).",
            ),
            ParamSchema::int("size", "Size", 100, Some(1), None).help("chunks: points per piece."),
            ParamSchema::string("attribute", "Attribute", "copynum").help("attribute: the point attribute read."),
            ParamSchema::int("component", "Component", 0, Some(0), Some(3)).help("attribute: which component."),
            ParamSchema::float("low", "Low", 0.0, None, None).help("attribute: the value where piece 0 starts (below it is piece 0 too)."),
            ParamSchema::float("width", "Width", 1.0, Some(1e-9), None).help("attribute: the range of values per piece."),
            ParamSchema::float("cell", "Cell", 0.5, Some(1e-6), None).help("grid: the side of a cell."),
        ],
        40,
    )
}

const PIECE: &str = "struct Params {
    d: vec4u,
    // low, 1 / width, 1 / cell, unused
    k: vec4f,
    rule: u32,
    size: u32,
    comp: u32,
    pad: u32,
};
@group(0) @binding(2) var<storage, read_write> piece: array<u32>;
@group(0) @binding(3) var<storage, read> X: array<u32>;
@group(0) @binding(4) var<storage, read> P: array<vec4f>;
";

const PIECE_BODY: &str = "fn point(i: u32) {
    var n = i / max(params.size, 1u);
    if (params.rule == 1u) {
        let x = ld_X(i)[min(params.comp, 3u)];
        n = u32(max(floor((x - params.k.x) * params.k.y), 0.0));
    } else if (params.rule == 2u) {
        let c = bitcast<vec3u>(vec3i(floor(P[i].xyz * params.k.z)));
        n = lsr_hash3(c).x;
    }
    piece[i] = n;
}
";

pub struct Piece {
    kernel: PointKernel,
    dummy: wgpu::Buffer,
    name: Arc<str>,
}

pub fn create_piece(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Piece {
        kernel: PointKernel::new(
            device,
            "pnt.piece",
            &format!("{PIECE}{}{PIECE_BODY}", typed_access("X", "params.d", true)),
            &[ReadWrite, Read, Read],
            48,
        ),
        dummy: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.piece.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }),
        name: Arc::from("piece"),
    })
}

impl GeoOp for Piece {
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
        let rule = menu_index(RULES, p.str("rule"));
        let (x, d) = if rule == 1 {
            let name = p.str("attribute");
            if name == "piece" {
                return Err("number pieces from another attribute than `piece`".into());
            }
            let (b, ty) = kit::readable_any(ctx, &g, name)?;
            (b, desc(ty))
        } else {
            (self.dummy.clone(), [1, 1, 0, 0])
        };
        let pos = if rule == 2 {
            kit::readable(ctx, &g, names::P, AttrType::VEC3)?
        } else {
            self.dummy.clone()
        };
        let hdr = kit::header_of(&g)?;
        let out = ctx.alloc(self.name.clone(), AttrType::U32, g.points());
        let pb = out.gpu_buffer().expect("on the GPU").clone();
        g.set_point(out);
        let mut u = Packer::<48>::new();
        for w in d {
            u.u32(w);
        }
        u.vec4([
            p.f32("low"),
            1.0 / p.f32("width").max(1e-9),
            1.0 / p.f32("cell").max(1e-6),
            0.0,
        ])
        .u32(rule)
        .u32(p.i64("size").clamp(1, i64::from(u32::MAX)) as u32)
        .u32(p.i64("component").clamp(0, 3) as u32);
        self.kernel.run(ctx, &hdr, &[&pb, &x, &pos], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}
