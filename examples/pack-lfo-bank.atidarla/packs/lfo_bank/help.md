# LFO Bank

Four low-frequency oscillators that share one rate and one wave shape, each a step further along
in its cycle than the last. Bind four parameters to its four channels and they move together but
not in step: a shape that circles, colours that chase each other, lights that sweep in turn.

## Outputs

| Socket | Type | What |
|---|---|---|
| `out` | SIG | Four channels, `lfo1` to `lfo4`, each between Low and High. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Rate | 0.25 Hz | Cycles per second, shared by all four (0.25 is one cycle every four seconds). |
| Shape | sine | sine, triangle, square or ramp. |
| Spread | 0.25 | Phase step from one channel to the next, in cycles. 0 moves all four together; 0.25 puts them a quarter cycle apart, so `lfo1` and `lfo2` trace a circle; 0.5 makes neighbours opposite. |
| Low | 0 | The value at the bottom of every wave. |
| High | 1 | The value at the top. Set Low above High to flip the waves. |

## Tips

- Bind a parameter to this node's channels (the binding menu), or read one in an expression:
  `op('lfo_bank1')['lfo2'] * 360` turns 0–1 into degrees.
- Two parameters bound to `lfo1` and `lfo2` with Spread 0.25 and Shape sine move a point round a
  circle.
- The waves follow the clock (time × Rate), so changing Rate while playing makes them jump to the
  new position; bind Rate to a slowly changing value, or change it between sections.
- Like all the clock's followers, the waves are the same in every render of the same frames.

## Inside

Four LFOs (Rate and Shape promoted to all of them; the second's phase is Spread, the third's and
fourth's follow it as two and three times Spread through expressions) → a Constant with four
channels bound to them → Math (maps −1…1 to Low…High).

## Versions

- 1.0.0 (2026-10-22): first version.
