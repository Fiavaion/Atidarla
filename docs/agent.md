# Agents: MCP, the assistant and Show Lock

Lasair offers its patch to agents through **typed tools** (FR-G1). Any MCP client (Claude Code,
other editors and agents), the in-app assistant and the command line all use the same tools and
the same rules. There is no arbitrary code execution: `python_exec` exists only in a session the
user granted `python.exec` (CLAUDE.md rule 9). The same scope is needed to write any other Python:
an expression outside the native subset (a Python-tier expression, PRD A22: lists, strings,
Python built-ins, …) or a Script node's `code`. Without it `set_param`, `create_node` and
`build_network` refuse them and say so, and `create_pack` refuses to promote a Script's `code`
(which would let the instance's parameter write Python); native expressions
(`0.5 + 0.5 * sin(time)`, `op('lfo1')['lfo']`) are always allowed. Even with the scope, Python an
agent writes runs only once it is on the allow-list ([`python-ops.md`](python-ops.md)): in the app
you allow it in the Allow dialog; headless, `serve --allow-python-ops` with a `python.exec`
session allows it as it is written (there is no dialog).

## Tools

| Tool | What it does | Scope |
|---|---|---|
| `list_ops`, `get_schema` | the operator catalogue and one operator's parameters, inputs, outputs, capability flags | – |
| `get_network`, `get_errors` | the network as the agent sees it (staged changes included), node errors after a cook | – |
| `create_node`, `delete_node`, `connect`, `disconnect`, `set_param` | stage graph changes (expressions and channel bindings included) | `edit` |
| `create_pack` | stage turning nodes into a new Pack (A40: promoted parameters, sockets from the boundary wires, semver, help) and swapping in one instance; accepted with the rest as one undo step | `edit` |
| `review_changes`, `discard_changes` | the staged changes as a list; drop them | – |
| `accept_changes` | apply the staged changes as **one undo step**, cook, return errors | `accept` |
| `screenshot_viewer`, `run_bench` | a PNG of an image node; per-node cook times against their budgets | – |
| `write_kernel` | write `kernels/<name>.atk` (becomes `kernel.<name>`); compile errors come back | `files` |
| `stage_list`, `stage_launch`, `stage_crossfader` | the Stage decks; launch a clip; move the crossfader | `stage` |
| `cue` | an in-show cue: words to whitelisted parameter moves, quantised to the clock (see Cues) | `stage` |
| `python_exec` | run Python in the project's interpreter | `python.exec` |

**Staging (FR-G2).** Graph changes are made on a preview of the network. `review_changes` lists
them; nothing reaches the show until they are accepted, by a person in the app, or by the agent
itself only when the session has `accept` (`lasair serve --auto-accept`, `lasair agent`).
A staged `create_pack` also carries the new Pack: a host accepting by hand takes both with
`Agent::take_staged_with_packs` and applies them with `Workspace::apply_with_packs`, one undo step.

## Show Lock (FR-G3)

`agent.toml` in the project:

```toml
[show_lock]
engaged = true
stage = false          # launches and the crossfader

[[show_lock.allow]]
node = "level1"
param = "brightness"
min = 0.2
max = 1.0

[[show_lock.allow]]
pack = "colour-grade"  # any promoted parameter of its instances
```

While engaged, every graph edit is refused, expressions and bindings cannot be set, and only
whitelisted parameters move, within their ranges, at once (each move one undo step). The red-team
test is `crates/lsr-agent/tests/show_lock.rs`.

## Cues (FR-G5)

`cue` turns a performer's words into parameter moves: `darker, slower on the next drop`, `a bit
bigger now`, `set brightness to 0.4`, `level1.opacity at 50%`. Code: `crates/lsr-agent/src/cues.rs`.

- **Only the whitelist moves.** A cue moves numeric parameters on the Show Lock allow-list
  (`[[show_lock.allow]]`), whether the lock is engaged or not, clamped to the narrower of the
  parameter's declared range and the entry's `min`/`max`. Parameters driven by an expression or
  a binding are left alone. A move is a node, a parameter and a number, so a cue can never be a
  graph edit. With no allow-list, cues do nothing and say so.
- **Lexicon first.** darker/brighter (brightness, exposure, gain, intensity, level, opacity),
  slower/faster (speed, rate, tempo, frequency), calmer/wilder (amount, strength, intensity,
  speed …), warmer/cooler (warmth, temperature), bigger/smaller (scale, size, radius, zoom),
  `more`/`less <param>`, `set <param> to <n>` and `<node>.<param> at <n>%`. A relative step is a
  quarter of the range (`a bit`: a tenth, `much`: a half). Naming a node limits the words to it.
  Anything else (`delete everything`, unknown parameters) plans nothing and returns a note.
- **A model, optionally.** When the lexicon finds nothing, `CueContext::plan_with` asks an `Llm`
  for moves through one typed tool, `propose_moves`. Every proposal goes through the same check;
  the rest are dropped with a note. The `cue` tool itself uses the lexicon only, so it never waits
  on a model. A host that wants a model captures a `CueContext` (owned data), plans on a worker
  thread and pushes the moves onto `Agent::cues`.
- **On the clock.** In `lsr_stage::beat_position` beats, a cue fires on the next boundary strictly
  after now: `now` the next beat, by default the next bar (4), `next drop` 16, `next phrase` 32
  (beat 5.3 → 8.0 for a bar). `quantum` overrides the words. A host calls
  `Agent::fire_cues(ws, beat)` as the clock advances; each due move is checked again against the
  show and the lock as they are then, and applied as one undo step (`Cue: level1.brightness =
  0.6`). A newer cue for the same parameter replaces a pending one. `lasair serve` fires cues
  every frame from the first deck's clock (seconds at 120 BPM without one).

**Why the `stage` scope.** Cues are performance moves, like launching a clip. Their own scope
would add a permission with nothing new to protect, because the allow-list already bounds them.
They are not refused by `stage = false` in `agent.toml`, which is about launches and the
crossfader. The red-team test is `crates/lsr-agent/tests/cues.rs`.

## `lasair serve` (FR-H3)

```sh
lasair serve show.lasair --mcp-stdio                 # MCP on stdin/stdout
lasair serve show.lasair --http 127.0.0.1:7421       # POST /mcp, REST POST /api/tools/<tool>
lasair serve show.lasair --osc 9000                  # /lsr/<node>/<param> <value>
```

The project is cooked headless at 60 fps (outputs, sharing and DMX run as in the app). HTTP
listens on loopback only. `--auto-accept` lets the agent accept its own changes; `--scope
python.exec` grants Python; `--no-save` keeps changes in memory. A missing project folder is
created empty. OSC (`--osc`, UDP from any host on the network, unauthenticated) is a session of its
own with the `edit` scope only (plus `accept` with `--auto-accept`, which then applies each
message at once; under Show Lock whitelisted moves apply at once anyway): it never has
`python.exec`, whatever `--scope` grants the MCP session, so it cannot write Python.

### Claude Code

```json
{"mcpServers": {"lasair": {"type": "stdio", "command": "lasair",
  "args": ["serve", "/path/to/show.lasair", "--mcp-stdio", "--auto-accept"]}}}
```

`claude mcp add lasair -- lasair serve /path/to/show.lasair --mcp-stdio --auto-accept` does
the same. Evidence that it works: `docs/milestones/m5/claude-code-mcp.md`.

## The assistant (FR-G4)

Prompt to network: the model reads the operator catalogue (generated from the registry, the same
corpus as `docs/operators`), calls the tools, accepts, reads the errors and repairs. Default model:
**qwen2.5-coder 7B through Ollama** on this machine (A8). Anthropic is opt-in and goes through the
egress gate, so Sovereign mode refuses it.

```sh
ollama pull qwen2.5-coder:7b
lasair agent ask show.lasair "a kaleidoscope of moving noise"
lasair agent eval --out docs/milestones/m5/eval.md      # M5 AC1
lasair agent eval --provider anthropic --model claude-sonnet-5-5 --allow-internet
```
