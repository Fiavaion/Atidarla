# 7. Live input and output

This chapter is about connecting Atidarla to the world: sound, controllers, other software,
lights, and projectors.

All of the input and output nodes do slow work on their own threads, so a busy device never
stops the picture.

## Audio

| Node | What it does |
|---|---|
| [Audio In](../operators/sig/audio_in.md) | Live sound from an input device (the default one, or `test-tone`) as audio channels |
| [Audio File In](../operators/sig/audio_file_in.md) | Plays a WAV file |
| [Audio Out](../operators/sig/audio_out.md) | Plays audio channels on an output device |
| [Spectrum](../operators/sig/spectrum.md) | Turns audio into frequency bands and a level |
| [Envelope](../operators/sig/envelope.md) | Follows the loudness of the sound |
| [Onset](../operators/sig/onset.md) | Detects hits |
| [Beat](../operators/sig/beat.md) | A tempo clock with `beat`, `bar` and pulse channels; tap tempo, nudge, resync, MIDI clock in and out |

To make a picture follow sound: add Audio File In (or Audio In), wire it into Spectrum, then
**bind** a parameter to one of Spectrum's bands (chapter 5). The Learn quest
[Make a picture follow sound](../../learn/quests/02-audio-reactive.toml) does exactly this and
builds a feedback loop on top. Finished examples:
[`audio-reactive-feedback`](../../examples/audio-reactive-feedback.atidarla/),
[`beat-clock`](../../examples/beat-clock.atidarla/).

## MIDI and OSC

Any number, toggle, menu, colour or vector parameter can follow a MIDI control or an OSC
address. A **mapping** sets a live value: it wins over the binding, expression and stored value
while the control moves, and it is not an edit (no undo step). Reference:
[`docs/mappings.md`](../mappings.md). The Learn quest
[Control it with MIDI or OSC](../../learn/quests/07-mapping.toml) takes about 12 minutes.

**Learn a mapping:**

1. Right-click a parameter's name in the Parameters panel and choose **Learn MIDI/OSC**. (For a
   colour or vector choose a component, or **Learn all components**.)
2. Move a knob, fader or pad, or send an OSC message. The first control that arrives becomes
   the mapping, and the Mappings window opens.

A dot before the parameter name shows it is mapped (blue) or waiting for a control (amber).

**View, Mappings** lists every mapping. For each one you can set the input range, the output
range (reverse it to invert), a curve (linear, exponential or stepped) and a mode:

- **absolute**: the control's position sets the value;
- **toggle**: each press flips between the two ends;
- **momentary**: the high end while held;
- **relative**: an endless encoder adds steps.

**Pick-up** (soft takeover) makes a control do nothing until it reaches the parameter's current
value, so a fader does not make the value jump. The **input monitor** shows the last 64 MIDI and
OSC messages.

Saving stores the value each mapping last wrote as the parameter's value, so the project
reopens looking the way your controls left it. Mappings are saved in `mappings.toml`.

**Other nodes:** [MIDI In](../operators/sig/midi_in.md) and
[MIDI Out](../operators/sig/midi_out.md); [OSC In](../operators/sig/osc_in.md) and
[OSC Out](../operators/sig/osc_out.md); [Input Devices](../operators/sig/input.md) (mouse over
the Viewer, keyboard, gamepads); [Timecode In](../operators/sig/timecode_in.md) (MIDI timecode
or LTC).

**OSCQuery.** Turn it on in the Mappings window (or start the app with `--oscquery`). Other
software on your network can then discover every parameter, read it and set it by OSC at
`/<node name>/<param>` on the project's OSC port (default 9000). Atidarla announces itself over
mDNS.

Notes: the OSC socket listens on every network interface, so your firewall may ask about it. On
Windows a MIDI input can be open in only one place at a time.

## DMX and lights

Atidarla sends DMX512 over Ethernet, as Art-Net or sACN. Two nodes do it:

- [DMX Out](../operators/sig/dmx_out.md) sends channels (0 to 1) to the slots of one universe.
- [Pixel Map](../operators/img/pixel_map.md) samples regions of a picture for fixtures and LED
  pixels through a **patch file**, and can send every universe itself.

The patch file is a short text file that says where each fixture lives on the picture, which
universe and address it uses, and what type of fixture it is. Built-in fixture types include
`rgb`, `rgbw`, `pixel-grb` (WS2812), `pixel-rgbw` and `moving-head`. You can add your own
in the project's `fixtures/` folder. Full guide: [`docs/dmx.md`](../dmx.md). The Learn quest
[Drive lights over DMX](../../learn/quests/08-dmx.toml) takes about 20 minutes. Example:
[`pixel-map`](../../examples/pixel-map.atidarla/).

DMX trails the picture by one or two frames. Values are 8-bit. You can test without lights: any
Art-Net or sACN monitor on the network, or on your own computer with Host `127.0.0.1`, shows
the output.

## Sharing pictures with other software

[**Share Out**](../operators/img/share_out.md) publishes a picture to other programs on the same
computer. [**Share In**](../operators/img/share_in.md) receives one.

| System | Uses |
|---|---|
| macOS | Syphon (needs the Metal backend, the default) |
| Windows | Spout (needs the DirectX 12 backend, the default) |

Elsewhere the nodes report an error. Receivers get an 8-bit picture at the input's size. Name
a Share Out something unique so the right sender is picked. Example:
[`texture-sharing`](../../examples/texture-sharing.atidarla/).

**NDI** (pictures over a network) is **not built yet**. The crate `atd-plugin-ndi` is a
placeholder. NDI is planned as an optional plugin that loads the NDI library at run time. The
same is true of tempo sharing between programs over a network (`atd-plugin-link`).

## Movies, stills and recording

- [Movie In](../operators/img/movie_in.md) plays HAP, HAP Alpha, HAP Q and HAP R `.mov` files
  (decoded on the GPU) and PNG/JPEG stills. Other video needs a build with FFmpeg
  ([chapter 1](01-install.md)).
- The Viewer has **Record**. Choose a `.mov` (ProRes) or `.mp4` (H.264) file and it records
  what the Viewer shows at 60 frames a second until you press **Stop**. View the Mixer to
  record a whole composition. Needs a build with FFmpeg. If the computer cannot keep up, frames
  are dropped and counted in the status.
- For exact, repeatable movies use an offline render (chapter 13).

## Outputs: projectors and mapping

An output is a [**Window**](../operators/cmp/window.md) node. Wire your finished picture into
its `in`. It shows that picture in a window on the display you choose. Add as many Window
nodes as you have projectors or screens. Guide: [`docs/outputs.md`](../outputs.md). There is
no Learn quest for projectors yet. Example: [`two-outputs`](../../examples/two-outputs.atidarla/).

Main Window parameters:

| Parameter | What it does |
|---|---|
| Enabled | Opens the window. Closing the window turns this off |
| Fullscreen, Display | Borderless fullscreen on display *n* (0 is the first) |
| Width, Height | The resolution of the mapped image. Match the display |
| Test Pattern | Grid, bars, gradient or circles, to line up projectors |
| Slices | The mapping, as text |

The **Outputs** tab (next to Patch and Stage) edits the slices with your mouse. Each **slice**
takes a rectangle of your picture and places it in the output through a **warp**:

- `perspective`: a corner pin, for a keystoned projector;
- `bilinear`, `catmull_rom` (smooth, for curved screens) and `bezier` for bent surfaces;
- **soft edges** fade the overlap between two projectors so the sum stays even;
- **masks** black out areas outside a polygon.

Drag the control points over the preview. Every drag is an undoable edit. **Match display** sets
the resolution to the display's.

Limits: 10-bit and HDR output are not available yet. Displays are chosen by number, not name.
Each window updates at the app's own frame rate.
