# 12. Batch Graph and the media bin

Sometimes you want many versions of one picture: ten prompts times ten seeds through a diffusion
model, or one noise pattern at five scales. **Batch Graph** does that. It cooks each combination
once, saves the results as images, and lets you drop the best ones into the Stage as clips.
Full reference: [`docs/batch.md`](../batch.md).

## The ideas

- A **wedge** is a list of parameters to vary. It lives in the project as a text file
  `batch/<name>.toml`.
- Each combination of values is a **work item**. The item is the main network with those values
  set, cooked once for one chosen node.
- Items run in parallel on one or more AI Hosts (this computer, other computers on your
  network, cloud pods or ComfyUI servers).
- Results go to the **media bin**: `media/bin/`. The bin remembers what made each image.
- Run again and only the items whose inputs changed are cooked. The others are skipped.

## Writing a wedge

A small wedge file:

```toml
format = 1
target = 3                    # the IMG node whose picture each item keeps
frame = 0
fps = 60.0
combine = "product"           # every combination; "zip" pairs values in order
scheduler = "local"           # where items run
concurrency = 2               # items in flight per host

[[axis]]
node = 3
param = "prompt1"
name = "prompt"
values = ["a red fox", "a blue whale"]

[[axis]]
node = 3
param = "seed"
seeds = { from = 1, count = 10 }
```

Nodes are named by their ID number (the IDs in `networks/main.toml`), so renaming a node does not
break a wedge. An axis can be a list of `values`, a numeric `range` with `steps`, or a `seeds`
range. A wedge may make at most 100 000 items. Where items run is the `scheduler`: `"local"`,
`"lan:*"` (all ready LAN hosts), `"lan:studio"`, `"cloud:runpod"` (refused in Sovereign mode and
once the spend cap is reached) or `"comfy:host:port"`.

An example to copy from: [`batch-wedge`](../../examples/batch-wedge.atidarla/). It sweeps a
noise seed against a scale through the built-in test model, and has a 10 prompts times 10 seeds
wedge for two hosts.

## Running a wedge

```sh
atidarla batch status myshow.atidarla                 # what would run, and where
atidarla batch run myshow.atidarla prompts            # cook what is missing
atidarla batch run myshow.atidarla prompts --force    # cook everything again
atidarla batch run myshow.atidarla prompts --jobs 4   # more items in flight
```

`run` prints a line for each item and a summary such as `100 items: 100 cooked, 0 already in the
bin, 0 failed`. An interrupted run keeps what it made; the next run does the rest.

Changing one prompt of a 10 times 10 wedge cooks only its ten items. Changes the cache cannot
see, such as an edited movie file or shader file, need `--force`.

`atidarla check` verifies wedges, and `atidarla fmt` rewrites them in canonical form.

## The media bin in the app

Open the **Stage** tab. Under the clip grid is the **Media Bin**. It shows thumbnails of the
results. Hover to see what made one. Use the wedge filter, or the attribute filter (for example
`prompt=fox` or `seed=7`).

Select a result and press **Use as clip**, or drag its thumbnail onto an empty cell of the grid.
A Movie In node that reads the image is added and wired into that slot, as one undo step.

Results are never deleted for you. When a wedge changes, earlier results stay in the bin.
