# M2 pre-work: Python custom operators

**Status: first slice done (2026-10-18), for Mark's review.** PRD §9 M2 "Python custom ops
with schema-derived UI"; FR-K4 for Python ops. Design and user reference:
[`docs/python-ops.md`](../../python-ops.md). Run ahead of M1 closing at Mark's request;
nothing here relies on a Proposed decision. The architecture choices it needed are made in the
smallest form and listed under "Questions for Mark" rather than recorded as decisions (PRD.md
was not edited).

## Criteria → tasks → tests

| Criterion | Built | Evidence (tests run) |
|---|---|---|
| Users write SIG and CPU IMG operators in Python | `@lasair.op(...)` with `Float Int Toggle Menu String Colour Vec Input`, `ctx` (`time dt frame par inputs state width height image()`), RGBA8 bytes or floats out; no numpy needed (optional, buffer protocol) | `sig_operators_cook_with_state_parameters_and_inputs`, `img_operators_output_their_pixels` |
| Schema derived from the declaration; the existing panel needs no per-op code | The decorator is read with `ast` as literals (never run) into `OperatorSchema`; the panel (`lsr-app/src/params.rs`) builds widgets from `ParamKind` as for built-in operators | `declarations_become_schemas_without_running_the_file` (the file starts with `raise SystemExit`), `bad_declarations_report_their_line` (8 cases) |
| Off the render thread (rule 1): a frame late live, waited for offline with a timeout | One worker per project beside the expression worker; per-node slots copied in place (try-lock live, bounded wait offline); a supervisor raises `TimeoutError` in a stuck cook and restarts the worker if that does not stop it | `live_results_arrive_late_without_blocking_the_cook` (slowest cook < 50 ms with a 100 ms op), `stuck_operators_time_out_and_the_worker_restarts` (interruptible loop: error names its line, no restart; `sleep(30)`: restart within 4 s, state afresh), `errors_show_on_the_node_with_the_line` |
| Rule 2 still holds | Nothing on the cook thread allocates for a Python node once warm | `ops_alloc`: 3400 cook-thread allocations over 200 frames for a Python SIG node and 3400 for a built-in one (both the engine's per-frame encoder) |
| Expression worker's offline wait cannot hang | `submit_wait` deadline (`OFFLINE_TIMEOUT`, 10 s); late expressions become node errors; `PyExprHost` no longer joins its thread on drop | `offline_waits_for_python_expressions_are_bounded` |
| FR-K4: hot reload keeps state | `state={...}` is the state schema; fields kept by name and Python type; broken edits stop the node and keep its state; watcher thread + non-blocking `poll`; `stale_edits` drops values and wires a new schema no longer has (one undo step) | `hot_reload_keeps_compatible_state` (5 → 15 kept, broken edit, fixed → 25, int change resets to 10), `the_watcher_picks_up_saved_files`, `reload_cleans_the_patch_after_incompatible_schema_changes` |
| Registry, palette, project files, `check` | `py.<file>.<name>` with `python.custom` provenance (`Registry::replace_custom`, `remove_custom`, `provenance`; `OperatorSchema` unchanged); palette "Project" group; `check` explains missing and failed files | registry `custom_operators_are_namespaced_and_carry_provenance`, `python_nodes_round_trip_in_project_files`, `project_operators_have_their_own_palette_group`, `check_reports_missing_and_failed_python_operators`, `opening_python_operators_loads_without_running_them` |
| Trust: nothing runs before the user allows it | Execution gate in the worker; app prompt (Allow and remember / Allow this time / Don't run; File menu later); SHA-256 of `ops/` remembered in user settings; `--allow-python-ops` for `check` and `render` | `nothing_runs_until_the_project_is_allowed` (a marker file written at import does not exist until allowed), `the_trust_store_remembers_contents_not_folders`, `check_runs_python_operators_only_when_allowed`, `python_operators_render_deterministically_only_when_allowed` |
| Offline determinism | Each offline frame waits for its own result | `offline_renders_of_python_operators_are_deterministic` (12 frames, two engines); CLI below |
| Rule 7: runnable example | `examples/python-ops.lasair` (`ops/motion.py`: Step Counter, Python LFO; `ops/pattern.py`: Rings) | `check --gpu --allow-python-ops`; CI passes the flag |

## Questions for Mark

1. **Folder name.** PRD §5.5 lists `scripts/*.py`; this uses `ops/*.py` (one constant,
   `lsr_py::ops::OPS_DIR`). Add `ops/` to §5.5, or move operators into `scripts/`?
2. **Trust model.** Opt-in per project, remembered by the SHA-256 of `ops/` (names and texts,
   line endings normalised) in the user's settings, never in the project; any change asks again;
   your own hot-reload edits run without asking but do not update the remembered hash.
   Headless needs `--allow-python-ops` per run. Is this the model you want, and should edits
   made while allowed update the remembered hash (less asking, but a `git pull` while the app is
   open would be trusted too)?
3. **Python expressions are not gated (A22, existing).** A project's parameter expressions run
   on open without asking, so the gate above does not make opening an untrusted project safe.
   Extend the same gate to Python expressions (the live-control example would then need the
   allow, and CI the flag)?
4. **No sandbox, no kill.** Operators run in-process with the app's privileges. A cook that
   ignores the interrupt (a C call, `except BaseException` in a loop) leaves an abandoned thread
   running until it returns. Real isolation and a hard kill need an out-of-process host (like the
   AI Host). Is in-process right for v1?
5. **Declarations are literals only** (read with `ast`, so a project shows its operators before
   anyone runs it). Computed parameter lists are an error. Acceptable?
6. **Offline latency differs from expressions:** Python operators are a frame late live but not
   offline (the cook waits for its own frame), while Python expressions are one frame late in
   both. Keep that difference?
7. **Undo after an incompatible reload:** the clean-up is one undo step, but undoing it is
   refused (the old schema is gone). OK for now?

## Not in this slice

IMG inputs to Python operators (GPU readback), DAT/PNT/MSH families, Pulse and Path
parameters, imports between files in `ops/`, numpy bundling (optional only), per-operator docs
pages for project operators (they are user content, not shipped operators), and the textport.

## Log

Newest first, with the evidence actually run (RTX 5070 Ti, Windows, DX12, Python 3.11.9).

| Date | Item | Evidence |
|---|---|---|
| 2026-10-18 | First slice | `cargo test -p lsr-py`: 23 passed (16 `ops`, 1 `ops_alloc`, 3 `engine`, 2 `expr`, 1 unit). `lsr-cli` new tests (3), `lsr-app` new tests (2) and registry test (1) pass. `cargo test --workspace`: 319 passed, 0 failed, 1 ignored (the pre-work baseline was 294/0/1). `lasair render examples/python-ops.lasair --range 0-60 --allow-python-ops` twice: 60/60 frames byte-identical (SHA-256), 60 distinct; without the flag it fails with "Python operators in this project are not allowed to run…". `lasair check --gpu --allow-python-ops examples/*.lasair`: 12 ok; without the flag the Python example is ok and lists its three nodes as not run. App smoke run on the example (`--smoke 60`): ok, palette shows "Project (3)". clippy, fmt, `docs --check` pass. A bug found on the way: `PyModule::from_code` re-runs into an existing module of the same name, so two projects' workers shared one Python namespace (caught by the CLI render test); each helper now has a unique name. |
| 2026-10-18 | Design written (`docs/python-ops.md`) | review only |
