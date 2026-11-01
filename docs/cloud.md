# Cloud AI hosts

Atidarla's first cloud targets (PRD A59) are a **self-hosted Runpod pod**
running our own AI Host container and **Decart**'s hosted real-time
video-to-video model. Both appear to the compute router as `cloud:<name>`
hosts, so in a node's route (`local → lan:studio → cloud:runpod`):

- **Sovereign mode** (on by default for new projects) skips them, and every
  connection to them is refused before a packet is sent (`atd_ai::egress`).
- Every frame they answer is charged to the project's **spend meter**, and
  the router stops using them once the project's cap is reached.

Keys are your own. They come from the environment or from a per-user
secrets file in the app's settings folder, whatever `ATD_MODELS_DIR` says (never from a project, never committed):

| OS | Secrets file |
|---|---|
| macOS | `~/Library/Caches/Atidarla/secrets.toml` |
| Windows | `%LOCALAPPDATA%\Atidarla\secrets.toml` |
| Linux | `$XDG_CACHE_HOME/atidarla/secrets.toml` (or `~/.cache/atidarla/`) |

```toml
[runpod]
api_key = "rpa_…"          # or RUNPOD_API_KEY in the environment (wins)

[runpod.tokens]            # written by `atidarla cloud runpod up`
studio = "…"               # the AI Host token of pod atd-studio
```

The file is created readable by you only (mode 600 on macOS/Linux).

## Runpod: your own AI Host on a rented GPU

The pod runs the AI Host container from `ai-host/Dockerfile` (CUDA, the
`diffusion` and `vision` extras, no weights baked in) and the engine reaches
it over AI Host Protocol v1 with the `inline` transport, exactly like a LAN
host (A60).

1. **Build and push the image** to a registry Runpod can pull from:

   ```sh
   docker build --platform linux/amd64 -t ghcr.io/<you>/atd-ai-host:1 ai-host
   docker push ghcr.io/<you>/atd-ai-host:1
   ```

2. **Bring the pod up** (creates `atd-<name>`, or resumes it if stopped):

   ```sh
   export RUNPOD_API_KEY=rpa_…
   atidarla cloud runpod up --name studio --image ghcr.io/<you>/atd-ai-host:1 \
       [--gpu "NVIDIA GeForce RTX 4090"] [--cloud-type SECURE] [--disk-gb 50]
   ```

   It creates the pod with port `7420/tcp` exposed and a fresh random
   `ATD_AI_HOST_TOKEN` (saved under `[runpod.tokens]`), then polls the pod
   until it has a public IP and a public port mapped to 7420 (Runpod picks a
   random port, which changes when the pod restarts), and prints
   `cloud:studio ready at <ip>:<port>` with the hourly price. In the app the
   same steps run on a worker thread (`atd_ai::cloud::runpod::spawn_up`) and
   register `cloud:studio` with the supervisor, with the spend meter's rate
   set from the pod's `costPerHr`.

3. **Model weights** are not in the image (rule 4, A61). Attach a Runpod
   network volume at `/models` and copy the model folders from your local
   cache into it; licences are accepted per user, as locally.

4. **Stop it.** A running pod bills by the hour whether or not frames flow.

   ```sh
   atidarla cloud runpod status --name studio   # id, state, address, price
   atidarla cloud runpod down --name studio     # stop: GPU billing ends, disk kept
   atidarla cloud runpod down --name studio --terminate   # delete pod and disk
   ```

   The app stops every pod it brought up when it exits
   (`atd_ai::cloud::runpod::stop_all`). The CLI's `up` leaves the pod running
   on purpose (that is its job) and says so with the price and the `down`
   command. A stopped pod still bills a little for its disk; terminate it when
   you are done. If a stop fails, the error says to stop it in the Runpod
   console.

**Costs:** the pod's `costPerHr` (USD/h) is charged for the time between
results while a node uses it. An RTX 4090 on Secure Cloud is roughly
$0.6–0.8/h; check Runpod's pricing page.

**Not yet confirmed against the live API** (built from Runpod's REST
reference with a local mock): the exact shape of `GET /pods?name=` (a bare
array is assumed; `{"pods": […]}` is accepted too) and `DELETE /pods/{id}`
for terminate.

## Decart: hosted real-time video-to-video

Decart's realtime models (Lucy 2.6 by default) take a live video stream and a
text prompt and return a restyled stream. Their media path is WebRTC only, so
the engine does not talk to Decart itself: a local AI Host process runs the
`video.v2v` adapter with Decart's backend, and the engine reaches that host
over our protocol like any other. The host is registered as `cloud:decart`
(`atd_ai::cloud::register_decart`), so it counts as a cloud target:

- In Sovereign mode the router skips it and the supervisor refuses to start
  the process at all (it would talk to the internet beyond the egress gate);
  turning Sovereign mode on stops a running one.
- Each frame result carries `cost_s`, the seconds Decart billed since the
  previous result (from its `generation_tick` messages), and the spend meter
  charges those at the registered rate.

Setup:

1. Install the host's `cloud` extra (Decart's MIT SDK `decart[realtime]`,
   pinned, with LiveKit's Apache-2.0 WebRTC client):
   `cd ai-host && uv sync --extra cloud`.
2. Put your key in the environment the app starts from:
   `export DECART_API_KEY=…` (the host inherits it). A node may instead pass
   `options.api_key`, but the environment is preferred so keys stay out of
   projects.
3. Use the Video Model node with model `decart/lucy-2.6` and route
   `cloud:decart`, with Sovereign mode off and a spend cap set.

**One prompt.** Decart takes a single prompt, so the node's Prompt Blend is
reduced to its highest-weight text; changing which text leads sends a new
prompt (Decart enhances it). Weights otherwise have no effect.

**Pricing:** $0.02 per billed second for Lucy 2.6 standard (720p), $0.01/s
for restyle models: `register_decart(supervisor, 72.0)` for Lucy 2.6
(`DECART_USD_PER_HOUR`). Results arrive at Decart's own rate and resolution
and are resized to the node's size; until the first generated frame arrives
the input is passed through and marked skipped.

**Terms:** bring your own key; Decart's watermark and provenance data are
passed through untouched.

## Verifying with real keys (hand checks)

- Runpod: `atidarla cloud runpod up --name check --image …`, add route
  `cloud:check` to a Diffusion node in the app with Sovereign off, see frames
  and the spend meter rise, then `atidarla cloud runpod down --name check
  --terminate` and confirm in the Runpod console that nothing is running.
- Decart: with `DECART_API_KEY` set and the `cloud` extra installed, a Video
  Model node on `cloud:decart` shows restyled frames; the spend meter rises
  by about $0.02 per second of generation, and Decart's dashboard agrees.
- Sovereign: `sudo scripts/sovereign-capture.sh` (macOS/Linux) or
  `scripts\sovereign-capture.ps1` (Windows, elevated) while a project that
  routes to both runs with Sovereign on: it must pass.
