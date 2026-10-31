# 14. Troubleshooting and FAQ

Where to look first:

- **A node has a message under it** on the canvas. Read it. Most problems are explained there.
- **The menu bar** shows messages from the last action, and a red **GPU error** if the graphics
  card reported a problem. Click either to dismiss it. Hover for the full text.
- **The Platform tab** shows your operating system, GPU and graphics backend.
- **`atidarla check --gpu <project>`** loads a project and cooks every node outside the app.
  It names what fails.

## Building and starting

**The build fails mentioning Python.** (Building from source only: an installed app brings its
own Python and ignores the one on your machine.) The app embeds Python and needs to find it. Install
Python 3.13 and make sure it is on your `PATH`. On macOS also set
`PYO3_PYTHON=$(brew --prefix python@3.13)/bin/python3.13` ([chapter 1](01-install.md)).

**The build fails on macOS about Metal or Syphon.** Atidarla builds the Syphon library from
source and needs Xcode with its Metal toolchain installed. Open Xcode once and let it finish.

**The first build is very slow.** That is normal. Later builds only rebuild what changed.

**Linux: the app does not start.** It needs a Vulkan driver and the X11 or Wayland libraries at
run time. See [chapter 1](01-install.md#optional-extras).

**The window opens but the picture is black.** Select a node whose output is a picture. The
Viewer shows the selected node. If the node has an error, it is shown under the node.

## The patch

**I cannot connect two nodes.** A wire only joins sockets of the same type
([chapter 4](04-operator-families.md#wire-types)). Start the drag on an output (right side) and
end on an input (left side). A wire that would make a loop is refused: use a Feedback node
for loops.

**I made a mistake.** Press Cmd/Ctrl+Z. Edits are undoable, including Stage launches, merges
and the assistant's accepted changes. Live values from the Timeline and MIDI/OSC are not edits
and are not in the undo history.

**A parameter will not change when I edit it.** It may be driven by something else. A binding,
an expression, a Timeline track or a mapping can override the stored value. A live value from a
track or mapping wins until it is released. **Stop** on the Timeline clears its values.

**An expression shows an error.** Read the message at the top of the Parameters panel. While it
fails, the parameter uses its stored value. Common causes: a typo in a node name inside
`op('...')`, a node that was renamed, or dividing by zero ([chapter 5](05-expressions-bindings.md)).

**A Pack project will not save.** Saving projects that use Packs from the app is not built yet.
It is refused so that the Packs are not replaced by their insides ([chapter 6](06-kernels-python-packs.md)).

**The project says it has conflicts and will not open.** It was written by a merge with
unresolved conflicts. Open it with **View, Merge, Open conflicted**
([chapter 11](11-collaboration.md)).

**A node says "not available on this platform".** The operator needs a capability your computer
lacks (for example a Windows-only sensor SDK or an NVIDIA GPU). The node's page lists its
fallback under **Platforms**.

## Performance

**The picture is slow or stutters.** Try, in this order: lower the project resolution
(`resolution` in `project.toml`) or a node's size; turn off heavy effects such as Fluid, Bloom
and large Blurs; check the AI Hosts window for hosts that are restarting; build with
`--release` (`cargo run --release -p atd-app`). `atidarla bench` measures the standard cases.

**An AI node is slow or delayed.** Model results are asynchronous. The Governor fills the gaps
(`hold`, `blend`, `flow` or `passthrough`). Real-time diffusion runs well on a Windows PC with
an NVIDIA GPU. On other computers route the node to such a computer (`lan:<name>`)
([chapter 8](08-ai.md)).

**Several windows are open on macOS and the picture stalls.** The project notes list an
intermittent repaint stall on macOS when output windows are open. It is not fixed yet
([`docs/milestones/M3.md`](../milestones/M3.md)).

## Sound, control and lights

**No sound input.** Audio In uses the default input device. Pick your microphone in its device
menu. The `test-tone` source is a steady tone. Speech to Text will never hear a pause in it.
Your operating system may also be blocking microphone access for the app.

**MIDI controller does nothing.** Open **View, Mappings** and look at the input monitor to see
whether messages arrive. Use **rescan** if you plugged it in after starting. On Windows a MIDI
input can be open in only one place at a time, so a MIDI In node on the same device may fail
after a mapping opens it.

**OSC does not arrive.** The default OSC port is 9000. Check the Mappings window shows the port
you expect, that the sender uses the right computer address, and that your firewall allows it.

**DMX lights do not respond.** Check the node for an error (a patch file error empties the
channels). Test with an Art-Net or sACN monitor on the network, or send to Host `127.0.0.1`.
Remember the lights trail the picture by one or two frames.

**Share In shows black.** It is black while there is no sender. Check the sender's name. Syphon
needs the Metal backend and Spout needs DirectX 12 (the defaults).

**NDI does not work.** NDI is not built yet ([chapter 7](07-live-io.md#sharing-pictures-with-other-software)).

## Python

**A node says "Python operators in this project are not allowed to run".** That is the safety
gate. Choose **File, Allow Python Operators...** if you trust the project
([chapter 6](06-kernels-python-packs.md#trust-python-operators-are-programs)). On the
command line add `--allow-python-ops`.

## AI

**An AI node reports no host or a missing model.** Open **View, AI Hosts** to see whether a host
is ready and **View, Models** to see whether the model is downloaded. Install the host's extras
([chapter 8](08-ai.md)): `diffusion` and `vision` for pictures, `stt` for speech
(`atidarla ai-host install <extra> --allow-internet` in an installed app).
`atidarla ai-host check` starts the local host and prints the error if it cannot start;
`atidarla ai-host status` shows which host command runs. An `ATD_AI_HOST_CMD` left set in your
environment replaces the bundled host: unset it.

**`ai-host install` fails.** Sovereign mode refuses the download without `--allow-internet`. Behind
a proxy or without internet, use `--wheelhouse <folder>` ([`docs/install.md`](../install.md#optional-ai-adapters)).
A failed install leaves the extras you had. `atidarla ai-host remove` starts over.

**A model will not download.** Sovereign mode blocks the internet. Use **Download once
anyway** in the Models window, or `atidarla models download <id> --allow-internet`. You must
also accept the model's licence first.

**The Assistant or LLM Chat says it cannot reach a model.** The default is Ollama on your
computer. Install Ollama, start it and run `ollama pull qwen2.5-coder:7b`.

**A cloud host is skipped.** Sovereign mode is on, or the spend cap is $0 or has been reached.
See the badge in the menu bar and the AI Hosts window.

**A ComfyUI node says the server is not reachable.** Start your ComfyUI server. The node still
loads and cooks without it. Check the `server` parameter (default `127.0.0.1:8188`).

## FAQ

**Do I need to know how to code?** No. Chapters 1 to 5 need no code. Kernels, WGSL and Python
(chapter 6) are optional.

**Is it free?** Yes. Atidarla is open source under the MIT or Apache-2.0 licence, at your option.

**Does it collect data about me?** No telemetry is added. AI features only connect to the
internet if you turn Sovereign mode off or choose to download a model.

**Does it work on Windows and on macOS?** Yes. Features land on both together. A few features
belong to one platform (Spout on Windows, Syphon on macOS, real-time local diffusion on a
Windows PC with an NVIDIA GPU) and each has a fallback. Linux is best-effort.

**Where are AI models stored, and are they in my project?** In a per-user folder, never in the
project or in an exported show.

**Can I use my own shaders?** Yes: WGSL, Compute, the Kernel DSL and ISF shaders
([chapter 6](06-kernels-python-packs.md)).

**Can I make a movie?** Yes: `atidarla render ... --out clip.mov` (needs a build with FFmpeg),
or **Record** in the Viewer.

**Will my project work in a later version?** Project files are text, with a version number in
them, and changes to the format are tested for round trips. Keep your projects in Git.

**Why does an expression that names a node stop working when I rename the node?** Expressions
use node names. See [chapter 5](05-expressions-bindings.md).

**Where do I get help?** Ask your lecturer first. When you report a problem, include the output of
`atidarla check --gpu <project>` and the summary shown in the Platform tab.

**Is there a user manual in the app?** Learn mode (**View, Learn**) has step-by-step quests that
check your progress ([`docs/learn.md`](../learn.md)).
