//! IMG operator family: GPU image operators (TouchDesigner TOP analogue),
//! PRD §6.
//!
//! Most IMG operators are full-screen fragment passes, defined declaratively
//! in [`ops`] as a schema, a WGSL body and a parameter packer, and run by
//! [`ShaderOp`]. Every operator here has its schema (with cook-time budget),
//! a generated docs page (`atidarla docs`) and a GPU unit test
//! (`tests/ops.rs`). Runnable example projects follow once the project
//! format (PRD A11) is ratified.

mod ops;
mod ops2;
mod stateful;

use atd_core::registry::Registry;
use atd_gpu::engine::{CookCtx, GpuOp, OpLibrary};
use atd_gpu::fullscreen::{FullscreenPass, MAX_PARAM_VEC4};

pub use ops::DEFS;
pub use ops2::DEFS2;
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
}

impl GpuOp for ShaderOp {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let packed = (self.pack)(ctx);
        let inputs: Vec<wgpu::Texture> = (0..ctx.inputs.len())
            .map(|i| ctx.input_or_black(i).clone())
            .collect();
        let refs: Vec<&wgpu::Texture> = inputs.iter().collect();
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &refs,
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
        .chain(STATEFUL.iter().map(|s| (s.0)()));
    for schema in schemas {
        registry
            .register(schema)
            .expect("IMG operator schemas are valid and unique");
    }
}

/// Registers this family's GPU implementations.
pub fn library(library: &mut OpLibrary) {
    for (schema, factory) in STATEFUL {
        library.add(schema().type_id, factory);
    }
    for def in defs() {
        let schema = (def.schema)();
        let inputs = schema.inputs.len();
        let label = schema.type_id.clone();
        let (helpers, body, pack) = (def.helpers, def.body, def.pack);
        library.add(schema.type_id, move |device, format| {
            Box::new(ShaderOp {
                pass: FullscreenPass::new(device, &label, format, inputs, helpers, body),
                pack,
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
        assert_eq!(registry.len(), DEFS.len() + DEFS2.len() + STATEFUL.len());
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
    }
}
