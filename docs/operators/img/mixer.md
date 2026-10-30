# Mixer (`img.mixer`)

IMG · version 1 · cook budget 200 µs

A Stage deck: composites up to 16 layers bottom (`layer0`) to top, each with an opacity, a blend and a crossfader group, then a master level.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `layer0` | Img | optional |
| `layer1` | Img | optional |
| `layer2` | Img | optional |
| `layer3` | Img | optional |
| `layer4` | Img | optional |
| `layer5` | Img | optional |
| `layer6` | Img | optional |
| `layer7` | Img | optional |
| `layer8` | Img | optional |
| `layer9` | Img | optional |
| `layer10` | Img | optional |
| `layer11` | Img | optional |
| `layer12` | Img | optional |
| `layer13` | Img | optional |
| `layer14` | Img | optional |
| `layer15` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `stage` | Stage Deck | toggle | `false` |  | The deck the Stage tab shows (the first Mixer when none is marked). |
| `crossfader` | Crossfader | float | `0.5` | 0 – 1 | 0 shows group A, 1 group B; both at full in the middle. Layers in no group ignore it. |
| `master` | Master | float | `1.0` | 0 – 1 | Multiplies the composite's colour (fade to black). |
| `autopilot` | Autopilot | menu | `"off"` | `off`, `next`, `random`, `column` | The Stage launches columns by itself: `next` in order, `random`, or always `column` Autopilot Column (FR-S8). |
| `autopilot_after` | Autopilot After | menu | `"beats"` | `beats`, `clip_end` | When the autopilot moves on: after Autopilot Beats, or when the longest clip of the column ends (its length in the layer's slots). |
| `autopilot_beats` | Autopilot Beats | int | `16` | 1 – 1024 | Beats between autopilot launches. |
| `autopilot_column` | Autopilot Column | int | `0` | 0 – 15 | The column a `column` autopilot launches. |
| `opacity0` | Opacity 0 | float | `1.0` | 0 – 1 | Opacity of layer 0. |
| `blend0` | Blend 0 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 0 combines with the layers below it. |
| `group0` | Group 0 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 0: `a`, `b` or `none`. |
| `opacity1` | Opacity 1 | float | `1.0` | 0 – 1 | Opacity of layer 1. |
| `blend1` | Blend 1 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 1 combines with the layers below it. |
| `group1` | Group 1 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 1: `a`, `b` or `none`. |
| `opacity2` | Opacity 2 | float | `1.0` | 0 – 1 | Opacity of layer 2. |
| `blend2` | Blend 2 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 2 combines with the layers below it. |
| `group2` | Group 2 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 2: `a`, `b` or `none`. |
| `opacity3` | Opacity 3 | float | `1.0` | 0 – 1 | Opacity of layer 3. |
| `blend3` | Blend 3 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 3 combines with the layers below it. |
| `group3` | Group 3 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 3: `a`, `b` or `none`. |
| `opacity4` | Opacity 4 | float | `1.0` | 0 – 1 | Opacity of layer 4. |
| `blend4` | Blend 4 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 4 combines with the layers below it. |
| `group4` | Group 4 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 4: `a`, `b` or `none`. |
| `opacity5` | Opacity 5 | float | `1.0` | 0 – 1 | Opacity of layer 5. |
| `blend5` | Blend 5 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 5 combines with the layers below it. |
| `group5` | Group 5 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 5: `a`, `b` or `none`. |
| `opacity6` | Opacity 6 | float | `1.0` | 0 – 1 | Opacity of layer 6. |
| `blend6` | Blend 6 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 6 combines with the layers below it. |
| `group6` | Group 6 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 6: `a`, `b` or `none`. |
| `opacity7` | Opacity 7 | float | `1.0` | 0 – 1 | Opacity of layer 7. |
| `blend7` | Blend 7 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 7 combines with the layers below it. |
| `group7` | Group 7 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 7: `a`, `b` or `none`. |
| `opacity8` | Opacity 8 | float | `1.0` | 0 – 1 | Opacity of layer 8. |
| `blend8` | Blend 8 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 8 combines with the layers below it. |
| `group8` | Group 8 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 8: `a`, `b` or `none`. |
| `opacity9` | Opacity 9 | float | `1.0` | 0 – 1 | Opacity of layer 9. |
| `blend9` | Blend 9 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 9 combines with the layers below it. |
| `group9` | Group 9 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 9: `a`, `b` or `none`. |
| `opacity10` | Opacity 10 | float | `1.0` | 0 – 1 | Opacity of layer 10. |
| `blend10` | Blend 10 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 10 combines with the layers below it. |
| `group10` | Group 10 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 10: `a`, `b` or `none`. |
| `opacity11` | Opacity 11 | float | `1.0` | 0 – 1 | Opacity of layer 11. |
| `blend11` | Blend 11 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 11 combines with the layers below it. |
| `group11` | Group 11 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 11: `a`, `b` or `none`. |
| `opacity12` | Opacity 12 | float | `1.0` | 0 – 1 | Opacity of layer 12. |
| `blend12` | Blend 12 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 12 combines with the layers below it. |
| `group12` | Group 12 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 12: `a`, `b` or `none`. |
| `opacity13` | Opacity 13 | float | `1.0` | 0 – 1 | Opacity of layer 13. |
| `blend13` | Blend 13 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 13 combines with the layers below it. |
| `group13` | Group 13 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 13: `a`, `b` or `none`. |
| `opacity14` | Opacity 14 | float | `1.0` | 0 – 1 | Opacity of layer 14. |
| `blend14` | Blend 14 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 14 combines with the layers below it. |
| `group14` | Group 14 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 14: `a`, `b` or `none`. |
| `opacity15` | Opacity 15 | float | `1.0` | 0 – 1 | Opacity of layer 15. |
| `blend15` | Blend 15 | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How layer 15 combines with the layers below it. |
| `group15` | Group 15 | menu | `"none"` | `none`, `a`, `b` | Crossfader group of layer 15: `a`, `b` or `none`. |

## Platforms

All platforms.

## Examples

- [Stage deck](../../../examples/stage-deck.atidarla/) — `deck`
