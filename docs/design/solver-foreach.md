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

**Built (2026-10-30): sub-steps.** Mark ratified sub-steps for M2 on 2026-10-30. The flatten-time
unrolling sketched in the first draft (N copies of a Pack's network, a `substeps` field in
`pack.toml`, a `dt` scale in detail values) was replaced by a smaller design that needs no
format change and works with or without a Pack:

1. `pnt.solver` has a **Sub-steps** parameter (1 by default, at most 64), saved like any other
   parameter (no `pack.toml` field; a Solver Block Pack promotes it like any parameter).
2. **Loops in the engine** (`atd_gpu::engine`, `GeoOp::repeat`). A loop is headed by a GEO node
   with a feedback GEO input; the input's source is the loop's end; the body is every planned
   node on a path from the head to the end. When the head's `repeat` asks for N passes, the
   engine unrolls the frame's plan: nodes of that stretch that do not depend on the head cook
   first, then the body N times in plan order, then the other nodes that depend on the head
   (they see the last pass). Each node still cooks with its own operator instance, so its
   state (an Emitter's count, say) carries from pass to pass. The plan and the scheduler are
   unchanged; the unrolled list is a reused buffer, so nothing allocates once warm.
3. **The step is divided by the engine**: in a `Repeat::SubSteps(N)` loop every body node gets
   `GeoCtx::dt = dt / N` (offline, the fixed step divided exactly), so no per-step operator
   changes. `GeoCtx::iteration` and `iterations` say which pass a cook is. The solver restarts
   (Reset, Start Frame, first cook) only on a frame's first pass; later passes continue from
   `prev`, the body's end as the previous pass left it (handed over in place when nothing else
   reads it).
4. **Each pass after the first is submitted on its own** (the engine submits what the frame
   has recorded before the head's next pass). `queue.write_buffer` takes effect at the next
   submit, so without this every pass's dispatches would see the last pass's uniforms; with it
   operators need no per-pass uniform slots. Cost: one submit per extra pass.
5. **Buffer pool order.** Loops drop pooled buffers in an order that changes from frame to
   frame, which made bind groups keyed by buffer identity miss their caches. The pool now
   returns a frame's buffers in a fixed (buffer-identity) order, so a steady state takes the
   same buffers for the same requests (`tests/alloc_batch.rs` runs a three-sub-step Solver).

Evidence: `solver_substeps_equal_a_four_times_finer_frame_rate` (four sub-steps at 50 fps are
bit-identical to one step at 200 fps over five frames), `one_frame_of_substeps_from_init`
(analytic), `an_emitter_in_substeps_numbers_every_particle_once` (fails without the per-pass
submit: 60 particles instead of 50), `tests/alloc_batch.rs` (no buffer, bind-group or texture
creation after warm-up with three sub-steps). Loops do not nest in M2: a loop inside another
loop's body makes one pass per outer pass.

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

1. ~~Solver sub-steps~~: ratified 2026-10-30 and built (above), by engine loops instead of
   flatten-time unrolling.
2. For-Each as "a piece attribute plus operators keyed by it" until the Kernel DSL: accept it
   as M2's For-Each, or wait for A12?
