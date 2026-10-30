# DMX, fixtures and LED pixel mapping (FR-S7)

Atidarla sends DMX512 over Ethernet, as Art-Net (ArtDmx, UDP 6454) or sACN (ANSI E1.31,
UDP 5568). Two operators do it:

| Operator | Use |
|---|---|
| [DMX Out](operators/sig/dmx_out.md) (`sig.dmx_out`) | Any channels (0–1) to the slots of one universe, from a start slot. |
| [Pixel Map](operators/img/pixel_map.md) (`img.pixel_map`) | An image to fixtures and LED pixels: samples image regions through a **patch** file and outputs the DMX slot values; sends every patched universe itself, or feeds DMX Out. |

Example: [`examples/pixel-map.atidarla`](../examples/pixel-map.atidarla/).

Sending runs on its own thread per universe: the cook only hands over the latest values
(CLAUDE.md rule 1). A universe is sent when it changes, at most 44 times a second, and
repeated every second as a keep-alive.

## Why `img.pixel_map`

PRD §6 lists "Pixel Out (points → DMX/LED/laser)" among the outputs. FR-S7 asks for "LED
pixel mapping from IMG regions", so the operator built in M3 reads an image: it is an IMG
operator with a SIG output, like Analyze, named for what it does. A points-driven variant
(sample positions from a PNT stream, lasers) can come later as `pnt.pixel_out` and reuse the
same fixture types and DMX layout (`atd_io::fixture`).

## Fixture types

A fixture type lists the DMX channels of **one pixel**, by role, in DMX order. A PAR is one
pixel; an LED strip or matrix is a pixel type repeated `count` times by the patch.

| Role | Value |
|---|---|
| `red`, `green`, `blue` | The sampled colour (minus the white part when the type has `white`). |
| `white` | `min(r, g, b)`, taken out of red, green and blue (white extraction). |
| `dimmer` | Full when the type has colour channels (the colours carry the image); the sample's Rec. 709 luminance for a dimmer-only type. |
| `amber` | 0 (not derived from RGB). |
| `pan`, `tilt` | The patch's `pan` / `tilt` (0–1, default 0.5): a fixed position. |
| `open` | Full (shutter open, strobe off on most fixtures). |
| `none` | 0 (unused channels, strobe, macros). |

Alpha is ignored. Values are 8-bit, `round(v × 255)`; there are no 16-bit (fine) channels yet.

**Built-in types:** `dimmer`, `rgb`, `drgb` (dimmer + RGB), `rgbw`, `drgbw`, `rgbwa`,
`pixel-rgb`, `pixel-grb` (WS2812), `pixel-rgbw` (SK6812), `moving-head` (pan, tilt, dimmer,
RGB, shutter).

**Project types** live in the project's `fixtures/` folder as `<id>.toml` and take precedence
over a built-in of the same ID:

```toml
# fixtures/led-bar.toml
name = "LED bar, dimmer + RGB + strobe"
channels = ["dimmer", "red", "green", "blue", "none"]
```

## The patch

A patch file (TOML, anywhere in the project; Pixel Map's `patch` parameter names it) places
fixtures on universes and on the image. Coordinates are 0–1 over the image, origin top-left.

```toml
[[fixture]]
id = "strip"            # stable ID, unique in the patch
type = "pixel-grb"      # fixture type ID
universe = 1            # Art-Net port address or sACN universe (default 1)
address = 1             # first slot, 1-512 (default 1)
count = 60              # pixels (default 1)
line = [[0.0, 0.95], [1.0, 0.95]]

[[fixture]]
id = "wall"
type = "pixel-rgb"
universe = 2
rect = [0.0, 0.0, 1.0, 0.5]   # x, y, width, height
grid = [8, 4]                 # columns, rows: 32 pixels
snake = true                  # every other row runs right to left

[[fixture]]
id = "head"
type = "moving-head"
address = 300
point = [0.5, 0.5]
pan = 0.25
tilt = 0.6
```

Each fixture has exactly one region:

| Region | Pixels sample |
|---|---|
| `point = [x, y]` | That point (every pixel the same). |
| `line = [[x0, y0], [x1, y1]]` | `count` equal segments of the line, each averaged. |
| `rect = [x, y, w, h]` | A `grid = [cols, rows]` of cells (or `count` columns in one row), each averaged, row by row from the top-left; `snake` reverses odd rows. One cell averages the whole rectangle. |

Averages are of a 4 × 4 grid of texels over each cell, so a cell much larger than 4 × 4
texels is sampled, not fully averaged; scale the image down first (Resolution) if that
matters.

**Layout rules.** A pixel's channels stay in one universe: a pixel that would run past slot
512 starts the next universe at slot 1 (170 RGB pixels per universe; a 200-pixel strip on
universe 1 continues on universe 2). Two fixtures writing the same slot are an error that
names the universe and slot. Unknown keys, a missing region, an unknown fixture type and
out-of-range values are errors naming the fixture.

The patch reloads when the file is saved (fixture type files are read again with it). Errors
show on the node; the channels go empty until the patch is fixed.

## Channels and sending

Pixel Map outputs one channel per slot, `u<universe>_<slot>` (0–1), from slot 1 to the
highest patched slot of each universe, universes in ascending order. **Channels Of**
(`universe`) limits them to one universe: wire that into DMX Out (Start 1) to send it.
**Send** (`artnet`, `sacn`) sends every patched universe directly to **Host** (empty:
broadcast for Art-Net, the universe's multicast group for sACN) on **Port** (0: the
standard port).

## Latency and the render thread

Sampling is one compute pass over all pixels; the results reach the CPU through three
readback buffers mapped after the frame is submitted and read on a later cook, so the cook
never waits for the GPU. Live, DMX trails the image by **one frame, sometimes two**. In an
offline render (`atidarla render`) the operator waits for the previous frame's readback, so
its channels are exactly one frame late, and it sends nothing (`docs/render.md`).

Patch files are read on a watcher thread. After warm-up the DMX path allocates nothing:
slot values, channel names and the senders' buffers are reused. Recording the compute pass
creates a bind group each measured frame, as Analyze does (counted by the allocation test).

## Testing without a fixture

`cargo test -p atd-ops-img --test pixel_map` renders a known gradient, maps it through a
patch with a strip and a project fixture, and checks the slot values against a CPU
reference, as channels and as Art-Net and sACN packets received by a loopback UDP socket
that parses them as a fixture would. Any Art-Net or sACN monitor on the LAN (or on the same
machine with Host `127.0.0.1`) shows the live output of the example.
