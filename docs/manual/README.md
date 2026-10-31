# Atidarla user manual

Atidarla is a free, open-source real-time visual instrument for Windows and macOS (Linux on a
best-effort basis). You build pictures, sound-driven animation, 3D and AI effects by joining
**nodes** with **wires**. You can then play them like a deck of clips, or sequence them on a
timeline. All three ways of working change the same project.

This manual is written for art and design students and lecturers who are new to node-based
tools, and for performers who want to take a patch on stage. You do not need to program.

If you are new, read chapters 1 and 2 first. They take you from nothing to a moving picture you
built yourself.

## Contents

| Chapter | What it covers |
|---|---|
| [1. Install and first launch](01-install.md) | What you need, how to build and start the app on macOS and Windows |
| [2. Your first patch in 10 minutes](02-first-patch.md) | Nodes, wires, parameters, the Viewer, a moving oscillator |
| [3. The three views](03-views.md) | Patch (node canvas), Stage (clip decks), Timeline (keyframes and cues), and the Outputs tab |
| [4. Operator families](04-operator-families.md) | IMG, SIG, PNT, MSH, MAT, DAT, CMP and AI: what each does and how they connect |
| [5. Expressions, channels and bindings](05-expressions-bindings.md) | Making a parameter follow a formula or a signal |
| [6. Kernels, WGSL, Python and Packs](06-kernels-python-packs.md) | Writing your own operators, and reusing patches |
| [7. Live input and output](07-live-io.md) | Audio, MIDI, OSC, DMX, picture sharing, NDI, projectors and mapping |
| [8. AI](08-ai.md) | AI Hosts, models and licences, diffusion, vision, ComfyUI, cloud, Sovereign mode, speech and chat |
| [9. The assistant and agents](09-assistant-agents.md) | The Assistant panel, in-show cues, Show Lock, MCP |
| [10. Shows and reliability](10-shows-reliability.md) | Saving, exporting a show, kiosk mode, the supervisor, the Output Governor |
| [11. Collaboration](11-collaboration.md) | Diff, merge and Git |
| [12. Batch Graph and the media bin](12-batch-media-bin.md) | Making many variations at once |
| [13. Command line reference](13-command-line.md) | Every `atidarla` command |
| [14. Troubleshooting and FAQ](14-troubleshooting.md) | When something does not work |

## How this manual is kept honest

- Every statement comes from the project's own documents or code. Where a feature is not built
  yet, the manual says so.
- The per-operator reference (every parameter, input and output) is generated from the program
  itself and lives in [`docs/operators`](../operators/README.md). The manual links to it and
  does not repeat it.
- Atidarla is still being built. The status of each part is in
  [`docs/milestones`](../milestones/) and in the repository
  [README](../../README.md). Pages of this manual that depend on an unfinished part say "not built yet".
- Keyboard shortcuts use **Cmd** on macOS and **Ctrl** on Windows.

## Other documents worth knowing

- [Learn mode](../learn.md): the in-app quests that teach Atidarla step by step.
- [Example projects](../../examples/README.md): open any of them to see a finished patch.
- [Shipped Packs](../../packs/README.md): ready-made reusable effects.
- [`PRD.md`](../../PRD.md): what Atidarla is meant to become.
