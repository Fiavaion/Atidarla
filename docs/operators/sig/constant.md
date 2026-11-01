# Constant (`sig.constant`)

SIG · version 1 · cook budget 20 µs

Up to four named channels with constant values.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `name1` | Name 1 | string | `"chan1"` |  | Channel name; leave empty to omit the channel. |
| `value1` | Value 1 | float | `0.0` |  | Channel value. |
| `name2` | Name 2 | string | `""` |  | Channel name; leave empty to omit the channel. |
| `value2` | Value 2 | float | `0.0` |  | Channel value. |
| `name3` | Name 3 | string | `""` |  | Channel name; leave empty to omit the channel. |
| `value3` | Value 3 | float | `0.0` |  | Channel value. |
| `name4` | Name 4 | string | `""` |  | Channel name; leave empty to omit the channel. |
| `value4` | Value 4 | float | `0.0` |  | Channel value. |

## Platforms

All platforms.

## Examples

- [Beat switcher](../../../examples/beat-switcher.lasair/) — `step`
- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.lasair/) — `rate`
