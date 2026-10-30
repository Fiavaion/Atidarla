# Layer (`img.layer`)

IMG · version 1 · cook budget 80 µs

A Stage layer: plays the clip wired into slot `target` and switches to a new target with a cut, fade or wipe, at once or on the next beat or bar of its clock. Clips are any IMG nodes wired into `clip0`…`clip15`. Clips not shown do not cook, so a stateful clip (feedback, a simulation, a movie) pauses while hidden.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `clip0` | Img | optional |
| `clip1` | Img | optional |
| `clip2` | Img | optional |
| `clip3` | Img | optional |
| `clip4` | Img | optional |
| `clip5` | Img | optional |
| `clip6` | Img | optional |
| `clip7` | Img | optional |
| `clip8` | Img | optional |
| `clip9` | Img | optional |
| `clip10` | Img | optional |
| `clip11` | Img | optional |
| `clip12` | Img | optional |
| `clip13` | Img | optional |
| `clip14` | Img | optional |
| `clip15` | Img | optional |
| `clock` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `target` | Target | int | `-1` | -1 – 15 | The clip slot to play; −1 plays nothing. Launching a clip sets this (an undoable edit); the layer switches when quantise allows. |
| `transition` | Transition | menu | `"cut"` | `cut`, `fade`, `wipe` | How the layer changes clip: `cut` at once, `fade` through the blend, `wipe` from left to right. |
| `transition_length` | Transition Length | float | `1.0` | ≥ 0 | Length of a fade or wipe, in Transition Unit; 0 cuts. |
| `transition_unit` | Transition Unit | menu | `"beats"` | `beats`, `seconds` | Beats follow the clock input's tempo (120 BPM without a clock); seconds are wall-clock (frame time offline). |
| `quantise` | Quantise | menu | `"off"` | `off`, `beat`, `bar` | `beat` or `bar` waits for the next boundary of the clock input's `beat` or `bar` channel before switching; `off`, or no clock, switches in the same frame. |
| `opacity` | Opacity | float | `1.0` | 0 – 1 | Multiplies the layer's alpha. |
| `blend` | Transition Blend | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How the incoming clip combines with the outgoing one half-way through a fade (`over` is a plain crossfade for opaque clips). Blending onto the layers below is set on the Mixer. |
| `bypass` | Bypass | toggle | `false` |  | Outputs nothing (transparent) while on; launches still queue and switch. |
| `slots` | Slots | string | `""` |  | Clip settings for the Stage, one line per slot: `slot,mode,beats,label`, e.g. `2,one_shot,4,Strobe hit`. Modes: trigger, toggle, flash, one_shot; beats is the clip length (one-shot return, autopilot clip end). Slots without a line are trigger clips named after their source node. |

## Platforms

All platforms.

## Examples

- [Stage deck](../../../examples/stage-deck.atidarla/) — `backs`, `shapes`, `accents`
