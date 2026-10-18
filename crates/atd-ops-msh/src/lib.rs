//! MSH operator family: CPU topology and modelling operators on the shared
//! GEO wire (PRD §6). Cooks run on worker threads and never block the render
//! thread (CLAUDE.md rule 1); offline renders wait for them.
//!
//! Design: `docs/design/msh-operators.md`. Each operator is a pure function
//! of its parameters and input meshes ([`RunFn`]); [`cook::MeshNode`] runs
//! it on a worker, uploads the result and swaps it in. [`run`] calls an
//! operator directly (tests and tools).
//!
//! Every operator ships complete (rule 7): schema with a cook-time budget
//! (the render-thread part: snapshot and hand-over; the worker time shows
//! in the node's status line), a generated docs page (`atidarla docs`), an
//! example project, a unit test and an offline-render class (`Waits`).

pub mod cook;
pub mod edit;
pub mod mesh;
pub mod normals;
pub mod prims;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use atd_gpu::engine::OpLibrary;
use serde_json::Value;

pub use cook::{Job, RunFn, Snap};
pub use mesh::{Column, Mesh, Values};

/// One operator: schema parts and its work.
struct Def {
    id: &'static str,
    label: &'static str,
    summary: &'static str,
    inputs: &'static [&'static str],
    params: fn() -> Vec<ParamSchema>,
    run: RunFn,
}

/// Render-thread budget of every MSH cook (snapshot, input comparison,
/// hand-over); the work itself runs on a worker.
pub const COOK_BUDGET_US: u32 = 100;

fn defs() -> Vec<Def> {
    vec![
        Def {
            id: "grid",
            label: "Grid",
            summary: "A flat rectangle of quads (a plane), with normals `N` and texture coordinates `uv`.",
            inputs: &[],
            params: prims::grid_params,
            run: prims::grid,
        },
        Def {
            id: "box",
            label: "Box",
            summary: "A closed box of quads, welded at its edges, with smooth point normals `N` (add Normals in flat mode for hard edges).",
            inputs: &[],
            params: prims::box_params,
            run: prims::cube,
        },
        Def {
            id: "sphere",
            label: "Sphere",
            summary: "A closed sphere: polar rings of quads, or an evenly subdivided icosahedron. Writes `N`.",
            inputs: &[],
            params: prims::sphere_params,
            run: prims::sphere,
        },
        Def {
            id: "tube",
            label: "Tube",
            summary: "A cylinder or cone along Y, optionally capped (closed), with smooth normals `N`. A radius of 0 makes a cone point.",
            inputs: &[],
            params: prims::tube_params,
            run: prims::tube,
        },
        Def {
            id: "torus",
            label: "Torus",
            summary: "A closed torus (ring) around Y, made of quads, with normals `N`.",
            inputs: &[],
            params: prims::torus_params,
            run: prims::torus,
        },
        Def {
            id: "circle",
            label: "Circle",
            summary: "A circle or arc: one polygon, an outline (polyline) or points, in a chosen plane.",
            inputs: &[],
            params: prims::circle_params,
            run: prims::circle,
        },
        Def {
            id: "line",
            label: "Line",
            summary: "A straight polyline between two points, with evenly spaced points (a curve for Sweep).",
            inputs: &[],
            params: prims::line_params,
            run: prims::line,
        },
        Def {
            id: "normals",
            label: "Normals",
            summary: "Computes normals `N`: smooth point normals with a cusp angle that keeps sharp edges, flat (faceted) normals, or one normal per primitive.",
            inputs: &["in"],
            params: normals::params,
            run: normals::run,
        },
        Def {
            id: "triangulate",
            label: "Triangulate",
            summary: "Splits every polygon into triangles (ear clipping, so concave polygons work). Polylines are kept.",
            inputs: &["in"],
            params: edit::triangulate_params,
            run: edit::triangulate,
        },
        Def {
            id: "transform",
            label: "Transform",
            summary: "Scales, rotates and moves the geometry about a pivot. Normals follow; a mirror flips polygons so they still face outward.",
            inputs: &["in"],
            params: edit::transform_params,
            run: edit::transform,
        },
        Def {
            id: "merge",
            label: "Merge",
            summary: "Combines up to four geometries into one. Attributes and groups are joined by name; missing values are zero (one for `Cd` and `pscale`).",
            inputs: &["in0", "in1", "in2", "in3"],
            params: edit::merge_params,
            run: edit::merge,
        },
        Def {
            id: "delete",
            label: "Delete",
            summary: "Deletes the primitives or points of a group (or everything outside it). Deleting points deletes the primitives that use them.",
            inputs: &["in"],
            params: edit::delete_params,
            run: edit::delete,
        },
        Def {
            id: "group",
            label: "Group",
            summary: "Makes a point or primitive group: elements inside a box, facing a direction, every n-th in an index range, or all; combined with an existing group by union, intersection or subtraction.",
            inputs: &["in"],
            params: edit::group_params,
            run: edit::group,
        },
        Def {
            id: "fuse",
            label: "Fuse",
            summary: "Welds points closer than a distance into one and removes primitives that collapse.",
            inputs: &["in"],
            params: edit::fuse_params,
            run: edit::fuse,
        },
    ]
}

fn schema(d: &Def) -> OperatorSchema {
    let socket = |name: &str| SocketSchema {
        name: name.into(),
        wire: WireType::Geo,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: format!("msh.{}", d.id),
        family: OpFamily::Msh,
        label: d.label.into(),
        summary: d.summary.into(),
        version: 1,
        params: (d.params)(),
        inputs: d.inputs.iter().map(|n| socket(n)).collect(),
        outputs: vec![socket("out")],
        capabilities: vec![],
        cook_budget_us: COOK_BUDGET_US,
        time_dependent: false,
        sink: false,
    }
}

/// Every schema this crate registers.
pub fn schemas() -> Vec<OperatorSchema> {
    defs().iter().map(schema).collect()
}

/// Registers this family's operators with the registry.
pub fn register(registry: &mut Registry) {
    for schema in schemas() {
        registry
            .register(schema)
            .expect("MSH operator schemas are valid and unique");
    }
}

/// Registers this family's implementations (worker-thread cooks).
pub fn library(library: &mut OpLibrary) {
    for d in defs() {
        let schema = schema(&d);
        let run = d.run;
        let id = schema.type_id.clone();
        library.add_geo(id, move |_| Box::new(cook::MeshNode::new(&schema, run)));
    }
}

/// Runs operator `type_id` on this thread with `params` over the schema
/// defaults (tests and tools).
pub fn run(
    type_id: &str,
    params: &[(&str, Value)],
    inputs: &[Option<Mesh>],
) -> Result<Mesh, String> {
    let d = defs()
        .into_iter()
        .find(|d| format!("msh.{}", d.id) == type_id)
        .ok_or_else(|| format!("no MSH operator `{type_id}`"))?;
    let snap = Snap::from_values(&schema(&d), params);
    let out = (d.run)(&snap, inputs, &Job::new())?;
    out.validate()?;
    Ok(out)
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
