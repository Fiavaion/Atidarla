//! AI operator family: nodes that submit work to AI Hosts asynchronously via the Output Governor, PRD §6 and §7.5.
//!
//! **Status:** M0 stub. Implementation lands in M4 (see `PRD.md` §9).

pub mod comfy;

use atd_core::registry::Registry;

/// Registers this family's operators with the registry.
///
/// No operators are registered yet: every operator must ship complete
/// (schema, docs page, example, unit test, cook-time budget).
pub fn register(_registry: &mut Registry) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_is_callable() {
        let mut registry = Registry::default();
        register(&mut registry);
    }
}
