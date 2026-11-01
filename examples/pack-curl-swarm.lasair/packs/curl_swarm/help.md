# Curl Swarm

A generator that runs by itself: a cloud of glowing particles born near the centre flows through
swirling noise, like smoke, starlings or plankton, and is pulled back so it never leaves the
frame. Particles change colour as they age and add up where they crowd together.

## Outputs

| Socket | Type | What |
|---|---|---|
| `out` | IMG | The swarm on black, at the project resolution. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Rate | 50000 | Particles born per second. At most 300,000 are alive at once. |
| Life | 5 s | How long a particle lives. |
| Swirl | 1.5 | Strength of the swirling flow. |
| Scale | 1.2 | Size of the swirls: higher makes more, smaller eddies. |
| Flow | 0.2 | How fast the swirls change; 0 freezes the pattern into fixed streams. |
| Pull | 0.6 | Pull back to the centre. 0 lets the swarm drift off; negative pushes it apart. |
| Birth Colour | warm white | Colour of a new particle. |
| Death Colour | blue, transparent | Colour at the end of its life; alpha 0 fades it out. |
| Brightness | 0.12 | Brightness of each particle. They add up where they crowd, so lower it if the core turns white. |
| Glow | 0.6 | Bloom around the brightest parts. |
| Reset | off | Restarts the swarm while on. |

## Tips

- Bind Swirl to an Audio Pulse's `level` for a swarm that boils on loud passages, or Pull to an
  LFO so it breathes in and out.
- Pull −1 for a moment, then back to 0.6, makes an explosion that gathers again.
- The swarm keeps its particles from frame to frame, so an offline render started part-way
  differs from the same frames of a longer render (see `docs/render.md`).

## Inside

Particles (born in a ball, curl noise, attraction, drag) → Render Points (additive) → Bloom.

## Versions

- 1.0.0 (2026-10-22): first version.
