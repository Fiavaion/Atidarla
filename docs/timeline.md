# Timeline

The Timeline animates parameters over time and fires cues: keyframe tracks, markers, a cue list,
a transport, and chasing external timecode (PRD FR-T1–T3, decision A54). It is a view over the
same graph as the Patch: it owns no media and no second model. Try it with
[`examples/timeline-show.atidarla`](../examples/timeline-show.atidarla/).

## How it drives the patch

- **Tracks write live values** (`Engine::set_live`). A live value wins over a parameter's
  binding, expression and stored value. It is not an edit: no undo step, and it is never saved. The
  Parameters panel still shows the stored value. **Stop** clears every value the timeline set.
- **Cue actions:** `set` changes a stored value (an edit: one undo step per cue, saved with the
  project). `live` holds a live value until Stop. `launch` plays a slot on layers: it sets their
  `target` parameter, as an edit.
- **Offline** (`atidarla render`), every action is held as a live value, so a frame depends only on
  the cues before its time ([`render.md`](render.md)).

## The panel

| Area | What it does |
|---|---|
| Transport | **Stop** (back to the start, or the loop start; clears values), **Play/Pause** (also Space over the tracks), **Go** (fires the next manual cue), timecode `hh:mm:ss:ff` at the timeline's frame rate, then seconds |
| Loop, Length | Turns the loop region on and sets its start and end; the timeline's length (playback pauses there) |
| Drive / Follow Stage | *Drive*: cues launch clips and columns. *Follow*: launches on the Stage move the cue list. The next cue that launches what the Stage just launched fires (its other actions run, its launch is not repeated), and the playhead jumps to it |
| Clock | *Internal clock*, or *Chase* a Timecode In node (`sig.timecode_in`, MTC or LTC). Shows the chase state (no code, locking, locked, freewheel) and an offset: timeline time = timecode + offset |
| Key at playhead | With a node selected in the Patch: pick a parameter (or one component of a colour or vector) and add a key with its current value, snapped to a frame. A key already at that time takes the new value |
| Marker, Cue, Manual cue | Add at the playhead (a manual cue has no time) |
| Ruler | Click or drag to scrub. Scrubbing fires no cue |
| Tracks | One row per (node, parameter, component), with its curve. Click a key to select it; drag it in time; Delete or Backspace removes it; right-click for interpolation (step, linear, bezier) or delete; right-click a track name to delete the track. Scroll to pan, Cmd/Ctrl + scroll to zoom |
| Key | The selected key's time, value and interpolation; Bezier handles `[seconds, value]` relative to the key |
| Cues | Name, time (or manual), condition, status (`>` current, `done` fired, `armed` waiting for its condition) and actions. `+ set` and `+ live` add the selected node's picked parameter at its current value; `+ launch` adds the selected layer (a node with `target`) to the cue's launch of a slot, so a column is one action over several layers |
| Markers | Name, time, Go to, delete |

Timeline edits mark the project as unsaved; **Save** writes `timeline.toml`, including the Stage
mode and the chased clock with its offset. Offline renders (`atidarla render`) always drive the
Stage from cues.

## Curves

`interp` on a key shapes the segment to the next key:

- `step` holds the key's value until the next key;
- `linear` (the default) ramps;
- `bezier` is a cubic through the key's `out` handle and the next key's `in` handle. With no
  handles it eases in and out (smoothstep). Handle times are kept inside the segment, so the curve
  stays a function of time; values may overshoot.

Before the first key a track holds the first value; after the last, the last. Integer parameters
round the value. Toggles are on at 0.5 and above. A track can animate a number, an integer, a
toggle, or one component of a colour or vector; the components without a track follow the
parameter's binding or expression if it has one, else keep their stored value (or one held by a
cue); see [Vectors and colours](expressions.md#vectors-and-colours-prd-a66). Text, menus and paths
change through cues only.

## Cues

- A **timed** cue fires once when the playhead crosses its time while playing, whatever the frame
  rate. Playing on after a pause or a scrub fires a cue right at the playhead. A loop fires the
  cues inside it again on every pass. Scrubbing fires nothing.
- A **manual** cue (no `t`) fires with **Go**, in cue-list order.
- A **condition** (`when`) uses the native expression subset ([`expressions.md`](expressions.md)):
  numbers, `time` (timeline seconds), `frame`, `op('name')['channel']` for a SIG channel,
  `op('name').par.param` (or `.par.param[i]` for a component) for a parameter's value as its
  binding or expression drives it, else its stored value (live values are left out; see
  [Vectors and colours](expressions.md#vectors-and-colours-prd-a66)), comparisons, `and`, `or`.
  For example `op('beat1')['beat'] > 0`. A timed cue with a condition is *armed* at its time and fires on the
  first frame its condition holds. A manual cue with a condition fires whenever it first holds
  while playing, or with Go. The nodes a condition names are cooked every frame, even when nothing
  is wired to them. Conditions read the channels of the previous frame. A condition outside the
  native subset is reported, and its cue never fires.
- When several cues hold a live value on the same parameter, the last fired wins. A track on that
  parameter wins over all of them.

## Timecode chase

Choose *Chase \<node\>* with a Timecode In node in the patch (it reads MTC from a MIDI port, or LTC
from an audio input, on its own thread). While the code runs:

| Behaviour | Value |
|---|---|
| Lock | After 2 consistent readings: MTC locks within 3.75 frames of the first quarter frame (24, 25 and 30 fps), LTC within 2.1–2.5 frames (48 kHz and 44.1 kHz audio in 60 Hz blocks); then tracks within one frame (tests in `crates/atd-timeline/src/chase.rs`) |
| Dropout | A gap of more than 3 frames switches to freewheel: the playhead runs on the local clock for 12 more frames, then waits |
| Re-lock | Code that resumes where the freewheel expects it re-locks on its first reading; code that jumped (the source seeked) re-locks at the new place after 2 readings, and the playhead jumps there without firing the cues in between |
| Offset | Seconds added to the timecode (for example −3600 to start an `01:00:00:00` show at 0) |

`atd_timeline::MtcInput` and `LtcInput` feed the same chase from raw quarter-frame bytes or audio
blocks, for hosts that read the devices themselves.

## `timeline.toml`

A project without the file has no timeline and loads unchanged. Saving writes it in canonical
form: tracks sorted by (node, parameter, component), keys by time, markers by time, cues by time
with manual cues last. Each key and each cue action is one line, so a change is a one-line diff.
`atidarla check` verifies the form and that every track and cue binds to the network.

```toml
# Atidarla timeline. Times are in seconds; tracks and cues refer to node IDs.
format = 1
length = 32.0          # seconds
fps = 60.0             # display, snapping and timecode
loop = [8.0, 16.0]     # optional loop region
stage = "follow"       # "drive" (default, not written) or "follow"

[clock]                # optional: chase timecode instead of the internal clock
node = 12              # the Timecode In node
offset = -3600.0       # timeline time = timecode + offset (default 0)

[[track]]
node = 2               # stable node ID
param = "rotation"
keys = [
    { t = 0.0, v = 0.0, interp = "bezier" },
    { t = 8.0, v = 90.0, interp = "bezier", out = [2.0, 40.0] },
    { t = 16.0, v = 360.0, in = [-1.0, 0.0] },
]

[[track]]
node = 3
param = "colour"
component = 0          # one channel of a colour or vector
keys = [
    { t = 0.0, v = 0.9 },
]

[[marker]]
id = 1                 # marker and cue IDs share one sequence
t = 8.0
name = "Build"

[[cue]]
id = 2
name = "Chorus"
t = 16.0               # leave out for a manual cue
when = "op('beat1')['beat'] > 0"
actions = [
    { launch = 1, layers = [7, 8] },
    { node = 4, param = "opacity", live = 0.5 },
    { node = 4, param = "operation", set = "add" },
]
```

Tracks and actions on nodes that no longer exist stay in the file. The panel shows them in red
and lists why they are skipped. Tracks on a Pack instance's parameters are not supported yet.
