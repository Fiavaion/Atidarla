# Neural CA (`img.nca`)

A neural cellular automaton: the fourth 2D solver of PRD §9 M2, after Reaction-Diffusion,
Fluid 2D and Ripple. It follows the model described in Mordvintsev, Randazzo, Niklasson and
Levin, "Growing Neural Cellular Automata" (Distill, 2020), implemented from that description
(CLAUDE.md rule 5); no code or weights come from it.

## The model

The grid has one cell per Cell Size × Cell Size output pixels. Each cell holds `channels`
numbers: 0-2 are colour (premultiplied by alpha), 3 is alpha, 4 and up are hidden. One step:

1. **Perceive.** For every channel: the cell's own value and three fixed 3×3 filters, so
   `4 × channels` numbers, in this order per channel:

   | Filter | Kernel (rows top to bottom) |
   |---|---|
   | identity | the cell |
   | `sobel_x` | `[-1 0 1; -2 0 2; -1 0 1] / 8` |
   | `sobel_y` | `[-1 -2 -1; 0 0 0; 1 2 1] / 8` (+y is down) |
   | `laplacian` | `[1 2 1; 2 -12 2; 1 2 1] / 16` |

   Outside the grid is zero (Edges `zero`) or the opposite edge (`wrap`).
2. **Update.** `ds = W2 · relu(W1 · p + b1) + b2`. The cell adds `ds` only if it *fires* this
   step: a hash of the cell, the step number and Random Seed (PCG) is below Fire Rate. The hash
   is integer arithmetic, so the same cells fire on every run and every GPU.
3. **Live.** A cell is kept only if some cell of its 3×3 neighbourhood had alpha above Alive
   Threshold before the step, and one has after it; other cells are set to zero. Where the
   `erase` input's luminance × alpha is above 0.5, cells are set to zero too (regeneration).

The output shows the colour channels over Background (Display `rgba`), or hidden channels 4-6
as RGB with 0.5 as zero (`hidden`). Smooth interpolates between cells.

**Seeds** (on the first cook, on Reset, and when the grid size or channel count changes):
`centre` (one cell with alpha and hidden channels 1, colour 0, as in the paper), `scatter` (such
cells at random, each cell with probability Density), `noise` (every channel uniform in 0-1) or
`image` (the `seed` input: colour × alpha, alpha, hidden channels 1 where alpha is above the
threshold).

## Built-in rules: hand-built, not trained

Rule 4 forbids shipping weights from elsewhere, so the built-in rules are written by hand, unit
by unit, by `GrowRule` in `crates/atd-ops-img/src/nca.rs` (8 channels, 20 hidden units):

- alpha grows into neighbours (a unit on the Laplacian of alpha), rises once a cell is alive and
  is pulled back above 0.9, so the organism spreads from any live cell and saturates;
- hidden channels 4 (`u`) and 5 (`v`) follow piecewise-linear activator-inhibitor kinetics with
  diffusion through the Laplacian (a Turing pattern) inside the live region, and `u` is kicked
  where the growth front passes;
- colour relaxes towards `base × alpha + hi × relu(u) + lo × relu(−u)`.

`grow` forms a labyrinth; `spots` feeds the inhibitor so the pattern breaks into spots; the
example's `coral.json` is a third set. Any cut (the `erase` input) regrows from its edges,
because growth needs only live neighbours.

## Weights files

Rule `file` reads Weights File, a JSON file in the project (a path relative to the project
folder). It is read and parsed on its own thread and reloads when saved; offline it is read once.
A file that fails to load or parse is reported on the node, which keeps its last image; the next
good save carries on from the state the last good weights left.

```json
{
  "format": "atidarla-nca",
  "version": 1,
  "name": "coral",
  "licence": "MIT OR Apache-2.0",
  "source": "where the weights come from",
  "channels": 8,
  "hidden": 20,
  "perception": ["identity", "sobel_x", "sobel_y", "laplacian"],
  "w1": [[...], ...],
  "b1": [...],
  "w2": [[...], ...],
  "b2": [...]
}
```

- `channels` is 4 to 16 and `hidden` 1 to 128. With `p = 4 × channels`: `w1` is `hidden` rows of
  `p` numbers (row `j`, column `4c + k` weighs filter `k` of channel `c`), `b1` is `hidden`
  numbers, `w2` is `channels` rows of `hidden`, `b2` is `channels`. `w1` and `w2` may also be
  flat arrays.
- `perception` must be exactly those four filters (the only ones implemented); it is there so a
  file says what it assumes.
- `licence` and `source` are not read by the operator but should say where the weights come from.
  A model trained elsewhere goes through the model manager with its licence (rule 4), not into a
  project or this repository.

`examples/neural-growth.atidarla/nca/coral.json` is written by the generator
(`cargo test -p atd-ops-img --test nca -- --ignored write_example_weights`), and a test checks
the shipped file still equals the generator's output.

## Performance and limits

Each network shape (channels, hidden units) gets its own compiled update shader, built on a
worker thread, with the perception and the per-unit sums written out as constants so they stay
in registers. Cells whose neighbourhood is dead skip the network. Measured on an RTX 5070 Ti
(DX12, release build): 1.18 ms per frame for 8 steps of the built-in rule on a 640×360 grid
(1280×720 at Cell Size 2) with every cell alive; 14.8 ms before the shader was specialised. State
is two `f32` storage buffers of `grid cells × channels`; a grid that would need more than the
device's largest storage buffer (128 MiB by default) is an error asking for a larger Cell Size.

Offline (`atidarla render`) it is stateful: identical across runs of the same range; it waits for
its pipelines and its weights file.
