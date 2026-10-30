# Learn mode

> FR-L1. Code: `crates/atd-app/src/learn/`. Quests: [`learn/quests/`](../learn/quests/).

Learn mode teaches Atidarla with **quests**: short, ordered lists of steps. A student follows the
steps in the app, and each step **checks itself** against the live project, so the quest always
knows what has been done and never needs a "next" button.

Open it from **View, Learn**. The panel lists the quests with their progress; choosing one shows
its steps. The current step is open, with its text and, if the author wrote one, a **hint** behind
a checkbox. Finished steps collapse to a tick, later steps stay greyed. **Start in a new project**
asks for an empty project (you are asked first if you have unsaved work); a quest that starts from
an example project shows **Open the example project** instead.

## The shipped quests

| Quest | Steps | Minutes | Teaches |
|---|---|---|---|
| Your first patch | 8 | 12 | Nodes, wires, parameters, the viewer, binding a parameter to a signal |
| Make a picture follow sound | 10 | 20 | Audio file, spectrum, a feedback loop, binding to a band |
| Your first clip deck | 7 | 15 | Mixer, layers, clips, launching from the Stage, transitions |
| Write a Kernel | 8 | 20 | The Kernel DSL for points and for pixels |
| A fountain of particles | 8 | 15 | The GPU particle system and its parameters |
| An AI clip on the deck | 8 | 20 | Prompt Clips, Diffusion with Prompts From, the built-in test model |
| Control it with MIDI or OSC | 5 | 12 | Learn mappings, OSC In, mappings versus bindings |
| Drive lights over DMX | 9 | 20 | DMX Out, Art-Net, Pixel Map and a patch file |
| Use a Pack | 6 | 12 | A project built from a Pack |
| A ComfyUI workflow as a node | 5 | 12 | A workflow as an operator, Auto Run |

## Progress

Progress is per user, not per project, in `learn.toml` in the app's config folder
(`~/Library/Application Support/Atidarla` on macOS, `%APPDATA%\Atidarla` on Windows,
`$XDG_CONFIG_HOME/atidarla` or `~/.config/atidarla` elsewhere):

```toml
format = 1

[steps]
"01-first-patch" = 8
"02-audio-reactive" = 3
```

The number is how many steps are done. Steps are done **in order**: the quest looks at the first
step that is not done, and when its check holds it moves on and tries the next one at once (so
a student who did two steps ahead of the text is not asked to do them again). A step that is done
stays done if the patch changes later. **Restart quest** forgets the count. A missing or damaged
file starts progress afresh.

## Writing a quest

A quest is one TOML file in `learn/quests/`, named `<nn>-<slug>.toml`. The number orders the
list. Add the file to `SHIPPED` in `crates/atd-app/src/learn/quest.rs` (it is embedded in the
program with `include_str!`; a test fails if the folder and the list disagree).

```toml
title = "Your first patch"          # required
summary = "One or two sentences."   # required
minutes = 12                        # 1 to 240
prerequisites = ["first-patch"]     # slugs of earlier quests (advice, not a lock)
start = "examples/pack-beat-flash.atidarla"   # optional example project to open

[[step]]
title = "Add a Noise node"
text = '''
Markdown (see below).
'''
hint = "Optional; shown when the student asks."
check = { kind = "node_exists", type = "img.noise" }
```

Unknown keys, a missing field or an unknown check kind make the file fail validation. Text
supports paragraphs, `- ` and `1. ` lists, `**bold**`, `` `code` `` and fenced code blocks. Use
only characters the app's default font has (plain letters and punctuation); the panel draws its
own tick and arrow marks. Quest text never names other products or their operator families
(CLAUDE.md rule 10); a test searches for them.

Operator type ids and parameter names are checked against the real registry by a test, so a
renamed parameter breaks the quest at build time, not in a student's lap. A test also plays
every shipped quest with a scripted student.

## Check kinds

A check is a pure function of a read-only **snapshot**: the network, the operator registry, each
node's cook count and error, the Stage model, the project's mappings, the Packs the project uses
and the project folder. Checks run at most 2.5 times a second while a quest is open, only for the
current step, and allocate nothing that grows with the frame rate.

In `type` and the `from_type` / `to_type` of `wired`, a type id is an exact operator
(`img.noise`). A project's own operators may be named by prefix: `comfy.*`, `kernel.*`, `py.*`,
`cmp.pack_*`.

| Kind | Keys | True when |
|---|---|---|
| `node_exists` | `type` | a node of that type exists |
| `wired` | `from_type`, `to_type`, optional `input` | a wire runs from a node of one type to a node of the other, into socket `input` if given |
| `param` | `type`, `name`, `op`, `value` | some node of that type has a parameter that passes `op` (below) |
| `cooks_without_error` | `type` | nodes of that type exist, at least one has cooked, none reports an error |
| `stage_layer_playing` | none | a layer of a Stage deck is playing a slot that holds a clip |
| `mapping_exists` | `param` | a MIDI/OSC mapping drives a parameter of that name on a node that exists |
| `kernel_compiles` | none | there is a kernel and every one compiles (inline code is compiled with the Kernel DSL compiler; kernels from files must have cooked without error) |
| `pack_instance` | `name` | the open project uses the Pack `name` |
| `file_exists` | `path` | a file exists at that path inside the project folder (a plain relative path; no `..`) |

`param` operations:

| `op` | `value` | True when |
|---|---|---|
| `gt`, `lt` | a number | the parameter's value is greater / less than it |
| `eq` | a number, string or boolean | the value equals it (6 equals 6.0) |
| `changed` | none | the parameter was set to something other than its default |
| `bound` | none | the parameter follows a SIG channel |
| `expr` | none | the parameter is driven by an expression |
| `contains` | a string | a text parameter contains it |

`cooks_without_error` needs the node to cook, and nodes cook when something shows or uses them:
tell the student to select the node (the Viewer cooks it) or to bind a parameter to it.

### Adding a check kind

Add a variant to `Check` in `learn/check.rs`, parse it in `Check::parse` (it lists the keys it
accepts, and any other key is an error), evaluate it in `Check::eval`, validate its names in
`Quest::check_against`, and add positive and negative tests next to the others in
`learn/tests.rs`. If it reads something the snapshot does not hold yet, add that to `Snapshot`
and fill it in `LearnPanel::frame`.

## Notes and limits

- A project that uses Packs opens expanded in the app and cannot be saved yet
  (`PACKS_READ_ONLY`), so the Packs quest explores a shipped example. Instances are recognised
  from the expanded node names (`flash1__clock`) and the Packs the project carries
  (`learn::pack_instances`).
- The ComfyUI quest works without a server: the node's status says it cannot reach one, which
  is not a node error.
- The AI quest's last step uses the built-in test model (`builtin.invert`) so it works without
  SD-Turbo; the Diffusion node itself needs SD-Turbo from the Models window.
- "Explain this network" belongs to the assistant, not to Learn mode.
