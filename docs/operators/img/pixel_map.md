# Pixel Map (`img.pixel_map`)

IMG · version 1 · cook budget 80 µs

Samples image regions for the fixtures and LED pixels of a DMX patch file and outputs their DMX slot values (0–1) as channels `u<universe>_<slot>`; optionally sends every patched universe over Art-Net or sACN. Values trail the image by a frame or two (GPU readback).

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `patch` | Patch | path | `""` |  | The patch file (TOML, `docs/dmx.md`): fixtures with type, universe, address and the point, line or rectangle they sample. Fixture types come from the project's `fixtures/` folder, then the built-ins. |
| `universe` | Channels Of | int | `0` | 0 – 63999 | The universe whose slots become output channels (to wire into DMX Out, which sends one universe); 0 outputs every patched universe in order. |
| `protocol` | Send | menu | `"off"` | `off`, `artnet`, `sacn` | `off`: channels only (wire them to DMX Out); `artnet` or `sacn`: also send every patched universe (at most 44 times a second, repeated every second). |
| `host` | Host | string | `""` |  | Destination node; empty broadcasts (Art-Net) or uses each universe's multicast group (sACN). |
| `port` | Port | int | `0` | 0 – 65535 | UDP port; 0 for the protocol's standard port. |

## Platforms

All platforms.

## Examples

- [Pixel map](../../../examples/pixel-map.lasair/) — `pixel_map1`
