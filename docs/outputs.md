# Outputs: windows, slices, warps, soft edges (FR-S6)

An output is a **Window** node (`cmp.window`). Wire your composition into its `in` input; the
node maps it into an image at the output's resolution, and the app shows that image in a
native window, on a display of your choice. Add as many as you have projectors or screens.
Example: [`examples/two-outputs.lasair`](../examples/two-outputs.lasair/) (two projectors
side by side with an overlap). Reference: [`operators/cmp/window.md`](operators/cmp/window.md).

## The window

| Parameter | What it does |
|---|---|
| Enabled | Opens the window. Closing the window turns this off (undoable). The image is mapped either way. |
| Fullscreen, Display | Borderless fullscreen on display *n* (0 is the first, in the system's order). Off: a normal window at Position, Window Size. |
| Width, Height | The resolution of the mapped image, in pixels. Set it to the display's: the Outputs tab offers **Match display** once the window knows which display it is on. 0 uses the project resolution. |
| Test Pattern | Replaces the input in every slice: `grid` (shows the warp), `bars` (75% colour bars), `gradient` (shows soft edges), `circles` (each slice in its own colour, with its number). |
| Slices | The mapping, as text (below). |

The image is stretched over the window. Projectors show colour only: alpha is ignored in the
window (areas no slice covers are black).

## Slices

Each slice takes a rectangle of the input and places it in the output through a warp. Slices
are drawn in order, later ones on top. With no slices the whole input fills the whole output.

The **Outputs** tab (next to Patch and Stage) edits them: pick an output and a slice, set the
source rectangle, the warp and its grid, and the soft edges, and drag the control points over
the preview. With **Edit mask** on, the preview edits the slice's mask instead: drag points,
click to add one, right-click to remove one. Every change is an undoable edit of the Slices
text, written in canonical form, so a dragged point changes one line of a diff.

The text is one `[slice name]` block per slice, then `key = values` lines. Coordinates are
fractions of the image, x to the right and y down; `#` starts a comment.

```toml
[slice left]
source = 0 0 0.55 1          # x y w h of the input
warp = perspective           # a corner pin
points = 0.02 0 1 0          # top-left x y, top-right x y
points = 0.06 1 0.97 0.98    # bottom-left, bottom-right
blend = 0 0.18 0 0           # soft edge on the right, 18% of the slice wide
```

| Key | Values | Default |
|---|---|---|
| `source` | `x y w h`: the part of the input this slice shows | `0 0 1 1` |
| `dst` | `x y w h`: where it goes in the output, when there are no `points` | `0 0 1 1` |
| `warp` | `bilinear`, `perspective`, `catmull_rom`, `bezier` | `bilinear` |
| `grid` | `cols rows`: control points per axis, 2 to 16 | `2 2` |
| `points` | `x y` pairs, row by row from the top left; one `points` line per row. Without them the grid spreads evenly over `dst` | — |
| `blend` | soft-edge widths `left right top bottom`, as fractions of the slice | `0 0 0 0` |
| `blend_power` | steepness of the blend curve: 1 linear, 2 the usual S-curve | `2` |
| `blend_gamma` | the display gamma the blend compensates; 2.2 for most projectors, 1 for a plain ramp | `2.2` |
| `mask` | a polygon of 3 to 32 `x y` points in the output; outside it the slice is black | none |
| `mask_invert` | `true`: black inside the polygon instead | `false` |
| `subdiv` | tessellation segments per axis, 1 to 64 (more for strong curves) | `32` |

**Warps.** `perspective` takes exactly four points (a 2×2 grid) and keeps straight lines
straight: use it for a keystoned projector. `bilinear` bends piecewise between the control
points of any grid; `catmull_rom` makes a smooth surface through all of them (for curved
screens); `bezier` takes 4, 7, 10, 13 or 16 points per axis and treats them as bicubic patches
(the patch corners lie on the surface, the others pull it). Changing the warp or the grid in
the Outputs tab resamples the points so the slice keeps its shape.

**Soft edges.** Where two projectors overlap, each fades its side of the overlap so the sum
stays even. Give both the same overlap width (here the overlap is 10% of the input and each
slice shows 55% of it, so `0.1 / 0.55 = 0.18`). The weight at distance *t* across the blend
zone is `f(t)^(1/gamma)` with `f(t) = 0.5 (2t)^p` below the middle and `1 - 0.5 (2 - 2t)^p`
above it, so two overlapping ramps add up to constant light on a display of that gamma.

**Masks.** Per slice, a polygon in output coordinates (even-odd rule). For a whole-output
mask, wire an image into the node's `mask` input: the output is multiplied by its luminance
times alpha.

## Headless and offline

The mapping is the node's cook, so `lasair check --gpu`, `lasair render --node <window>`
and tests produce exactly the image a projector gets; nothing opens a window. Renders of the
same project and range are identical (`docs/render.md`: Window is *pure*).

## Limits

- **10-bit and HDR output** are not available yet: eframe gives every window the same 8-bit
  surface format, and the engine renders 8-bit images. A 10-bit path needs an output window
  with its own surface (Rgb10a2 or Rgba16Float where the display supports it) and an engine
  format to match.
- **Displays** are chosen by index; the app cannot list their names yet. An index past the
  last display leaves the window where it is (a warning is logged).
- Each window paints in its own pass (an egui deferred viewport) when the engine cooked a new
  frame, but the cook runs in the main window's frame: an output does not update faster than
  the app's own frame rate.
