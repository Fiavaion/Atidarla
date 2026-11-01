# 9. The assistant and agents

Lasair can build and change a patch from a written request. It does this through a fixed set of
**typed tools**: the same tools for the in-app assistant, for outside programs, and for the
command line. There is no way for an agent to run arbitrary code unless you grant it
explicitly.

Full reference: [`docs/agent.md`](../agent.md).

## The Assistant panel

Open **View, Assistant**. Type what you want, for example *a kaleidoscope of moving noise*, and
press **Build**.

1. A model reads Lasair's operator catalogue and uses the tools to build the patch.
2. It works on a **copy** of your patch. Nothing reaches your show yet.
3. When it finishes, the panel lists the **staged changes**.
4. Press **Accept** to apply them as **one undo step**, or **Reject** to drop them.

Other buttons:

- **Explain network**, or **Explain *node name*** when a node is selected: the model describes
  what your patch does.
- **Repair N node error(s)**: appears when nodes have errors, and asks the model to fix them.
- **Stop** ends a run in progress.

The **Model** menu picks where the model runs. The default is **Ollama on your computer**
(`qwen2.5-coder:7b`). To use it, install Ollama and run `ollama pull qwen2.5-coder:7b`.
Anthropic's cloud models are available only if you choose them and provide your own key, and
Sovereign mode refuses them ([chapter 8](08-ai.md#cloud-and-sovereign-mode)).

The model never waits on the picture and the picture never waits on the model. The same request
from a terminal:

```sh
lasair agent ask show.lasair "a kaleidoscope of moving noise"
```

## In-show cues

A performer can type a short instruction during a show. In the Assistant panel open **In-show
cues**, type for example `darker, slower on the next drop`, and press **Cue**.

- A cue can only move **numeric parameters you have put on the allow-list** (the Show Lock
  list below). It cannot add or remove anything. With no allow-list it does nothing and says so.
- The words come from a fixed list: darker or brighter, slower or faster, calmer or wilder,
  warmer or cooler, bigger or smaller, `more` or `less` followed by a parameter name, `set
  <param> to <number>`, and `<node>.<param> at <number>%`. A relative step is a quarter of the
  range (`a bit`: a tenth, `much`: a half).
- The move is **on the clock**. By default it happens on the next bar. `now` means the next
  beat. `next drop` means the next 16 beats and `next phrase` the next 32.
- Parameters driven by an expression or a binding are left alone.
- **Cancel queued** drops cues that have not fired.

## Show Lock

Show Lock stops accidental or unwanted changes while you perform. Put a file called `agent.toml`
in the project folder:

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

While the lock is engaged:

- every change to the graph is refused, including expressions and bindings;
- only the listed parameters can move, and only inside their ranges;
- the assistant's changes cannot be accepted (the panel says why).

Each allowed move is one undo step.

## Connecting other programs: `lasair serve`

`lasair serve` runs your project **without a window**, at 60 frames a second, and offers it to
other programs:

```sh
lasair serve show.lasair --mcp-stdio                 # MCP on stdin and stdout
lasair serve show.lasair --http 127.0.0.1:7421       # HTTP, on this computer only
lasair serve show.lasair --osc 9000                  # /lsr/<node>/<param> <value>
```

**MCP** (Model Context Protocol) is the standard way for AI programs, such as editors and coding
assistants, to use tools. Lasair's tools include `list_ops`, `get_schema`, `get_network`,
`create_node`, `connect`, `set_param`, `review_changes`, `accept_changes`, `screenshot_viewer`,
`write_kernel`, `stage_launch`, `stage_crossfader` and `cue`. Each tool needs a **scope**
(`edit`, `accept`, `files`, `stage`, or `python.exec`). The agent's changes are staged and
reviewed first, and the agent accepts its own changes only if you start the server with
`--auto-accept`. `python_exec` exists only if you grant `--scope python.exec`.

To add Lasair to Claude Code:

```sh
claude mcp add lasair -- lasair serve /path/to/show.lasair --mcp-stdio --auto-accept
```

The HTTP server listens on the loopback address only. Use `--no-save` to keep an agent's
changes in memory only.

**Safety.** Only run `--auto-accept` on projects you can afford to change, and keep a copy
(or use Git, [chapter 11](11-collaboration.md)).
