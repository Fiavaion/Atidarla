# Network-generation eval

**Model:** `ollama:qwen2.5-coder:7b` · **Prompts:** `docs/milestones/m5/eval-prompts.toml` · **Valid:** 17/20 (**85%**) · **Time:** 490 s · **Max turns:** 40

Valid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops), and for prompts marked animated something outside the channel nodes moves (an expression or a channel binding on it, or a SIG node wired into it).

| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |
|---|---|---|---|---|---|---|
| noise-blur | valid | 2 | img.blur, img.noise | 0/1 | 7 s |  |
| gradient-colour | valid | 4 | img.constant, img.hsv_adjust, img.ramp, sig.lfo | 7/4 | 29 s |  |
| feedback-trails | valid | 4 | img.composite, img.feedback, img.noise, img.transform | 1/2 | 17 s |  |
| kaleido | valid | 2 | img.kaleidoscope, img.noise | 0/1 | 9 s |  |
| lfo-brightness | valid | 4 | img.constant, img.level, img.noise, sig.lfo | 0/1 | 12 s |  |
| edges | valid | 5 | img.blend, img.constant, img.edge, img.level, pnt.noise | 7/4 | 23 s |  |
| mirror-tile | valid | 3 | img.mirror, img.noise, img.tile | 0/1 | 7 s |  |
| two-sources-blend | **invalid** | 5 | img.mixer, img.noise, img.ramp, sig.lfo, sig.math | 5/4 | 54 s | missing img.blend |
| threshold | valid | 3 | img.blur, img.noise, img.threshold | 0/1 | 7 s |  |
| shape-spin | valid | 6 | img.blend, img.shape, img.transform, pnt.attribute_noise, pnt.scatter, pnt.vortex | 11/4 | 38 s |  |
| bloom-glow | valid | 7 | img.blend, img.bloom, img.composite, img.noise, img.ramp, img.shape | 12/4 | 68 s |  |
| displace | valid | 3 | img.displace, img.noise, img.ramp | 5/2 | 15 s |  |
| particles | valid | 6 | cmp.window, img.noise, img.render_points, pnt.emitter, pnt.points, pnt.trail | 13/4 | 47 s |  |
| mesh-sphere | valid | 2 | img.render, msh.sphere | 0/1 | 12 s |  |
| math-channels | **invalid** | 4 | img.noise, img.transform, sig.lfo, sig.math | 5/4 | 24 s | nothing is animated |
| colour-grade | valid | 3 | img.hsv_adjust, img.level, img.noise | 0/1 | 10 s |  |
| lookup-recolour | valid | 3 | img.lookup, img.noise, img.ramp | 0/1 | 11 s |  |
| composite-layers | **invalid** | 6 | img.blend, img.mixer, img.noise | 9/4 | 55 s | missing img.composite |
| reaction-diffusion | valid | 4 | img.level, img.noise, img.reaction_diffusion | 28/4 | 39 s |  |
| text-overlay | valid | 3 | img.blend, img.noise, img.text | 0/1 | 7 s |  |
