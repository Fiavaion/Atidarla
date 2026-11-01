"""`onnx`: vision models through ONNX Runtime with the best available execution provider.

Every task turns its model's output back into an RGBA8 frame:

| task       | output frame                                                    |
|------------|-----------------------------------------------------------------|
| generic    | the model's image output (1 channel → grey, 3/4 → colour)       |
| depth      | depth as grey (normalised per frame, near = bright)              |
| segment    | class colours, foreground mask in alpha                          |
| background | the input frame with the matte in alpha                          |
| upscale    | the model's (larger) image output                                |
| flow       | flow to the previous frame: R/G = dx/dy around 128, B = speed     |
| pose       | the input with keypoints drawn (heatmap models only)              |
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from ..capabilities import installed
from ..errors import INVALID_PARAMS, MODEL_LOAD_FAILED, UNSUPPORTED, RpcError
from ..frames import Frame
from . import Adapter, Progress

TASKS = ("generic", "depth", "segment", "pose", "flow", "background", "upscale")
IMAGENET_MEAN = (0.485, 0.456, 0.406)
IMAGENET_STD = (0.229, 0.224, 0.225)
# Execution providers, best first.
PREFERRED_EPS = (
    "TensorrtExecutionProvider",
    "CUDAExecutionProvider",
    "DmlExecutionProvider",
    "CoreMLExecutionProvider",
    "ROCMExecutionProvider",
    "OpenVINOExecutionProvider",
    "CPUExecutionProvider",
)


class OnnxState:
    def __init__(self) -> None:
        self.prev = None  # previous input (flow), float32 [3, H, W]


class OnnxAdapter(Adapter):
    kind = "onnx"
    weight_options = ("providers",)

    @classmethod
    def available(cls) -> bool:
        return installed("onnxruntime") and installed("numpy")

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        super().__init__(model_id, path, options, progress)
        import onnxruntime as ort

        if not path or not Path(path).expanduser().is_file():
            raise RpcError(MODEL_LOAD_FAILED, f"onnx needs path = an .onnx file, got {path!r}")
        available = ort.get_available_providers()
        wanted = options.get("providers") or [ep for ep in PREFERRED_EPS if ep in available]
        if "CPUExecutionProvider" not in wanted:
            wanted = [*wanted, "CPUExecutionProvider"]
        progress("session", 0.2)
        self.session = ort.InferenceSession(str(Path(path).expanduser()), providers=wanted)
        self.device = self.session.get_providers()[0].removesuffix("ExecutionProvider").lower()
        self.inputs = self.session.get_inputs()
        progress("ready", 1.0)

    def new_state(self) -> OnnxState:
        return OnnxState()

    # ---- pre-processing ---------------------------------------------------

    def _layout(self, shape: list, spec: dict) -> str:
        if spec.get("layout") in ("NCHW", "NHWC"):
            return spec["layout"]
        if len(shape) == 4 and shape[1] in (1, 3, 6):
            return "NCHW"
        if len(shape) == 4 and shape[-1] in (1, 3, 6):
            return "NHWC"
        return "NCHW"

    @staticmethod
    def _fixed(dim: Any) -> int | None:
        return dim if isinstance(dim, int) and dim > 0 else None

    def _model_size(self, shape: list, layout: str, spec: dict, frame: Frame) -> tuple[int, int]:
        """(height, width) the model wants: fixed dims, else params.input.size, else the frame's."""
        h, w = (shape[2], shape[3]) if layout == "NCHW" else (shape[1], shape[2])
        h, w = self._fixed(h), self._fixed(w)
        if h and w:
            return h, w
        size = spec.get("size")
        if isinstance(size, list) and len(size) == 2 and all(isinstance(v, int) and v > 0 for v in size):
            return size[1], size[0]  # [width, height]
        return frame.height, frame.width

    @staticmethod
    def _resize(img, h: int, w: int):
        """Bilinear resize of a float32 [C, H, W] array."""
        import numpy as np

        _, ih, iw = img.shape
        if (ih, iw) == (h, w):
            return img
        ys = np.clip((np.arange(h) + 0.5) * ih / h - 0.5, 0, ih - 1)
        xs = np.clip((np.arange(w) + 0.5) * iw / w - 0.5, 0, iw - 1)
        y0, x0 = ys.astype(np.int64), xs.astype(np.int64)
        y1, x1 = np.minimum(y0 + 1, ih - 1), np.minimum(x0 + 1, iw - 1)
        fy, fx = (ys - y0)[None, :, None], (xs - x0)[None, None, :]
        top = img[:, y0][:, :, x0] * (1 - fx) + img[:, y0][:, :, x1] * fx
        bottom = img[:, y1][:, :, x0] * (1 - fx) + img[:, y1][:, :, x1] * fx
        return (top * (1 - fy) + bottom * fy).astype(np.float32)

    def _feed(self, rgb, task: str, spec: dict, frame: Frame, extra=None):
        """Model input from a float32 [3, H, W] image in [0, 1]."""
        import numpy as np

        inp = self.inputs[0]
        layout = self._layout(list(inp.shape), spec)
        h, w = self._model_size(list(inp.shape), layout, spec, frame)
        x = self._resize(rgb, h, w)
        if task in ("depth", "segment", "pose", "background"):
            mean, std = IMAGENET_MEAN, IMAGENET_STD
        else:
            mean, std = (0.0, 0.0, 0.0), (1.0, 1.0, 1.0)
        mean, std = spec.get("mean", mean), spec.get("std", std)
        try:
            x = (x - np.asarray(mean, np.float32)[:, None, None]) / np.asarray(std, np.float32)[:, None, None]
        except ValueError:
            raise RpcError(INVALID_PARAMS, "input.mean and input.std need 3 values") from None
        feeds = {}
        if extra is not None:
            prev = (self._resize(extra, h, w) - np.asarray(mean, np.float32)[:, None, None]) / np.asarray(
                std, np.float32
            )[:, None, None]
            if len(self.inputs) >= 2:
                feeds[self.inputs[1].name] = self._pack(prev, layout)
            else:
                x = np.concatenate([prev, x])  # one 6-channel input: previous then current
        feeds[inp.name] = self._pack(x, layout)
        return feeds

    @staticmethod
    def _pack(x, layout: str):
        import numpy as np

        x = x[None]
        if layout == "NHWC":
            x = x.transpose(0, 2, 3, 1)
        return np.ascontiguousarray(x, dtype=np.float32)

    # ---- post-processing --------------------------------------------------

    @staticmethod
    def _chw(out):
        """The first output as float32 [C, H, W]."""
        import numpy as np

        y = np.asarray(out, dtype=np.float32)
        while y.ndim > 3 and y.shape[0] == 1:
            y = y[0]
        if y.ndim == 2:
            y = y[None]
        if y.ndim != 3:
            raise RpcError(UNSUPPORTED, f"model output of shape {np.shape(out)} is not an image")
        if y.shape[0] not in (1, 2, 3, 4) and y.shape[-1] in (1, 2, 3, 4):
            y = y.transpose(2, 0, 1)  # HWC
        return y

    @staticmethod
    def _to_u8(x):
        import numpy as np

        return np.clip(np.rint(x * 255.0), 0, 255).astype(np.uint8)

    def _rgba(self, rgb, alpha) -> Frame:
        import numpy as np

        _, h, w = rgb.shape
        out = np.empty((h, w, 4), np.uint8)
        out[..., :3] = self._to_u8(rgb).transpose(1, 2, 0)
        out[..., 3] = self._to_u8(alpha) if alpha is not None else 255
        return Frame(w, h, memoryview(out).cast("B"))

    @staticmethod
    def _unit(y):
        """Brings an image-like output into [0, 1] (0–255 outputs are rescaled)."""
        return y / 255.0 if float(y.max(initial=0.0)) > 1.5 else y

    def process(self, st: OnnxState, frame: Frame, params: dict) -> tuple[Frame, bool]:
        import numpy as np

        task = params.get("task", self.options.get("task", "generic"))
        if task not in TASKS:
            raise RpcError(INVALID_PARAMS, f"task must be one of {TASKS}")
        spec = params.get("input") or {}
        if not isinstance(spec, dict):
            raise RpcError(INVALID_PARAMS, "input must be an object")
        px = np.frombuffer(frame.data, np.uint8, count=frame.nbytes).reshape(frame.height, frame.width, 4)
        rgb = px[..., :3].transpose(2, 0, 1).astype(np.float32) / 255.0
        alpha_in = px[..., 3].astype(np.float32) / 255.0
        H, W = frame.height, frame.width

        extra = None
        if task == "flow":
            extra = st.prev if st.prev is not None and st.prev.shape == rgb.shape else rgb
            st.prev = rgb
        y = self._chw(self.session.run(None, self._feed(rgb, task, spec, frame, extra))[0])

        if task == "upscale":
            return self._rgba(self._unit(y[:3]) if y.shape[0] >= 3 else np.repeat(self._unit(y[:1]), 3, 0), None), False
        if task == "generic":
            if y.shape[0] in (3, 4):
                y = self._resize(self._unit(y), H, W)
                return self._rgba(y[:3], y[3] if y.shape[0] == 4 else alpha_in), False
            g = self._resize(self._unit(y[:1]), H, W)
            return self._rgba(np.repeat(g, 3, 0), alpha_in), False
        if task == "depth":
            d = y[:1]
            lo, hi = float(d.min()), float(d.max())
            d = (d - lo) / (hi - lo) if hi > lo else np.zeros_like(d)
            return self._rgba(np.repeat(self._resize(d, H, W), 3, 0), None), False
        if task in ("segment", "background"):
            mask, colours = self._mask(y)
            mask = self._resize(mask[None], H, W)[0]
            if task == "background":
                return self._rgba(rgb, mask * alpha_in), False
            return self._rgba(self._resize(colours, H, W), mask), False
        if task == "flow":
            f = self._resize(y[:2], H, W)
            speed = np.sqrt((f**2).sum(0, keepdims=True))
            vis = np.concatenate([0.5 + f / 64.0, np.clip(speed / 32.0, 0, 1)])
            return self._rgba(np.clip(vis, 0, 1), None), False
        return self._pose(y, rgb, alpha_in), False

    @staticmethod
    def _mask(y):
        """Foreground probability [H, W] and class colours [3, H, W] from a mask or logits."""
        import numpy as np

        if y.shape[0] == 1:
            m = y[0]
            if float(m.min()) < 0 or float(m.max()) > 1:
                m = 1.0 / (1.0 + np.exp(-m))
            return m.astype(np.float32), np.repeat(m[None], 3, 0).astype(np.float32)
        e = np.exp(y - y.max(0, keepdims=True))
        prob = e / e.sum(0, keepdims=True)
        cls = prob.argmax(0)
        rng = np.random.default_rng(7)  # a stable palette
        palette = rng.random((y.shape[0], 3)).astype(np.float32)
        palette[0] = 0.0
        return (1.0 - prob[0]).astype(np.float32), palette[cls].transpose(2, 0, 1)

    def _pose(self, y, rgb, alpha):
        import numpy as np

        k, h, w = y.shape
        if not (1 <= k <= 133 and h >= 8 and w >= 8):
            raise RpcError(UNSUPPORTED, f"pose output {y.shape} is not keypoint heatmaps")
        _, H, W = rgb.shape
        out = rgb.copy()
        r = max(2, min(H, W) // 128)
        for j in range(k):
            idx = int(y[j].argmax())
            if float(y[j].flat[idx]) < 0.1:
                continue
            cy, cx = (idx // w + 0.5) * H / h, (idx % w + 0.5) * W / w
            y0, y1 = int(max(0, cy - r)), int(min(H, cy + r + 1))
            x0, x1 = int(max(0, cx - r)), int(min(W, cx + r + 1))
            out[:, y0:y1, x0:x1] = np.array([1.0, 0.2, 0.2], np.float32)[:, None, None]
        return self._rgba(out, alpha)
