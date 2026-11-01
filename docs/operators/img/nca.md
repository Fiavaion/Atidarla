# Neural CA (`img.nca`)

IMG · version 1 · cook budget 2000 µs

A neural cellular automaton: every cell perceives its neighbours through fixed Sobel and Laplacian filters and updates itself with a tiny neural network, at random but repeatable moments. The built-in rules (hand-built, not trained) grow a patterned organism from a seed and regrow what the `erase` image removes; a weights file in the project supplies any other network. `seed` can start it from an image. Reference: docs/nca.md.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `seed` | Img | optional |
| `erase` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `rule` | Rule | menu | `"grow"` | `grow`, `spots`, `file` | `grow`: a labyrinth pattern growing from the seed; `spots`: spots instead; `file`: the network in Weights File. |
| `weights` | Weights File | path | `""` |  | A Neural CA weights file (JSON, format in docs/nca.md), used when Rule is `file`; reloads when it changes. |
| `cell_size` | Cell Size | int | `2` | 1 – 16 | Output pixels per cell; larger is faster and coarser. |
| `steps` | Steps | int | `8` | 1 – 64 | Updates per frame (growth speed). |
| `fire_rate` | Fire Rate | float | `0.5` | 0 – 1 | Chance that a cell updates in a step. |
| `threshold` | Alive Threshold | float | `0.1` | 0 – 1 | A cell lives while a neighbour's alpha is above this; others are cleared. |
| `seed_mode` | Seed | menu | `"centre"` | `centre`, `scatter`, `noise`, `image` | How Reset starts: one centre cell, cells scattered at random (Density), every channel random, or the `seed` image (colour and alpha; hidden channels 1 where alive). |
| `density` | Density | float | `0.0005` | 0 – 1 | Chance that a cell is a seed, for the `scatter` seed. |
| `random_seed` | Random Seed | int | `1` | ≥ 0 | Selects a different but repeatable sequence of updates and scattered seeds. |
| `edges` | Edges | menu | `"zero"` | `zero`, `wrap` | Outside the grid is dead (`zero`) or the opposite edge (`wrap`). |
| `display` | Display | menu | `"rgba"` | `rgba`, `hidden` | `rgba`: the colour channels over Background; `hidden`: channels 4-6 as RGB (0.5 is zero). |
| `background` | Background | colour | `[0.0,0.0,0.0,1.0]` |  | Shown where cells are transparent. |
| `smooth` | Smooth | toggle | `false` |  | Interpolate between cells instead of showing square cells. |
| `reset` | Reset | pulse | `false` |  | Starts again from the seed. |

## Platforms

All platforms.

## Examples

- [Neural Growth](../../../examples/neural-growth.lasair/) — `organism`, `coral`
