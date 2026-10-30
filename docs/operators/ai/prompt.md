# Prompt Clip (`ai.prompt`)

AI · version 1 · cook budget 50 µs

An AI clip for the Stage: wire it into a layer slot and the AI nodes that follow the deck (their `Prompts From` names the deck's Mixer) take its prompt, seed and LoRA. The clips playing on crossfader sides A and B are blended by the crossfader. Shows its input (or black) as its picture.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `image` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `prompt` | Prompt | string | `"a watercolour painting"` |  | The prompt this clip plays. |
| `negative` | Negative | string | `""` |  | What to steer away from while this clip leads the blend. |
| `seed` | Seed | int | `1` | ≥ 0 | Noise seed while this clip leads the blend. |
| `reseed` | Reseed | menu | `"off"` | `off`, `launch`, `beat`, `bar` | Moves the seed on: each time the clip is launched, every beat, or every bar of the deck's clock. |
| `lora` | LoRA | path | `""` |  | A LoRA file used while this clip leads the blend. |
| `lora_weight` | LoRA Weight | float | `1.0` | 0 – 2 | Strength of the LoRA. |

## Platforms

All platforms.

## Examples

No example project uses it yet.
