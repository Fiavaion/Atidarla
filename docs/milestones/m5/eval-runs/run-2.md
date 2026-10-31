# Network-generation eval

**Model:** `ollama:qwen2.5-coder:7b` · **Prompts:** `docs/milestones/m5/eval-prompts.toml` · **Valid:** 18/20 (**90%**) · **Time:** 438 s · **Max turns:** 40

Valid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops), and for prompts marked animated something outside the channel nodes moves (an expression or a channel binding on it, or a SIG node wired into it).

| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |
|---|---|---|---|---|---|---|
| noise-blur | valid | 2 | img.blur, img.noise | 0/1 | 6 s |  |
| gradient-colour | valid | 5 | img.hsv_adjust, img.level, img.ramp, sig.math, sig.timer | 7/4 | 29 s |  |
| feedback-trails | valid | 4 | img.composite, img.feedback, img.noise, img.transform | 2/2 | 9 s |  |
| kaleido | valid | 2 | img.kaleidoscope, img.noise | 0/1 | 6 s |  |
| lfo-brightness | valid | 3 | img.constant, img.level, sig.lfo | 2/3 | 13 s |  |
| edges | valid | 4 | img.constant, img.edge, img.level, pnt.noise | 4/4 | 22 s |  |
| mirror-tile | valid | 3 | img.mirror, img.noise, img.tile | 0/1 | 4 s |  |
| two-sources-blend | **invalid** | 6 | img.composite, img.constant, img.noise, sig.lfo, sig.math, sig.pattern | 8/4 | 42 s | missing img.blend |
| threshold | valid | 3 | img.blur, img.noise, img.threshold | 0/1 | 5 s |  |
| shape-spin | valid | 3 | img.shape, pnt.scatter, pnt.vortex | 4/4 | 23 s |  |
| bloom-glow | valid | 6 | img.blend, img.bloom, img.composite, img.noise, img.shape | 12/4 | 56 s |  |
| displace | valid | 3 | img.displace, img.noise, img.ramp | 5/2 | 14 s |  |
| particles | valid | 6 | cmp.window, img.noise, img.render_points, pnt.emitter, pnt.points, pnt.trail | 14/4 | 24 s |  |
| mesh-sphere | valid | 2 | img.render, msh.sphere | 0/1 | 8 s |  |
| math-channels | valid | 4 | img.noise, img.transform, sig.lfo, sig.math | 4/4 | 44 s |  |
| colour-grade | valid | 3 | img.hsv_adjust, img.level, img.noise | 1/2 | 9 s |  |
| lookup-recolour | valid | 3 | img.lookup, img.noise, img.ramp | 1/2 | 28 s |  |
| composite-layers | **invalid** | 6 | img.blend, img.mixer, img.noise | 10/4 | 68 s | missing img.composite; 1 node error(s): mixer1: expression for `stage`: NameError: name 'blend1' is not defined |
| reaction-diffusion | valid | 4 | img.blend, img.noise, img.ramp, img.reaction_diffusion | 1/2 | 17 s |  |
| text-overlay | valid | 3 | img.blend, img.noise, img.text | 0/1 | 11 s |  |
