"""SIG operators written in Python: a stepping counter and an LFO (docs/python-ops.md)."""

import math

import atidarla as atd


@atd.op(
    family="SIG",
    label="Step Counter",
    summary="Counts 1, 2, 3 ... at Rate steps per second and starts again after Wrap.",
    params=[
        atd.Float("rate", default=1.0, min=0.0, max=30.0, help="Steps per second."),
        atd.Int("wrap", default=8, min=1, max=64, help="The count goes back to 1 after this."),
    ],
    state={"phase": 0.0},
)
def counter(ctx):
    ctx.state.phase += ctx.dt * ctx.par.rate
    step = int(ctx.state.phase) % ctx.par.wrap + 1
    return {"count": step, "phase": ctx.state.phase % 1.0}


@atd.op(
    family="SIG",
    label="Python LFO",
    summary="A low-frequency oscillator: sine, triangle or square.",
    params=[
        atd.Menu("shape", options=("sine", "triangle", "square")),
        atd.Float("frequency", default=0.25, min=0.0, max=20.0, help="Cycles per second."),
        atd.Float("amplitude", default=1.0, min=0.0, help="Peak value."),
        atd.Float("offset", default=0.0, help="Added to the output."),
    ],
)
def wobble(ctx):
    t = ctx.time * ctx.par.frequency
    if ctx.par.shape == "sine":
        v = math.sin(2 * math.pi * t)
    elif ctx.par.shape == "triangle":
        v = 4 * abs(t % 1.0 - 0.5) - 1
    else:
        v = 1.0 if t % 1.0 < 0.5 else -1.0
    return {"wobble": ctx.par.offset + ctx.par.amplitude * v}
