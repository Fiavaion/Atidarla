# MIDI and OSC mappings

Any number, toggle, menu, colour or vector parameter can follow a MIDI control or an OSC address (FR-S5). A mapping sets a **live value**: it wins over the parameter's binding, expression and stored value while the control moves, it is not an edit (no undo step), and the next frame shows it (PT-8: at most 2 frames, measured at 1 frame on an M5 Pro). Decision A54 in `PRD.md` explains the model.

## Learn a mapping

1. Right-click a parameter's name in the Parameters panel and choose **Learn MIDI/OSC**. For a colour or vector choose a component (x / red … w / alpha) or **Learn all components**.
2. Move a knob, fader or pad, or send an OSC message. The first control that arrives becomes the mapping, and the Mappings window opens.

A dot before the parameter name shows it is mapped (blue) or waiting for a control (amber; right-click to cancel). Learn picks defaults from the parameter: its range as the output range, **toggle** mode for toggles, **momentary** for pulses, a **step** curve with one level per option for menus. Note-offs are not learned (the note-on is).

## The Mappings window

**View → Mappings**. At the top: the OSC port the project listens on, OSCQuery on/off and its HTTP port, and the MIDI inputs that are open (**rescan** opens devices plugged in since). Then one row per mapping:

| Setting | Meaning |
|---|---|
| source → target | The control and the parameter (`node.param`, `[n]` for a component). A red target means the node is no longer in the patch |
| in | The control's range (default: 0–127 for CC and notes, −8192–8191 for pitch bend, 0–1 for OSC). Values outside are clamped |
| out | The parameter values the input range maps to. Reverse it (1 → 0) to invert |
| curve | **linear**; **exponential** with an amount (above 0 starts slow, below 0 starts fast); **step** with a number of levels |
| mode | **absolute**: the control's position sets the value. **toggle**: each press (a note-on, or a CC rising past the middle of its input range) flips between the two ends of the output range. **momentary**: the high end while held, the low end when released. **relative**: an endless encoder; each message is a step added to the current value (MIDI two's complement: 1–63 up, 65–127 down; OSC: the signed step), scaled so the input range's width is one full sweep of the output range |
| pick-up | Soft takeover for absolute controls: after the parameter changed elsewhere (or on first use), the control does nothing until it reaches the parameter's value (within 2 % of the output range) or passes it |
| delete | Removes the mapping |

The **input monitor** lists the last 64 MIDI and OSC messages with their age, whether or not they are mapped.

## Saving

Mappings are part of the project (`mappings.toml`, below) and count as unsaved changes. Live values themselves are never saved, but **saving stores the value each mapping last wrote as its parameter's value**, only in the components mappings drive (one undo step, "Store mapped values"; bindings and expressions stay), so a project reopens looking the way the controls left it. Live values from the timeline or OSCQuery are not stored. The edit is made only after the project files were written: a save that fails changes nothing. Opening another project clears all live values.

Deleting or retargeting a mapping releases its parameter: the live value goes, unless another mapping still drives the parameter or another writer (the timeline, OSCQuery) has set it since. The timeline likewise clears on Stop only the values it still owns.

## OSCQuery

Turn on **OSCQuery** in the Mappings window (saved with the project), or start the app with `--oscquery`. Lasair then:

- answers HTTP on the OSCQuery port (default 9001) with the [OSCQuery](https://github.com/Vidvox/OSCQueryProposal) namespace: `GET /` gives one container per node and one entry per parameter with `FULL_PATH`, `TYPE`, `VALUE`, `RANGE`, `ACCESS` and `DESCRIPTION`; `GET /node/param` one entry; `?VALUE` (or another attribute) just that attribute; `GET /?HOST_INFO` the project name and OSC port;
- announces `_oscjson._tcp` (the HTTP server) and `_osc._udp` (the OSC port) over mDNS, so controller apps find it on the network;
- sets a parameter when OSC arrives at its address, `/<node name>/<param>` (all arguments for a colour or vector), on the project's OSC port (default 9000).

Text and file parameters are advertised read-only. Values are current when asked: a request makes the app publish them on its next frame. `LISTEN` (WebSocket streaming) is not offered.

A value set over OSCQuery is a live value too: it holds until another writer changes the parameter or another project is opened, and it is not written back on save. Messages to one address between two frames give one write, the latest. A colour or vector sent fewer numbers than it has components holds those, and its other components keep following their binding or expression on every frame until the next message.

## `mappings.toml`

A project without mappings has no file, and opens as before. The file is text with stable IDs, written in one order, so it diffs line by line:

```toml
# Lasair mappings: MIDI and OSC controls to parameters (docs/mappings.md).
# Targets refer to node IDs in networks/main.toml.
format = 1
next_id = 2

[osc]
port = 9000
oscquery = true
oscquery_port = 9001

[[mapping]]
id = 0
source = { midi = "cc", number = 7, channel = 1, device = "nanoKONTROL" }
target = { node = 3, param = "colour", component = 0 }
input = [0.0, 127.0]
output = [0.0, 1.0]
curve = "linear"
mode = "absolute"
pickup = false

[[mapping]]
id = 1
source = { osc = "/fader/1", arg = 0 }
target = { node = 4, param = "gamma" }
input = [0.0, 1.0]
output = [0.5, 2.0]
curve = { exp = 2.0 }
mode = "relative"
pickup = true
```

- `source`: `midi = "cc" | "note" | "bend"`, `number` (0–127, for CC and notes), `channel` (1–16; leave it out for any channel), `device` (case-insensitive part of the input's name; leave it out for any device). Or `osc = "/address"` with `arg`, the argument index among the numeric arguments (0 = first).
- `target`: `node` is the node's ID in `networks/main.toml` (renaming a node changes nothing here), `param` its parameter, `component` for colours and vectors.
- `curve`: `"linear"`, `{ exp = <amount> }` or `{ step = <levels ≥ 2> }`.
- Hand-written files may leave out `input` (the source's range), `output` (0–1), `curve` (linear), `mode` (absolute), `pickup` (false), `next_id` and `[osc]` (port 9000, OSCQuery off on 9001). Unknown keys are errors.

## Notes

- MIDI inputs open when the project has a MIDI mapping, a learn starts or the Mappings window opens, and stay open. On Windows a MIDI input can be open in only one place at a time, so a MIDI In operator on the same device may fail after that.
- The OSC socket listens on every network interface; your firewall may ask about it the first time.
- Mappings target the main network (`networks/main.toml`).
