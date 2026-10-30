//! AI operator family (PRD §6, §7.5): nodes that run models on AI Hosts
//! asynchronously and reach the frame loop only through the Output
//! Governor, plus Control Prep, which prepares control images on the GPU,
//! and the text nodes Speech to Text and LLM Chat (FR-A13).

pub mod chat;
pub mod clip;
pub mod comfy;
pub mod control;
pub mod node;
pub mod schemas;
pub mod stt;
pub mod text;

use atd_ai::supervisor::Supervisor;
use atd_core::registry::Registry;
use atd_gpu::engine::OpLibrary;

/// Registers this family's operator schemas.
pub fn register(registry: &mut Registry) {
    for (type_id, ..) in schemas::HOSTED {
        registry
            .register(schemas::schema(type_id))
            .expect("AI operator schemas are valid and unique");
    }
    for schema in [
        control::schema(),
        clip::schema(),
        stt::schema(),
        chat::schema(),
    ] {
        registry
            .register(schema)
            .expect("AI operator schemas are valid and unique");
    }
}

/// Registers this family's GPU implementations, running on the
/// process-wide supervisor's hosts.
pub fn library(library: &mut OpLibrary) {
    library_with(library, Supervisor::global());
}

/// Like [`library`], with hosts from `supervisor` (tests).
pub fn library_with(library: &mut OpLibrary, supervisor: &'static Supervisor) {
    for (type_id, _, kind) in schemas::HOSTED {
        let kind = *kind;
        library.add(*type_id, move |device, format| {
            node::create(device, format, kind, supervisor)
        });
    }
    library.add(control::schema().type_id, control::create);
    library.add(clip::schema().type_id, clip::create);
    library.add_sig(stt::schema().type_id, move || stt::create(supervisor));
    library.add_sig(chat::schema().type_id, chat::create);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_register_with_budgets_and_help() {
        let mut registry = Registry::default();
        register(&mut registry);
        assert_eq!(registry.iter().count(), schemas::HOSTED.len() + 4);
        for op in registry.iter() {
            assert!(op.type_id.starts_with("ai."), "{}", op.type_id);
            assert!(op.cook_budget_us > 0, "{}", op.type_id);
            assert!(!op.summary.is_empty(), "{}", op.type_id);
            for p in &op.params {
                assert!(!p.help.is_empty(), "{}.{} has no help", op.type_id, p.name);
            }
        }
        let diffusion = registry.get("ai.diffusion").unwrap();
        assert_eq!(diffusion.capabilities[0].flag.to_string(), "win+nvidia");
    }
}
