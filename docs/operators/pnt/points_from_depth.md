# Points from Depth (`pnt.points_from_depth`)

PNT · version 1 · cook budget 150 µs

A depth image back-projected into a point cloud through a pinhole camera, one point per cell of Columns × rows: writes `P` (camera space: Y up, looking down −Z, then moved by Translate and Rotate), `Cd` (the colour image at the same place, or white), `uv`, `depth` (metres), `valid` (1, or 0 where the depth is missing or outside Near to Far: those points get a transparent `Cd`; remove them with Delete on `valid`) and `id`. Reads depth from AI depth estimation (inverse), a linear depth image, or a sensor's raw depth (scaled: a 16-bit image in millimetres is the default).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `depth` | Img | optional |
| `colour` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `columns` | Columns | int | `160` | 1 – 4096 | Points across the image (at most one per pixel); the rows follow the image's aspect ratio, so the count is Columns × rows. Each point samples the image at its cell's centre. |
| `encoding` | Encoding | menu | `"inverse"` | `linear`, `inverse`, `scaled` | How a pixel's value (its red channel) gives depth: linear: Near at 0 to Far at 1; inverse: bright is near (disparity, as AI depth estimation outputs), Near at 1 to Far at 0; scaled: value × Scale metres (a sensor's raw depth). |
| `near` | Near | float | `0.5` | ≥ 0 | The nearest depth in metres (linear, inverse: at the end of the range; scaled: closer points are invalid). |
| `far` | Far | float | `4.0` | ≥ 0 | The farthest depth in metres (farther points are invalid). |
| `scale` | Scale | float | `65.535` | ≥ 0 | scaled: metres per unit of the value (65.535: a 16-bit image whose integer values are millimetres, as Kinect and Orbbec sensors give). |
| `zero_invalid` | Zero Is Invalid | toggle | `true` |  | A value of 0 means no depth (sensors write 0 where they see nothing). |
| `camera` | Camera | menu | `"fov"` | `fov`, `pinhole` | fov: a centred pinhole camera with the vertical field of view FOV; pinhole: the focal lengths and principal point below (from a sensor's calibration). |
| `fov` | FOV | float | `60.0` | 1 – 179 | fov: the vertical field of view in degrees. |
| `focal` | Focal | vec2 | `[504.0,504.0]` |  | pinhole: fx, fy in pixels of the depth image. |
| `principal` | Principal Point | vec2 | `[320.0,288.0]` |  | pinhole: cx, cy in pixels from the image's top-left corner. |
| `translate` | Translate | vec3 | `[0.0,0.0,0.0]` |  | Moves the cloud (the camera's position). |
| `rotate` | Rotate | vec3 | `[0.0,0.0,0.0]` |  | Rotates the cloud about X, Y, Z in degrees (the camera's orientation), before Translate. |

## Platforms

All platforms.

## Examples

- [PNT Capture](../../../examples/pnt-capture.atidarla/) — `dome`
