# M2 pre-work: Python custom operators

**Status: in progress (2026-10-18).** PRD §9 M2 "Python custom ops with schema-derived UI";
FR-K4 for Python ops. Design and user reference: [`docs/python-ops.md`](../../python-ops.md).
Run ahead of M1 closing at Mark's request; nothing here relies on a Proposed decision, and
the architecture questions are listed below rather than decided.

## Criteria → tasks → tests

| Criterion | Tasks | Tests (planned) |
|---|---|---|
| Users write SIG and CPU IMG operators in Python | `@atidarla.op` decorator, parameter constructors, `ctx`, `state`, pixel buffers (no numpy needed) | engine tests: SIG counter, LFO with a parameter, CPU IMG generator |
| Schema derived from the declaration; the existing parameter panel needs no per-op code | Static read of the decorator with `ast` (no execution) into `OperatorSchema`/`ParamSchema` | schema-level test of every parameter kind, ranges, help, inputs; bad declarations report their line |
| Cooks off the render thread (rule 1), a frame late live, waited for offline with a timeout | A Python operator worker beside the expression worker; per-node slots with reused buffers; offline wait bounded by timeout + grace; a supervisor thread interrupts or restarts a stuck worker | live latency test; steady-state allocation test of the cook-thread side; offline determinism over two renders; timeout (interruptible and not); exception with line number |
| Expression worker: the offline `submit_wait` must not hang | Deadline on `submit_wait`; missing results become node errors | test with a hung expression |
| FR-K4: hot reload keeps state | `state` declaration as the state schema; migration rule by name and type; file watcher; registry update; patch clean-up on incompatible schema changes | test: cook N frames, edit the file, state continues; incompatible field resets |
| Registry, palette, project files, `check` | `py.<file>.<name>` type IDs with `python.custom` provenance; Project group in the palette; round-trip; `check` explains missing and failed files | registry tests; round-trip test; check tests (missing file, bad declaration, not allowed) |
| Trust: nothing from a project runs before the user allows it | Execution gate in the worker; remembered SHA-256 of `ops/` in user settings; app prompt; `--allow-python-ops` for `check` and `render` | test: not-allowed nodes report and run nothing; allow runs them; store remembers by hash and forgets on change |
| Rule 7: runnable example | `examples/python-ops.atidarla` with `ops/*.py` | `check --gpu --allow-python-ops`; CI step |

## Questions for Mark

See the final report of this work; they are repeated in the log below once answered.

## Log

Newest first, with the evidence actually run.

| Date | Item | Evidence |
|---|---|---|
| 2026-10-18 | Design written (`docs/python-ops.md`) | review only |
