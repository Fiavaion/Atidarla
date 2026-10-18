//! PNT operator family: GPU-resident point operators on the shared GEO wire
//! (PRD §6), built on the attribute-buffer framework in `atd_gpu::geo`
//! (design: `docs/design/geometry-attributes.md`).
//!
//! Every operator ships complete (CLAUDE.md rule 7): schema with a cook-time
//! budget, a generated docs page (`atidarla docs`), a runnable example
//! (`examples/points.atidarla`, `examples/particles.atidarla`), GPU unit tests
//! (`tests/`) and an offline-render class (`atd-cli` `classify`).
//!
//! | Operator | Reads | Writes |
//! |---|---|---|
//! | `pnt.points` | — | `P`, `Cd`, `id` |
//! | `pnt.noise` | `P` (and `v` in velocity mode) | `P` or `v` |
//! | `pnt.particles` | its previous state (`prev` feedback or itself) | `P`, `v`, `age`, `life`, `id`, `Cd` (GPU count) |
//! | `img.render_points` (IMG) | `P`, `Cd` | an image |

mod noise;
mod particles;
mod points;
mod render;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use atd_gpu::engine::OpLibrary;

pub use particles::MAX_CAPACITY;

/// A PNT operator schema: GEO inputs (`(name, feedback)`), one GEO output.
pub(crate) fn pnt(
    id: &str,
    label: &str,
    summary: &str,
    inputs: &[(&str, bool)],
    params: Vec<ParamSchema>,
    cook_budget_us: u32,
) -> OperatorSchema {
    let socket = |name: &str, feedback: bool| SocketSchema {
        name: name.into(),
        wire: WireType::Geo,
        optional: true,
        feedback,
    };
    OperatorSchema {
        type_id: format!("pnt.{id}"),
        family: OpFamily::Pnt,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs: inputs.iter().map(|(n, fb)| socket(n, *fb)).collect(),
        outputs: vec![socket("out", false)],
        capabilities: vec![],
        cook_budget_us,
        time_dependent: false,
        sink: false,
    }
}

/// Every schema this crate registers (PNT operators and Render Points).
pub fn schemas() -> Vec<OperatorSchema> {
    vec![
        points::schema(),
        noise::schema(),
        particles::schema(),
        render::schema(),
    ]
}

/// Registers this family's operators with the registry (and `img.render_points`,
/// the IMG operator that draws geometry).
pub fn register(registry: &mut Registry) {
    for schema in schemas() {
        registry
            .register(schema)
            .expect("PNT operator schemas are valid and unique");
    }
}

/// Registers this family's GPU implementations.
pub fn library(library: &mut OpLibrary) {
    library.add_geo(points::schema().type_id, points::create);
    library.add_geo(noise::schema().type_id, noise::create);
    library.add_geo(particles::schema().type_id, particles::create);
    library.add(render::schema().type_id, render::create);
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
        assert_eq!(registry.len(), schemas().len());
        for op in registry.iter() {
            assert!(op.cook_budget_us > 0, "{} has a cook budget", op.type_id);
            assert!(!op.summary.is_empty(), "{} has a summary", op.type_id);
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
