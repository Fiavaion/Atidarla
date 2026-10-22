# Spinning Solid

A 3D solid (a cube, a dodecahedron and the like) lit by one light and turning slowly in front of
the camera. Give it a colour, a colour per face, or wrap any image around it through the
`texture` input: every face shows the whole image.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `texture` | IMG | Optional. An image shown on every face (multiplied by Colour; set Colour to white to see it as it is). |
| `out` | IMG | The rendered solid, at the project resolution. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Shape | dodecahedron | tetrahedron, cube, octahedron, dodecahedron, icosahedron, or a flat polygon. |
| Spin | 30°/s | How fast the camera circles the solid; negative turns the other way. |
| Tilt | 25° | How far the camera looks down on it. |
| Distance | 3.6 | Camera distance: lower is closer and bigger. |
| Colour | orange | Surface colour. |
| Face Colours | off | Every face its own colour, around the hue circle (times Colour). |
| Roughness | 0.35 | Sharp highlight (low) to dull (high). |
| Metallic | 0 | Plastic (0) to metal (1). |
| Emission | black | Light the surface gives off; a bright colour glows through the bloom. |
| Glow | 0.5 | Bloom around highlights and emission. |
| Background | near black | Behind the solid. Alpha 0 leaves it transparent, to composite over something else. |

## Tips

- Feed Kaleidoscope Tunnel, a movie or a camera into `texture` for a spinning video cube.
- Wire the output through Audio Pulse for a solid that flashes with the music, or bind Spin to
  an LFO to rock it back and forth.
- Spin accumulates over time, so an offline render started part-way shows a different angle than
  the same frames of a longer render (see `docs/render.md`).

## Inside

Constant (Spin) → Speed (accumulated angle) → bound to Render's orbit azimuth; Platonic Solid and
a PBR material (with the `texture` image as its map) → Render → Bloom.

## Versions

- 1.0.0 (2026-10-22): first version.
