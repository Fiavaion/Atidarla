# 5. Expressions, channels and bindings

A parameter does not have to be a fixed number. Three things can drive it. This chapter covers
the first two. Chapter 7 covers the third.

| Driver | What it is | Where you set it |
|---|---|---|
| **Binding** | The parameter follows a **channel** of a SIG node | The **bind...** menu next to the parameter |
| **Expression** | The parameter follows a formula | The **=** button next to the parameter |
| **Live value** | A control, the Timeline or a mapping writes the value for a while | MIDI/OSC mappings, tracks and cues (chapters 3 and 7) |

## Channels

A **channel** is a named number that changes over time. SIG nodes output channels. For example
an [LFO](../operators/sig/lfo.md) has a channel called `lfo`, a [Beat](../operators/sig/beat.md)
node has `beat`, `beat_phase`, `bar` and others, and
[Spectrum](../operators/sig/spectrum.md) has frequency bands. A node's page in the
[operator reference](../operators/README.md) lists its channels under **Outputs**.

A channel is written `node:channel`, for example `lfo1:lfo`.

## Bindings: follow a channel

To make a parameter follow a channel:

- Open the **bind...** menu next to the parameter and choose the channel. Or
- drag a wire from a SIG node's output and drop it on the parameter's **name**. Or
- drag a channel from the channel viewer onto the parameter's name.

In chapter 2 you bound the noise's **Z** to `lfo1:lfo`. Remove a binding from the same menu.

Vectors and colours can be bound too, for example a [Transform](../operators/img/transform.md)'s
**Scale**. One channel drives every component: bind **Scale** to `lfo1:lfo` and the image grows
and shrinks evenly. A colour's channel drives red, green and blue; its alpha keeps the value you
set. To drive the components one each, bind to the whole node (the entry marked
*per component* in the **bind...** menu): when the node has at least as many channels as the
parameter has components, its first channel drives the first component, the second the second,
and so on (a colour takes three: red, green, blue). With fewer channels its first channel drives
them all.

To shape the signal on its way
(scale it, smooth it, limit it, add a delay), put SIG nodes between the source and the
parameter: [Math](../operators/sig/math.md), [Lag](../operators/sig/lag.md),
[Filter](../operators/sig/filter.md), [Limit](../operators/sig/limit.md).

## Expressions: follow a formula

Any number, toggle, vector or colour parameter can follow an expression. Click **=** next to
the parameter and type. The panel shows the current result. Errors appear at the top of the panel, and while an
expression fails the parameter uses its stored value.

Examples:

```text
6 + 2 * floor(time / 4 % 3)           # Segments steps 6, 8, 10, 6, ...
lerp(0.2, 1.0, op('lfo1')['lfo'])     # blend between 0.2 and 1.0 using a channel
op('level1').par.contrast * 0.5       # half of another node's Contrast
```

(The first is from the example
[`kaleidoscope-tunnel`](../../examples/kaleidoscope-tunnel.atidarla/).)

On a vector or colour the result drives every component, as a binding does: `1 + 0.25 * sin(time)`
on a Transform's **Scale** pulses the image evenly, `0.5` on a colour makes it mid-grey with its
alpha unchanged. A Python expression may instead give one number per component, as a tuple or
list: `(1 + 0.25 * sin(time), 1)` stretches only across. The tuple must have as many numbers as
the parameter drives (three for a colour); otherwise it is an error and the stored value is used.

### The two tiers

**Native.** Expressions written in the subset below are calculated in Rust on every frame,
before their node cooks, without using extra memory. Use these for anything live.

**Python.** Anything else is treated as Python. It runs on a Python worker thread and its result
arrives one frame late. The picture never waits for it. Python sees the same names as native
expressions plus the rest of the language and Python's `math` functions.

Every native expression is also valid Python and gives the same answer in both tiers.

### The native subset

| Write | Meaning |
|---|---|
| `1.5`, `1_000`, `2e-3`, `True`, `False`, `pi`, `e`, `tau` | Numbers and constants |
| `+ - * / // % **` | Arithmetic, with Python's rules |
| `< <= > >= == !=` | Comparisons (give 1 or 0); they can be chained: `a < b < c` |
| `and`, `or`, `not`, `a if c else b` | Logic |
| `abs min max round int float bool pow` | Built-in functions |
| `sqrt exp log log2 log10 sin cos tan asin acos atan atan2 hypot degrees radians floor ceil trunc` | Maths |
| `clamp(x, low, high)`, `lerp(a, b, t)` | Atidarla's own helpers |
| `time`, `frame` | Seconds and frames since start. The node then cooks every frame |
| `me.digits` | The number at the end of the node's name (`level3` gives 3) |
| `op('name').par.param`, `op('name').par.param[1]` | Another node's parameter, or one component of a vector or colour |
| `op('name')['channel']` | The current value of a SIG node's channel |

Mistakes are errors, not silent zeros: dividing by zero, `sqrt(-1)`, `log(0)` and an unknown
name all report an error.

### Things to know

- **Renaming a node breaks expressions that name it.** `op('level1')` stops working if you
  rename `level1`. MIDI/OSC mappings and the Timeline refer to nodes by stable ID and are not affected.
- Nodes named in `op('...')` are *dependencies*: they cook first.
- A loop (two parameters reading each other) is reported as an error. A loop through channels
  reads the previous frame's value.
- In a project file an expression is saved with the value it falls back to:
  `params.segments = { value = 8, expr = "6 + 2 * floor(time / 4 % 3)" }`.
- **Safety.** A Python expression is a program. Opening a project never runs one: the app asks
  first, in the same dialog as Python operators and Script nodes
  ([chapter 6](06-kernels-python-packs.md)), and lists each Python expression. Until you allow
  it, the parameter keeps its saved value and the node says "Python expressions in this project
  are not allowed to run"; native expressions keep working. "Allow and remember" remembers the
  `ops/` files only, so a project with Python expressions asks every time it opens. Typing a
  Python expression into a new project, or one you have allowed, works straight away. Headless,
  `atidarla check`, `render`, `serve` and the other commands run them only with
  `--allow-python-ops`.

Full reference: [`docs/expressions.md`](../expressions.md). Another example:
[`live-control`](../../examples/live-control.atidarla/) uses a Python expression on Contrast.

## Which should I use?

- A parameter should follow a **sound, a clock, a controller or another node's output**: use a
  binding.
- A parameter should follow a **calculation** (steps, a formula, another parameter): use an
  expression.
- A parameter should be **played from a knob or fader**: use a MIDI/OSC mapping
  ([chapter 7](07-live-io.md#midi-and-osc)).
- A parameter should **change on a schedule**: use the Timeline
  ([chapter 3](03-views.md#timeline-keyframes-and-cues)).
