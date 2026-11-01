# Sensor In (`pnt.sensor_in`)

PNT · version 1 · cook budget 200 µs

Depth and body tracking from a depth sensor, as points (FR-C3). Devices: test, a built-in scene (a waving figure in front of a wall, 320 × 288 depth with sensor noise and its body tracking); recording, a sequence of point caches played at Recording FPS (looping); k4a, a live Azure Kinect or Orbbec Femto camera (Windows with the K4A SDK; see Platforms). Output points: the depth image as a point cloud (`P` in camera space, Y up, looking down −Z, then Rotate and Translate; `Cd`, `depth`, `valid`, `body`: the tracked body's index or −1, `id`); body: the 32 joints of each tracked body in the K4A order (`joint` 0 pelvis … 26 head … 31 ear_right, `body`, `Cd`) with the bones as lines.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `device` | Device | menu | `"test"` | `test`, `recording`, `k4a` | test: the built-in scene; recording: point caches from Recording; k4a: a live Azure Kinect or Orbbec Femto camera (where available). |
| `output` | Output | menu | `"points"` | `points`, `body` | points: the depth as a point cloud; body: the tracked joints and bones (recording: whatever the files hold). |
| `columns` | Columns | int | `160` | 1 – 4096 | points: points across the depth image (at most one per pixel); the rows follow its aspect ratio. |
| `near` | Near | float | `0.5` | ≥ 0 | Depth closer than this (metres) is invalid. |
| `far` | Far | float | `4.0` | ≥ 0 | Depth farther than this (metres) is invalid. |
| `recording` | Recording | path | `""` |  | recording: a point-cache sequence with `$F` for the frame number (for example `capture/depth.$F4.lsrpc`, made with `lasair points convert`). |
| `recording_fps` | Recording FPS | float | `30.0` | ≥ 0.1 | recording: frames per second of the recording. |
| `first` | First Frame | int | `1` |  | recording: the first file's frame number. |
| `length` | Length | int | `300` | ≥ 1 | recording: frames in the recording; playback loops over them. |
| `translate` | Translate | vec3 | `[0.0,0.0,0.0]` |  | Moves the result (the sensor's position). |
| `rotate` | Rotate | vec3 | `[0.0,0.0,0.0]` |  | Rotates the result about X, Y, Z in degrees (the sensor's orientation), before Translate. |

## Platforms

- Requires `win+sdk:k4a`; elsewhere: Live depth capture uses the Azure Kinect Sensor SDK (or Orbbec's K4A wrapper for Femto cameras) on Windows. Elsewhere Sensor In plays recordings (point-cache sequences) and its test scene; the k4a device reports that it is not available.

## Examples

- [PNT Capture](../../../examples/pnt-capture.lasair/) — `sensor`, `joints`
