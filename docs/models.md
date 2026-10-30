# Model manager

AI models are never part of the repository or of a project (CLAUDE.md rule 4, PRD A61).
The model manager (FR-A12, `atd_ai::models`) knows which models exist, what their
licences allow, where their files are, and downloads them only after the user has
accepted the licence.

## Where things live

| What | Where |
|---|---|
| Catalogue | `crates/atd-ai/src/models/catalogue.toml`, embedded in the build |
| Model files | per-user cache, one folder per model ID: macOS `~/Library/Caches/Atidarla/models`, Windows `%LOCALAPPDATA%\Atidarla\models`, Linux `$XDG_CACHE_HOME/atidarla/models` (else `~/.cache/atidarla/models`). `ATD_MODELS_DIR` overrides all three. |
| Licence acceptances | `accepted.toml` beside the cache folder (e.g. `~/Library/Caches/Atidarla/accepted.toml`): model ID, licence ID, licence URL, date. Per user, never in a project. |
| What a project needs | `project.toml`: `[[models]]` (id, licence class, SHA-256 of the main file) and `[ai] commercial_safe_only` |

```toml
# project.toml
[ai]
commercial_safe_only = true

[[models]]
id = "taesd"
licence_class = "commercial-safe"
sha256 = "db169d69145ec4ff064e49d99c95fa05d3eb04ee453de35824a6d0f325513549"
```

Both are written only when they have content (so older projects keep their bytes),
models in ID order. Unknown keys are refused, as everywhere in the format.

## Licence classes

| Class | Meaning | Badge |
|---|---|---|
| `commercial-safe` | Permissive (MIT, Apache-2.0, BSD): any use, commercial included | green, text `COMMERCIAL-SAFE` |
| `restricted` | Commercial use only under conditions (revenue cap, registration, use policy), e.g. the Stability AI Community Licence | orange, text `RESTRICTED` |
| `non-commercial` | Research or personal use only | vermillion, text `NON-COMMERCIAL` |

Badge colours are from the Okabe–Ito palette; the text always carries the meaning.

**Every** class needs an explicit acceptance before download, commercial-safe included.
An acceptance counts only while the catalogue names the same licence ID and URL, so a
model whose licence (or pinned revision) changes must be accepted again.

A project with **commercial-safe only** set hides restricted and non-commercial models in
the Models window, and `atd_ai::models::blocked` / `licence_class` let AI nodes refuse them.
The catalogue's class wins over the class a project file records.

## Status and checks

- **not downloaded / partial / ready / corrupt** come from file sizes (metadata only).
  `partial` also covers interrupted downloads (`*.part` files).
- **Verify** reads every file and compares its SHA-256 with the catalogue, on a worker
  thread. A same-size file with other bytes is only found this way.

## Downloads

- From Hugging Face at the pinned commit:
  `https://huggingface.co/<repo>/resolve/<revision>/<path>`.
- HTTP(S) with `ureq`, rustls (ring) and the operating system's trust store; proxies from
  the environment are ignored so nothing bypasses the egress gate.
- Every connection, redirects to CDNs included, resolves through `atd_ai::egress`.
  **Sovereign mode** (default on) refuses internet hosts before any packet is sent. The
  app then offers **Download once anyway**, and the CLI takes `--allow-internet`. Either
  lifts the gate for that one download only.
- Each file is written to `<file>.part`, resumed with an HTTP range request, and renamed
  into place only when its size and SHA-256 match. A mismatch deletes the `.part` and
  reports the two hashes. Cancel keeps the `.part` files for the next run.
- Downloads and checks run on their own threads (`Job`); the render thread never waits.

## Using it

App: **View → Models**. Each model has Read licence, Accept licence…, Download / Resume /
Cancel (with progress), Verify and Remove…; the commercial-safe-only switch at the top is a
project setting (saved with the project).

CLI:

```sh
atidarla models list                         # class, licence, size, status, accepted
atidarla models accept sd-turbo              # prints the licence and asks you to type `accept`
atidarla models download sd-turbo --allow-internet
atidarla models verify sd-turbo              # full SHA-256 check
atidarla models path [id]
```

Code: `models_root()`, `model_dir(id)`, `licence_class(id)`, `catalogue()`, `status`,
`start_verify`, `start_download`, `missing` (which of a project's models are not ready,
unknown, or a different version), `visible` / `blocked` / `allowed` (the commercial-safe
filter).

## Catalogue and licence evidence

Checked 2026-10-30 with the Hugging Face API (`https://huggingface.co/api/models/<repo>`
for `cardData.license` and the head commit; `…/tree/<rev>?recursive=1` for sizes and LFS
SHA-256) and the model cards. Small non-LFS files were fetched at the pinned revision and
hashed; each one's git blob SHA-1 matched the tree. The SD-Turbo and TAESD files already in
the cache on Mark's Mac were hashed locally and match the catalogue
(`atidarla models verify sd-turbo`: 12/12 files).

| ID | Task | Licence | Class | Size | Source (pinned) | Evidence |
|---|---|---|---|---|---|---|
| `sd-turbo` | diffusion (`diffusion.sd`), fp16 diffusers subset | Stability AI Community Licence (`LicenseRef-StabilityAI-Community`) | restricted | 2.58 GB | `stabilityai/sd-turbo@b261bac6` | [LICENSE.md](https://huggingface.co/stabilityai/sd-turbo/blob/b261bac6fd2cf515557d5d0707481eafa0485ec2/LICENSE.md): free for research, non-commercial and commercial use under US $1M annual revenue, with registration for commercial use. The API reports no `cardData.license`; the repo's LICENSE.md is the licence. Mark accepted it on 2026-10-30 (PRD A61). |
| `sdxl-turbo` | diffusion (`diffusion.sd`), fp16 diffusers subset | Stability AI Community Licence | restricted | 6.94 GB | `stabilityai/sdxl-turbo@71153311` | [LICENSE.md](https://huggingface.co/stabilityai/sdxl-turbo/blob/71153311d3dbb46851df1931d3ca6e939de83304/LICENSE.md) (same text as SD-Turbo); card metadata says `license: other`, `license_name: sai-nc-community`, but the linked file is the Community Licence of July 2024 |
| `taesd` | tiny VAE for SD 1.x/2.x | MIT | commercial-safe | 9.8 MB | `madebyollin/taesd@614f7681` | [card](https://huggingface.co/madebyollin/taesd/blob/614f76814bbe30edbe2e627ace1c2234c81a2c0e/README.md), `cardData.license: mit` |
| `taesdxl` | tiny VAE for SDXL | MIT | commercial-safe | 9.8 MB | `madebyollin/taesdxl@b20258aa` | [card](https://huggingface.co/madebyollin/taesdxl/blob/b20258aaef75ef61e659c1e0f14f251cf0ad153e/README.md), `cardData.license: mit` |
| `depth-anything-v2-small` | Depth (ONNX) | Apache-2.0 | commercial-safe | 99.1 MB | `onnx-community/depth-anything-v2-small@4472b736` | [card](https://huggingface.co/onnx-community/depth-anything-v2-small/blob/4472b7362082ad9968fee890ca0f1e5aca36b93d/README.md), `cardData.license: apache-2.0` (only the Small model is Apache-2.0; Base and Large are CC-BY-NC-4.0) |
| `birefnet-lite` | Segment / background removal (ONNX) | MIT | commercial-safe | 224.0 MB | `onnx-community/BiRefNet_lite@de15b22b` | [card](https://huggingface.co/onnx-community/BiRefNet_lite/blob/de15b22ba131738a16dff04aab8bdf8dc32e3ac1/README.md), `cardData.license: mit`. Chosen over RMBG-1.4, which is non-commercial. |
| `dwpose` | Pose: whole-body keypoints + YOLOX-L detector (ONNX) | Apache-2.0 | commercial-safe | 351.1 MB | `yzd-v/DWPose@1a714410` | [card](https://huggingface.co/yzd-v/DWPose/blob/1a7144101628d69ee7a3768d1ee3a094070dc388/README.md), `cardData.license: apache-2.0`. YOLO-pose models were avoided (AGPL). |
| `raft` | Optical flow (ONNX) | BSD-3-Clause (RAFT) AND MIT (ONNX conversion) | commercial-safe | 64.1 MB | `opencv/optical_flow_estimation_raft@06ce4aec` | [README](https://huggingface.co/opencv/optical_flow_estimation_raft/blob/06ce4aec9b40dd3121e3d856211f14493d5f9931/README.md) "License" section, with `BSD-3-LICENSE.txt` and `MITLICENSE.txt` in the repo (no `cardData.license`) |
| `swin2sr-x4` | Upscale ×4 (ONNX) | Apache-2.0 | commercial-safe | 53.8 MB | `onnx-community/swin2SR-realworld-sr-x4-64-bsrgan-psnr-ONNX@9b3baf05` | [card](https://huggingface.co/onnx-community/swin2SR-realworld-sr-x4-64-bsrgan-psnr-ONNX/blob/9b3baf051f6708d0b697580489e4415b64c7378e/README.md), `cardData.license: apache-2.0`. Real-ESRGAN (BSD-3) has no ONNX build from a trusted publisher on the Hub; Qualcomm's repo holds no files. |
| `whisper-base` | Speech-to-text (`stt.whisper`: CTranslate2 weights for faster-whisper) | MIT | commercial-safe | 147.9 MB | `Systran/faster-whisper-base@ebe41f70` | [card](https://huggingface.co/Systran/faster-whisper-base/blob/ebe41f70d5b6dfa9166e2c581c45c9c0cfc57b66/README.md), `cardData.license: mit`; a conversion of OpenAI Whisper base (MIT). The runtime (faster-whisper, CTranslate2) is MIT too. The default of the Speech to Text node. |
| `whisper-small` | Speech-to-text (`stt.whisper`) | MIT | commercial-safe | 486.2 MB | `Systran/faster-whisper-small@536b0662` | [card](https://huggingface.co/Systran/faster-whisper-small/blob/536b0662742c02347bc0e980a01041f333bce120/README.md), `cardData.license: mit`; a conversion of OpenAI Whisper small (MIT). |

No non-commercial model is in the catalogue: every vision task has a commercial-safe choice.

### Adding a model

1. Find a revision: `https://huggingface.co/api/models/<repo>` gives `sha` (the head
   commit) and `cardData.license`. Read the model card and any LICENSE file.
2. List files: `https://huggingface.co/api/models/<repo>/tree/<sha>?recursive=1`. For weights,
   `lfs.oid` is the SHA-256. For small files, fetch them at `<sha>` and hash them.
3. Add a `[[model]]` entry with the class and the licence URL pinned to `<sha>`, add a
   row above, and run `cargo test -p atd-ai`. The catalogue test refuses unpinned
   revisions, unknown classes, unsafe paths and missing hashes.
