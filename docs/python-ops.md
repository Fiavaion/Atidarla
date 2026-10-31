# Python custom operators

A project can define its own SIG and IMG operators in Python: one `.py` file per group of
operators in the project's `ops/` folder. Each operator declares its parameters, inputs and
state; Atidarla derives the operator's schema from the declaration, so the operator appears in
the palette (under **Project**), gets a parameter panel like any built-in operator, saves in
the project file and is checked by `atidarla check`, with no UI code of its own. PRD: §9 M2
("Python custom ops with schema-derived UI"), FR-K4 (state-preserving hot reload), §5.2
`atd-py`. Status and evidence: `docs/milestones/m2/python-ops.md`.

**Python operators run arbitrary code with your user account's privileges.** A project's
Python operators do not run until you allow them (see [Trust](#trust-running-code-from-a-project)).

## Writing an operator

```python
# myshow.atidarla/ops/motion.py
import atidarla as atd

@atd.op(
    family="SIG",
    label="Counter",
    summary="Counts up by Step every frame.",
    params=[
        atd.Float("step", default=1.0, min=0.0, max=10.0, help="Added every frame."),
        atd.Toggle("hold", help="Stops counting while on."),
    ],
    state={"count": 0.0},
)
def counter(ctx):
    if not ctx.par.hold:
        ctx.state.count += ctx.par.step
    return {"count": ctx.state.count}
```

The node type is `py.<file>.<function>` (here `py.motion.counter`): that is what the project
file stores, so renaming the file or the function renames the operator type.

### The declaration (`@atd.op(...)`)

| Argument | Meaning |
|---|---|
| `family` | `"SIG"` (outputs channels) or `"IMG"` (outputs an image). Required. |
| `label`, `summary` | Palette label (default: the function name) and one-line description. |
| `params` | A list of parameters, below. |
| `inputs` | A list of `atd.Input("name")`: SIG inputs (channels). Default: none. |
| `state` | A dict of state fields and their initial values (see [State](#state-and-hot-reload)). |
| `size` | IMG only: the output size in pixels, `(width, height)`. Default `(64, 64)`. |
| `timeout` | Seconds one cook may take before it is stopped and reported. Default 2, at most 60. |

| Parameter | Panel widget |
|---|---|
| `atd.Float(name, label=None, default=0.0, min=None, max=None, help="")` | Number (dragged, kept within the limits) |
| `atd.Int(name, label=None, default=0, min=None, max=None, help="")` | Integer (dragged, kept within the limits) |
| `atd.Toggle(name, label=None, default=False, help="")` | Check box |
| `atd.Menu(name, label=None, options=("a", "b"), default=None, help="")` | Drop-down (default: first option) |
| `atd.String(name, label=None, default="", help="")` | Text |
| `atd.Colour(name, label=None, default=(1, 1, 1, 1), help="")` | Colour picker (RGBA) |
| `atd.Vec(name, label=None, default=(0.0, 0.0), help="")` | 1 to 4 numbers |

The label defaults to the name with `_` as spaces and each word capitalised. Float, Int and
Toggle parameters can follow a channel or an expression like any other parameter.

**The declaration is read without running the file.** Atidarla parses the file with Python's
`ast` module and reads the decorator's arguments as literals: numbers, strings, `True`/`False`/
`None`, lists, tuples, dicts, and the `atd.Float(...)`-style calls above with literal arguments.
A variable or a computed value in the declaration is an error with its line number. This is
what lets a project show its operators, panels and wires before you decide whether to run its
code.

### Cooking (`ctx`)

The function is called once per cook with one argument, `ctx`:

| Name | Value |
|---|---|
| `ctx.time`, `ctx.dt`, `ctx.frame` | Seconds since start, seconds since the previous frame, frame number. |
| `ctx.par.<name>` | The parameter's current value: `float`, `int`, `bool`, `str`, or a tuple for Colour and Vec. Channel bindings and expressions are already applied. |
| `ctx.inputs["<name>"]` | A SIG input as a dict `{channel: value}` (latest sample); `{}` when unconnected. |
| `ctx.state` | The node's state (below). |
| `ctx.width`, `ctx.height`, `ctx.image()` | IMG only: the output size, and a new zeroed `bytearray` of `width * height * 4`. |

A SIG operator returns a dict `{channel name: number}`; the channels keep the dict's order.
An IMG operator returns its pixels as RGBA, 8 bits per channel, rows from the top: a
`bytes`/`bytearray` (or any object with the buffer protocol) of `width * height * 4` bytes, or
a sequence of that many floats from 0 to 1.

Python operators cook on every frame (like Python expressions, they are time-dependent).
The operator API offers nothing beyond `ctx`: no file, network or process helpers. Your code
can still import any module the embedded Python can import (see [Trust](#trust-running-code-from-a-project)).

**Numpy is optional.** Atidarla neither needs nor bundles it. If the Python that Atidarla
embeds has numpy installed, an operator may `import numpy` and return
`array.astype(numpy.uint8).tobytes()` (or the array itself: it has the buffer protocol).
Without numpy, pure Python is fast enough for small images (the 64×64 default) and for
channels; scale an image up with the Resolution operator.

**Not in this slice:** IMG inputs to a Python operator (reading images back from the GPU),
DAT/PNT/MSH families, Pulse and Path parameters, and imports between files in `ops/`.

## Threads, latency and offline renders

Python operators never run on the render thread (CLAUDE.md rule 1). They run on one Python
worker thread per project (`atd-py`, a sibling of the expression worker, so a stuck operator
does not stop expressions and the operators' worker can be restarted on its own).

- **Live** (the app, `atidarla check`): each cook hands the node's parameters and inputs to
  the worker and shows the newest result that has arrived, so an operator's output is a frame
  late (sometimes more under load). The render thread never waits, and nothing it does for a
  Python operator allocates once warm (tested).
- **Offline** (`atidarla render`): the cook waits for the operator's result for this frame, so
  a deterministic operator gives the same frames on every render (tested). The wait is
  bounded: a cook still running at its timeout is stopped as below, and the cook thread never
  waits longer than the timeout plus 3 s; the frame then fails with the node's error.
- **Errors** show on the node, with the line in your file: `ZeroDivisionError: division by
  zero (ops/motion.py, line 14, in counter)`. A bad return value is an error too.
- **Timeouts.** A cook that runs longer than its `timeout` is interrupted (a `TimeoutError`
  is raised inside it, so the node error names the line it was stuck on). Code that cannot be
  interrupted (a long `time.sleep`, a C call) gets one more second; then the worker thread is
  abandoned and a new one started: every Python operator's state starts afresh, and the stuck
  node reports what happened. The app never freezes; offline, the frame fails.

## State and hot reload

An operator keeps everything it needs between frames in `ctx.state`, declared up front:

```python
@atd.op(family="SIG", state={"phase": 0.0, "hits": 0})
def wobble(ctx): ...
```

Each node has its own state, created from the declared initial values. Only declared fields
exist (`ctx.state.other = 1` raises `AttributeError`), so the declaration is the state's schema.

**Hot reload (FR-K4).** While a project is open, saving a file in `ops/` reloads it: the
schema is read again (new parameters appear in the panel), the module is run again, and every
node keeps its state where the new declaration is compatible:

- a field keeps its value if the new declaration still has it **and** the old value has the
  same Python type as the new initial value (`float` and `int` are different types; so are
  `bool` and `int`);
- any other field starts from its new initial value (changing `"hits": 0` to `"hits": 0.0`
  resets it); fields no longer declared are dropped;
- the initial values themselves only matter for new nodes and reset fields, so editing an
  initial value does not reset a running node.

If the edited file does not load (a syntax error, a bad declaration, an exception at import),
its nodes show the error and stop cooking; their state is untouched and they continue from it
when the file is fixed. If the schema change removes a parameter a node has set, changes its
type so the stored value no longer fits, or removes an input that is wired, the reload also
drops that value or wire from the patch (one undo step) and says so.

Opening the project again, or a worker restart after a stuck operator, starts all state afresh.

## Trust: running code from a project

A Python operator is a program. A project folder someone sends you, or that you clone, can
contain any Python in `ops/`, and once it runs it can do whatever your user account can: read
and write your files, use the network, start programs. **Atidarla does not sandbox Python
operators.** The only protection is that a project's Python operators do not run until you
allow them:

- **Opening a project never runs its `ops/` code.** The declarations are read with `ast`
  (parsed, not executed), so the operators appear in the palette and the patch, their nodes
  and parameters load and save normally, and each such node shows "Python operators in this
  project are not allowed to run".
- **The app asks**, listing the files: *Allow and remember*, *Allow this time*, or *Don't run*.
  "Remember" stores a SHA-256 of the `ops/` files (names and contents, line endings
  normalised) in your user settings (`allowed-python-ops.txt` in the Atidarla settings folder:
  `%APPDATA%\Atidarla` on Windows, `~/Library/Application Support/Atidarla` on macOS,
  `$XDG_CONFIG_HOME/atidarla` elsewhere), never in the project. The next time a project with
  exactly those files opens, it runs without asking; any change to any file in `ops/` (or a
  new file) asks again.
- While a project is open and allowed, your own edits hot-reload without asking (you are the
  one editing). They do not update the remembered hash, so the next open asks again.
- **Headless:** `atidarla check`, `render`, `bake`, `batch`, `serve`, `soak` and `agent ask` /
  `agent eval` run a project's Python (operators, Script nodes and Python expressions) only with
  `--allow-python-ops` (for that run; nothing is remembered). Without it, `check` reports the
  nodes and expressions it did not run and still passes, `render` fails if the rendered image
  needs one, and `serve` cooks the show without them. `atidarla run --allow-python-ops` (an
  unattended show, which cannot answer the Allow dialog) passes the flag on to the app
  (`atidarla-app <project> --allow-python-ops`), which then allows the opened project's Python
  after every restart without asking.
- **Merge and paste:** merging into the open project (the Merge window) or pasting nodes from the
  clipboard can bring in Script nodes or Python expressions nobody allowed. When they do, the
  allowance is withdrawn and the dialog asks again, listing them; until then the network's
  Python stops (operators in `ops/` are not affected).
- **Agents** need the `python.exec` scope to write Python expressions or Script code
  ([`agent.md`](agent.md)).

**Threat model.** In scope: a project file from someone else running code when you open it.
Out of scope: anything that can already write to your disk (it can change Atidarla itself);
and a malicious file crashing the app while its declaration is parsed (deeply nested
expressions can exhaust Python's parser; files over 1 MB are refused). Files are only read from
`ops/` itself (not subfolders); `ops/` is not on Python's import path, so a stray `.pyc` or
module there is never imported.

**Script DATs** (`dat.script`, `crates/atd-py/src/script.rs`) are project Python too, written in a
node's `code` parameter rather than in `ops/`. They run on their own worker thread and only after an
explicit choice: the app's Allow dialog, which also opens for a project whose network has Script
nodes and lists them ("Allow this time" or "Allow and remember"; the remembered hash covers `ops/`
files, not network code, so it never allows scripts by itself and the dialog asks again on the
next open), or `--allow-python-ops` for the headless commands. Opening another project stops them
until it is allowed. Until then their nodes report the same "not allowed" error as Python
operators. Known limit: a script that never returns holds the Script worker (no timeout yet; the
show is not affected).

**Python expressions** (PRD A22: any parameter expression outside the native subset) are network
code too and follow the Script DAT rule exactly. The engine asks its Python host whether the
project's Python is allowed once per frame (`PythonHost::allowed`, the same process-wide flag as
Script DATs, `atd_py::script::allow`); while it is not, no expression job is offered to the
Python worker (which also refuses any it is handed), the parameter keeps its stored value, and
the node reports "Python expressions in this project are not allowed to run". Native-tier
expressions are evaluated in Rust and keep working. The Allow dialog opens for a project with
Python expressions and lists each one; "Allow and remember" does not cover them (the remembered
hash is of `ops/` files: hashing network text would make every edit of any parameter a new trust
decision, and a project that differs only in an expression would look trusted), so the dialog
asks again on the next open. Opening another project withdraws the allowance; a new project,
and the patch the app starts with, are your own, so their Python runs. While a project is
allowed, Python expressions you type evaluate as soon as you commit them; in one that is not,
the parameter panel says why they do not.

## Where it lives

`crates/atd-py/src/ops/`: `read.rs` (the `ast` reader, no execution), `host.rs` (worker,
supervisor, slots), `node.rs` (the engine side: SIG and IMG nodes), `trust.rs` (the remembered
hashes), `mod.rs` (`PythonOps`: registration, hot reload, `stale_edits`); the Python half is
`crates/atd-py/src/ops_host.py`. The registry knows these operators by their `py.` type IDs and
`python.custom` provenance (`Registry::replace_custom`, `Registry::provenance`). Tests:
`crates/atd-py/tests/ops.rs` and `ops_alloc.rs`, plus `check`/`render` tests in `atd-cli` and
the open/palette tests in `atd-app`.

## Commands

```sh
atidarla check --gpu --allow-python-ops examples/python-ops.atidarla
atidarla render examples/python-ops.atidarla --out renders/py --range 0-60 --allow-python-ops
```

`check` reports a missing file (`node counter1 uses py.motion.counter: ops/motion.py does not
exist`), a declaration error with its line, and, with `--gpu`, cook errors with theirs.

## Example

[`python-ops`](../examples/python-ops.atidarla/): a SIG counter, a SIG LFO with parameters and
an IMG pattern generator written in Python, driving built-in operators.
