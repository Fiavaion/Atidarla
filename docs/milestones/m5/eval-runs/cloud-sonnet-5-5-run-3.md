# Network-generation eval

**Model:** `anthropic:claude-sonnet-5-5` · **Prompts:** `docs/milestones/m5/eval-prompts.toml` · **Valid:** 20/20 (**100%**) · **Time:** 126 s · **Max turns:** 40

Valid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops), and for prompts marked animated something outside the channel nodes moves (an expression or a channel binding on it, or a SIG node wired into it).

| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |
|---|---|---|---|---|---|---|
| noise-blur | valid | 2 | img.blur, img.noise | 0/2 | 5 s |  |
| gradient-colour | valid | 2 | img.hsv_adjust, img.ramp | 0/5 | 10 s |  |
| feedback-trails | valid | 5 | img.composite, img.feedback, img.level, img.noise, img.transform | 0/2 | 9 s |  |
| kaleido | valid | 2 | img.kaleidoscope, img.noise | 0/2 | 5 s |  |
| lfo-brightness | valid | 3 | img.constant, img.level, sig.lfo | 0/2 | 6 s |  |
| edges | valid | 3 | img.edge, img.noise, img.threshold | 0/3 | 6 s |  |
| mirror-tile | valid | 3 | img.mirror, img.ramp, img.tile | 0/2 | 5 s |  |
| two-sources-blend | valid | 4 | img.blend, img.noise, img.ramp, sig.lfo | 0/2 | 7 s |  |
| threshold | valid | 3 | img.blur, img.noise, img.threshold | 0/2 | 5 s |  |
| shape-spin | valid | 2 | img.shape, img.transform | 0/2 | 5 s |  |
| bloom-glow | valid | 8 | img.bloom, img.composite, img.shape, img.transform, sig.lfo | 0/3 | 11 s |  |
| displace | valid | 3 | img.displace, img.noise, img.ramp | 0/2 | 5 s |  |
| particles | valid | 2 | img.render_points, pnt.particles | 0/2 | 5 s |  |
| mesh-sphere | valid | 2 | img.render, msh.sphere | 0/2 | 4 s |  |
| math-channels | valid | 4 | img.noise, img.transform, sig.lfo, sig.math | 0/2 | 6 s |  |
| colour-grade | valid | 5 | img.composite, img.constant, img.hsv_adjust, img.level, img.noise | 0/2 | 7 s |  |
| lookup-recolour | valid | 3 | img.lookup, img.noise, img.ramp | 0/2 | 5 s |  |
| composite-layers | valid | 5 | img.composite, img.noise, img.ramp, img.shape | 0/3 | 9 s |  |
| reaction-diffusion | valid | 5 | img.composite, img.level, img.noise, img.ramp, img.reaction_diffusion | 0/3 | 7 s |  |
| text-overlay | valid | 2 | img.noise, img.text | 0/2 | 4 s |  |
