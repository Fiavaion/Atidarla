# Attribute from Channels (`pnt.attribute_from_channels`)

PNT · version 1 · cook budget 60 µs

Writes SIG values into a point attribute: point i reads sample i (a spectrum or a history across the points), a sample picked by a 0-1 float attribute (interpolated), or the last value of channel i (a bank of LFOs, one per point). A vector attribute's components read consecutive channels. Combined with the attribute's value (set, add, multiply, max, min). The channels are uploaded each frame they change.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `channels` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `name` | Name | string | `"pscale"` |  | The attribute written (created at zero, with Type, when missing). |
| `type` | Type | menu | `"float"` | `float`, `vec2`, `vec3`, `vec4`, `int` | The type of a new attribute (an existing one keeps its type). |
| `lookup` | Lookup | menu | `"index"` | `index`, `attribute`, `channels` | index: point i reads sample i of the channels (a spectrum or a trail across the points); attribute: the float attribute Attribute (0 to 1) picks the sample position, interpolated; channels: point i reads the last sample of channel i (one value per channel, such as a bank of LFOs). |
| `attribute` | Attribute | string | `"u"` |  | attribute: the float attribute (0 at the first sample, 1 at the last). |
| `first` | First Channel | int | `0` | ≥ 0 | index, attribute: the channel the first component reads; the next components read the next channels (the last channel repeats). |
| `wrap` | Wrap | toggle | `false` |  | index, channels: wraps past the last sample or channel instead of holding it. |
| `scale` | Scale | float | `1.0` |  | Multiplies the values read. |
| `operation` | Operation | menu | `"set"` | `set`, `add`, `multiply`, `max`, `min` | How the result meets the attribute's value: set (replace), add, multiply, max or min. |
| `amount` | Amount | float | `1.0` |  | Blends from the attribute's value (0) to the combined result (1). |

## Platforms

All platforms.

## Examples

- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `pulse`
