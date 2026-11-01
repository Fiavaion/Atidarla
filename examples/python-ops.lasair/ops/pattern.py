"""An IMG operator in pure Python (no numpy): concentric rings (docs/python-ops.md)."""

import math

import lasair as lsr


@lsr.op(
    family="IMG",
    label="Rings",
    summary="Concentric rings, drawn on the CPU in Python at 64x64.",
    params=[
        lsr.Int("rings", default=4, min=1, max=32, help="How many rings fit in the radius."),
        lsr.Float("phase", default=0.0, help="Moves the rings outwards (one ring per unit)."),
        lsr.Colour("tint", default=(1.0, 0.6, 0.2, 1.0), help="Colour of the bright rings."),
        lsr.Toggle("invert", help="Dark rings on light."),
    ],
    size=(64, 64),
)
def rings(ctx):
    w, h = ctx.width, ctx.height
    img = ctx.image()
    r, g, b, a = ctx.par.tint
    n = ctx.par.rings
    for y in range(h):
        dy = (y + 0.5) / h - 0.5
        for x in range(w):
            dx = (x + 0.5) / w - 0.5
            d = math.sqrt(dx * dx + dy * dy) * 2.0
            v = 0.5 + 0.5 * math.cos(2 * math.pi * (d * n - ctx.par.phase))
            if ctx.par.invert:
                v = 1.0 - v
            i = (y * w + x) * 4
            img[i] = int(255 * v * r)
            img[i + 1] = int(255 * v * g)
            img[i + 2] = int(255 * v * b)
            img[i + 3] = int(255 * a)
    return img
