# 1. Install and first launch

> **Installers.** If you have an installer (the macOS DMG, the Windows setup `.exe` or the Linux
> AppImage), use it and skip to [Starting the app](#starting-the-app): it brings its own Python
> and, on Windows, the Microsoft Visual C++ runtime, so you install nothing else first. See
> [`docs/install.md`](../install.md), which also covers silent installs for computer labs.
> The rest of this page builds the app from its source code with Rust's tools. This takes a few
> steps, then it runs like any other program; the rest of this manual works the same either way.

## What you need

| You need | Why | Notes |
|---|---|---|
| A computer with a recent GPU | Atidarla draws on the GPU | Windows (DirectX 12) or macOS (Metal). Linux needs a Vulkan driver and is best-effort. |
| **Rust** (through `rustup`) | To build the app | The exact version is pinned in `rust-toolchain.toml`; `rustup` installs it for you the first time you build. |
| **Python 3.13** (3.11 or newer works) | Atidarla embeds Python for expressions, Python operators and the Textport | Only to build from source (the installers bundle their own). Must be on your `PATH`. |
| **uv** | Only for AI features | A Python package manager. See [uv](https://docs.astral.sh/uv/). |
| **Ollama** | Only for the Assistant and LLM Chat, if you want a local language model | Optional. See chapter 9. |

Sources: [`README.md`](../../README.md) (Quick start) and [`docs/HANDOFF.md`](../HANDOFF.md) (Local set-up).

## macOS

1. Install Xcode from the App Store, open it once, and let it finish installing its components.
   Atidarla builds its Syphon picture-sharing library from source, which needs Xcode with the
   Metal toolchain.
2. Install the tools with Homebrew:

   ```sh
   brew install rustup python@3.13 pkgconf ffmpeg
   ```

   `ffmpeg` is only needed if you want video formats other than HAP and still images (see below).
   The project picks its pinned Rust version by itself the first time you build.
3. Tell the build where Python is:

   ```sh
   export PYO3_PYTHON=$(brew --prefix python@3.13)/bin/python3.13
   ```

   Add that line to your shell start-up file (for example `~/.zshrc`) so you do not have to type
   it again.
4. Get the Atidarla source folder (from your lecturer, or by cloning the repository), open a
   terminal in it, and run:

   ```sh
   cargo run -p atd-app
   ```

The first build takes several minutes. Later starts are quick.

## Windows

1. Install Rust with [rustup](https://rustup.rs). Its installer tells you if the Microsoft C++
   build tools are missing and how to add them.
2. Install Python 3.13 and tick **Add python.exe to PATH** in its installer.
3. Get the Atidarla source folder, open a terminal in it, and run:

   ```sh
   cargo run -p atd-app
   ```

On Windows the app draws with DirectX 12 by default.

## Starting the app

```sh
cargo run -p atd-app                                  # opens the built-in demo patch
cargo run -p atd-app -- examples/stage-deck.atidarla  # opens a project folder
cargo run --release -p atd-app                        # same, built for speed
```

The demo patch is noise, a kaleidoscope and a level node, animated by two oscillators. You
should see a moving picture in the **Viewer** straight away.

The built program is called `atidarla-app`. Command-line tools are in a second program called
`atidarla` (chapter 13).

## What you see

The window has a **menu bar** (File, Edit, View) and dockable panels. The Patch tab (the node
canvas) is in the middle. The panels you use first are:

| Panel | What it does |
|---|---|
| **Operators** (left) | The list of node types, grouped by family. Click one to add a node. |
| **Patch** | The canvas where you place and wire nodes. |
| **Parameters** | The controls of the selected node. |
| **Viewer** | The picture of the selected node. |
| **Platform** | What the app found: your operating system, GPU and graphics backend. The same summary is on the right of the menu bar. |

Use **View** in the menu bar to bring any panel or tab back: Patch, Stage, Outputs, Timeline,
Viewer, Platform, Mappings, Models, AI Hosts, Learn, Diff, Merge, Assistant and Textport. Tabs can be
dragged to other places.

## Optional extras

**Video other than HAP.** Movie In plays HAP movies and PNG/JPEG stills in every build. Other
video (H.264, HEVC, ProRes, VP9 and more) needs a build with FFmpeg:
`cargo run -p atd-app --features ffmpeg`. FFmpeg development files must be installed (the
Homebrew `ffmpeg` and `pkgconf` above provide them on macOS). Bundling FFmpeg's libraries with
the app is not done yet.

**Shader packs.** To use ISF shaders, set `ATIDARLA_ISF_PATH` to one or more folders of `.fs`
files (`;` between folders on Windows, `:` elsewhere). Each shader that translates appears as an
`img.isf_*` operator.

**AI features.** AI nodes run in a separate program, the AI Host. The installers ship it and the
app starts it when needed; add the heavier adapters with `atidarla ai-host install <extra>`
([chapter 8](08-ai.md)). From a source checkout, set it up once:

```sh
cd ai-host
uv sync                                    # built-in test models only
uv sync --extra diffusion --extra vision   # picture diffusion and vision models
uv sync --extra stt                        # speech to text
```

Model files are not installed with Atidarla. You download them later, with a licence check
(chapter 8).

**Linux.** The app needs a Vulkan driver. On Debian or Ubuntu:
`apt install libxkbcommon-x11-0 mesa-vulkan-drivers libasound2-dev libudev-dev`.

## Check that it works

Run the project check on an example. It loads the project and cooks every node on the GPU:

```sh
cargo run -p atd-cli -- check --gpu examples/kaleidoscope-tunnel.atidarla
```

A line starting `ok` means the project loaded and every node cooked, for example
`ok    examples/kaleidoscope-tunnel.atidarla  10 nodes, 60 frames cooked in 1.0 s`. Problems are covered in
[chapter 14](14-troubleshooting.md).

Next: [your first patch](02-first-patch.md).
