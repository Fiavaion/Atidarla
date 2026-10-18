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

mod attr;
mod emitter;
mod field;
mod kit;
mod neighbours;
mod noise;
mod particles;
mod points;
mod render;
mod select;
mod sort;
mod sources;
mod transform;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use atd_gpu::engine::{GeoOp, OpLibrary};

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

type Factory = fn(&wgpu::Device) -> Box<dyn GeoOp>;
type Schema = fn() -> OperatorSchema;

/// The GEO operators with their own module: schema and factory.
const OPS: &[(Schema, Factory)] = &[
    (points::schema, points::create),
    (noise::schema, noise::create),
    (particles::schema, particles::create),
    (transform::schema, transform::create),
    (select::delete_schema, select::create_delete),
    (select::subset_schema, select::create_subset),
    (select::group_schema, select::create_group),
    (select::age_schema, select::create_age),
    (attr::attribute_schema, attr::create_attribute),
    (attr::math_schema, attr::create_math),
    (attr::randomize_schema, attr::create_randomize),
    (attr::colour_schema, attr::create_colour),
    (sources::scatter_schema, sources::create_scatter),
    (sources::merge_schema, sources::create_merge),
    (sources::copy_schema, sources::create_copy),
    (emitter::schema, emitter::create),
    (sort::schema, sort::create),
    (neighbours::normals_schema, neighbours::create_normals),
    (neighbours::smooth_schema, neighbours::create_smooth),
    (neighbours::connect_schema, neighbours::create_connect),
];

/// Every schema this crate registers (PNT operators and Render Points).
pub fn schemas() -> Vec<OperatorSchema> {
    let mut all: Vec<OperatorSchema> = OPS.iter().map(|(schema, _)| schema()).collect();
    all.push(render::schema());
    all.extend(field::schemas());
    all
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
    for (schema, create) in OPS {
        library.add_geo(schema().type_id, *create);
    }
    library.add(render::schema().type_id, render::create);
    for id in field::ids() {
        library.add_geo(format!("pnt.{id}"), move |d| field::create(d, id));
    }
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
