# Attribute Math (`pnt.attribute_math`)

PNT · version 1 · cook budget 60 µs

Computes a point attribute from another, component by component: Result = A (op) B, where B is a second attribute or a constant. Arithmetic, min/max, power, mix, fit (remap a range), clamp, and one-input functions (length, normalize, abs, negate, floor, fract, sin, cos, set).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `a` | A | string | `"P"` |  | The first operand, a point attribute. |
| `operation` | Operation | menu | `"multiply"` | `add`, `subtract`, `multiply`, `divide`, `min`, `max`, `power`, `mix`, `fit`, `clamp`, `length`, `normalize`, `abs`, `negate`, `floor`, `fract`, `sin`, `cos`, `set` | add, subtract, multiply, divide, min, max, power: A op B; mix: A to B by Amount; fit: A from [Range Low, Range High] to [B, B2]; clamp: A between B and B2; length (a float), normalize, abs, negate, floor, fract, sin, cos: of A; set: B. |
| `b_from` | B From | menu | `"constant"` | `constant`, `attribute` | Whether B is the constant or an attribute. |
| `constant` | Constant | vec4 | `[1.0,1.0,1.0,1.0]` |  | B when it is a constant. |
| `b` | B | string | `"v"` |  | B when it is an attribute. |
| `constant2` | B2 | vec4 | `[1.0,1.0,1.0,1.0]` |  | fit: the high end of the output range; clamp: the upper bound. |
| `range` | Range | vec2 | `[0.0,1.0]` |  | fit: the input range (low, high). |
| `amount` | Amount | float | `0.5` |  | mix: 0 gives A, 1 gives B. |
| `result` | Result | string | `""` |  | The attribute written (empty: A itself). |
| `result_type` | Result Type | menu | `"same"` | `same`, `float`, `vec2`, `vec3`, `vec4`, `int` | The result's type: the same as A (a float for length), or another. |

## Platforms

All platforms.

## Examples

- [PNT For-Each](../../../examples/pnt-foreach.atidarla/) — `spread`, `place`
- [PNT Solver](../../../examples/pnt-solver.atidarla/) — `warm`
