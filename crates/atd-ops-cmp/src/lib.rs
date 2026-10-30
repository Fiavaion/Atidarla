//! CMP operator family: containers, Packs, panels, outputs, cameras,
//! lights, PRD §6.
//!
//! **Status:** M3: `cmp.window` ([`window`]), an output with slices, warps,
//! masks, soft edges and test patterns. Pack instances (`cmp.pack_*`) are
//! registered by `atd-core` from the project's Packs.

use atd_core::registry::Registry;
use atd_gpu::engine::OpLibrary;

pub mod window;

/// Registers this family's operator schemas.
pub fn register(registry: &mut Registry) {
    registry
        .register(window::schema())
        .expect("CMP operator schemas are valid and unique");
}

/// Registers this family's GPU implementations.
pub fn library(library: &mut OpLibrary) {
    library.add(window::TYPE_ID, window::create);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_register_with_budgets_help_and_implementations() {
        let mut registry = Registry::default();
        register(&mut registry);
        let mut lib = OpLibrary::default();
        library(&mut lib);
        assert_eq!(registry.len(), 1);
        for op in registry.iter() {
            assert!(op.cook_budget_us > 0, "{} has a cook budget", op.type_id);
            assert!(!op.summary.is_empty());
            assert!(
                lib.contains(&op.type_id),
                "{} has an implementation",
                op.type_id
            );
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
}
