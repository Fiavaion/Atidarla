# ComfyUI workflows as operators

> FR-A10, D9, M4 AC6. Code: `crates/atd-ai/src/comfy.rs` (protocol, parameter rules),
> `crates/atd-ops-ai/src/comfy.rs` (operator, project files, import). Example:
> [`examples/comfy-invert.atidarla`](../examples/comfy-invert.atidarla/).

A ComfyUI workflow saved in **API format** becomes an operator of the project. Its widget
inputs (seeds, steps, prompts, menus) become the operator's parameters; its `LoadImage`
nodes become IMG inputs; the image its first `SaveImage` or `PreviewImage` node saves
becomes the IMG output. The node runs the workflow on a ComfyUI server and shows the
result.

**Atidarla does not include, install or start ComfyUI.** ComfyUI is GPL-3.0 software, so
Atidarla only talks to a server you run yourself, over its HTTP and WebSocket API
(CLAUDE.md rule 3, PRD A6). The server can be on this machine or on the LAN. In Sovereign
mode (on by default) a server on the internet is refused before anything is sent.

## Importing a workflow

In ComfyUI, build the workflow and save it with **Workflow → Export (API)**. (The normal
"Save" writes the UI format, which lists widgets by position, not by name; importing one
says so.) Then, with the ComfyUI server running:

```sh
atidarla comfy import my-workflow.json --project show.atidarla [--server 192.168.1.20:8188] [--name grade]
```

or, in the app, **File → Import ComfyUI Workflow…** (it reads from `127.0.0.1:8188`; use the
command for another server). The import asks the server for `/object_info` (the
description of every node class) and writes two files:

| File | What |
|---|---|
| `comfy/<name>.json` | The workflow, as exported (keys sorted). |
| `comfy/<name>.info.json` | The server's `/object_info` for the node classes the workflow uses. |

The second file is why the project opens without a server, and why the same files always
give the same operator: parameters come from it, never from whichever server is reachable
when the project opens. `LoadImage`'s list of the server's input files is left out of it.
A workflow that uses node classes the server does not have is refused at import (install
the custom nodes first). Import again to pick up a changed workflow or a server update.

`<name>` (default: from the file name) is lower-case letters, digits and `_`; the operator
is `comfy.<name>`. Edit the JSON files by hand if you like: they are read when the project
opens.

## The operator

Every workflow operator has three parameters of its own, then the workflow's:

| Parameter | What |
|---|---|
| `server` | The ComfyUI server, `host:port` (default `127.0.0.1:8188`; `http://` is accepted; https is not supported). |
| `run` | Pulse: run the workflow now. |
| `auto_run` | Run whenever a parameter or an input image changes. Changes are waited out for 250 ms (at most 1 s while they keep coming, so an animated input still runs as often as the server keeps up), and only one prompt per node is in flight: a change during a run starts the next run when it ends. |

The status line under the parameters shows `queued`, `running <class> (node <id>) n/m`
(with the sampler's steps when the server reports them), `done in t s`, or what went
wrong: `server not reachable at …`, the server's validation errors per node
(`the server refused the workflow: node 3 (KSampler): …`), a node that failed while
running (`node 3 (KSampler) failed: …`), or Sovereign mode's refusal. While it runs and
after an error, the node keeps showing its last result (black before the first). A problem
with the server is not a node error, so a project without its server still opens, cooks and
passes `atidarla check --gpu`.

### Which inputs become parameters

The exposure rule (`atd_ai::comfy::derive`):

1. Every widget input of every node becomes a parameter: inputs whose value in the workflow
   is a value, not a link (`["4", 0]`, a wire from node 4's output 0). Link-only inputs
   (`MODEL`, `CLIP`, `LATENT`, …) never are. `LoadImage`'s `image` is not a parameter: it
   is fed from the IMG input.
2. **Selecting:** if any node's title starts with `atd:` (set the title in ComfyUI, e.g.
   `atd: Sampler`), only the inputs of nodes titled that way are exposed. Use this to show
   the five controls that matter out of a workflow with fifty.
3. **Names:** `<node title or class>_<input>` as a lower-case identifier: `KSampler`'s
   `seed` is `ksampler_seed`; a node titled `atd: Sampler` gives `sampler_seed`. When two
   nodes would give the same name (two `CLIPTextEncode` nodes titled `Prompt`), each gets
   its node ID: `prompt_text_6`, `prompt_text_7`. A name that is one of the operator's own
   (`server`, `run`, `auto_run`) also gets its node ID.
4. **Order:** by node ID (numerically), then in the order the node class declares its
   inputs. The same files always give the same names in the same order.
5. **Kinds:** `INT` → integer with the class's range, `FLOAT` → float with its range,
   `STRING` → text (multi-line when the class says so), `BOOLEAN` → toggle, a list of
   options (either `[["a", "b"], {…}]` or `["COMBO", {"options": […]}]`) → menu. The
   default is the value in the workflow. A class missing from the info file is an error
   (import again); an input the class does not list is typed from its value.
6. **Seeds:** an integer the class marks `control_after_generate`, or named `seed` or
   `noise_seed`, also gets a toggle `<name>_random`: when on, each run uses a new random
   seed (offline, one that depends only on the frame), and the parameter keeps its value.

### Images

- **Inputs:** each `LoadImage` node, by node ID, is an IMG input `in0`, `in1`, … When a
  run starts, the node copies the connected input images to the CPU without waiting for
  the GPU (a frame or two later), encodes them as PNG on its worker thread and uploads
  them (`POST /upload/image`, to the server's `input/atidarla/` folder, named by a hash
  of the pixels), and the run uses the uploaded file. ComfyUI's `LoadImage` drops alpha.
  An input left unconnected keeps the workflow's own `image` value (a file on the server).
- **Output:** the first image of the first `SaveImage` or `PreviewImage` node (one titled
  `atd:` first) is fetched (`GET /view`), decoded on the worker (PNG or JPEG) and uploaded
  to the GPU. The output takes the result's size from the frame after it arrives.

### Never blocking

Live, everything that waits runs on the node's worker thread (rule 1): connecting,
uploading, the run, the download and decoding. The cook records the input copies,
collects what the worker sent and draws the image it holds. While a prompt is in flight
the node cooks every frame to collect progress, allocating no more than a WGSL node's
frame does (rule 2, `crates/atd-ops-ai/tests/comfy_alloc.rs`). Closing the project stops
the worker after the current run notices; the server may finish that prompt on its own.

### Offline render

`atidarla render` waits for the server (class **Waits** in [`render.md`](render.md)): a
frame whose parameters or input images changed reads its inputs back, runs the workflow
and waits for the result; other frames reuse the last result. A frame whose run fails
(no server, a refused workflow) fails the render with the message. Whether two renders
match depends on the workflow: a fixed seed on the same server gives the same images;
`*_random` seeds are a function of the frame.

## The protocol, as used

| Call | When |
|---|---|
| `GET /object_info` | Import only. |
| `POST /upload/image` (multipart: `image`, `type=input`, `subfolder=atidarla`, `overwrite=true`) | Each run with a connected input. |
| WebSocket `/ws?clientId=…` | Each run, opened before the prompt is queued. Events: `execution_start`, `execution_cached`, `executing` (node `null` = done), `progress` and `progress_state` (both understood), `executed`, `execution_success`, `execution_error`, `execution_interrupted`; binary preview frames are ignored. |
| `POST /prompt` `{prompt, client_id}` | Each run. A 400 lists `node_errors`, shown per node. |
| `GET /history/{prompt_id}` | Polled about once a second as well as the events, so a missed event or no WebSocket still finishes the run; read at the end for the output's file name. |
| `GET /view?filename=&subfolder=&type=` | The result. |

Every connection goes through `atd_ai::egress`: the server's name is resolved once, the
gate refuses internet addresses in Sovereign mode, and the HTTP and WebSocket connections
go only to the addresses it returned (the `Host` header keeps the name you typed). There
is no TLS.

## Limits

- One image out: the first image of one output node. Video, batches and the media bin
  (FR-A10's "result to … media bin") are not done yet.
- No hot reload of `comfy/` files while the project is open: open it again.
- The server's queue is shared: another client's prompts run before yours (`queued`).
- UI-format workflows are refused with an explanation (export the API format). Node IDs
  that are not plain numbers (subgraphs, `5:3`) work and sort after the numeric ones.
