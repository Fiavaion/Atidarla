# Batch Graph: wedges and the media bin

> FR-B1–B3, D8, M5 AC4. Code: `crates/lsr-format/src/{batch,media_bin}.rs` (files),
> `crates/lsr-ai/src/batch/` (cache keys, host placement, runner),
> `crates/lsr-cli/src/batch.rs` (offline engines, `lasair batch`),
> `crates/lsr-app/src/media_bin.rs` (Media Bin in the Stage tab). Example:
> [`examples/batch-wedge.lasair`](../examples/batch-wedge.lasair/).

A **wedge** sweeps parameters of a project: ten prompts × ten seeds through Diffusion, a
noise seed against a scale, LoRA or ControlNet weights, any parameter of any node. Each
combination is a **work item**: the main network with the item's values set, cooked offline
for one target node. Items run in parallel across AI Hosts (this machine, LAN machines,
cloud pods, ComfyUI servers), each result lands in the project's **media bin** with what
made it, and a re-run cooks only the items whose inputs changed.

## Wedge files

A wedge is project data: `batch/<name>.toml` (names use `a-z 0-9 _ -`).

```toml
format = 1
target = 3                    # the IMG node whose image each item keeps (ID in networks/main.toml)
frame = 0                     # the frame each item cooks, at frame / fps seconds
fps = 60.0
size = [512, 512]             # optional: the project resolution when absent
combine = "product"           # every combination; "zip": item i takes value i of every axis
scheduler = "lan:*"           # where items run, see below
concurrency = 2               # items in flight per host

[[axis]]
node = 3
param = "prompt1"
name = "prompt"               # optional attribute name; default <node name>.<param>
values = ["a red fox", "a blue whale"]

[[axis]]
node = 3
param = "seed"
seeds = { from = 1, count = 10 }

[[axis]]
node = 5
param = "lora_weight"
range = { from = 0.2, to = 0.8, steps = 4 }   # both ends included
```

- An axis is one parameter of one node of the main network: a list of `values` (any
  parameter value: text, numbers, colours), a numeric `range` in `steps`, or a `seeds` range.
  The value is set as a constant, replacing an expression or a binding on that parameter.
- `product` varies the last axis fastest; `zip` needs every axis the same length. A wedge
  makes at most 100 000 items.
- Nodes are referred to by ID, so renaming a node changes nothing. `lasair check` verifies
  wedges (canonical form, nodes and parameters exist); `lasair fmt` rewrites them.

## Running

```sh
lasair batch run <project> <wedge> [--jobs N] [--force] [--allow-python-ops]
lasair batch status <project> [wedge]
```

`run` prints a line per item (`item 37 cooked on lan:b in 212 ms`) and a summary
(`100 items: 100 cooked (lan:a 49, lan:b 51), 0 already in the bin, 0 failed, in 10.3 s`).
`--jobs` overrides the wedge's `concurrency`; `--force` cooks every item even if it is in
the bin. `status` shows each wedge's item count, how many are in the bin, and the AI
adapters its target needs, without cooking anything.

### What an item cooks

Each worker is an offline engine (the same as `lasair render`) on the machine's GPU; all
workers share one GPU device. For each item the worker loads the network with the item's
values, cooks frame F, then frame F + 1, and keeps frame F + 1: AI nodes answer one frame
behind offline (`docs/render.md`), so that is the AI result for frame F's input. Each item
starts from a fresh engine, so nothing carries over from the previous item. Workers never
run on the app's render thread.

### Schedulers (FR-B2)

| `scheduler` | Items run on |
|---|---|
| `"local"` (default) | the local AI Host |
| `"lan:*"` | every LAN host in `[[ai.hosts]]` that is ready and can run the target's AI nodes |
| `"lan:studio"`, `["lan:a", "lan:b"]` | these hosts, in this order |
| `"cloud:runpod"` | a cloud host (A59); refused in Sovereign mode and once the spend cap is reached |
| `"comfy:192.168.1.20:8188"` | a ComfyUI server: the target's ComfyUI workflow nodes run there |

Hosts are asked to start (or connect) and get 15 s to become ready. A host is used when it
has every adapter the target's AI nodes need (`diffusion.sd` for Diffusion, `onnx` for the
vision nodes, `video.v2v` for Video Model, the chosen adapter for AI Model); the others are
named in a warning with the reason. Each host gets `concurrency` workers, and each worker's
AI nodes have their `route` pinned to its host (ComfyUI nodes their `server`). Workers take
items from one queue as they finish, so equal hosts take turns and a faster host takes more.
A failed item goes back on the queue for any worker, up to three tries; a worker whose host
fails three items in a row stops, and when no worker is left the remaining items fail with
the reason. A target without AI or ComfyUI nodes cooks on this machine whatever the
scheduler says.

A batch run has a supervisor of its own (its hosts are those of the project, with your
tokens from the per-user `hosts.toml`), so it does not disturb the app's hosts.

## Cache (FR-B3)

Each item has a **cache key**: a SHA-256 over the target's upstream subgraph after the
item's values are set — every upstream node's type, inputs and channel bindings, every
parameter's value (stored or default; expressions and bindings as written), the checksums
of pinned models (`[[models]]` in `project.toml`) the nodes name — and the frame, frame rate
and output size. Node names, canvas positions and placement parameters (an AI node's
`route`, `in_flight`, `budget_ms`; a ComfyUI node's `server`) are not part of it: where an
item runs does not change what it is.

An item whose image `media/bin/<key>.png` exists is not cooked again. So a re-run with
nothing changed cooks nothing, and changing one prompt of a 10 × 10 wedge cooks exactly its
ten items. An item another wedge already made is listed under this wedge too, without
cooking. Items of one run with the same key cook once.

Not covered by the key: the contents of files nodes read (movies, images, project shaders,
kernels, ComfyUI workflow files), expressions that read other nodes by name, and model
weights not pinned in `project.toml`. Use `--force` after changing those.

## The media bin

```text
media/bin/<key>.png      one image per item (RGBA8 PNG)
media/bin/index.toml     one entry per wedge and item
```

```toml
[[item]]
hash = "3f0c…"           # the cache key (64 hex digits): the image is media/bin/<hash>.png
wedge = "prompts"
index = 37               # the item's position in its wedge
host = "lan:b"
size = [128, 128]
created = 2026-10-30T12:00:00Z
cook_ms = 212.4
attributes = { prompt = "a cathedral made of coral", "diffusion1.seed" = 8 }
```

Entries are sorted by wedge, index and key, attributes by name, so the file diffs well. The
index is written after every item: an interrupted run keeps what it made, and the next run
cooks the rest. Results are never removed automatically; when a wedge changes, its earlier
results stay in the bin under their old values.

### In the app

The Stage tab shows the **Media Bin** under the clip grid: thumbnails (loaded on a
background thread, which also re-reads the index when a run adds to it), the item's
metadata on hover, a wedge filter, and an attribute filter (`prompt=fox`, `seed=7`).
Select a result and press **Use as clip**, or drag its thumbnail onto an empty cell of the
grid: a Movie In node (`img.movie_in`) reading `media/bin/<key>.png` is added and wired into
that slot (the first free slot of the top layer for the button), as one undo step.

## Hand check: AC4 with two reference hosts

```sh
cargo run -p lsr-ai --bin lsr-ai-refhost -- --listen 127.0.0.1:7421 &
cargo run -p lsr-ai --bin lsr-ai-refhost -- --listen 127.0.0.1:7422 &
cargo run -p lsr-cli -- batch run examples/batch-wedge.lasair prompts
cargo run -p lsr-cli -- batch run examples/batch-wedge.lasair prompts   # 0 cooked
```

The automated version is `a_100_item_wedge_runs_across_two_lan_hosts_into_the_media_bin`
in `crates/lsr-cli/src/batch.rs`. The example's results are not committed (`media/bin/` is
in the example's `.gitignore`).
