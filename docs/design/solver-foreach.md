# Solver Block and For-Each on GEO wires (M2 design, short)

**Status:** draft for review, 2026-10-18, written before the code (M2 PNT batch). Builds on
[`geometry-attributes.md`](geometry-attributes.md) §3.2 (Solver, For-Each) and its §9 notes.
**Covers:** the "Solver Block" and "For-Each" of PRD §9 M2 AC5 (PNT ≥ 30 operators), the
smallest correct versions, and what is left for a decision.

## What already exists

- **State on the wire** (design D6): any GEO feedback input reads the previous frame's value,
  and the engine hands that value over in place when nothing else reads it (GEO-1). Particles
  uses this: Particles → forces → `prev`.
- **Nested networks** (FR-P4): a Pack is a network with sockets and promoted parameters,
  flattened into its nodes when a network loads or changes (`atd_core::pack`), so the engine
  never sees a container. A wire inside a Pack may be a feedback wire like any other.
- **Per-step operators** (this batch): forces, Collide, Integrate and Age read the frame's
  step `dt`, so a loop of them is a simulation.

## Solver Block

**Built: `pnt.solver`, the loop's entry.** Inputs `init` and `prev` (feedback). It outputs
`init` on its first cook, while Reset is on, until Start Frame, or when `prev` has no geometry;
otherwise `prev`. (A state whose live count dropped to zero on the GPU is not seen as empty:
that would need a readback.) Wire the end of a body (for example Gravity → Collide → Integrate → Age)
into `prev`. This is design §3.2's data contract; the body is ordinary operators.

**A Solver Block as a nested network** is then a Pack whose network is `pnt.solver` plus a
body wired back into the solver's `prev`, with the Pack input on `init` and the Pack output on
the body's end. Nothing new is needed for this: flattening keeps the inner feedback wire, and a
test cooks such a Pack (`crates/atd-ops-pnt/tests/batch/solver.rs`). Offline it is Stateful
(`docs/render.md`).

**Not built: sub-steps.** Running the body N times per frame needs N copies of it in one
frame, because the engine cooks each node once per frame. The smallest design that fits:

1. A Pack definition gains an optional `substeps` (1 by default), saved in `pack.toml`
   (rule 8: a round-trip test).
2. Flattening walks a solver Pack's network N times, with paths `[instance, k]` (stable IDs,
   as today), and chains the copies: copy 0's solver keeps the feedback from copy N−1's end;
   copy k > 0's solver takes copy k−1's end on `init` and passes it through (a solver option
   the flattener sets).
3. Every copy steps by `dt / N`: the solver writes the scale into the geometry's detail
   values, and the per-step operators multiply `dt` by it (one helper in `atd-ops-pnt`).

Cost: N times the body's dispatches, no allocation after warm-up (the copies hand their
buffers over in place). This changes `atd-core` (flattening, `PackDef`) and `atd-format`
(`pack.toml`), so it waits for a yes (question 1).

## For-Each

Design §3.2: "M2 For-Each runs a body of per-element PNT ops once over all elements, with the
piece or iteration index as an attribute" (RESEARCH §2.3: compiled to one dispatch).

**Built: `pnt.piece` (For-Each Begin)** writes a `piece` number to every point: by chunks of
N consecutive points, by buckets of an attribute's value (for example `copynum` from Copy to
Points, or a coordinate), or by cells of a spatial grid. The body is then the ordinary
operators run once over all points, and those that take a per-point key act per piece:
**Randomize** gains Key Attribute (values seeded by `piece`, so every piece gets its own
random value, colour, scale…), and Group, Delete, Subset, Sort and Colour by Attribute already
read any attribute. Per-piece parameters beyond these (a different rotation per piece, an
expression of `@piece`) are the Kernel DSL's job (A12, Proposed).

**Not built:** iterations ("repeat the body N times", unrolled into N dispatches per operator
with per-iteration parameters at dynamic uniform offsets, as Sort already does for its
stages), and running an MSH body per piece (left out of M2 by the design).

## Questions for Mark

1. Solver sub-steps by flatten-time unrolling and a `dt` scale (above): build it in M2, or is
   one step per frame (raise the frame rate offline) enough for now?
2. For-Each as "a piece attribute plus operators keyed by it" until the Kernel DSL: accept it
   as M2's For-Each, or wait for A12?
