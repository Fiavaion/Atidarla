# Spark Fountain

A particle fountain that runs by itself. Sparks shoot up out of a small nozzle, are bent by
swirling air, fall back under gravity and bounce off an invisible floor. Fast sparks are bright,
slow ones glow like embers. Drop it in as a layer and composite it over anything.

## Outputs

| Socket | Type | What |
|---|---|---|
| `out` | IMG | The fountain on black, at the project resolution. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Rate | 30000 | Sparks born per second. At most 200,000 are alive at once, so Rate × Life above that stops adding sparks. |
| Force | 2.6 | Launch speed: how high the jet goes. |
| Spread | 0.3 | Random speed in any direction: 0 is a tight jet, 1 a wide spray. |
| Life | 3 s | How long a spark lives. |
| Gravity | 0, −2.4, 0 | Pull on every spark. A sideways x leans the fountain; a positive y makes it rain upwards. |
| Stir | 0.6 | Swirling air that bends the sparks. |
| Bounce | 0.4 | Share of their speed sparks keep when they hit the floor; 0 makes them pool. |
| Palette | heat | Colours from slow to fast: `heat`, `ice`, `rainbow`, or the plain two- and three-colour ramps. |
| Size | 2 px | Spark size. |
| Glow | 0.6 | Bloom around the brightest sparks. |

## Tips

- Bind Force or Rate to an Audio Pulse's `level` so the fountain surges with the music.
- Sparks add up where they overlap, so a dense jet turns white; lower Rate or Size if it does.
- The fountain keeps its sparks from frame to frame, so an offline render started part-way
  differs from the same frames of a longer render (see `docs/render.md`).

## Inside

Particles (the state, with its own forces off) → Emitter (from a ring of points scattered on a
circle, Force along its normal) → Gravity → Turbulence → Drag → Collide (the floor), fed back into
Particles; then Colour by Attribute (speed) → Render Points → Bloom.

## Versions

- 1.0.0 (2026-10-22): first version.
