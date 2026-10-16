# Input Devices (`sig.input`)

SIG · version 1 · cook budget 20 µs

Mouse, keyboard or gamepad as channels. Mouse: position over the viewed image (0–1, y up), buttons and wheel. Keyboard: 1 while each listed key is held. Gamepad: sticks, triggers and buttons.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `device` | Device | menu | `"mouse"` | `mouse`, `keyboard`, `gamepad` | Which input to read. |
| `keys` | Keys | string | `"space"` |  | Keyboard: key names separated by spaces (`a`, `space`, `arrowup`, `1`, `enter`); one channel each. |
| `gamepad` | Gamepad | int | `1` | 1 – 16 | Gamepad: which connected gamepad (1 = first). |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
