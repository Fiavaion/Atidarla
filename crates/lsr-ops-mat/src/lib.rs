//! MAT operator family (PRD §6) and the 3D renderer that reads it,
//! `img.render` ("Render (GEO + MAT + camera)").
//!
//! Materials flow on `MAT` wires as small CPU values (`lsr_gpu::mat`,
//! contract in docs/milestones/m2/mat-render.md); the renderer draws GEO
//! values with them. Every operator ships complete (CLAUDE.md rule 7):
//! schema with a cook-time budget, a generated docs page (`lasair docs`),
//! example projects (`examples/render-*.lasair`), GPU tests (`tests/`)
//! and an offline-render class (`lsr-cli` `classify`).
//!
//! | Operator | Reads | Gives |
//! |---|---|---|
//! | `mat.unlit` | image `map` | colour × `Cd` × map at `uv` |
//! | `mat.pbr` | image `map` | lit albedo (Lambert + GGX), roughness, metallic, emission |
//! | `mat.matcap` | image `matcap` | lit-sphere look-up by view-space normal |
//! | `mat.toon` | — | banded diffuse, highlight, rim |
//! | `mat.line` | — | polygon edges and polylines in pixels, optional fill |
//! | `mat.point_sprite` | — | points as discs or squares (`pscale`) |
//! | `mat.splat` | — | points as Gaussian splats (`scale`, `orient`, `Cd`, `sh0`…), GPU-sorted and blended |
//! | `img.render` (IMG) | GEO `P N Cd uv pscale`, prim `Cd`; MAT | an image |
//! | `msh.platonic` (MSH) | — | `P`, `N`, `uv` (split per face), prim `Cd` |

mod camera;
mod materials;
mod platonic;
mod render;
mod splat;
mod splat_sort;
pub mod triangulate;

use lsr_core::registry::{OperatorSchema, Registry};
use lsr_gpu::engine::OpLibrary;

pub use platonic::build as platonic_geometry;

/// Every schema this crate registers.
pub fn schemas() -> Vec<OperatorSchema> {
    let mut all = materials::schemas();
    all.push(splat::schema());
    all.push(render::schema());
    all.push(platonic::schema());
    all
}

/// Registers this family's operators (and `img.render`, `msh.platonic`).
pub fn register(registry: &mut Registry) {
    for schema in schemas() {
        registry
            .register(schema)
            .expect("MAT operator schemas are valid and unique");
    }
}

/// Registers this family's implementations.
pub fn library(library: &mut OpLibrary) {
    for schema in materials::schemas() {
        let factory = materials::create(&schema.type_id).expect("every MAT schema has an op");
        library.add_mat(schema.type_id, factory);
    }
    library.add_mat(splat::schema().type_id, splat::create);
    library.add(render::schema().type_id, render::create);
    library.add_geo(platonic::schema().type_id, platonic::create);
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
        let mats = registry
            .iter()
            .filter(|s| s.family == lsr_core::registry::OpFamily::Mat)
            .count();
        assert!(mats >= 6, "PRD M2: MAT ≥ 6 ({mats})");
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
