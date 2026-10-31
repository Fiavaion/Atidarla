# Parameter expressions

Any number, toggle, vector or colour parameter can follow an expression instead of a value:
click **=** next to it in the Parameters panel and type. The panel shows the current result; errors
appear at the top of the panel, and while an expression fails the parameter uses its stored
value. In a project file an expression is saved with the value it falls back to:

```toml
params.segments = { value = 8, expr = "6 + 2 * floor(time / 4 % 3)" }
```

## Two tiers (PRD A22)

**Native.** An expression written in the subset below is evaluated in Rust on every frame,
before its node cooks, without allocating memory. Use it for anything that runs live.

**Python.** Anything else is Python, evaluated on the Python worker thread. Its result is
used from the next frame on (one frame late), and the render thread never waits for it.
Python sees the same names as the native tier, the rest of the language, and the functions of
Python's `math` module.

Every native expression is also valid Python and gives the same result in both tiers (a test
compares them on a corpus of expressions, errors included).

## The native subset

| Write | Meaning |
|---|---|
| `1.5`, `1_000`, `2e-3`, `True`, `False`, `pi`, `e`, `tau` | Numbers and constants |
| `+ - * / // % **` | Arithmetic, with Python's rules (`-7 // 2` is −4, `-7 % 3` is 2, `-2 ** 2` is −4) |
| `< <= > >= == !=`, `a < b < c` | Comparisons (1 or 0), chained as in Python |
| `and`, `or`, `not`, `a if c else b` | Logic; `and`/`or` give back an operand, as in Python |
| `abs min max round int float bool pow` | Built-ins; `round` rounds halves to even, like Python |
| `sqrt exp log log2 log10 sin cos tan asin acos atan atan2 hypot degrees radians floor ceil trunc` | Maths (`log(x, base)` too) |
| `clamp(x, low, high)`, `lerp(a, b, t)` | Atidarla additions (also defined for Python) |
| `time`, `frame` | Seconds and frames since start. The node then cooks every frame |
| `me.digits` | The number at the end of the node's name (`level3` → 3) |
| `op('name').par.param`, `op('name').par.param[1]` | Another node's parameter, or one component of a vector or colour |
| `op('name')['channel']` | The current value of a SIG node's channel |

Nodes named in `op('…')` are dependencies: they cook first, and a change to them recooks the
expression's node. A reference loop (two parameters reading each other) is reported as an
error; a loop through channels reads the previous frame's value.

Errors follow Python: dividing by zero, `sqrt(-1)`, `log(0)` and a name that does not exist
are errors, not silent zeros. Renaming a node breaks expressions that name it, as in
TouchDesigner.

## Vectors and colours (PRD A66)

An expression on a vector or colour gives one number, and that number drives every component:
`1 + 0.25 * sin(time)` on `img.transform`'s `scale` scales both axes. On a colour it drives red,
green and blue; alpha keeps its stored value. A vector's first four components are driven, any
further ones keep their stored values.

A **Python** expression may give a tuple or list instead, one number per component:
`(1 + 0.25 * sin(time), 1)` on `scale`, `(1, 0.5 * op('lfo1')['lfo'], 0)` on a colour. It must
have exactly as many numbers as the parameter drives (a colour: three); any other length, or a
tuple on a number parameter, is an error and the stored value is used. The native tier gives
single numbers only.

Channel bindings follow the same rule. A binding to one channel (`lfo1:lfo`) drives every
component. A binding to a whole SIG node (no channel name) drives one component per channel
when the node has at least as many channels as there are components (channel 0 → component 0,
and so on); with fewer, its first channel drives them all.

`op('name').par.param[i]` reads component *i* as driven, so other expressions, Python's
`op('name').par.param` tuple and Timeline tracks that key only some components (the others take
the driven value) all see what the node cooks with. A MIDI or OSC mapping to one component, or an
OSCQuery route given fewer numbers than components, likewise leaves the other components following
the binding or expression.

In a project file nothing new is written: a driven vector is the same inline table as a number,
with the vector it falls back to:

```toml
params.scale = { value = [1.0, 1.0], expr = "1 + 0.25 * sin(time)" }
params.colour = { value = [0.0, 0.0, 0.0, 1.0], bind = { node = 3, channel = "" } }
```

## Python

```python
[1.0, 1.25, 1.5][int(time) % 3]        # a list
sum(op('lfo1')['lfo'] for _ in range(3))
me.name.upper().count('L')
```

`op('name').par.size` is a tuple for vectors; `me.name` is the node's name.

## Examples

- [`kaleidoscope-tunnel`](../examples/kaleidoscope-tunnel.atidarla/): a native expression on Segments.
- [`live-control`](../examples/live-control.atidarla/): a Python expression on Contrast.
