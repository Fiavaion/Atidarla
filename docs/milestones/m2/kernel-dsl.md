# M2 pre-work: Kernel DSL compiler (`atd-kernel`)

> **ISOLATED AND REMOVABLE. Decision A12 (Kernel DSL) is still PROPOSED in PRD §0.**
> Mark authorised running ahead of the milestone order, not ratifying A12. Everything in this
> area is therefore confined to the self-contained crate `crates/atd-kernel` (plus its tests),
> the reference page `docs/kernel-dsl.md`, this note, and one opt-in CLI subcommand
> (`atidarla kernel`). Nothing in the engine, the registry, the app or the file format uses
> it; no operator is registered. Deleting the crate, the CLI subcommand and the two docs
> removes the feature completely.

Status: in progress (2026-10-18).

## Plan: FR-K1..K3 (compile side) → tasks → tests

| Requirement | Task | Test |
|---|---|---|
| FR-K1 `@attr` with schema-generated bindings, `ch()` hooks | Lexer, parser, typed IR; attribute typing (sigil, schema, standard names); `Bindings` and `ParamDecl` | Golden WGSL; binding layout tests (16-byte stride for 3-component types) |
| FR-K2 helpers | `point()`, `nearpoint(s)`, noise family, maths, matrices, quaternions, `texture()` | Golden WGSL; CPU interpreter vs GPU on a grid |
| FR-K3 inline errors, Generated WGSL | `Diagnostic { line, col, len, message, hint }`; naga validation mapped back to the kernel | Error-position tests; deterministic fuzz (never panics) |
| AC2 groundwork | The ten-line curl-noise kernel in `docs/kernel-dsl.md` | Compiles and validates; doc-example test |
