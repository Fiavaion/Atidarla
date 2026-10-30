# M5 AC2: Claude Code drives Atidarla via MCP

**Date:** 2026-10-30 · **Machine:** Apple M5 Pro (Metal) · **Client:** Claude Code 2.1.310, headless (`claude -p`), default model · **Server:** `atidarla serve <dir> --mcp-stdio --auto-accept` (release build of this branch)

## Set-up

`mcp.json` (only this server, `--strict-mcp-config`; tools allowed: `mcp__atidarla`):

```json
{"mcpServers": {"atidarla": {"type": "stdio", "command": "<repo>/target/release/atidarla",
  "args": ["serve", "<scratch>/mcp15.atidarla", "--mcp-stdio", "--auto-accept"]}}}
```

Prompt:

> Using only the atidarla MCP tools, build an audio-free, self-animating visual patch of at least 15 nodes in the open Atidarla project: an image chain (generators, colour and geometry effects, a feedback loop, compositing) driven by a few SIG nodes (LFOs, math) through parameter expressions or bindings. Read operator schemas before using an operator. Accept your changes, then call get_errors and fix every error until there are none. Finish with a screenshot of the final image node and a one-paragraph description of the patch. Do not ask me questions.

## Result

- **18 nodes**, accepted in one session, `get_errors` empty; no manual fixes.
- Independent check of the saved project: `atidarla check --gpu` → `ok … 18 nodes, 60 frames cooked in 1.0 s`.
- Wall time 1 min 43 s, 88 turns, USD 0.84.
- Tool calls: get_schema 16, create_node 18, connect 18, set_param 21, accept_changes 3, get_errors 2, screenshot_viewer 4, list_ops 2, get_network 2.
- Operators used: img.noise, img.ramp, img.lookup, img.shape, img.transform, img.feedback, img.displace, img.blur, img.hsv_adjust, img.kaleidoscope, img.composite ×2, img.level, img.bloom; sig.lfo ×2, sig.noise, sig.math.
- The resulting network: [`claude-code-mcp-network.toml`](claude-code-mcp-network.toml).

Claude's own note at the end: a grey wash came from `final_comp`'s blend mode, and it "couldn't confirm the operation name". `get_schema` does list menu options under `kind`, so the tool description could say so more plainly. Not a failure of the criterion.
