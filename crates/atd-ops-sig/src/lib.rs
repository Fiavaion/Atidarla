//! SIG operator family: channel operators (TouchDesigner CHOP analogue),
//! PRD §6.
//!
//! SIG operators run on the CPU at control rate (one sample per channel per
//! engine frame; audio-rate channels arrive with Audio In). They write into
//! the node's reused channel buffer, so steady-state cooks do not allocate.
//! Any numeric parameter of any operator can follow a SIG channel (channel
//! export, FR-P2).
//!
//! Every operator here has a schema (with cook-time budget and help text), a
//! generated docs page (`atidarla docs`) and CPU unit tests. Runnable example
//! projects follow once the project format (PRD A11) is ratified.

mod audio;
mod ops;
mod sig2;

use atd_core::registry::Registry;
use atd_gpu::engine::OpLibrary;

pub use ops::DEFS;
pub use sig2::DEFS2;

/// Every SIG operator definition.
pub fn defs() -> impl Iterator<Item = &'static Def> {
    DEFS.iter().chain(DEFS2)
}

/// A SIG operator definition: schema plus implementation factory.
#[derive(Debug)]
pub struct Def {
    pub schema: fn() -> atd_core::registry::OperatorSchema,
    pub create: fn() -> Box<dyn atd_gpu::engine::SigOp>,
}

/// Registers this family's operator schemas.
pub fn register(registry: &mut Registry) {
    for def in defs() {
        registry
            .register((def.schema)())
            .expect("SIG operator schemas are valid and unique");
    }
}

/// Registers this family's implementations.
pub fn library(library: &mut OpLibrary) {
    for def in defs() {
        let create = def.create;
        library.add_sig((def.schema)().type_id, move || create());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_register_with_budgets_and_help() {
        let mut registry = Registry::default();
        register(&mut registry);
        assert_eq!(registry.len(), DEFS.len() + DEFS2.len());
        for op in registry.iter() {
            assert!(op.cook_budget_us > 0, "{} has a cook budget", op.type_id);
            assert!(!op.summary.is_empty());
            assert_eq!(op.outputs.len(), 1);
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
        }
    }

    #[test]
    fn every_schema_has_an_implementation() {
        let mut lib = OpLibrary::default();
        library(&mut lib);
        for def in defs() {
            assert!(lib.contains(&(def.schema)().type_id));
        }
    }
}
