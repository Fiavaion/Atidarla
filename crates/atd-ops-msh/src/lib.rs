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

// Mesh code walks several parallel per-element arrays by index.
#![allow(clippy::needless_range_loop)]

pub mod bevel;
pub mod boolean;
pub mod carve;
pub mod clean;
pub mod clip;
pub mod cook;
pub mod edit;
pub mod file;
pub mod fracture;
pub mod hull;
pub mod io;
pub mod measure;
pub mod mesh;
pub mod mirror;
pub mod model;
pub mod normals;
pub mod points;
pub mod prims;
pub mod reduce;
pub mod remesh;
pub mod revolve;
pub mod smooth;
pub mod sweep;
pub mod text3d;
pub mod topo;
pub mod unwrap;
pub mod vat;

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
        Def {
            id: "subdivide",
            label: "Subdivide",
            summary: "Smooths polygons by Catmull-Clark subdivision: each level splits every polygon into quads and rounds the shape (open borders stay as curves). Attributes are interpolated.",
            inputs: &["in"],
            params: model::subdivide_params,
            run: model::subdivide,
        },
        Def {
            id: "extrude",
            label: "Extrude",
            summary: "Pushes polygons out along their normals and adds side walls, each polygon on its own (with an inset) or as connected regions. The moved polygons are the group `extrude_front`, the walls `extrude_side`.",
            inputs: &["in"],
            params: model::extrude_params,
            run: model::extrude,
        },
        Def {
            id: "uv_project",
            label: "UV Project",
            summary: "Writes texture coordinates `uv` per point by a planar, cylindrical, spherical or box projection, fitted to the bounds.",
            inputs: &["in"],
            params: model::uv_params,
            run: model::uv_project,
        },
        Def {
            id: "polyreduce",
            label: "PolyReduce",
            summary: "Reduces the polygon count while keeping the shape: quadric error edge collapses on triangles, down to a share of the triangles or until the error passes a tolerance. Borders can be kept.",
            inputs: &["in"],
            params: reduce::params,
            run: reduce::run,
        },
        Def {
            id: "boolean",
            label: "Boolean",
            summary: "Union, intersection or difference of two closed meshes (BSP trees). The result is welded; pieces from A and B are the groups `from_a` and `from_b`. Up to 200,000 triangles per input; seams may keep T-junctions; normals are not written (add Normals).",
            inputs: &["a", "b"],
            params: boolean::params,
            run: boolean::run,
        },
        Def {
            id: "sweep",
            label: "Sweep",
            summary: "Sweeps a profile along a curve: the first primitive of `profile` (drawn in the XY plane) or a circle, along the first primitive of `curve`, with rotation-minimising frames, scale and twist. Writes `N` and `uv`.",
            inputs: &["curve", "profile"],
            params: sweep::sweep_params,
            run: sweep::sweep,
        },
        Def {
            id: "skin",
            label: "Skin",
            summary: "Lofts a surface through the input's primitives in order (profiles with equal point counts), optionally as a closed ring and with end caps.",
            inputs: &["in"],
            params: sweep::skin_params,
            run: sweep::skin,
        },
        Def {
            id: "scatter",
            label: "Scatter",
            summary: "Seeded random points on the surface, evenly by area, with the surface normal `N`, an `id` and the surface's other attributes interpolated: points for PNT operators or Iso Surface.",
            inputs: &["in"],
            params: points::scatter_params,
            run: points::scatter,
        },
        Def {
            id: "iso_surface",
            label: "Iso Surface",
            summary: "Builds a closed mesh where a field crosses zero (marching tetrahedra): blended spheres around the input points (radius × `pscale`), or an analytic sphere, box, torus or gyroid. Writes `N` from the field.",
            inputs: &["in"],
            params: points::iso_params,
            run: points::iso_surface,
        },
        Def {
            id: "file_in",
            label: "Mesh File In",
            summary: "Reads a static mesh from an OBJ, PLY or glTF 2.0 (.gltf, .glb) file: positions, polygons and lines, normals, texture coordinates and colours (glTF: the default scene with its node transforms).",
            inputs: &[],
            params: file::in_params,
            run: file::file_in,
        },
        Def {
            id: "file_out",
            label: "Mesh File Out",
            summary: "Writes its input to an OBJ, PLY or glTF 2.0 (.gltf, .glb) file whenever it changes, and passes it through. Files are byte-identical for the same geometry.",
            inputs: &["in"],
            params: file::out_params,
            run: file::file_out,
        },
        Def {
            id: "revolve",
            label: "Revolve",
            summary: "Turns every primitive of the input about an axis (a lathe): a full turn or an arc, with end caps for closed profiles. Points on the axis are welded. Writes `N` and `uv`.",
            inputs: &["in"],
            params: revolve::params,
            run: revolve::run,
        },
        Def {
            id: "carve",
            label: "Carve",
            summary: "Keeps the part of each polyline between two fractions of its length, or the part of each polygon whose `uv` u or v lies in a range. Cuts interpolate every point attribute and stay welded.",
            inputs: &["in"],
            params: carve::params,
            run: carve::run,
        },
        Def {
            id: "convex_hull",
            label: "Convex Hull",
            summary: "The smallest convex mesh around the input's points (quickhull): outward triangles, the points' attributes kept. Flat inputs give one polygon.",
            inputs: &["in"],
            params: hull::params,
            run: hull::run,
        },
        Def {
            id: "fracture",
            label: "Fracture",
            summary: "Breaks a closed mesh into Voronoi pieces around seeded random points inside it (or the `points` input), with an optional gap. Writes `piece` per point and primitive; cut faces are the group `inside`, the original surface `outside`. Heavy: usually baked.",
            inputs: &["in", "points"],
            params: fracture::params,
            run: fracture::run,
        },
        Def {
            id: "uv_unwrap",
            label: "UV Unwrap",
            summary: "Cuts the surface into charts by normal angle, flattens each (least squares conformal maps, or planar) and packs them into the unit square: writes `uv` (points split along seams) and the primitive attribute `chart`.",
            inputs: &["in"],
            params: unwrap::params,
            run: unwrap::run,
        },
        Def {
            id: "text_3d",
            label: "Text 3D",
            summary: "Extruded letters from a built-in or TrueType/OpenType font: filled glyph outlines (holes kept) with sides, in the groups `front`, `back` and `side`.",
            inputs: &[],
            params: text3d::params,
            run: text3d::run,
        },
        Def {
            id: "vat_in",
            label: "VAT In",
            summary: "Plays vertex animation textures in the layout of Houdini VAT 3.0 (soft, rigid, fluid, sprite): the exported mesh moved by its position (and rotation) texture at a frame, with colours. Drive Frame with an expression to play.",
            inputs: &[],
            params: vat::params,
            run: vat::run,
        },
        Def {
            id: "colour",
            label: "Colour",
            summary: "Writes the point colour `Cd`: a constant, or from the normals, texture coordinates, position in the bounds, or a seeded random colour per point or per primitive, times a tint.",
            inputs: &["in"],
            params: model::colour_params,
            run: model::colour,
        },
        Def {
            id: "bevel",
            label: "Bevel",
            summary: "Bevels sharp edges: each becomes a flat chamfer or a rounded strip of segments, with patches where strips meet at a corner. Selected by angle and an optional point group; edges at open borders or non-manifold points stay. The new polygons are the group `bevel`.",
            inputs: &["in"],
            params: bevel::params,
            run: bevel::run,
        },
        Def {
            id: "remesh",
            label: "Remesh",
            summary: "Rebuilds the surface as even triangles of about one edge length (isotropic remeshing: split, collapse, flip, relax, project back), keeping open borders and sharp edges. Attributes are interpolated from the nearest point of the input surface; polylines are dropped.",
            inputs: &["in"],
            params: remesh::params,
            run: remesh::run,
        },
        Def {
            id: "smooth",
            label: "Smooth",
            summary: "Smooths the surface without changing its topology: Taubin smoothing (keeps the size) or Laplacian smoothing (shrinks), optionally keeping open borders and limited to a point group.",
            inputs: &["in"],
            params: smooth::params,
            run: smooth::run,
        },
        Def {
            id: "clean",
            label: "Clean",
            summary: "Repairs geometry: welds duplicate points, removes degenerate and duplicate primitives and unused points, and makes polygon winding consistent (closed parts facing outward).",
            inputs: &["in"],
            params: clean::params,
            run: clean::run,
        },
        Def {
            id: "mirror",
            label: "Mirror",
            summary: "Reflects the geometry across a plane, with or without the original; the seam can be welded so a half model becomes one closed piece. The reflected primitives are the group `mirrored`.",
            inputs: &["in"],
            params: mirror::params,
            run: mirror::run,
        },
        Def {
            id: "clip",
            label: "Clip",
            summary: "Cuts the geometry with a plane and keeps one side or both (groups `above` and `below`), welded along the cut. Cap fills the cut (group `clip_cap`) so closed input stays closed.",
            inputs: &["in"],
            params: clip::params,
            run: clip::run,
        },
        Def {
            id: "measure",
            label: "Measure",
            summary: "Writes measurements as attributes: primitive `area`, `perimeter` and normal `N`, and point `curvature` (mean or Gaussian).",
            inputs: &["in"],
            params: measure::params,
            run: measure::run,
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
