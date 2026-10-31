# 3. The three views

Atidarla has three ways to work on one project: **Patch**, **Stage** and **Timeline**. They are
views of the same graph. There is no second copy of your work. Change a parameter in one view
and the others see it. A fourth tab, **Outputs**, sets up projectors and screens.

Open any view from the **View** menu.

## Patch: the node canvas

The Patch is where you build. You added nodes, wired them and changed parameters in
[chapter 2](02-first-patch.md). A few more things you will use all the time:

- **Wire types.** A wire only joins sockets of the same type (IMG to IMG, SIG to SIG, and so
  on). Atidarla refuses a mismatched wire, and refuses a wire that would make a loop, except
  into a socket made for loops such as the `loop` input of [Feedback](../operators/img/feedback.md).
  See [chapter 4](04-operator-families.md).
- **Parameters** can be a number you set, a **binding** to a signal, or an **expression**
  ([chapter 5](05-expressions-bindings.md)).
- **Comment boxes** keep a patch readable. Select nodes and press C.
- **Everything is undoable.** The Edit menu shows what Undo and Redo will do.
- **Node names** are stable. Renaming a node does not break the project file, but it does break
  any expression that names it (chapter 5).

## Stage: clip decks

The Stage is the performance view: a grid of **clips** you launch live. A clip is any IMG
node. The Stage reads a **deck** that you build in the Patch.

### Build a deck

1. Add a **Mixer** ([`img.mixer`](../operators/img/mixer.md)) from the IMG group. A Mixer is the
   deck. It composites up to 16 layers, from `layer0` at the bottom upwards.
2. Add a **Layer** ([`img.layer`](../operators/img/layer.md)) and wire its `out` into the
   Mixer's `layer0`. A layer plays one clip at a time.
3. Wire any IMG nodes into the layer's `clip0`, `clip1`, ... inputs. These are the clips. Slot 0
   is the first column of the Stage grid.
4. Open the **Stage** tab. The grid shows one row per layer and one cell per clip.

The Learn quest [Your first clip deck](../../learn/quests/03-first-clip-deck.toml) walks
through exactly this. The example
[`stage-deck`](../../examples/stage-deck.atidarla/) is a finished deck of three layers by four
columns. If the Stage shows "No deck yet", there is no Mixer in the project.

### Playing the Stage

| Control | What it does |
|---|---|
| Click a cell | Launches that clip. The cell shows `>` while it plays and `...` while it waits for the next beat or bar. The colour helps, but the mark carries the meaning. |
| **Column N** buttons | Launch a whole column, on every layer, as one undo step |
| **Stop** (per layer), **Stop all** | Stop a layer, or the whole deck |
| **Crossfader A/B** | Fades between the layers in crossfader group A and group B. Set a layer's group on the Mixer (`group0`, `group1`, ...) |
| **Master** | Fades the whole composite to black |
| Autopilot | Shows the Mixer's autopilot mode. Set it on the Mixer: it can launch the next or a random column by itself every N beats or when the longest clip ends |
| Beat | The deck's clock position |

Launching a clip is an ordinary, undoable edit of the layer's `target` parameter. It is saved
with the project.

### Launch modes, transitions and timing

Set these on the Layer node:

- **Transition**: `cut`, `fade` or `wipe`, and **Transition Length** in beats or seconds.
- **Quantise**: `off`, `beat` or `bar`. With a clock wired into the layer's `clock` input (for
  example a [Beat](../operators/sig/beat.md) node), a launch waits for the next beat or bar.
  Without a clock the layer assumes 120 BPM for transition lengths and switches at once.
- **Slots**: one line per clip, `slot,mode,beats,label`. The modes are `trigger` (the default),
  `toggle` (launching again stops it), `flash` (plays while you hold the cell) and `one_shot`
  (plays for its length in beats, then returns). Example: `2,one_shot,4,Strobe hit`.

Clips that are not showing do not cook, so a clip with memory (a feedback loop, a simulation, a
movie) pauses while hidden.

Autopilot, one-shot returns and flash releases are app behaviour. They do not run in an offline
render ([chapter 13](13-command-line.md)).

### The media bin

Under the clip grid the Stage shows the **Media Bin**: images made by batch runs. Select one
and press **Use as clip**, or drag it onto an empty cell. See
[chapter 12](12-batch-media-bin.md).

## Timeline: keyframes and cues

The Timeline animates parameters over time and fires cues. It is a view over the same graph. It
owns no media. Full reference: [`docs/timeline.md`](../timeline.md). Try it with
[`timeline-show`](../../examples/timeline-show.atidarla/).

### Keyframes

1. Select a node in the Patch.
2. In the Timeline, use **Key at playhead**, pick a parameter, and add a key. The key holds the
   parameter's current value, snapped to a frame.
3. Move the playhead (click or drag the ruler), change the parameter, and add another key.
4. Press **Play** (or Space over the tracks).

Right-click a key to choose how the curve leaves it: `step`, `linear` or `bezier`. Drag a key to
move it in time. Delete or Backspace removes it.

A track writes a **live value**. It wins over a binding, an expression and the stored value, but
it is not an edit: it takes no undo step and it is never saved into the parameter. **Stop**
clears every live value the timeline set.

### Cues and markers

- A **marker** is a named point in time.
- A **cue** fires actions when the playhead reaches its time: `set` (change a stored value, one
  undo step), `live` (hold a live value until Stop) and `launch` (play a Stage slot on layers).
- A **manual cue** has no time. It fires when you press **Go**.
- A cue can have a **condition**, written like an expression, for example
  `op('beat1')['beat'] > 0`.
- **Drive / Follow Stage**: in *Drive*, cues launch Stage clips. In *Follow*, launches on the
  Stage move the cue list.

### Clock and timecode

The Timeline can run on its own clock, or **Chase** a **Timecode In** node that reads MIDI
timecode (MTC) or LTC. It locks within a few frames, freewheels through short dropouts, and
takes an offset in seconds.

The timeline is saved as `timeline.toml` in the project folder when you save.

## Outputs: projectors and screens

The **Outputs** tab edits the slices, warps, masks and soft edges of **Window** nodes. It is
covered in [chapter 7](07-live-io.md#outputs-projectors-and-mapping).

## Kiosk mode

For a show, run the app full screen with only the Viewer. See
[chapter 10](10-shows-reliability.md#kiosk-mode-and-the-supervisor).
