//! PNT operator family: GPU-resident point operators on the shared GEO wire
//! (PRD §6), built on the attribute-buffer framework in `atd_gpu::geo`
//! (design: `docs/design/geometry-attributes.md`).
//!
//! Every operator ships complete (CLAUDE.md rule 7): schema with a cook-time
//! budget, a generated docs page (`atidarla docs`), a runnable example (a
//! test checks every operator is used in `examples/`), GPU unit tests
//! (`tests/`) and an offline-render class (`atd-cli` `classify`). The M2
//! batch is described in `docs/milestones/m2/pnt-batch.md`.
//!
//! | Operator | Reads | Writes |
//! |---|---|---|
//! | `pnt.points`, `pnt.scatter` | — | `P`, `id` (`Cd`; `N` on surfaces) |
//! | `pnt.noise` | `P` (and `v` in velocity mode) | `P` or `v` |
//! | `pnt.particles` | its previous state (`prev` feedback or itself) | `P`, `v`, `age`, `life`, `id`, `Cd` (GPU count) |
//! | `pnt.emitter` | a state, a source's `P` (`N`, `v`) | appends `P`, `v`, `age`, `life`, `id` (`Cd`) |
//! | forces: `pnt.attract`, `vortex`, `drag`, `gravity`, `wind`, `turbulence` | `P`, `v` | `v` |
//! | `pnt.collide`, `pnt.integrate` | `P`, `v` | `P`, `v` |
//! | deformers: `pnt.bend`, `twist`, `taper`, `wave`, `transform` | `P` (`v`, `N`) | `P` (`v`, `N`) |
//! | `pnt.delete`, `subset`, `age` | rule attributes | every attribute (compacted, GPU count) |
//! | `pnt.group` | rule attributes | `group` |
//! | `pnt.merge`, `copy`, `sort` | every attribute | every attribute (`copynum`) |
//! | `pnt.attribute`, `attribute_math`, `randomize`, `colour` | any attribute | any attribute (`Cd`) |
//! | `pnt.normals`, `smooth`, `connect` (grid hash) | `P` (any) | `N`; the attribute; lines, `near`, `neardist` |
//! | `pnt.kernel` | the attributes its code reads (Kernel DSL) | the attributes its code writes |
//! | `pnt.solver`, `pnt.piece` | `init`, `prev`; an attribute or `P` | the state; `piece` |
//! | `pnt.foreach`, `pnt.foreach_end` | every attribute (pieces: one attribute) | every attribute, `iteration`; the passes merged |
//! | `pnt.points_from_image`, `points_from_depth` (IMG in) | an image (a depth and a colour image) | `P`, `Cd`, `uv`, `id`; `value`; `depth`, `valid` |
//! | `pnt.attribute_from_image` (IMG in) | `uv` or `P`, an image | any attribute |
//! | `pnt.file_in`, `pnt.sensor_in` (M6 capture, `docs/point-caches.md`) | a file (worker thread); a sensor or its test scene | the file's attributes; `P`, `Cd`, `depth`, `valid`, `body`, `id` or joints and bones |
//! | `img.render_points` (IMG) | `P`, `Cd`, polylines | an image |

mod attr;
pub mod cache;
mod emitter;
mod field;
mod file_in;
mod foreach;
mod from_image;
mod fuse;
mod image_kit;
mod kernel;
mod kit;
mod neighbours;
mod noise;
mod particles;
mod points;
pub mod project;
mod render;
mod select;
mod sensor;
mod solver;
mod sort;
mod sources;
mod transform;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use atd_gpu::engine::{GeoOp, OpLibrary};

pub use particles::MAX_CAPACITY;
pub use sensor::{JOINT_NAMES as SENSOR_JOINTS, test_pose as sensor_test_pose};

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
    (solver::solver_schema, solver::create_solver),
    (solver::piece_schema, solver::create_piece),
    (kernel::schema, kernel::create),
    (foreach::begin_schema, foreach::create_begin),
    (foreach::end_schema, foreach::create_end),
    (from_image::points_schema, from_image::create_points),
    (from_image::depth_schema, from_image::create_depth),
    (from_image::attribute_schema, from_image::create_attribute),
    (file_in::schema, file_in::create),
    (sensor::schema, sensor::create),
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
    library.set_point_fuser(fuse::create);
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

    /// CLAUDE.md rule 7: every operator of this crate is used by an example
    /// project (`examples/*.atidarla/networks/*.toml`).
    #[test]
    fn every_operator_has_an_example() {
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut text = String::new();
        for project in std::fs::read_dir(&examples).unwrap().flatten() {
            let Ok(networks) = std::fs::read_dir(project.path().join("networks")) else {
                continue;
            };
            for file in networks.flatten() {
                text.push_str(&std::fs::read_to_string(file.path()).unwrap_or_default());
            }
        }
        let missing: Vec<String> = schemas()
            .into_iter()
            .map(|s| s.type_id)
            .filter(|t| !text.contains(&format!("type = \"{t}\"")))
            .collect();
        assert!(
            missing.is_empty(),
            "operators without an example: {missing:?}"
        );
    }
}
