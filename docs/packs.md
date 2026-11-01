# Packs and nested networks

A **Pack** is a network saved as a reusable unit: a name, a semantic version, embedded help,
promoted parameters, and input and output sockets by wire type (FR-P4, PRD §5.5). A
project-local nested network is the same thing without a lock. In a network, a Pack instance
is an ordinary node of type `cmp.pack_<name>` (CMP family, PRD §6), and its parameters are the
promoted parameters. So wires, type checks, cycles, channel bindings, expressions, undo/redo and
the project file work as they do for built-in operators.

Code: `crates/lsr-core/src/pack.rs` (model, validation, flattening, lock, update, Make Pack),
`crates/lsr-format/src/pack.rs` (files, libraries), `crates/lsr-cli/src/project.rs` (`check`,
`fmt`). Shipped Packs: [`packs/`](../packs/README.md).

## The model

| Part | What it is |
|---|---|
| Network | The nodes inside the Pack, an ordinary network with stable IDs. |
| Promoted parameter | Name, label, default, optional `min`/`max`, help, and the inner parameters it drives (one or more, all of one kind). Its kind is the targets' kind; `min`/`max` may only narrow their range. Label and help fall back to the first target's. |
| Input | A name and the inner inputs it feeds (which must not be wired inside). Its wire type is theirs. |
| Output | A name and the inner output it shows. |
| Version | `MAJOR.MINOR.PATCH`. Requests: `1.2.3` (exactly), `^1.2` (compatible, as in Cargo), `*`. |
| Help | Markdown, shown in the app; `help.md` in the Pack folder. |
| Locked | A locked Pack cannot be edited inside. Library Packs are locked. |
| Derived from | For an unlocked copy: the Pack and version it was copied from. |

**Checks** (`PackSet::register`, which turns each Pack into an operator schema, in dependency
order): the inner network validates against the registry; every target and socket exists;
targets share a kind; ranges and defaults fit; names are unique; a Pack does not contain itself
(directly or through others); nesting is at most 16 deep; contained Packs are known. Errors name
the Pack and, from the CLI, the file (`packs/glow/pack.toml: Pack `glow`: parameter `amount`:
...`). Damaged files, cycles and very deep nesting are errors, never panics or stack overflows
(fuzz tests in both crates).

**Feedback.** A Pack input is a feedback input (a wire into it may close a loop) only if no path
from it reaches an output except through a feedback input inside. Otherwise a loop through the
Pack is rejected, like any cycle.

## Cooking: flattened before the cook

`PackSet::flatten` expands every instance, recursively, into the nodes inside it, and the engine
cooks that flat network. **Chosen over cooking a Pack as a unit** because the flat network uses
the existing scheduler unchanged: lazy pull, caching, feedback, bindings, expressions (native and
Python), SIG and IMG mixed, offline mode. A unit would have to re-implement all of it inside one
operator.

- Flattening runs when a network is loaded or edited, never per frame. Planning and cooking the
  flat network allocates nothing after warm-up (`crates/lsr-core/tests/pack_plan_alloc.rs`), and
  the cook order is the same every frame and every run.
- Nodes outside any Pack keep their IDs and names. A node inside instance `glow1` is named
  `glow1__blur1` (shown as `glow1/blur1`), with an ID derived from the instance path and its ID
  inside the Pack (bit 62 set). It is the same after any other edit, so operator state (feedback
  buffers, movie decoders) can survive re-flattening.
- A promoted parameter's value (a constant, a binding or an expression) replaces each inner
  parameter it drives. An unset promoted parameter takes the Pack's default.
- Inner wires and bindings are remapped. Wires across the boundary connect straight to the inner
  sockets. A binding to a Pack instance follows its first SIG output.
- In expressions, `op('name')` is renamed for the scope it was written in: inside a Pack,
  `op('red_shift')` becomes `op('aberration1__red_shift')`. At any level, `op('pulse1')['level']`
  naming a Pack instance becomes its first SIG output's node.

Known limits: `me` in a promoted expression is the inner node it drives (so `me.digits` reads
that node's number). `op('instance').par.<promoted>` does not resolve (use a binding, or read the
inner node). Renaming an instance changes its inner flat names, which resets their cook state.

## Lock, unlock, edit

- `PackSet::edit(name, edits)` applies edits inside an unlocked Pack as one undoable step; it is
  refused for a locked Pack, and rolled back if the Pack would become invalid (for example,
  removing a node a promoted parameter drives).
- `PackSet::unlock(network, instance)` copies the locked Pack to `<name>_local<n>` (unlocked,
  `derived_from = "name@version"`) and returns the edits that switch that one instance to the
  copy. Other instances are unaffected.
- `PackSet::relock` returns an unlocked copy's instance to the Pack it came from, keeping its
  values by the update rule below.

## Update in place

`PackSet::update(new, base, networks)` replaces a Pack across the project: every instance in
every network and inside every other Pack. It returns a report:

```text
glow 1.0.0 → 1.1.0 (compatible)
  - parameter `seed`
  ~ parameter `amount`: float 0…5 → float 0…4
  + parameter `tint`
  main/glow2: `amount`: 4.5 clamped to 4.0
  main/glow2: `seed` = 3.0 dropped: the parameter was removed
```

The safe rule for each instance's values (`fit_instance`):

- a value the new parameter accepts is kept (a constant equal to the new default is dropped, so
  the instance follows the default);
- a number outside the new range is clamped;
- anything else is reset to the new default (the parameter disappeared or changed kind), and the
  old value is in the report so it can be restored;
- wires to sockets that disappeared or changed wire type are removed; bindings to the instance
  are removed if it has no SIG output any more.

If anything fails, nothing changes. A change of major version (or of minor for 0.x) is reported
as breaking. Undo is one project-level step: keep a copy of the Packs and networks before
updating.

## Make Pack

`make_pack(network, registry, selection, name, promote, instance)` turns selected nodes into a
new unlocked Pack (0.1.0) and the edits that replace them with one instance, as one undo step:

- wires from outside become inputs (one per outside source), wires to outside become outputs
  (one per inside source); with neither, the last selected node nothing inside reads becomes
  `out`;
- `promote` lists the parameters to promote; their current values become defaults;
- a selected parameter bound to a node outside, or with an expression naming one, is promoted
  automatically and the instance keeps the binding or expression;
- outside parameters bound to a selected SIG node follow the instance;
- outside expressions that named a selected node are reported as warnings.

Flattening the result gives the original graph back (a test checks this).

## Files

In a project, each Pack the project uses has its own copy:

```text
myshow.lasair/
  networks/main.toml       … type = "cmp.pack_glow", params.amount = 0.5 …
  packs/glow/pack.toml     interface and metadata
  packs/glow/network.toml  the network inside (the same form as networks/*.toml)
  packs/glow/help.md       the help page (only when there is help)
```

`pack.toml`:

```toml
# Lasair Pack. Its network is network.toml; its help page is help.md.
format = 1
name = "glow"
version = "1.2.0"
label = "Glow"
summary = "A soft glow around bright areas."
locked = true

[[param]]
name = "amount"
label = "Amount"
default = 0.8
min = 0.0
max = 1.0
help = "How strong the glow is."
to = [{ node = 1, param = "opacity" }]

[[input]]
name = "in"
to = [{ node = 0, input = "in" }, { node = 1, input = "a" }]

[[output]]
name = "out"
from = { node = 1, output = "out" }
```

Keys are written in this order; empty optional keys (`label`, `summary`, `min`, `max`, `help`,
`derived_from`) are left out; node references are stable IDs into `network.toml`. Changing a
default, a range, the version or the lock is a one-line diff, and so is changing an instance's
value in `networks/main.toml`. Hand-written equivalents are accepted (`min = 0`, `to` as one
table) and `lasair fmt` rewrites them canonically. Unknown keys are errors. Folders under
`packs/` without a `pack.toml` are skipped. Saving removes the files of a Pack no longer in the
project and keeps any other file in its folder.

**Libraries.** A folder of Pack folders (the repository's `packs/`, or your own) is a library:
`glow/`, plus `glow@1.0.0/` for an older version kept beside it. `Library::find("glow", ^1)`
returns the newest compatible version; a missing version is an error listing those available.

## Command line

```sh
lasair check examples/*.lasair      # projects: their Packs are checked, errors name the file
lasair check --gpu examples/*.lasair   # cooks the flattened network
lasair check packs/*/                 # Pack folders, checked with the rest of their library
lasair check --gpu packs/*/           # also cooks one instance of each with nothing connected
lasair fmt packs/glow my.lasair     # canonical form
lasair render my.lasair --out frames --node glow1        # a Pack instance's output
lasair render my.lasair --out frames --node glow1/blur1  # a node inside it
```

## In the app (so far)

Opening a project with Packs works: each instance is shown expanded (`glow1__blur1`) and cooks;
saving it is refused, because saving the expanded patch would replace the Packs with their
insides. Make Pack, the Palette, entering and leaving Packs, and update indicators are the next
step (`docs/milestones/m2/packs.md`).
