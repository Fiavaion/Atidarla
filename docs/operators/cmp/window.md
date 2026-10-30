# Window (`cmp.window`)

CMP · version 1 · cook budget 150 µs

An output to a projector or screen: maps its input through slices (source rectangle to a warped destination: corner pin, grid or Bezier warp), masks and soft edges, or shows a test pattern, at the output's resolution. The app opens a native window per enabled Window on the chosen display; headless runs and renders produce the mapped image only.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |
| `mask` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `enabled` | Enabled | toggle | `true` |  | Opens the window in the app. The image is mapped either way (the Outputs panel previews it); closing the window turns this off. |
| `title` | Title | string | `"Output"` |  | The window's title (windowed mode). |
| `display` | Display | int | `0` | 0 – 15 | The display a fullscreen window opens on, in the system's order (0 is the first). An index past the last display leaves the window where it is. |
| `fullscreen` | Fullscreen | toggle | `false` |  | Borderless fullscreen on Display; off, a normal window at Position and Window Size. |
| `position` | Position | vec2 | `[80.0,80.0]` |  | Top-left corner of the window in screen points (windowed mode). |
| `window_size` | Window Size | vec2 | `[960.0,540.0]` |  | Inner size of the window in points (windowed mode). The image is scaled to fill it. |
| `width` | Width | int | `1920` | 0 – 8192 | Width of the mapped image in pixels; set it to the display's. 0 uses the project resolution. |
| `height` | Height | int | `1080` | 0 – 8192 | Height of the mapped image in pixels; 0 uses the project resolution. |
| `pattern` | Test Pattern | menu | `"off"` | `off`, `grid`, `bars`, `gradient`, `circles` | Replaces the input in every slice to line projectors up: `grid` (shows the warp), `bars` (75% colour bars), `gradient` (shows edge blends), `circles` (each slice's colour and number). |
| `slices` | Slices | string | `""` |  | The mapping, one `[slice name]` block per slice with `key = values` lines (x right, y down, fractions of the image): `source = x y w h`, `dst = x y w h`, `warp = bilinear\|perspective\|catmull_rom\|bezier`, `grid = cols rows`, `points = x y …` (one line per row of control points), `blend = left right top bottom`, `blend_power`, `blend_gamma`, `mask = x y …`, `mask_invert`, `subdiv`. Empty: the whole input over the whole output. See docs/outputs.md. |

## Platforms

All platforms.

## Examples

- [Two outputs](../../../examples/two-outputs.atidarla/) — `left`, `right`
