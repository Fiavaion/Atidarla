# Agents: MCP, the assistant and Show Lock

Atidarla offers its patch to agents through **typed tools** (FR-G1). Any MCP client (Claude Code,
other editors and agents), the in-app assistant and the command line all use the same tools and
the same rules. There is no arbitrary code execution: `python_exec` exists only in a session the
user granted `python.exec` (CLAUDE.md rule 9).

## Tools

| Tool | What it does | Scope |
|---|---|---|
| `list_ops`, `get_schema` | the operator catalogue and one operator's parameters, inputs, outputs, capability flags | – |
| `get_network`, `get_errors` | the network as the agent sees it (staged changes included), node errors after a cook | – |
| `create_node`, `delete_node`, `connect`, `disconnect`, `set_param` | stage graph changes (expressions and channel bindings included) | `edit` |
| `review_changes`, `discard_changes` | the staged changes as a list; drop them | – |
| `accept_changes` | apply the staged changes as **one undo step**, cook, return errors | `accept` |
| `screenshot_viewer`, `run_bench` | a PNG of an image node; per-node cook times against their budgets | – |
| `write_kernel` | write `kernels/<name>.atk` (becomes `kernel.<name>`); compile errors come back | `files` |
| `stage_list`, `stage_launch`, `stage_crossfader` | the Stage decks; launch a clip; move the crossfader | `stage` |
| `python_exec` | run Python in the project's interpreter | `python.exec` |

**Staging (FR-G2).** Graph changes are made on a preview of the network. `review_changes` lists
them; nothing reaches the show until they are accepted, by a person in the app, or by the agent
itself only when the session has `accept` (`atidarla serve --auto-accept`, `atidarla agent`).

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
test is `crates/atd-agent/tests/show_lock.rs`.

## `atidarla serve` (FR-H3)

```sh
atidarla serve show.atidarla --mcp-stdio                 # MCP on stdin/stdout
atidarla serve show.atidarla --http 127.0.0.1:7421       # POST /mcp, REST POST /api/tools/<tool>
atidarla serve show.atidarla --osc 9000                  # /atd/<node>/<param> <value>
```

The project is cooked headless at 60 fps (outputs, sharing and DMX run as in the app). HTTP
listens on loopback only. `--auto-accept` lets the agent accept its own changes; `--scope
python.exec` grants Python; `--no-save` keeps changes in memory. A missing project folder is
created empty.

### Claude Code

```json
{"mcpServers": {"atidarla": {"type": "stdio", "command": "atidarla",
  "args": ["serve", "/path/to/show.atidarla", "--mcp-stdio", "--auto-accept"]}}}
```

`claude mcp add atidarla -- atidarla serve /path/to/show.atidarla --mcp-stdio --auto-accept` does
the same. Evidence that it works: `docs/milestones/m5/claude-code-mcp.md`.

## The assistant (FR-G4)

Prompt to network: the model reads the operator catalogue (generated from the registry, the same
corpus as `docs/operators`), calls the tools, accepts, reads the errors and repairs. Default model:
**qwen2.5-coder 7B through Ollama** on this machine (A8). Anthropic is opt-in and goes through the
egress gate, so Sovereign mode refuses it.

```sh
ollama pull qwen2.5-coder:7b
atidarla agent ask show.atidarla "a kaleidoscope of moving noise"
atidarla agent eval --out docs/milestones/m5/eval.md      # M5 AC1
atidarla agent eval --provider anthropic --model claude-sonnet-5-5 --allow-internet
```
