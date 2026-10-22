# Film Grain

Makes an image look as if it was shot and projected on film: fine grain that changes every
frame, a slight flicker in brightness, softer contrast and faded colour. Turn Saturation to 0 for
old black-and-white film.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image. |
| `out` | IMG | The image with grain, flicker and fade. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Amount | 0.12 | Strength of the grain. 0.05 is subtle, 0.3 heavy. |
| Grain Scale | 480 | Grains across the image width: higher is finer. Lower it for a blotchy, low-budget look. |
| Monochrome | on | One grain for all colours (black-and-white stock); off gives coloured grain. |
| Flicker | 0.04 | How much the brightness wavers. |
| Contrast | 0.9 | Below 1 lifts the blacks and dims the whites, like a faded print. |
| Saturation | 0.8 | Colour intensity; 0 is black and white. |

## Tips

- The grain multiplies the image, so it shows most in bright areas and stays out of pure black,
  as on film.
- Put Vignette after it for a projector's darker corners.
- The grain follows the frame number, so an offline render gives the same grain every time.

## Inside

Noise (SIG) → bound to Level brightness (flicker, with Contrast) → HSV Adjust (Saturation); Noise
(IMG, its z following the frame number) multiplied over the result by a Composite.

## Versions

- 1.0.0 (2026-10-22): first version.
