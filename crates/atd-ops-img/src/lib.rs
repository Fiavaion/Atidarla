//! IMG operator family: GPU image operators (TouchDesigner TOP analogue),
//! PRD §6.
//!
//! Most IMG operators are full-screen fragment passes, defined declaratively
//! in [`ops`] as a schema, a WGSL body and a parameter packer, and run by
//! [`ShaderOp`]. Every operator here has its schema (with cook-time budget),
//! a generated docs page (`atidarla docs`) and a GPU unit test
//! (`tests/ops.rs`). Runnable example projects follow once the project
//! format (PRD A11) is ratified.

mod analyze;
mod channel_image;
mod fluid;
pub mod isf;
mod movie;
mod ops;
mod ops2;
mod resolution;
mod share;
mod stateful;
mod text;
mod time_machine;
mod wgsl;

use atd_core::registry::Registry;
use atd_gpu::engine::{CookCtx, GpuOp, OpLibrary};
use atd_gpu::fullscreen::{FullscreenPass, MAX_PARAM_VEC4};

pub use ops::DEFS;
pub use ops2::DEFS2;
pub use share::{SENDERS_KEY as SHARE_SENDERS_KEY, senders as share_senders};
pub use stateful::STATEFUL;

/// Every declarative (single-pass) operator definition.
pub fn defs() -> impl Iterator<Item = &'static Def> {
    DEFS.iter().chain(DEFS2)
}

/// Parameter data for one cook: up to [`MAX_PARAM_VEC4`] `vec4f` slots.
#[derive(Debug, Clone, Copy, Default)]
pub struct Packed {
    pub v: [[f32; 4]; MAX_PARAM_VEC4],
    pub n: usize,
}

impl Packed {
    pub fn push(&mut self, v: [f32; 4]) -> &mut Self {
        self.v[self.n] = v;
        self.n += 1;
        self
    }
}

/// Declarative definition of a full-screen IMG operator.
#[derive(Debug)]
pub struct Def {
    pub schema: fn() -> atd_core::registry::OperatorSchema,
    /// WGSL placed before `shade` (helper functions).
    pub helpers: &'static str,
    /// WGSL body of `fn shade(uv: vec2f) -> vec4f`; inputs are `in0`, `in1`…
    /// and parameters `params.v[i]`.
    pub body: &'static str,
    pub pack: fn(&CookCtx<'_>) -> Packed,
}

/// Runs a [`Def`].
#[derive(Debug)]
pub struct ShaderOp {
    pass: FullscreenPass,
    pack: fn(&CookCtx<'_>) -> Packed,
    size: Option<SizeRule>,
}

/// An operator's output size rule (see `GpuOp::output_size`).
type SizeRule =
    fn(&atd_gpu::engine::Params<'_>, Option<(u32, u32)>, (u32, u32)) -> Option<(u32, u32)>;

/// Shader operators whose output size is not simply their input's.
fn size_rule(type_id: &str) -> Option<SizeRule> {
    match type_id {
        "img.crop" => Some(ops2::crop_size),
        _ => None,
    }
}

impl GpuOp for ShaderOp {
    fn output_size(
        &self,
        p: &atd_gpu::engine::Params<'_>,
        input: Option<(u32, u32)>,
        project: (u32, u32),
    ) -> Option<(u32, u32)> {
        self.size.and_then(|f| f(p, input, project))
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        const MAX_INPUTS: usize = 4;
        let packed = (self.pack)(ctx);
        let n = ctx.inputs.len();
        if n > MAX_INPUTS {
            return Err(format!("at most {MAX_INPUTS} inputs per shader operator"));
        }
        // Inputs without a source read the black texture; no allocation.
        let mut refs: [&wgpu::Texture; MAX_INPUTS] = [ctx.black; MAX_INPUTS];
        for (r, input) in refs.iter_mut().zip(ctx.inputs) {
            if let Some(t) = input {
                *r = t;
            }
        }
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &refs[..n],
            &packed.v[..packed.n],
            ctx.output,
        );
        Ok(())
    }
}

/// Registers this family's operator schemas.
pub fn register(registry: &mut Registry) {
    let schemas = defs()
        .map(|d| (d.schema)())
        .chain(STATEFUL.iter().map(|s| (s.0)()))
        .chain([
            channel_image::schema(),
            movie::schema(),
            text::schema(),
            analyze::schema(),
            wgsl::schema(),
            share::out_schema(),
            share::in_schema(),
        ]);
    for schema in schemas {
        registry
            .register(schema)
            .expect("IMG operator schemas are valid and unique");
    }
}

/// Registers this family's GPU implementations.
pub fn library(library: &mut OpLibrary) {
    library.add(channel_image::schema().type_id, channel_image::create);
    library.add(movie::schema().type_id, movie::create);
    library.add(text::schema().type_id, text::create);
    library.add(wgsl::schema().type_id, wgsl::create);
    library.add(share::out_schema().type_id, share::create_out);
    library.add(share::in_schema().type_id, share::create_in);
    library.add_img_to_sig(analyze::schema().type_id, analyze::create);
    for (schema, factory) in STATEFUL {
        library.add(schema().type_id, factory);
    }
    for def in defs() {
        let schema = (def.schema)();
        let inputs = schema.inputs.len();
        let label = schema.type_id.clone();
        let (helpers, body, pack) = (def.helpers, def.body, def.pack);
        let size = size_rule(&schema.type_id);
        library.add(schema.type_id, move |device, format| {
            Box::new(ShaderOp {
                pass: FullscreenPass::new(device, &label, format, inputs, helpers, body),
                pack,
                size,
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_register_with_budgets_and_help() {
        let mut registry = Registry::default();
        register(&mut registry);
        assert_eq!(
            registry.len(),
            DEFS.len() + DEFS2.len() + STATEFUL.len() + 7
        );
        for op in registry.iter() {
            assert!(op.cook_budget_us > 0, "{} has a cook budget", op.type_id);
            assert!(!op.summary.is_empty(), "{} has a summary", op.type_id);
            for p in &op.params {
                assert!(
                    !p.help.is_empty(),
                    "{}.{} has help text",
                    op.type_id,
                    p.name
                );
                p.validate(&p.default)
                    .unwrap_or_else(|e| panic!("{}: {e}", op.type_id));
            }
            assert!(
                op.params.len() <= MAX_PARAM_VEC4 * 4,
                "{} fits the uniform block",
                op.type_id
            );
        }
    }

    #[test]
    fn every_schema_has_an_implementation() {
        let mut lib = OpLibrary::default();
        library(&mut lib);
        for def in defs() {
            assert!(lib.contains(&(def.schema)().type_id));
        }
        for (schema, _) in STATEFUL {
            assert!(lib.contains(&schema().type_id));
        }
        assert!(lib.contains(&channel_image::schema().type_id));
        assert!(lib.contains(&movie::schema().type_id));
        assert!(lib.contains(&text::schema().type_id));
        assert!(lib.contains(&analyze::schema().type_id));
        assert!(lib.contains(&wgsl::schema().type_id));
        assert!(lib.contains(&share::out_schema().type_id));
        assert!(lib.contains(&share::in_schema().type_id));
    }
}
