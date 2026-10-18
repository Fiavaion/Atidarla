# Kernel DSL reference

> **Status: experimental, decision A12 is still Proposed (PRD §0).** The compiler lives in
> the self-contained crate `crates/atd-kernel`. No operator uses it yet and nothing is
> registered; removing the crate removes the feature. See
> [`docs/milestones/m2/kernel-dsl.md`](milestones/m2/kernel-dsl.md) for the open questions
> and the integration plan.

A kernel is a short program that runs once per element: once per point (target **Points**)
or once per pixel (target **Pixels**). It reads and writes the element's attributes with
`@name`, reads parameters with `ch()`, and calls helpers for noise, maths, matrices,
quaternions, neighbouring points and input images. It is VEX-flavoured (Houdini's wrangle
language) but smaller and stricter, because it must compile to a single WGSL compute or
fragment shader that runs the same on DX12, Metal and Vulkan.

The compiler turns the source into a typed IR, checks it, and lowers it to WGSL. It returns
the WGSL text (shown in the "Generated WGSL" view), a description of the bindings the engine
must provide, the parameters it found, and a source map. Errors carry a line, a column, a
length, a message and often a hint, so an editor can underline them.

```sh
atidarla kernel particles.atk                  # check it, print bindings and parameters
atidarla kernel particles.atk --emit-wgsl      # print the generated WGSL
atidarla kernel grade.atk --target pixels
```

## The ten-line curl-noise particle kernel

This is the kernel of M2's acceptance criterion AC2 (curl noise + attract + age). It
compiles as written (a test extracts every `kernel` block of this page and compiles it).

```kernel points
// Curl noise + attract + age: the M2 AC2 particle kernel.
float freq   = ch("freq", 0.5, 0.0, 4.0);
float amp    = ch("amp", 2.0, 0.0, 10.0);
float pull   = ch("attract", 0.3, 0.0, 5.0);
vec3  target = chv("target", {0, 0, 0});
float dt     = @TimeInc;
v@v += curlnoise(v@P * freq + set(0, 0, @Time * 0.1)) * amp * dt;
v@v += (target - v@P) * pull * dt;
v@P += v@v * dt;
f@age += dt;
```

It binds three point attributes (`P` and `v` read and written, `age` read and written,
created if missing) and creates four parameters (`freq`, `amp`, `attract`, `target`).

## Program structure

A kernel is a list of statements, run top to bottom for each element, plus optional
function definitions anywhere at the top level.

```kernel points
float wave(float x, float k) {
    return sin(x * k) * 0.5 + 0.5;
}

float h = wave(@P.x, ch("k", 3.0));
@P.y = h;
```

Comments are `// line` and `/* block */`. Statements end with `;`. Blocks use `{ }`.
`return;` in the kernel body ends the kernel for this element; attributes written so far
are kept (as in VEX).

## Types

| Type | Also written | WGSL | Notes |
|---|---|---|---|
| `float` | | `f32` | 32-bit IEEE float |
| `int` | | `i32` | 32-bit, wraps on overflow at run time |
| `bool` | | `bool` | `true`, `false`; comparisons give `bool` |
| `vec2` | `vector2` | `vec2f` | |
| `vec3` | `vector` | `vec3f` | |
| `vec4` | `vector4`, `quat` | `vec4f` | a quaternion is a `vec4` (x, y, z, w), w real |
| `mat3` | `matrix3` | `mat3x3f` | column-major |
| `mat4` | `matrix` | `mat4x4f` | column-major |

Strings exist only as literal arguments of `ch()` and `point()`. There are no arrays.

**Implicit conversions** (in declarations, assignments, arguments and returns): `int` →
`float`; `bool` → `int` (0 or 1); a scalar (`int` or `float`) → any vector (all components).
Everything else needs an explicit constructor: `int(x)` truncates toward zero and saturates,
`float(i)`, `bool(x)` is `x != 0`, `vec3(v4)` drops `w`, `vec4(v3, 1)`.

## Variables

```kernel points
float a = 1;            // explicit type; the int converts to float
let   b = a * 2;        // inferred: float
vec3  c;                // no initialiser: zero
int   i = 3, j = i + 1; // several at once
```

A variable is visible from its declaration to the end of its block. An inner block may
declare a name again (shadowing). There are no global variables; every element starts
fresh.

## Literals

`1`, `0x1F` (int, up to 2147483647; `-2147483648` is allowed; a hexadecimal literal up to `0xFFFFFFFF` gives the `int` with those bits, for hash constants such as `0x9E3779B1`), `1.5`, `.5`, `2.`, `1e-3`
(float), `true`, `false`. `{a, b, c}` builds a `vec2`, `vec3` or `vec4` from 2 to 4 scalars
(any expressions, as in a `set()` call). Constants: `M_PI`, `M_TWO_PI`, `M_E`.

## Operators

From lowest to highest precedence:

| Operators | Operands | Result |
|---|---|---|
| `c ? a : b` | `c` a condition; `a`, `b` the same type after conversion | that type (both sides are evaluated) |
| `\|\|`, `&&` | conditions | `bool`, short-circuit |
| `\|`, `^`, `&` | `int` | `int` |
| `==`, `!=` | scalars, `bool`s, or vectors of one size | `bool` (vectors: all components equal) |
| `<`, `<=`, `>`, `>=` | `int` or `float` | `bool` |
| `<<`, `>>` | `int` | `int` (shift count taken modulo 32; `>>` is arithmetic) |
| `+`, `-` | numbers, vectors, matrices (same size) | widened type |
| `*`, `/`, `%` | numbers and vectors; `*` also matrix × vector, vector × matrix, matrix × matrix, matrix × scalar | widened type |
| `-x`, `+x`, `!x`, `~x` | number or vector; condition; `int` | |
| `f(x)`, `v.xyz`, `v[i]` | calls, swizzles, indexing | |

"Widened": `int` with `float` gives `float`; a scalar with a vector gives the vector.
A **condition** (`if`, `?:`, `!`, `&&`, `||`) may be a `bool`, an `int` or a `float`
(non-zero is true), as in VEX.

`/` and `%` on `int` never fault: `x / 0` is `x`, `x % 0` is 0, `MIN / -1` is `MIN`.
`%` on floats is the truncated remainder (like C's `fmod`).

**Swizzles**: `.x .y .z .w` or `.r .g .b .a`, one to four letters (`v.zyx`, `c.rgb`,
`p.xxy`). **Indexing**: `v[i]` reads component `i` (a run-time index is clamped to the
vector); `m[i]` reads column `i` of a matrix. On the left of an assignment only one
component may be named (`@P.y = 0;`, `v[2] += 1;`); assign a whole vector to change several.

Assignment is a statement, not an expression: `=`, `+=`, `-=`, `*=`, `/=`, `%=`, and the
statements `x++;`, `x--;`, `++x;`, `--x;`. `a op= b` means `a = a op b`.

## Control flow

```kernel points
if (@P.y < 0) {
    @P.y = -@P.y;
    v@v.y = abs(v@v.y);
} else if (@P.y > 10) {
    @P.y = 10;
}

for (int k = 0; k < 4; k++) {
    if (k == 2) continue;
    @P += set(0, 0.01, 0) * k;
}
```

**`for` loops have a bounded trip count.** The only accepted form is

```
for (int i = START; i CMP END; STEP) BODY
```

with `CMP` one of `<`, `<=`, `>`, `>=`, and `STEP` one of `i++`, `++i`, `i--`, `--i`,
`i += K`, `i -= K` with `K` a positive integer literal, counting in the direction of `CMP`.
`START` and `END` are any `int` expressions, evaluated once before the loop. The body may
not assign `i`. Every loop also stops after 1,048,576 iterations, so no kernel can hang the
GPU. There is no `while` or `do`. `break` and `continue` work in `for` and `foreach`.

## Functions

```kernel points
vec3 bend(vec3 p, float k) {
    float c = cos(k * p.y);
    float s = sin(k * p.y);
    return set(c * p.x - s * p.z, p.y, s * p.x + c * p.z);
}

@P = bend(@P, ch("bend", 0.5, -2, 2));
```

`TYPE name(TYPE a, TYPE b) { ... }` (the VEX keyword `function` may come first; `void` for
no result). Parameters are passed **by value** (VEX passes by reference): assigning to a
parameter changes the function's copy only. A function with a result must `return` on every
path. Functions may call other functions declared anywhere at the top level, but not
themselves, directly or indirectly (WGSL has no recursion). Functions cannot use `@`:
pass attributes in as arguments. They can call `ch()` and every helper.

## Attributes: `@name`

`@name` reads or writes attribute `name` of the current element. A type sigil before the
`@` gives its type, as in VEX:

| Sigil | Type | Example |
|---|---|---|
| `f@` | `float` | `f@age` |
| `i@` | `int` | `i@id` |
| `u@` | `vec2` | `u@uv` |
| `v@` | `vec3` | `v@vel` |
| `p@` | `vec4` | `p@orient` |

Matrix (`2@ 3@ 4@`) and string (`s@`) attributes are not in the M2 attribute model and are
rejected. The type of an attribute is decided, in order, by: a sigil on any of its uses in
the kernel (two different sigils are an error); the input's attribute schema, when the
engine supplies one; the standard names below; otherwise `float` (as in VEX).

| Standard name | Type | Storage |
|---|---|---|
| `P`, `v`, `N`, `accel`, `force`, `up`, `rest`, `scale` | `vec3` | `f32×3` (16-byte stride) |
| `Cd` | `vec4` | `f32×4` |
| `uv` | `vec2` | `f32×2` |
| `orient` | `vec4` (quaternion) | `f32×4` |
| `age`, `life`, `pscale`, `mass` | `float` | `f32` |
| `id` | `int` | `u32` (converted on read and write) |

Note: VEX's `Cd` and `uv` are 3-vectors. Atidarla's attribute model uses `vec4` colour and
`vec2` UVs (`docs/design/geometry-attributes.md`). A sigil wins over this table, so with no
schema `v@Cd` makes a 3-component `Cd`; when the engine supplies the input's schema, a sigil
that disagrees with the input's type is an error (no silent conversion).

Writing an attribute that the input does not have creates it (zero-filled before the
kernel runs). Every element writes only its own attributes, so kernels need no atomics and
are deterministic.

### Read-only globals

| Points | Pixels | Type | Meaning |
|---|---|---|---|
| `@ptnum` | | `int` | index of this point |
| `@numpt` | | `int` | number of points |
| | `@ix`, `@iy` | `int` | pixel column and row (0, 0 is the top-left pixel) |
| | `@P` | `vec2` | pixel centre in 0..1 (u right, v down) |
| | `@res` | `vec2` | output size in pixels |
| `@Time` | `@Time` | `float` | seconds, from the engine (fixed step offline) |
| `@TimeInc` | `@TimeInc` | `float` | seconds since the previous frame (the step) |
| `@Frame` | `@Frame` | `float` | frame number |

In a **Pixels** kernel the only attribute is `@Cd` (`vec4`): it starts as input image 0 at
this pixel (transparent black when nothing is connected) and its final value is the
output pixel.

## Parameters: `ch()`

`ch("name", default, min, max)` reads a parameter and makes the operator show a control
for it. `default`, `min` and `max` are optional and must be constants (literals, `-`,
constructors and `{}`). Using one name twice is fine if both uses agree.

| Call | Result | Control |
|---|---|---|
| `ch(name, ...)`, `chf(name, ...)` | `float` | slider |
| `chi(name, ...)` | `int` | integer slider |
| `chu(name, {x, y})` | `vec2` | two fields |
| `chv(name, {x, y, z})` | `vec3` | three fields |
| `chp(name, {x, y, z, w})` | `vec4` | four fields |

Parameter values are uniforms, not part of the shader text: changing a value never
recompiles.

## Helpers

All helpers are pure functions of their arguments.

**Maths** (componentwise on `float` and vectors; `abs min max clamp sign` also on `int`):
`abs sign floor ceil frac (fract) trunc round sqrt exp log log2 sin cos tan asin acos atan
atan2(y, x) pow(x, y) min max clamp(x, lo, hi) lerp(a, b, t) (mix) step(edge, x)
smoothstep(lo, hi, x) smooth(lo, hi, x) fit(x, omin, omax, nmin, nmax) fit01(x, nmin, nmax)
radians degrees`. `round` rounds halves to even. `smoothstep`/`smooth` and `fit` clamp
like VEX; `fit` with `omin == omax` gives the middle of the new range.

**Vectors**: `length length2 distance distance2 dot cross normalize` (a zero vector
normalises to zero), constructors `vec2() vec3() vec4() set()` (`set` picks the size from
its argument count, as in VEX).

**Matrices**: `ident()` (`mat4`), `ident3()`, `mat3(...)`, `mat4(...)` (from columns or
scalars, column by column), `transpose`, `determinant`, `invert` (a singular matrix gives
the zero matrix).

**Quaternions** (`vec4`, w real): `quaternion(angle, axis)` (radians), `qmultiply(a, b)`,
`qrotate(q, v)`, `qinvert(q)`, `qconvert(q)` (to `mat3`), `slerp(a, b, t)`.

**Noise** (implemented once in WGSL and once in Rust from the same algorithm; a GPU test
compares them on a grid):

| Helper | Arguments | Result | Range |
|---|---|---|---|
| `perlin(p)` | `vec2` or `vec3` | `float` | about −1..1 (gradient noise, quintic fade) |
| `simplex(p)` | `vec2` or `vec3` | `float` | about −1..1 (Gustavson, kernel radius 0.5, so continuous across simplex edges) |
| `worley(p)` | `vec2` or `vec3` | `float` | distance to the nearest feature point (F1), 0..~1.5 |
| `curlnoise(p)` | `vec3` | `vec3` | curl of a Perlin vector potential (divergence-free) |
| `rand(x)` | `int`, `float`, `vec2`, `vec3` | `float` | 0 ≤ r < 1, a hash of the bits of `x` |

The hash is PCG3D (Jarzynski and Olano, 2020) on integer cell coordinates, so lattice
values are bit-identical on every GPU; interpolation is ordinary float arithmetic, so
results may differ in the last bits between GPUs (fused multiply-add).

**Geometry** (Points only; input `0` is the kernel's own geometry, the only one supported
in this version):

| Helper | Result | Notes |
|---|---|---|
| `point(0, "name", i)` | the attribute's type | value of attribute `name` at point `i`; zero if `i` is out of range. Always reads the input values, never this kernel's writes |
| `npoints(0)` | `int` | number of points (same as `@numpt`) |
| `nearpoint(0, pos)`, `nearpoint(0, pos, maxdist)` | `int` | the closest point to `pos` (lowest index on ties), or −1 |
| `foreach (int j; nearpoints(0, pos, radius, maxpts)) { ... }` | | runs the body for up to `maxpts` points within `radius` of `pos` |

**Limits of this version**: `nearpoint` and `nearpoints` scan every point (O(N) per element,
so O(N²) per kernel); keep them to small point counts (thousands, not millions) until the
engine provides a spatial grid. `nearpoints` visits points in index order, not nearest
first as VEX does, and may only be used as the source of a `foreach` (there are no arrays).

**Images**: `texture(uv)` samples input image 0 at `uv` (0..1, top-left origin, bilinear,
clamped, level 0); `texture(n, uv)` samples input image `n` (an integer literal, 0 to 3).
Works in both targets; in a Points kernel images are separate inputs of the operator.

## Determinism

- A kernel is a pure function of: the source, the target, the input attribute schema, the
  parameter values, the element index, the element's attributes (and, for `point()`,
  `nearpoint(s)`, other elements' input attributes), the input images, and the engine's
  `@Time`, `@TimeInc` and `@Frame`. There is no hidden clock and no random state: `rand()`
  and the noise helpers hash their arguments. Seed randomness with an attribute, for example
  `rand(i@id)` or `rand(@ptnum + 7919 * int(@Frame))`.
- Elements write only themselves, so there are no data races and no atomics. `point()` and
  `nearpoints()` read the input values from a separate binding when the kernel also writes
  that attribute.
- The compiler's output is stable text: the same source, target and schema always give
  byte-identical WGSL (tested), so it can be cached and diffed. Parameter values are not
  in the text.
- The same machine and backend gives the same bits on every run. Different GPUs may
  differ in the last bits of float results (fused multiply-add, `sqrt`, transcendental
  precision).

## Errors

Every error names a line and column (1-based, counted in characters) and a length, with a
message and, when there is an obvious fix, a hint:

```
grade.atk:2:9: error: expected `int`, found `float`
  int n = @P.x * 4;
          ^^^^^^^^
  hint: convert explicitly with int(...)
```

The compiler never panics, whatever the input (a fuzz test feeds it thousands of mutated
kernels and random text), and reports at most 25 errors, in source order. A syntax error
stops before type checking; a bad character or number hides the parser's follow-on errors on
the same line.

The generated WGSL is validated with naga before it is returned. The type checker is meant
to reject everything naga would, so a naga rejection is a compiler bug; it is still reported
at the kernel construct that produced the WGSL (`Origin::Wgsl`), with a hint to report it.
Integer arithmetic wraps, in constant expressions too: `2147483647 + 1` is `-2147483648`.

## Differences from VEX, at a glance

- Types `vec2/3/4`, `mat3/4` (VEX names accepted); `bool` exists and comparisons give it.
- Function parameters are by value; no recursion; functions cannot use `@`.
- `for` must count with a bounded integer; no `while`/`do`.
- No arrays or strings; `nearpoints` only in `foreach`.
- `Cd` is a `vec4`, `uv` a `vec2`, `id` an `int` stored as `u32`.
- Only input 0 for geometry lookups; no `detail()`, `prim()`, `setpointattrib()`,
  `addpoint()` or `removepoint()` (geometry creation is up to operators).
