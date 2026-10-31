# Network-generation eval

**Model:** `ollama:qwen2.5-coder:7b` · **Prompts:** `docs/milestones/m5/eval-prompts.toml` · **Valid:** 18/20 (**90%**) · **Time:** 377 s · **Max turns:** 40

Valid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops), and for prompts marked animated something outside the channel nodes moves (an expression or a channel binding on it, or a SIG node wired into it).

| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |
|---|---|---|---|---|---|---|
| noise-blur | valid | 2 | img.blur, img.noise | 0/1 | 4 s |  |
| gradient-colour | valid | 3 | img.hsv_adjust, img.ramp, sig.lfo | 0/1 | 9 s |  |
| feedback-trails | valid | 4 | img.composite, img.feedback, img.noise, img.transform | 1/2 | 16 s |  |
| kaleido | valid | 2 | img.kaleidoscope, img.noise | 0/1 | 4 s |  |
| lfo-brightness | valid | 4 | img.constant, img.level, img.noise, sig.lfo | 0/1 | 11 s |  |
| edges | valid | 4 | img.constant, img.edge, img.level, pnt.noise | 7/4 | 19 s |  |
| mirror-tile | valid | 3 | img.mirror, img.noise, img.tile | 0/1 | 7 s |  |
| two-sources-blend | valid | 4 | img.blend, img.noise, sig.lfo, sig.pattern | 8/4 | 26 s |  |
| threshold | valid | 3 | img.blur, img.noise, img.threshold | 0/1 | 5 s |  |
| shape-spin | valid | 3 | img.shape, pnt.scatter, pnt.vortex | 4/4 | 17 s |  |
| bloom-glow | valid | 7 | img.blend, img.bloom, img.composite, img.noise, img.ramp, img.shape, img.transform | 12/4 | 60 s |  |
| displace | valid | 3 | img.displace, img.noise, img.ramp | 5/2 | 9 s |  |
| particles | valid | 7 | cmp.window, img.noise, img.render_points, pnt.emitter, pnt.points, pnt.points_from_image, pnt.trail | 11/3 | 50 s |  |
| mesh-sphere | valid | 2 | img.render, msh.sphere | 0/1 | 8 s |  |
| math-channels | **invalid** | 4 | img.noise, img.transform, sig.lfo, sig.math | 5/4 | 19 s | nothing is animated |
| colour-grade | valid | 3 | img.hsv_adjust, img.level, img.noise | 1/2 | 10 s |  |
| lookup-recolour | valid | 3 | img.lookup, img.noise, img.ramp | 1/2 | 13 s |  |
| composite-layers | **invalid** | 6 | img.blend, img.mixer, img.noise | 6/4 | 50 s | missing img.composite |
| reaction-diffusion | valid | 4 | img.level, img.noise, img.reaction_diffusion | 28/4 | 32 s |  |
| text-overlay | valid | 3 | img.blend, img.noise, img.text | 0/1 | 7 s |  |
