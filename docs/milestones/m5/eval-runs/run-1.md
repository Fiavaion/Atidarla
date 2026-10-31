# Network-generation eval

**Model:** `ollama:qwen2.5-coder:7b` · **Prompts:** `docs/milestones/m5/eval-prompts.toml` · **Valid:** 19/20 (**95%**) · **Time:** 369 s · **Max turns:** 40

Valid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops), and for prompts marked animated something outside the channel nodes moves (an expression or a channel binding on it, or a SIG node wired into it).

| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |
|---|---|---|---|---|---|---|
| noise-blur | valid | 2 | img.blur, img.noise | 0/1 | 14 s |  |
| gradient-colour | valid | 3 | img.hsv_adjust, img.ramp, sig.lfo | 0/1 | 11 s |  |
| feedback-trails | valid | 4 | img.composite, img.feedback, img.noise, img.transform | 1/2 | 9 s |  |
| kaleido | valid | 2 | img.kaleidoscope, img.noise | 0/1 | 6 s |  |
| lfo-brightness | valid | 3 | img.constant, img.level, sig.lfo | 0/1 | 6 s |  |
| edges | valid | 5 | img.blend, img.constant, img.edge, img.level, pnt.noise | 4/4 | 23 s |  |
| mirror-tile | valid | 3 | img.mirror, img.noise, img.tile | 0/1 | 9 s |  |
| two-sources-blend | valid | 4 | img.blend, img.noise, img.ramp, sig.lfo | 4/4 | 33 s |  |
| threshold | valid | 3 | img.blur, img.noise, img.threshold | 0/1 | 8 s |  |
| shape-spin | valid | 3 | img.shape, pnt.scatter, pnt.vortex | 4/4 | 25 s |  |
| bloom-glow | valid | 5 | img.bloom, img.composite, img.noise, img.shape, img.transform | 6/4 | 27 s |  |
| displace | valid | 3 | img.displace, img.noise, img.ramp | 5/2 | 14 s |  |
| particles | valid | 6 | cmp.window, img.noise, img.render_points, pnt.emitter, pnt.points, pnt.trail | 13/4 | 38 s |  |
| mesh-sphere | valid | 2 | img.render, msh.sphere | 0/1 | 10 s |  |
| math-channels | valid | 4 | img.noise, img.transform, sig.lfo, sig.math | 1/2 | 16 s |  |
| colour-grade | valid | 3 | img.hsv_adjust, img.level, img.noise | 0/1 | 9 s |  |
| lookup-recolour | valid | 3 | img.lookup, img.noise, img.ramp | 1/2 | 13 s |  |
| composite-layers | **invalid** | 6 | img.blend, img.mixer, img.noise | 8/4 | 50 s | missing img.composite |
| reaction-diffusion | valid | 4 | img.level, img.noise, img.reaction_diffusion | 28/4 | 38 s |  |
| text-overlay | valid | 3 | img.blend, img.noise, img.text | 0/1 | 10 s |  |
