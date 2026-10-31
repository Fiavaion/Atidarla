# Network-generation eval

**Model:** `ollama:qwen2.5-coder:7b` · **Prompts:** `docs/milestones/m5/eval-prompts.toml` · **Valid:** 19/20 (**95%**) · **Time:** 746 s · **Max turns:** 40

Valid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops).

| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |
|---|---|---|---|---|---|---|
| noise-blur | valid | 2 | img.blur, img.noise | 0/1 | 4 s |  |
| gradient-colour | valid | 4 | img.hsv_adjust, img.ramp, sig.math, sig.timer | 12/4 | 25 s |  |
| feedback-trails | valid | 4 | img.composite, img.feedback, img.noise, img.transform | 19/4 | 33 s |  |
| kaleido | valid | 2 | img.kaleidoscope, img.noise | 0/1 | 8 s |  |
| lfo-brightness | valid | 4 | img.constant, img.level, img.noise, sig.lfo | 4/4 | 27 s |  |
| edges | valid | 2 | img.edge, img.noise | 0/1 | 8 s |  |
| mirror-tile | valid | 4 | img.mirror, img.noise, img.ramp, img.tile | 10/4 | 31 s |  |
| two-sources-blend | valid | 6 | img.blend, img.constant, img.noise, img.ramp, sig.lfo, sig.math | 4/4 | 73 s |  |
| threshold | valid | 3 | img.blur, img.noise, img.threshold | 0/1 | 13 s |  |
| shape-spin | valid | 4 | img.render, img.shape, img.transform, pnt.vortex | 15/4 | 34 s |  |
| bloom-glow | valid | 6 | img.blend, img.bloom, img.composite, img.noise, img.shape, img.transform | 22/4 | 46 s |  |
| displace | valid | 3 | img.displace, img.noise, img.ramp | 5/2 | 15 s |  |
| particles | valid | 6 | img.noise, img.render, img.render_points, pnt.copy, pnt.emitter, pnt.points | 12/4 | 145 s |  |
| mesh-sphere | valid | 2 | img.render, msh.sphere | 6/4 | 42 s |  |
| math-channels | valid | 4 | img.noise, img.transform, sig.lfo, sig.math | 16/4 | 48 s |  |
| colour-grade | valid | 3 | img.hsv_adjust, img.level, img.noise | 1/2 | 32 s |  |
| lookup-recolour | valid | 3 | img.lookup, img.noise, img.ramp | 1/2 | 22 s |  |
| composite-layers | **invalid** | 5 | img.mixer, img.noise | 20/4 | 26 s | missing img.composite |
| reaction-diffusion | valid | 4 | img.level, img.noise, img.reaction_diffusion, pnt.colour | 16/4 | 90 s |  |
| text-overlay | valid | 3 | img.blend, img.noise, img.text | 12/4 | 24 s |  |
