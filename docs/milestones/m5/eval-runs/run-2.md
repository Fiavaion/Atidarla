# Network-generation eval

**Model:** `ollama:qwen2.5-coder:7b` · **Prompts:** `docs/milestones/m5/eval-prompts.toml` · **Valid:** 18/20 (**90%**) · **Time:** 391 s · **Max turns:** 40

Valid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops), and for prompts marked animated something outside the channel nodes moves (an expression or a channel binding on it, or a SIG node wired into it).

| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |
|---|---|---|---|---|---|---|
| noise-blur | valid | 2 | img.blur, img.noise | 0/1 | 5 s |  |
| gradient-colour | valid | 4 | img.hsv_adjust, img.ramp, sig.math, sig.timer | 9/4 | 23 s |  |
| feedback-trails | valid | 4 | img.composite, img.feedback, img.noise, img.transform | 1/2 | 9 s |  |
| kaleido | valid | 2 | img.kaleidoscope, img.noise | 0/1 | 4 s |  |
| lfo-brightness | valid | 4 | img.blend, img.constant, img.level, sig.lfo | 0/1 | 5 s |  |
| edges | valid | 6 | img.constant, img.edge, img.level, img.ramp, img.transform, pnt.noise | 6/4 | 31 s |  |
| mirror-tile | valid | 3 | img.mirror, img.noise, img.tile | 0/1 | 3 s |  |
| two-sources-blend | valid | 4 | img.blend, img.noise, img.ramp, sig.lfo | 4/4 | 30 s |  |
| threshold | valid | 3 | img.blur, img.noise, img.threshold | 0/1 | 5 s |  |
| shape-spin | valid | 5 | img.shape, pnt.attribute_noise, pnt.scatter, pnt.vortex | 10/4 | 30 s |  |
| bloom-glow | valid | 7 | img.bloom, img.composite, img.noise, img.render_points, img.shape, pnt.attribute_from_image, pnt.wave | 15/4 | 56 s |  |
| displace | valid | 3 | img.displace, img.noise, img.ramp | 5/2 | 14 s |  |
| particles | valid | 6 | cmp.window, img.noise, img.render_points, pnt.emitter, pnt.points, pnt.trail | 13/4 | 30 s |  |
| mesh-sphere | valid | 2 | img.render, msh.sphere | 0/1 | 6 s |  |
| math-channels | **invalid** | 4 | img.noise, img.transform, sig.lfo, sig.math | 6/4 | 18 s | nothing is animated |
| colour-grade | valid | 3 | img.hsv_adjust, img.level, img.noise | 1/2 | 9 s |  |
| lookup-recolour | valid | 3 | img.lookup, img.noise, img.ramp | 1/2 | 10 s |  |
| composite-layers | **invalid** | 6 | img.blend, img.mixer, img.noise | 6/4 | 54 s | missing img.composite; 1 node error(s): mixer1: expression for `stage`: NameError: name 'blend1' is not defined |
| reaction-diffusion | valid | 4 | img.level, img.noise, img.reaction_diffusion | 32/4 | 39 s |  |
| text-overlay | valid | 3 | img.blend, img.noise, img.text | 0/1 | 10 s |  |
