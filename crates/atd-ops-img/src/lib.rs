//! IMG operator family: GPU image operators (TouchDesigner TOP analogue), PRD §6.
//!
//! **Status:** M0 stub. Implementation lands in M1 (see `PRD.md` §9).

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
