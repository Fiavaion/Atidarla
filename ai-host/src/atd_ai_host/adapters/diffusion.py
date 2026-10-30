"""`diffusion.sd`: real-time Stable Diffusion img2img (SD-Turbo class models).

Clean-room implementation of the StreamDiffusion techniques (Kodaira et al.,
arXiv 2312.12491) from the paper's equations; diffusers and transformers are
used only to load the networks (UNet2DConditionModel, CLIPTextModel,
AutoencoderTiny / AutoencoderKL).

With alpha_t = sqrt(abar_t) and beta_t = sqrt(1 - abar_t):

- SDEdit:            x_t0 = alpha_t0 * x0 + beta_t0 * eps0
- CFG:               eps = eps_u + g * (eps_c - eps_u)
- virtual residual:  eps'_i = (x_ti - alpha_ti * ref_i) / beta_ti
- R-CFG:             eps = d * eps'_i + g * (eps_c - d * eps'_i)
    self:     ref_i = x0 (the element's input latent), no negative pass
    onetime:  one real negative pass on the newest element gives
              ref_0 = (x_t0 - beta_t0 * eps_u) / alpha_t0
- stream batch: one U-Net pass denoises a batch whose elements sit at
  different steps; element i predicts x0_i = (x_ti - beta_ti * eps_i) / alpha_ti
  and moves to the next step with x_t(i+1) = alpha_t(i+1) * x0_i + beta_t(i+1) * eps_i
  (deterministic DDIM). The oldest element's x0 is the output.
- similarity filter (SSF): S = <I, I_ref> / (|I| |I_ref|);
  P(skip) = max(0, (S - eta) / (1 - eta)); a frame that is not skipped becomes the reference.
"""

from __future__ import annotations

import json
import logging
import math
import random
from collections import OrderedDict
from pathlib import Path
from typing import Any

from ..capabilities import installed
from ..errors import INVALID_PARAMS, MODEL_LOAD_FAILED, UNSUPPORTED, RpcError
from ..frames import Frame
from . import Adapter, Progress

log = logging.getLogger("atd_ai_host.diffusion")

EMBED_CACHE = 64
RCFG_MODES = ("none", "self", "onetime")


def pick_device(torch) -> str:
    if torch.cuda.is_available():
        return "cuda"
    if torch.backends.mps.is_available():
        return "mps"
    return "cpu"


def turbo_timesteps(steps: int, strength: float, train_steps: int = 1000) -> list[int]:
    """`steps` denoising timesteps, trailing spacing, starting at `strength` of the schedule.

    strength 1.0 starts from (almost) pure noise at t=999; 0.5 starts at t=499.
    """
    start = max(1, min(train_steps, round(strength * train_steps)))
    ts = [max(0, round(start * (steps - i) / steps) - 1) for i in range(steps)]
    return sorted(set(ts), reverse=True)


def alphas_cumprod(config: dict, torch):
    n = config.get("num_train_timesteps", 1000)
    lo, hi = config.get("beta_start", 0.00085), config.get("beta_end", 0.012)
    if config.get("beta_schedule", "scaled_linear") == "scaled_linear":
        betas = torch.linspace(lo**0.5, hi**0.5, n, dtype=torch.float64) ** 2
    else:
        betas = torch.linspace(lo, hi, n, dtype=torch.float64)
    return torch.cumprod(1.0 - betas, dim=0)


def _variant(folder: Path) -> str | None:
    return "fp16" if any(folder.glob("*.fp16.safetensors")) or any(folder.glob("*.fp16.bin")) else None


class Params:
    """Validated `FrameSubmit.params` for `diffusion.sd`."""

    def __init__(self, p: dict) -> None:
        prompts = p.get("prompts", [{"text": "", "weight": 1.0}])
        if isinstance(prompts, str):
            prompts = [{"text": prompts, "weight": 1.0}]
        if not isinstance(prompts, list) or not prompts:
            raise RpcError(INVALID_PARAMS, "prompts must be a non-empty list of {text, weight}")
        blend: dict[str, float] = {}
        for item in prompts:
            if not isinstance(item, dict) or not isinstance(item.get("text"), str):
                raise RpcError(INVALID_PARAMS, "each prompt needs a text")
            w = item.get("weight", 1.0)
            if not isinstance(w, (int, float)) or isinstance(w, bool) or not math.isfinite(w) or w < 0:
                raise RpcError(INVALID_PARAMS, "prompt weights must be finite and >= 0")
            blend[item["text"]] = blend.get(item["text"], 0.0) + float(w)
        total = sum(blend.values())
        if total <= 0:
            blend = dict.fromkeys(blend, 1.0)
            total = float(len(blend))
        self.blend = tuple((t, w / total) for t, w in blend.items() if w > 0)
        self.negative = self._get(p, "negative", str, "")
        self.seed = int(self._num(p, "seed", 0, 0, 2**63 - 1))
        self.strength = self._num(p, "strength", 0.5, 0.0, 1.0)
        self.steps = int(self._num(p, "steps", 1, 1, 4))
        self.guidance = self._num(p, "guidance", 1.0, 0.0, 30.0)
        self.rcfg = self._get(p, "rcfg", str, "none")
        if self.rcfg not in RCFG_MODES:
            raise RpcError(INVALID_PARAMS, f"rcfg must be one of {RCFG_MODES}")
        self.delta = self._num(p, "delta", 1.0, 0.0, 10.0)
        self.similarity = self._num(p, "similarity", 0.0, 0.0, 1.0)
        lora = p.get("lora") or []
        if not isinstance(lora, list) or not all(isinstance(x, dict) and isinstance(x.get("path"), str) for x in lora):
            raise RpcError(INVALID_PARAMS, "lora must be a list of {path, weight}")
        self.lora = tuple((x["path"], float(x.get("weight", 1.0))) for x in lora)

    @staticmethod
    def _get(p: dict, name: str, kind: type, default: Any) -> Any:
        v = p.get(name, default)
        if not isinstance(v, kind):
            raise RpcError(INVALID_PARAMS, f"{name} must be {kind.__name__}")
        return v

    @staticmethod
    def _num(p: dict, name: str, default: float, lo: float, hi: float) -> float:
        v = p.get(name, default)
        if not isinstance(v, (int, float)) or isinstance(v, bool) or not lo <= v <= hi:
            raise RpcError(INVALID_PARAMS, f"{name} must be a number in [{lo}, {hi}]")
        return float(v)


class StreamState:
    """One session's stream: latents in flight, noise, similarity reference, buffers."""

    def __init__(self) -> None:
        self.layout: tuple | None = None
        self.size: tuple[int, int] = (0, 0)  # processing size (multiples of 64)
        self.seed: int | None = None
        self.rng = random.Random(0)
        self.noise = None  # eps0, [1, 4, h, w]
        self.pending = None  # latents in flight at steps 1..n-1, [n-1, 4, h, w]
        self.pending_ref = None  # their R-CFG references
        self.batch = None  # [n, 4, h, w]
        self.batch_ref = None
        self.timesteps = None  # [n] (or [2n] for full CFG)
        self.alpha = None  # [n, 1, 1, 1]
        self.beta = None
        self.host_in = None  # uint8 [H, W, 4] on the CPU
        self.dev_in = None  # uint8 [H, W, 4] on the device
        self.ssf_ref = None
        self.prev_out: Frame | None = None


class DiffusionAdapter(Adapter):
    kind = "diffusion.sd"
    weight_options = ("vae", "vae_path", "device", "dtype")

    @classmethod
    def available(cls) -> bool:
        return all(installed(m) for m in ("torch", "diffusers", "transformers"))

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        super().__init__(model_id, path, options, progress)
        import torch
        from diffusers import AutoencoderKL, AutoencoderTiny, UNet2DConditionModel
        from transformers import CLIPTextModel, CLIPTokenizer

        if not path:
            raise RpcError(MODEL_LOAD_FAILED, "diffusion.sd needs path = the model folder")
        root = Path(path).expanduser()
        if not (root / "unet").is_dir() or not (root / "text_encoder").is_dir():
            raise RpcError(MODEL_LOAD_FAILED, f"{root} is not a diffusers model folder (unet/, text_encoder/)")
        self.torch = torch
        self.device = options.get("device") or pick_device(torch)
        dtype_name = options.get("dtype") or ("float32" if self.device == "cpu" else "float16")
        self.dtype = {"float16": torch.float16, "bfloat16": torch.bfloat16, "float32": torch.float32}[dtype_name]

        progress("tokenizer", 0.05)
        self.tokenizer = CLIPTokenizer.from_pretrained(root, subfolder="tokenizer")
        progress("text_encoder", 0.15)
        self.text_encoder = (
            CLIPTextModel.from_pretrained(
                root, subfolder="text_encoder", dtype=self.dtype, variant=_variant(root / "text_encoder")
            )
            .to(self.device)
            .eval()
        )
        progress("unet", 0.35)
        self.unet = (
            UNet2DConditionModel.from_pretrained(
                root, subfolder="unet", torch_dtype=self.dtype, variant=_variant(root / "unet")
            )
            .to(self.device)
            .eval()
        )
        progress("vae", 0.85)
        vae_path = Path(options.get("vae_path") or root.parent / "taesd").expanduser()
        if options.get("vae", "tiny") == "tiny" and (vae_path / "config.json").is_file():
            self.vae = AutoencoderTiny.from_pretrained(vae_path, torch_dtype=self.dtype).to(self.device).eval()
            self.tiny_vae = True
        else:
            if options.get("vae", "tiny") == "tiny":
                log.warning("no TAESD at %s; using the model's full VAE (slower)", vae_path)
            self.vae = (
                AutoencoderKL.from_pretrained(
                    root, subfolder="vae", torch_dtype=self.dtype, variant=_variant(root / "vae")
                )
                .to(self.device)
                .eval()
            )
            self.tiny_vae = False
        self.vae_scale = float(self.vae.config.scaling_factor)
        with open(root / "scheduler" / "scheduler_config.json") as f:
            self.abar = alphas_cumprod(json.load(f), torch)
        self.embeds: OrderedDict[str, Any] = OrderedDict()
        self._lora_loaded: dict[str, str] = {}
        self._lora_active: tuple = ()
        progress("ready", 1.0)

    def new_state(self) -> StreamState:
        return StreamState()

    def close(self) -> None:
        del self.unet, self.text_encoder, self.vae
        self.embeds.clear()
        if self.device == "cuda":
            self.torch.cuda.empty_cache()
        elif self.device == "mps":
            self.torch.mps.empty_cache()

    # ---- conditioning -----------------------------------------------------

    def embed(self, text: str):
        """Text embedding, encoded once per distinct text (LRU cache shared by all sessions)."""
        hit = self.embeds.get(text)
        if hit is not None:
            self.embeds.move_to_end(text)
            return hit
        ids = self.tokenizer(
            text, padding="max_length", max_length=self.tokenizer.model_max_length, truncation=True, return_tensors="pt"
        ).input_ids.to(self.device)
        with self.torch.inference_mode():
            emb = self.text_encoder(ids)[0]
        self.embeds[text] = emb
        if len(self.embeds) > EMBED_CACHE:
            self.embeds.popitem(last=False)
        return emb

    def condition(self, blend: tuple[tuple[str, float], ...]):
        """Prompt Blend: the weighted sum of the distinct prompts' embeddings (weights sum to 1)."""
        if len(blend) == 1:
            return self.embed(blend[0][0])
        acc = None
        for text, w in blend:
            term = self.embed(text).float() * w
            acc = term if acc is None else acc.add_(term)
        return acc.to(self.dtype)

    def _apply_lora(self, lora: tuple) -> None:
        if lora == self._lora_active:
            return
        if lora and not installed("peft"):
            raise RpcError(UNSUPPORTED, "lora needs the peft package on the host")
        try:
            for path, _ in lora:
                if path not in self._lora_loaded:
                    name = f"lora{len(self._lora_loaded)}"
                    self.unet.load_lora_adapter(path, prefix="unet", adapter_name=name)
                    self._lora_loaded[path] = name
            if lora:
                self.unet.enable_lora()
                self.unet.set_adapters([self._lora_loaded[p] for p, _ in lora], [w for _, w in lora])
            elif self._lora_active:
                self.unet.disable_lora()
        except (OSError, ValueError, KeyError, RuntimeError) as e:
            raise RpcError(UNSUPPORTED, f"cannot apply lora: {type(e).__name__}: {e}") from None
        self._lora_active = lora

    # ---- frames -----------------------------------------------------------

    def _prepare(self, st: StreamState, p: Params, frame: Frame, guided: str) -> None:
        """(Re)allocates the stream's buffers when the size, schedule or seed changes."""
        torch = self.torch
        W, H = frame.width, frame.height
        pw, ph = max(64, round(W / 64) * 64), max(64, round(H / 64) * 64)
        layout = (W, H, pw, ph, p.steps, p.strength, guided)
        if st.layout == layout and st.seed == p.seed:
            return
        ts = turbo_timesteps(p.steps, p.strength)
        n = len(ts)
        h, w = ph // 8, pw // 8
        gen = torch.Generator("cpu").manual_seed(p.seed)
        st.noise = torch.randn((1, 4, h, w), generator=gen, dtype=torch.float32).to(self.device, self.dtype)
        st.rng = random.Random(p.seed)
        abar = self.abar[ts]
        st.alpha = abar.sqrt().view(n, 1, 1, 1).to(self.device, self.dtype)
        st.beta = (1 - abar).sqrt().view(n, 1, 1, 1).to(self.device, self.dtype)
        t = torch.tensor(ts, device=self.device, dtype=torch.long)
        st.timesteps = torch.cat([t, t]) if guided == "full" else t
        st.batch = torch.empty((n, 4, h, w), device=self.device, dtype=self.dtype)
        st.batch_ref = torch.empty_like(st.batch)
        st.pending = st.pending_ref = None
        st.host_in = torch.empty((H, W, 4), dtype=torch.uint8)
        st.dev_in = torch.empty((H, W, 4), dtype=torch.uint8, device=self.device)
        st.ssf_ref = None
        st.prev_out = None
        st.layout, st.seed = layout, p.seed
        st.size = (pw, ph)

    def _upload(self, st: StreamState, frame: Frame):
        """Input pixels as a [1, 3, ph, pw] tensor in [-1, 1]."""
        import numpy as np

        torch = self.torch
        st.host_in.numpy().reshape(-1)[:] = np.frombuffer(frame.data, dtype=np.uint8, count=frame.nbytes)
        st.dev_in.copy_(st.host_in, non_blocking=True)
        x = st.dev_in[..., :3].permute(2, 0, 1).unsqueeze(0).to(self.dtype).div_(127.5).sub_(1.0)
        pw, ph = st.size
        if (pw, ph) != (frame.width, frame.height):
            x = torch.nn.functional.interpolate(x, size=(ph, pw), mode="bilinear", align_corners=False)
        return x

    def _encode(self, x):
        if self.tiny_vae:
            return self.vae.encode(x).latents
        return self.vae.encode(x).latent_dist.mean * self.vae_scale

    def _decode(self, z):
        if self.tiny_vae:
            return self.vae.decode(z).sample
        return self.vae.decode(z / self.vae_scale).sample

    def _download(self, st: StreamState, img, frame: Frame) -> Frame:
        import numpy as np

        torch = self.torch
        if img.shape[-2:] != (frame.height, frame.width):
            img = torch.nn.functional.interpolate(
                img, size=(frame.height, frame.width), mode="bilinear", align_corners=False
            )
        rgb = img[0].add(1.0).mul_(127.5).clamp_(0, 255).round_().to(torch.uint8).permute(1, 2, 0).cpu().numpy()
        out = np.empty((frame.height, frame.width, 4), dtype=np.uint8)
        out[..., :3] = rgb
        out[..., 3] = st.host_in.numpy()[..., 3]  # alpha passes through
        return Frame(frame.width, frame.height, memoryview(out).cast("B"))

    def _similar(self, st: StreamState, x, eta: float) -> bool:
        """The stochastic similarity filter: True if this frame should be skipped."""
        flat = x.reshape(-1).float()
        if st.ssf_ref is not None and st.prev_out is not None:
            s = self.torch.nn.functional.cosine_similarity(flat, st.ssf_ref, dim=0).item()
            if st.rng.random() < max(0.0, (s - eta) / (1.0 - eta)):
                return True
        if st.ssf_ref is None:
            st.ssf_ref = flat.clone()
        else:
            st.ssf_ref.copy_(flat)
        return False

    def process(self, st: StreamState, frame: Frame, params: dict) -> tuple[Frame, bool]:
        p = Params(params)
        torch = self.torch
        self._apply_lora(p.lora)
        guided = p.rcfg if p.guidance > 1.0 else "off"
        if guided == "none":
            guided = "full"
        self._prepare(st, p, frame, guided)
        with torch.inference_mode():
            x = self._upload(st, frame)
            if p.similarity > 0 and self._similar(st, x, p.similarity):
                return st.prev_out, True
            x0 = self._encode(x)
            cond = self.condition(p.blend)
            uncond = self.embed(p.negative) if guided in ("full", "onetime") else None
            out_latent = self._stream_step(st, x0, cond, uncond, guided, p)
            img = self._decode(out_latent)
            out = self._download(st, img, frame)
        st.prev_out = out
        return out, False

    def _stream_step(self, st: StreamState, x0, cond, uncond, guided: str, p: Params):
        """One U-Net pass over the stream batch; returns the finished element's x0."""
        torch = self.torch
        n = st.batch.shape[0]
        a, b = st.alpha, st.beta
        x_t0 = a[0] * x0 + b[0] * st.noise  # SDEdit
        if st.pending is None and n > 1:
            # Fill the pipeline with the first frame at every later step.
            st.pending = a[1:] * x0 + b[1:] * st.noise
            st.pending_ref = x0.expand(n - 1, -1, -1, -1).clone()
        st.batch[0:1] = x_t0
        st.batch_ref[0:1] = x0
        if n > 1:
            st.batch[1:] = st.pending
            st.batch_ref[1:] = st.pending_ref
        x = st.batch
        if guided == "full":
            hidden = torch.cat([uncond.expand(n, -1, -1), cond.expand(n, -1, -1)])
            eps_u, eps_c = self.unet(torch.cat([x, x]), st.timesteps, encoder_hidden_states=hidden).sample.chunk(2)
            eps = eps_u + p.guidance * (eps_c - eps_u)
        elif guided == "onetime":
            hidden = torch.cat([cond.expand(n, -1, -1), uncond])
            t = torch.cat([st.timesteps, st.timesteps[:1]])
            out = self.unet(torch.cat([x, x_t0]), t, encoder_hidden_states=hidden).sample
            eps_c, eps_u0 = out[:n], out[n:]
            st.batch_ref[0:1] = (x_t0 - b[0] * eps_u0) / a[0]  # x-hat_0 from the one real negative pass
            eps = self._rcfg(eps_c, x, st.batch_ref, a, b, p)
        else:
            eps_c = self.unet(x, st.timesteps, encoder_hidden_states=cond.expand(n, -1, -1)).sample
            eps = self._rcfg(eps_c, x, st.batch_ref, a, b, p) if guided == "self" else eps_c
        x0_pred = (x - b * eps) / a
        if n > 1:
            st.pending = a[1:] * x0_pred[:-1] + b[1:] * eps[:-1]
            st.pending_ref = st.batch_ref[:-1].clone()
        return x0_pred[-1:]

    @staticmethod
    def _rcfg(eps_c, x, ref, a, b, p: Params):
        virtual = (x - a * ref) / b  # residual noise against the reference
        return p.delta * virtual + p.guidance * (eps_c - p.delta * virtual)
