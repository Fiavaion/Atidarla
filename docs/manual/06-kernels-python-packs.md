# 6. Kernels, WGSL, Python operators and Packs

Lasair's built-in operators cover a lot. When you need something they do not do, you can
write your own, or reuse a patch someone else made. This chapter is for when you are
comfortable with chapters 1 to 5. You do not need any of it to make your first shows.

| You want to... | Use |
|---|---|
| Move or colour many points, or every pixel, with a short program | A **Kernel** (Kernel DSL) |
| Write a picture shader by hand | A **WGSL** node |
| Run a shader that keeps state or iterates | A **Compute** node |
| Write a new SIG or IMG operator in Python | A **Python operator** |
| Run a short Python script on text or tables | A **Script** node |
| Try Python commands against the open project | The **Textport** |
| Reuse a patch as one node | A **Pack** |

## Kernels (Kernel DSL)

A **kernel** is a short program that runs once for every point or once for every pixel, on the
GPU. You write it in the Kernel DSL, a small, strict language made for this. It compiles to a
shader that runs the same on Windows and macOS.

The Learn quest [Write a Kernel](../../learn/quests/04-kernel-dsl.toml) takes about 20 minutes.
Full language reference: [`docs/kernel-dsl.md`](../kernel-dsl.md). It starts with a ten-line
curl-noise particle kernel.

There are two ways to use one:

- A **Kernel** node: [`pnt.kernel`](../operators/pnt/kernel.md) (a Points kernel, runs per point)
  or [`img.kernel`](../operators/img/kernel.md) (a Pixels kernel, runs per pixel). Type the code
  in the node, or point it at a file. A file reloads whenever you save it. The last version that
  compiled keeps running while you edit, and the node shows the line and column of any error.
- A **project kernel operator**. Put a file in your project's `kernels/` folder
  (`name.atk` for points, `name.pixels.atk` for pixels). It becomes an operator `kernel.name` in
  the Operators panel under **Project**. Its `ch()` lines become sliders and fields. Changing a
  slider never recompiles.

Edits reload live and **keep the simulation going**: a particle system continues from its
current points when you save a new kernel.

Check a kernel from the command line without opening the app:

```sh
lasair kernel particles.atk                  # errors, bindings and parameters
lasair kernel particles.atk --emit-wgsl      # print the generated shader
lasair kernel grade.atk --target pixels
```

Examples: [`kernel-particles`](../../examples/kernel-particles.lasair/),
[`kernel-pixels`](../../examples/kernel-pixels.lasair/).

## WGSL and Compute nodes

- [**WGSL**](../operators/img/wgsl.md) runs a full-screen shader you write in WGSL (the
  shading language of the web's WebGPU). You write the body of `fn shade(uv: vec2f) -> vec4f`.
  Inputs, a sampler, four parameters and `time()` are provided. A file reloads when saved; the
  last working shader keeps running while you edit.
- [**Compute**](../operators/img/compute.md) runs a compute shader, once per output pixel. Use
  it when the shader needs to iterate, keep images from frame to frame, or use a scratch buffer.
  `#bind` lines declare inputs, state images, parameters and buffers. Reference:
  [`docs/compute.md`](../compute.md). Example:
  [`compute-shaders`](../../examples/compute-shaders.lasair/).
- **ISF shaders.** If you have a folder of ISF `.fs` shaders, set `LASAIR_ISF_PATH` and each
  one that translates appears as an operator `img.isf_*`. The command
  `lasair isf-import <dir> --report` translates a folder and tells you how many worked.

Shader code is shared with others as files in the project folder, so it travels with the project.

## Python operators

A project can define its own SIG and IMG operators in Python: one `.py` file in the project's
`ops/` folder. You declare the parameters, inputs and state; Lasair builds the operator, its
parameter panel and its place in the Operators panel (under **Project**, as `py.<name>`). Edits
hot-reload and keep state. Reference with a worked example:
[`docs/python-ops.md`](../python-ops.md). Example project:
[`python-ops`](../../examples/python-ops.lasair/).

### Trust: Python operators are programs

**A Python operator can do anything your user account can do**: read and write files, use the
network, start programs. Lasair does not sandbox it. So:

- Opening a project **never runs** its `ops/` code. Its nodes load and show "Python operators in
  this project are not allowed to run".
- The app asks you, listing the files. Choose **Allow and remember**, **Allow this time** or
  **Don't run**. (The File menu also has **Allow Python Operators...**.)
- "Remember" stores a fingerprint of the files on your computer, not in the project. Any change
  to the files asks again.
- Only allow code from people you trust. This matters when you open a project from the internet
  or a classmate.
- On the command line, `check`, `render`, `bake` and `batch run` run Python operators only with
  `--allow-python-ops`.

Python **Script** nodes ([`dat.script`](../operators/dat/script.md)) are project Python too and
follow the same rules. Python *expressions* are not yet gated (chapter 5).

## The Textport

**View, Textport** opens a Python console at the bottom of the window. Type a line of Python
and press Enter. It can read the open project (nodes, parameter values, the frame and time) and
change parameters. Each change is undoable like any other. Your show keeps running while you
type. Reference: [`docs/textport.md`](../textport.md).

## Packs: reusable patches

A **Pack** is a network saved as one reusable node. It has a name, a version number, help text,
a few **promoted parameters** (the controls you want to show) and input and output sockets. A
Pack instance is a node of type `cmp.pack_<name>`. Wires, bindings, expressions and undo work on
it like on any other node.

A starter library is in [`packs/`](../../packs/README.md): Audio Pulse, Beat Flash, Chromatic
Aberration, Colour Grade, Curl Swarm, Duotone, Feedback Trails, Film Grain, Kaleidoscope Tunnel,
LFO Bank, Neon Edges, RGB Delay, Spark Fountain, Spinning Solid and Vignette. Each has an
example project, for example [`pack-colour-grade`](../../examples/pack-colour-grade.lasair/).
The Learn quest [Use a Pack](../../learn/quests/09-packs.toml) takes about 12 minutes.

What works today, from [`docs/packs.md`](../packs.md) and [`docs/learn.md`](../learn.md):

- Projects that use Packs **open and play** in the app. The Packs appear expanded into their
  inner nodes.
- **Saving a project that uses Packs from the app is refused for now.** Saving the expanded
  patch would replace the Packs with their insides. Make Pack, the Palette, entering and leaving
  Packs and update indicators in the app are not built yet.
- The assistant can stage a new Pack from selected nodes (`create_pack`, chapter 9).
- A Pack is locked in a library. A project can unlock a copy and edit that.
- You can check Packs from the command line: `lasair check packs/*/` and `lasair fmt`.

Each project carries its own copy of every Pack it uses, so a shared project needs nothing
installed. Updating a Pack to a newer version is an explicit step that reports what changed.

## Neural CA

The [Neural CA](../operators/img/nca.md) node grows patterns from a tiny neural network on every
pixel. The built-in rules are hand-built, not trained. A weights file in the project supplies
another network. Reference: [`docs/nca.md`](../nca.md). Example:
[`neural-growth`](../../examples/neural-growth.lasair/).
