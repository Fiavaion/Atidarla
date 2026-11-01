# Show export (`lasair export-show`, FR-S9)

A show bundle is a project folder that carries everything it plays: every file its nodes
read, wherever it lived, copied inside it with the parameters pointing there by relative
path. Copy the folder (or the zip) to another machine, on another OS, and open it: no path
needs fixing (M3 AC2).

## Usage

```sh
lasair export-show shows/opening.lasair dist/ --zip
# dist/opening.lasair/      the bundle (a project folder)
# dist/opening.lasair.zip   the same, zipped, if --zip

lasair verify-show dist/opening.lasair --gpu     # on the receiving machine
```

| Option | Meaning |
|---|---|
| `<project>` | The project folder. |
| `<out>` | Folder the bundle is written into, as `<out>/<project folder name>`. It must be outside the project; the bundle folder must not exist yet (or be empty). |
| `--zip` | Also write `<out>/<project folder name>.zip`, holding the bundle under its folder name. Files are in sorted order with a fixed timestamp, so the same bundle gives the same archive; movies, images and other compressed media are stored, text is deflated. Large files use Zip64. |
| `--no-gpu` | Verify the bundle without cooking it (for machines without a GPU adapter). |

## What export does

1. Loads and validates the project (as `lasair check`): networks, Packs, shaders, Python
   operator declarations.
2. Finds every **path parameter** set on a node, in every network and inside every Pack the
   project holds: Movie In, Audio File In, WGSL and Compute files, Kernel files, Text fonts,
   Neural CA weights, Mesh File In, Pixel Map patches; any operator whose parameter is of
   kind `path` (the registry says which, so new operators are covered without changes here).
3. Resolves each like the engine does (relative to the project folder; Windows backslashes
   count as separators):
   - **inside the project**: kept, rewritten with `/` separators if needed;
   - **outside the project**: copied to `media/<file name>`. The same file named by several
     nodes is copied once; a different file with a name already taken (in `media/` of the
     project, or by another copied file) becomes `name-2.ext`, `name-3.ext`, ... A `.gltf`
     file goes to `media/<stem>/` with the buffers and images it loads, keeping their
     relative layout;
   - **missing**: the export stops and lists every missing file with its node and parameter;
   - a **folder** where a file is expected stops the export.

   Mesh File Out's `file` names a file the node *writes*: it is not copied; a path outside
   the project is rewritten to `out/<file name>` in the bundle.
4. Copies the project folder (without `.cache/`, which is rebuilt) and the external files,
   then saves the project with the rewritten parameters in canonical form (rule 8). Packs
   are already folders of the project, so they travel with it; so do `shaders/`, `ops/`,
   `fixtures/`, patch files and anything else in the folder.
5. Writes `bundle.toml` (below) and verifies the bundle as `verify-show` does, with `--gpu`
   unless `--no-gpu`.

The original project is never modified.

## `bundle.toml`

Text, deterministic (sorted, no timestamps), at the bundle's root:

```toml
format = 1
name = "Audio-reactive feedback"
lasair = "0.0.1"
exported_on = "macos-aarch64"

[[file]]                       # every file in the bundle, sorted
path = "media/beat.wav"
sha256 = "2ca6cc22…"
bytes = 176444
external = true                # copied in from outside the project

[[node]]                       # nodes that need a platform capability (PRD A19)
in = "networks/main.toml"
node = "share_out1"
type = "img.share_out"
needs = "win|mac"
fallback = "hide"
explanation = "Texture sharing uses Spout on Windows and Syphon on macOS; …"

[models]
required = []                  # the model manifest, filled from M4
```

`[[node]]` tells the receiving machine in advance what will not run there and what stands
in for it; `lasair check` and the app report the same when the bundle is opened (node
"not available on this platform", with the fallback's explanation). `[models]` is a
placeholder: from M4, AI nodes list the models they need (name, source, licence), and the
model manager fetches them on open. **Model weights are never put in a bundle** (rule 4).

## `verify-show`

`lasair verify-show <bundle> [--gpu]` checks a bundle where it is, as the receiving
machine needs it:

- every path parameter is relative, uses `/`, has no `..` or drive letter, and names a file
  that exists in the bundle **with exactly that case** in every folder and file name (a
  project made on Windows or macOS, whose file systems ignore case, would otherwise break on
  a case-sensitive one);
- every file is listed in `bundle.toml` with a matching SHA-256, and every listed file is
  there (a copy that lost or changed a file is caught);
- `lasair check` passes, with `--gpu` cooking every node for 30 frames; platform-flagged
  nodes are reported, not failed.

## Not covered

- Paths written **inside** files rather than in parameters: `#bind` lines of a Compute
  shader, `#include`s, Python code under `ops/` that opens files. Keep those files in the
  project folder (relative to it), and they travel with it.
- Pack **libraries** outside the project: the project already holds a copy of each Pack it
  uses (`docs/packs.md`), so nothing outside is needed.
- Models (M4) and fonts the system provides (a Text node with no font file uses the
  built-in font everywhere).

## Windows → macOS check (M3 AC2)

CI-portable tests (`cargo test -p lsr-cli export`) export a project whose nodes name files
by absolute path, move the bundle elsewhere, delete the originals and verify it, and check
that tampering is caught. The cross-OS run needs two machines, or CI with an artifact
hand-over:

1. On the Windows PC: `lasair export-show <project> D:\dist --zip`.
2. Copy `D:\dist\<project>.zip` to the Mac (USB, network share, cloud drive) and unzip it.
3. On the Mac: `lasair verify-show <project folder> --gpu`, then open it in the app
   (`cargo run -p lsr-app -- <project folder>`) and play it. Nodes flagged for Windows only
   are listed by `verify-show` with their substitutes.
4. Optional, for byte-level parity of a stateless project: `lasair render` the same range
   on both machines. Frames are only promised identical on the same machine and adapter
   (`docs/render.md`), so compare by eye or with a tolerance across OSes.

Evidence on this machine (2026-10-30, Apple M5 Pro, Metal): the audio-reactive feedback
example with its WAV moved outside the project and named by absolute path exports with
`--zip` (4 files, 1 external), verifies with `--gpu`, and renders frames 0–19 byte-identical
to the original project.
