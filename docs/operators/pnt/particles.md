# Particles (`pnt.particles`)

PNT · version 1 · cook budget 300 µs

A GPU particle system: emits particles at a rate, ages them, removes them at the end of their life and moves them with curl noise, attraction to a point, gravity and drag. Writes `P`, `v`, `age`, `life`, `id` and `Cd`; the live count stays on the GPU. Wire the end of a chain of force operators into `prev` to carry their effect to the next frame.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `prev` | Geo | feedback: reads the source's previous frame; optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `capacity` | Capacity | int | `1000000` | 1 – 16000000 | Most particles alive at once; memory is set aside for this many. Changing it restarts the system. |
| `rate` | Rate | float | `100000.0` | ≥ 0 | Particles emitted per second (none once the capacity is reached). |
| `initial` | Initial | int | `0` | 0 – 16000000 | Particles emitted on the first frame (and on reset), with ages spread over their life. |
| `life` | Life | float | `4.0` | ≥ 0.001 | Seconds a particle lives. |
| `life_variance` | Life Variance | float | `0.25` | 0 – 1 | Random variation of the life, as a fraction (0.25: ±25%). |
| `emitter` | Emitter | menu | `"sphere"` | `point`, `sphere`, `box` | Where particles are born: at the centre, in a ball or in a box. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the emitter. |
| `radius` | Radius | float | `0.25` | ≥ 0 | Radius of the ball, or half the side of the box. |
| `velocity` | Velocity | vec3 | `[0.0,0.0,0.0]` |  | Initial velocity (units per second). |
| `spread` | Spread | float | `0.3` | ≥ 0 | Random initial speed added in any direction, up to this. |
| `curl_amplitude` | Curl Amplitude | float | `1.0` |  | Strength of the curl-noise force (units per second²). |
| `curl_frequency` | Curl Frequency | float | `1.5` | ≥ 0 | Noise cells per unit of the curl-noise field. |
| `curl_speed` | Curl Speed | float | `0.2` |  | How fast the curl-noise field evolves (field units per second). |
| `attract_point` | Attract Point | vec3 | `[0.0,0.0,0.0]` |  | Point particles are pulled toward. |
| `attract` | Attract | float | `0.5` |  | Pull toward the point per unit of distance (a spring; negative pushes away). |
| `gravity` | Gravity | vec3 | `[0.0,0.0,0.0]` |  | Constant acceleration (units per second²). |
| `drag` | Drag | float | `0.5` | ≥ 0 | Velocity lost per second, as a fraction. |
| `colour_birth` | Birth Colour | colour | `[1.0,0.8,0.4,1.0]` |  | `Cd` of a new particle. |
| `colour_death` | Death Colour | colour | `[0.2,0.3,1.0,0.0]` |  | `Cd` at the end of its life (in between, mixed by age). |
| `seed` | Seed | int | `1` | ≥ 0 | Random seed; the same seed and frames give the same particles. |
| `reset` | Reset | toggle | `false` |  | While on, restarts the system every frame (empty, then the initial burst). |

## Platforms

All platforms.

## Examples

- [Particles](../../../examples/particles.lasair/) — `system`
- [PNT Fountain](../../../examples/pnt-fountain.lasair/) — `system`
- [Soak show](../../../examples/soak-show.lasair/) — `system`
