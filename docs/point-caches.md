# Point caches, depth and sensors (D12, FR-C3)

Capture-to-stage for points: files and sequences of points, images and depth turned into
points, and depth sensors. The operators:

| Operator | Use |
|---|---|
| [Point File In](operators/pnt/file_in.md) (`pnt.file_in`) | A point file or a numbered sequence: `.lsrpc`, PLY, CSV, XYZ/PTS. |
| [Points from Image](operators/pnt/points_from_image.md) (`pnt.points_from_image`) | A grid of points coloured by an image, optionally pushed out by its brightness. |
| [Points from Depth](operators/pnt/points_from_depth.md) (`pnt.points_from_depth`) | A depth image back-projected through a pinhole camera. |
| [Attribute from Image](operators/pnt/attribute_from_image.md) (`pnt.attribute_from_image`) | An image looked up at each point (uv, a planar projection or the point number). |
| [Sensor In](operators/pnt/sensor_in.md) (`pnt.sensor_in`) | Depth and body tracking: a test scene, recordings, live K4A cameras. |

Example: [`examples/pnt-capture.lasair`](../examples/pnt-capture.lasair/).

## Point File In

Reads on a worker thread (CLAUDE.md rule 1): the file is parsed and its attributes uploaded to
the GPU there, and the node swaps the result in when it is ready. Live, the last file stays on
screen until the next arrives; offline (`lasair render`) each frame waits for its own file.

| Column or property | Becomes |
|---|---|
| `x y z` | `P` |
| `nx ny nz` | `N` |
| `red green blue [alpha]` or `r g b [a]` | `Cd` (8- and 16-bit PLY colours scaled to 0-1; XYZ colours above 1 read as 0-255) |
| `u v` (`s t`, `texture_u texture_v`) | `uv` |
| `id` | `id` (`u32`: the point's identity) |
| anything else | a float attribute of the same name (invalid characters become `_`) |

At most 16 attributes are read from one file. Without an `id` column the point number is
written as `id`.

**Sequences.** `$F` in the file name is the Frame parameter; `$F4` pads it to four digits
(`cache.$F4.lsrpc` → `cache.0012.lsrpc`), as Houdini names caches. Drive Frame with an
expression, for example `frame + 1`, or `int(time * 24) % 120 + 1` to loop 120 frames at 24
fps. A missing frame is a node error.

**Show bundles.** `lasair export-show` copies single files; it does not yet expand `$F`
sequences, so a project that plays one reports the pattern as missing. Keep sequences inside
the project folder and copy the folder.

## The `.lsrpc` format

Lasair's own point cache: binary, little-endian, so a frame of a sequence loads with no
parsing. Version 1:

| Bytes | Field |
|---|---|
| 6 | magic `LSRPC\0` |
| 2 | version (`u16`, 1) |
| 4 | point count (`u32`) |
| 4 | attribute count (`u32`) |
| per attribute | name length (`u8`), name (UTF-8), scalar type (`u8`: 0 `f32`, 1 `i32`, 2 `u32`), components (`u8`, 1-4) |
| per attribute, in order | count × components values, tightly packed |

## Converting caches

```sh
lasair points convert scan.ply scan.lsrpc
lasair points convert 'sim/fluid.$F4.csv' 'cache/fluid.$F4.lsrpc' --range 1-240
lasair points convert scan_cm.xyz scan.lsrpc --scale 0.01   # centimetres to metres
lasair points info cache/fluid.0001.lsrpc
```

`convert` reads `.lsrpc`, PLY, CSV and XYZ/PTS and writes `.lsrpc`, PLY or CSV (chosen by the
output's extension); quote names with `$F` so the shell leaves them alone.

**Houdini `.bgeo` / `.bgeo.sc`** and other formats without an open specification are not read
by Lasair (no GPL or closed code in core crates, rule 3). Convert them with the application
that wrote them, then with `lasair points convert`:

```sh
# Houdini (Apprentice and Indie included) ships gconvert:
for f in fluid.*.bgeo.sc; do gconvert "$f" "${f%.bgeo.sc}.ply"; done
lasair points convert 'fluid.$F4.ply' 'fluid.$F4.lsrpc' --range 1001-1240
```

From a Houdini scene, a ROP Geometry Output node writing `.ply` does the same. Alembic point
caches: export PLY or CSV from the DCC (Blender: File → Export → Stanford PLY, or a CSV
writer).

## Points from Depth

A pixel's red channel gives depth through Encoding: `inverse` (bright is near: the output of
AI depth estimation such as Depth (`ai.depth`)), `linear` (Near at 0 to Far at 1) or `scaled`
(value × Scale metres: a sensor's raw 16-bit depth in millimetres is Scale 65.535). The camera
is a pinhole: a centred one with a vertical field of view, or the focal lengths and principal
point of a calibration (OpenCV convention, pixels from the top-left). Points are in camera
space, Y up, looking down −Z (A39), then Rotate and Translate place the camera.

Depth that is missing (0, with Zero Is Invalid) or outside Near to Far gives `valid` 0 and a
transparent `Cd`; remove those points with Delete (rule attribute, `valid` < 0.5). Depth is
read without interpolation, so no points are invented between the foreground and the
background.

## Sensor In

**Devices.** `test`: a built-in scene, 320 × 288 depth (a binned K4A narrow field of view) of a
figure waving in front of a wall, with depth noise and the figure's body tracking; it is a
function of time and frame, so renders repeat exactly. `recording`: a point-cache sequence
(`$F`) played at Recording FPS, looping over Length frames from First Frame. `k4a`: a live
Azure Kinect, or an Orbbec Femto camera through Orbbec's K4A wrapper.

**Outputs.** `points`: the depth image as points, like Points from Depth, plus `body` (the
tracked body's index, −1 elsewhere). `body`: the 32 joints of each tracked body in the K4A
order (pelvis, spine_navel, spine_chest, neck, clavicle/shoulder/elbow/wrist/hand/handtip/
thumb left then right, hip/knee/ankle/foot left then right, head, nose, eye_left, ear_left,
eye_right, ear_right) with `joint` (0-31) and `body`, and the bones as two-point lines (Render
Points draws them with Draw set to lines or both).

**Capability (PRD A19).** Live capture needs the K4A SDK, which is Windows-first: the operator
declares `win+sdk:k4a` with a Degrade fallback. Where the flag does not hold the node still
loads and runs the test scene and recordings; the `k4a` device reports why it is not
available, and `lasair check --gpu` lists such a node as not available on this platform
rather than failing.

**Status.** No SDK is linked into the core crates, so `cargo deny` and the macOS build are
unaffected. The capture backend is planned as a plugin crate that loads `k4a.dll` (and the body
tracking SDK) at run time and registers the `sdk:k4a` capability when it finds them; until it
lands, the `k4a` device reports that it is not in this build. To play a session today, export
its frames as PLY point clouds with the vendor's tools and convert them with `lasair points
convert`.
