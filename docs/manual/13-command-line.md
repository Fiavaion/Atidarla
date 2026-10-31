# 13. Command line reference

Atidarla has two programs:

- `atidarla-app` is the window you have been using. `cargo run -p atd-app` starts it.
- `atidarla` is the command-line tool for work that needs no window: offline renders,
  checks, show export, servers and more. `cargo run -p atd-cli -- <command>` runs it when you
  build from source. The examples below write `atidarla <command>`; use the `cargo` form
  if you have not installed it.

This chapter was written from the program's own help text (`atidarla --help` and
`atidarla <command> --help`, version 0.0.1). Run those yourself for the latest.

A **project** is a folder ending in `.atidarla`. A `git:<rev>` can stand in for a project in
`diff` and `merge` (chapter 11).

## At a glance

| Command | What it does |
|---|---|
| [`render`](#render) | Render a project offline to PNG frames or a movie |
| [`run`](#run) | Run the app under a supervisor that restarts it |
| [`serve`](#serve) | Run a project without a window and offer it over MCP, HTTP and OSC |
| [`bench`](#bench) | Run the performance suite |
| [`check`](#check) | Check that projects load, are valid and are in canonical form |
| [`bake`](#bake) | Save one node's output: meshes or images |
| [`batch`](#batch) | Run batch wedges into the media bin |
| [`agent`](#agent) | Build a request into a project with the assistant, or run its evaluation |
| [`points`](#points) | Convert and inspect point caches |
| [`docs`](#docs) | Generate the operator reference pages |
| [`export-show`, `verify-show`](#export-show-and-verify-show) | Make and check a portable show bundle |
| [`diff`, `merge`, `merge-file`](#diff-merge-and-merge-file) | Compare and combine projects |
| [`fmt`](#fmt) | Rewrite projects in canonical form |
| [`models`](#models) | The AI model manager |
| [`comfy`](#comfy) | Import a ComfyUI workflow |
| [`cloud`](#cloud) | Start and stop a Runpod AI Host |
| [`kernel`](#kernel) | Check a Kernel DSL file |
| [`isf-import`](#isf-import) | Translate ISF shaders to WGSL |
| [`registry`](#registry) | Print the operator registry as JSON |

## render

Deterministic offline render of a project to numbered PNG files or a movie. Frame *i* is cooked
at *i/fps* seconds, with a fixed step and no wall clock, so two renders of the same project
on the same computer are identical. Live inputs (audio devices, MIDI, OSC) open nothing and sit idle, and outputs such as DMX and Share Out send nothing. The render warns about each. Reference:
[`docs/render.md`](../render.md).

```sh
atidarla render <PROJECT> --out <OUT> [options]
atidarla render examples/audio-reactive-feedback.atidarla --out renders/arf --range 0-300
```

| Option | Meaning |
|---|---|
| `--out <OUT>` | A folder for `frame_000000.png`, ... (created if missing), or a `.mov` or `.mp4` file |
| `--range <A-B>` | Frames A (inclusive) to B (exclusive). Default `0-60` |
| `--fps <N>` | Frames per second. Default 60 |
| `--fixed-step` | Accepted for compatibility. The step is always fixed |
| `--node <NAME>` | The node to render. Default: the last IMG node nothing reads from |
| `--codec <C>` | For movies: `prores` (default for `.mov`), `prores-hq`, `prores4444` (with alpha), `h264` (default for `.mp4`), `h265` |
| `--bitrate <BPS>` | H.264 and H.265 bit rate, bits per second |
| `--allow-python-ops` | Run the project's Python operators (they are code; chapter 6) |

Movie files need a build with FFmpeg. The Timeline drives a render: cues and tracks are applied
at each frame's time. Stage autopilot, one-shot returns and flash releases do not run offline.

## run

The supervisor for shows (chapter 10). Starts `atidarla-app`, restarts it after a crash or a
freeze, and stops it on Ctrl-C.

```sh
atidarla run <PROJECT> [--kiosk] [options]
```

| Option | Meaning |
|---|---|
| `--kiosk` | Full screen, only the Viewer, no cursor |
| `--max-restarts <N>` | Restarts in a row before giving up. Default 10 (0 never restarts) |
| `--restart-delay <S>` | Seconds before the first restart, doubling to a limit of 60. Default 1 |
| `--heartbeat-timeout <S>` | Seconds without a heartbeat before a restart. Default 15 (0 turns it off) |
| `--startup-grace <S>` | Seconds allowed to the first heartbeat. Default 90 |

The app is looked up as `atidarla-app` beside `atidarla`, or at the path in the environment
variable `ATD_APP`. See [`docs/run.md`](../run.md).

## serve

Runs a project without a window at 60 frames a second and offers it to other programs
(chapter 9). The project folder is created empty if it does not exist.

```sh
atidarla serve <PROJECT> [--mcp-stdio] [--http 127.0.0.1:7421] [--osc 9000] [options]
```

| Option | Meaning |
|---|---|
| `--mcp-stdio` | Speak MCP on stdin and stdout (log messages go to stderr) |
| `--http <ADDR>` | MCP and a REST interface over HTTP, on a loopback address such as `127.0.0.1:7421` |
| `--osc <PORT>` | Receive OSC on a UDP port, `/atd/<node>/<param> <value>` |
| `--auto-accept` | Let the agent accept its own staged changes |
| `--scope <SCOPE>` | Extra scopes, for example `python.exec` |
| `--no-save` | Do not write accepted changes to the project folder |
| `--seconds <S>` | Stop after this many seconds (for tests) |

## bench

Runs the performance suite and writes JSON.

```sh
atidarla bench [--out <FILE>] [--no-gpu]
```

`--out` writes the results to a file instead of the screen. `--no-gpu` skips cases that need a
GPU. For meaningful numbers build with `--release`.

## check

Loads each project, validates it against the operator registry, and checks it is in canonical
form. With `--gpu` it also cooks every node.

```sh
atidarla check <PROJECTS>... [--gpu] [--frames <N>] [--allow-python-ops]
atidarla check examples/*.atidarla
atidarla check --gpu --frames 120 myshow.atidarla
```

`--frames` is how many frames `--gpu` cooks (default 60). Python operators are listed as "not
run" unless `--allow-python-ops` is given. It also checks Pack folders (`atidarla check
packs/*/`) and batch wedges, and lists nodes that this platform cannot run.

## bake

Saves one node's output to disk with the offline engine. An IMG node gives PNG files. A mesh
(MSH) node gives mesh files.

```sh
atidarla bake <PROJECT> [--node <NAME>] [--out <DIR>] [--range A-B] [--fps <N>] [--format <F>] [--sequence] [--allow-python-ops]
```

| Option | Meaning |
|---|---|
| `--node <NAME>` | Default: the last mesh node nothing reads from, else the last IMG node |
| `--out <DIR>` | Folder to write into. Default `bake` |
| `--range A-B` | Frames, end exclusive. Default `0-1` |
| `--format <F>` | Mesh format: `obj`, `ply` (default), `gltf` or `glb` |
| `--sequence` | One file per frame even if every frame is the same |

A static mesh gives one `mesh.ply`; an animated one gives `mesh_000012.ply` and so on. Only
meshes from the MSH family can be baked; point (PNT) nodes live on the GPU and give an error.

## batch

```sh
atidarla batch run <PROJECT> <WEDGE> [--jobs <N>] [--force] [--allow-python-ops]
atidarla batch status <PROJECT> [WEDGE]
```

`run` cooks every work item not already in the media bin. `status` shows each wedge's items, how
many are already in the bin, and where they would run. See chapter 12.

## agent

```sh
atidarla agent ask <PROJECT> "<REQUEST>" [--provider ollama|anthropic] [--model <M>] [--max-steps <N>] [--allow-internet]
atidarla agent eval [--prompts <FILE>] [--out <FILE>] [--only <TEXT>] [--provider ...] [--model ...]
```

`ask` builds your request into the project and **accepts and saves** the changes. The default
provider is `ollama` (local) with model `qwen2.5-coder:7b`. `anthropic` is opt-in, needs
`ANTHROPIC_API_KEY`, and is refused in Sovereign mode unless you add `--allow-internet`.
`--max-steps` is the number of model turns (default 40). `eval` runs the assistant's test
prompts and writes a report.

## points

```sh
atidarla points convert <INPUT> <OUTPUT> [--range A-B] [--scale <X>]
atidarla points info <FILE>
```

`convert` reads `.atdpc`, `.ply`, `.csv`, `.xyz`, `.pts` or `.txt` and writes `.atdpc`, `.ply`
or `.csv`, chosen by the output name. For a numbered sequence put `$F` or `$F4` in both names
and give `--range` (inclusive). `--scale` multiplies every position (use 0.01 to convert
centimetres to metres). `info` prints a file's point count and attributes. Reference:
[`docs/point-caches.md`](../point-caches.md).

## docs

Generates the operator reference pages in `docs/operators` from the registry.

```sh
atidarla docs [--out <DIR>] [--examples <DIR>] [--check]
```

`--check` makes it fail if the saved pages differ from what the program would generate. Use it
to find out whether the pages are up to date. It does not look at this manual.

## export-show and verify-show

```sh
atidarla export-show <PROJECT> <OUT> [--zip] [--no-gpu]
atidarla verify-show <BUNDLE> [--gpu]
```

See chapter 10 and [`docs/show-export.md`](../show-export.md).

## diff, merge and merge-file

```sh
atidarla diff <A> <B> [--project <PROJECT>]
atidarla merge <BASE> <OURS> <THEIRS> [--out <DIR>] [--project <PROJECT>]
atidarla merge-file <BASE> <OURS> <THEIRS> [PATH]
```

`merge` exits with 0 for a clean merge, 1 for conflicts and 2 for an error. `merge-file` is for
Git's merge driver. See chapter 11.

## fmt

```sh
atidarla fmt <PROJECTS>...
```

Rewrites project folders in canonical form. Use it after editing project files by hand.

## models

```sh
atidarla models list
atidarla models accept <ID> [--yes]
atidarla models download <ID> [--allow-internet]
atidarla models verify <ID>
atidarla models path [ID]
```

See chapter 8. `accept` shows the licence and asks you to type `accept`. `--yes` skips the
question for scripts.

## comfy

```sh
atidarla comfy import <WORKFLOW.json> --project <PROJECT> [--server host:port] [--name <NAME>]
```

Imports a ComfyUI workflow saved in API format as an operator `comfy.<name>`. Default server
`127.0.0.1:8188`. See chapter 8.

## cloud

```sh
atidarla cloud runpod up --name <NAME> --image <IMAGE> [--gpu <TYPE>] [--cloud-type SECURE|COMMUNITY] [--disk-gb <N>] [--secrets <FILE>]
atidarla cloud runpod status --name <NAME>
atidarla cloud runpod down --name <NAME> [--terminate]
```

`up` creates (or resumes) a pod that runs the AI Host container and waits until it is
reachable. **The pod bills by the hour until you run `down`.** `down` stops it; `--terminate`
deletes the pod and its disk. Default GPU `NVIDIA GeForce RTX 4090`, default disk 50 GB. See
[`docs/cloud.md`](../cloud.md).

## kernel

```sh
atidarla kernel <FILE.atk> [--target points|pixels] [--emit-wgsl]
```

Compiles a Kernel DSL file and prints its errors, bindings and parameters, or the generated
shader with `--emit-wgsl`. `--target` defaults to `pixels` for `*.pixels.atk` and `points` for
other files.

## isf-import

```sh
atidarla isf-import <DIR> [--report] [--gpu] [--json <FILE>] [--out <DIR>] [--min-pass <PERCENT>] [--compile-timeout <S>] [--thumbs <PNG>]
```

Translates ISF shaders (`.fs` files, searched recursively) to WGSL. `--report` lists every
failure by kind. `--gpu` also builds each shader and cooks it for two frames.

## registry

```sh
atidarla registry
```

Prints every operator's schema and capability flags as JSON.

## Exit codes and messages

Commands exit with 0 on success and non-zero on failure. Warnings go to the error stream. The
`render` command prints the graphics adapter and backend it used.
