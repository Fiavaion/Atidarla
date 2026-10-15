# MSH library licences (M0 acceptance)

**Date:** 2026-10-15 · **Method:** shallow clones of each upstream repo at HEAD (LICENSE files and in-tree third-party code), crates.io metadata, and the unpacked crate sources. The GitHub REST API was unavailable, so "latest" means the newest git tag, not the Releases page.
**Policy:** core crates must be MIT OR Apache-2.0 compatible, with no GPL, AGPL, LGPL-static or non-commercial code (PRD A4, CLAUDE.md rule 3).

## Verdicts

| Library (PRD §6) | Upstream licence | Latest tag | Third-party code that matters | Best Rust binding | Verdict |
|---|---|---|---|---|---|
| [Manifold](https://github.com/elalish/manifold) (Boolean) | Apache-2.0 | v3.5.4 (2026-09-25) | Clipper2 **BSL-1.0**; oneTBB Apache-2.0 (optional); small MIT/BSD-3 snippets | `manifold3d` / `manifold-csg` 0.4.1 (Apache-2.0 OR MIT) | **OK with conditions** |
| [meshoptimizer](https://github.com/zeux/meshoptimizer) (PolyReduce) | MIT | v1.3 (2026-09-25) | none | `meshopt` 0.6.2 (MIT OR Apache-2.0), but it vendors the old 0.25 | **OK** |
| [xatlas](https://github.com/jpcy/xatlas) (UV Unwrap) | MIT | no tags; HEAD 2022-07-26 (unmaintained) | embedded MIT notices + an OpenNL excerpt (BSD-3) | `xatlas-rs-v2` 0.1.4 (MIT; byte-identical to upstream HEAD; bindgen at build time) | **OK** |
| [Geogram](https://github.com/BrunoLevy/geogram) (Remesh) | BSD-3-Clause | v1.10.1 (2026-09-01) | **HLBFGS (non-commercial), TetGen (AGPL-3.0), Triangle (no commercial redistribution), all ON by default** | none on crates.io | **OK only with build flags; needs Mark's sign-off** |

## Conditions and notes

**Manifold**
- Allow `BSL-1.0` in `deny.toml` when the dependency is added. Credit Manifold, Clipper2, PhysX (`tri_dist.h`, BSD-3) and wi-re (`svd.h`, MIT) in NOTICE.
- `manifold-csg-sys` runs `git clone` of Manifold at build time. That is not acceptable for reproducible or offline builds: vendor or fork the `-sys` crate, or prebuild and point `MANIFOLD_CSG_LIB_DIR` at the result.
- TBB is on by default through the `parallel` feature. Its licence is fine, but decide whether its thread pool should run alongside our MSH worker threads.
- Manifold `main` (unreleased) replaces Clipper2 with an in-tree backend.
- `manifold-rust` 0.16.0 (Apache-2.0) is a pure-Rust port worth benchmarking against PT-12.

**meshoptimizer**
- Bump the vendored source to 1.3 with a small in-house `-sys` crate (or a patched `meshopt`).
- 1.3 adds an experimental `meshopt_remesh` voxel remesher (MIT), a possible Geogram substitute for Remesh.

**xatlas**
- Reproduce the OpenNL BSD-3 notice in NOTICE.
- Prefer our own thin `-sys` crate with pre-generated bindings, so Windows and macOS CI don't need libclang.

**Geogram**
- Build only with `-DGEOGRAM_WITH_HLBFGS=OFF -DGEOGRAM_WITH_TETGEN=OFF -DGEOGRAM_WITH_TRIANGLE=OFF -DGEOGRAM_WITH_GRAPHICS=OFF -DGEOGRAM_LIB_ONLY=ON`.
- Delete those `third_party` directories from any vendored copy, and add a CI check, because cargo-deny cannot see C++ sources.
- Without HLBFGS, CVT remeshing falls back to Lloyd iterations: lower quality and slower.
- OpenNL may `dlopen` CHOLMOD/ARPACK at runtime if present. Never ship CHOLMOD (licence unverified, believed LGPL/GPL).
- **Alternatives to weigh first:** meshoptimizer 1.3 `meshopt_remesh` (MIT), `remesh` (MIT OR Apache-2.0, early stage), `baby_shark` (MIT, remeshing quality unverified).

**Other crates seen**
- `boolmesh` is MPL-2.0 (file-level copyleft). It is not on the allow list, and allowing it would need a policy decision.
- `csgrs` currently fails to resolve: its dependency `core2` is yanked.

## Not verified
GitHub Release-page dates; MeshLib's licence terms (believed commercial, so excluded); CHOLMOD's licence; `meshoptimizer-rs`'s claim of byte-identical output; `baby_shark`'s remeshing features.
