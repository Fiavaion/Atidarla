"""JSON-RPC request handling for one session."""

from __future__ import annotations

import platform
import time
import uuid
from typing import Any

from . import PROTOCOL_VERSION, __version__
from .models import BUILTIN, Model
from .ring import FrameRing

VERSION_MISMATCH = -32001
UNKNOWN_MODEL = -32002
BAD_SLOT = -32003
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602


class RpcError(Exception):
    def __init__(self, code: int, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


def detect_vendor() -> str:
    """Best-effort vendor guess without importing any ML runtime."""
    if platform.system() == "Darwin" and platform.machine() == "arm64":
        return "apple"
    return "cpu"


class Session:
    def __init__(self) -> None:
        self.started = time.monotonic()
        self.ring: FrameRing | None = None
        self.models: dict[str, Model] = {}
        self.frames = 0
        self.closed = False

    def handle(self, method: str, params: Any) -> Any:
        handler = getattr(self, "rpc_" + method.replace(".", "_"), None)
        if handler is None:
            raise RpcError(METHOD_NOT_FOUND, f"unknown method {method!r}")
        return handler(params or {})

    def rpc_session_open(self, p: dict) -> dict:
        if p.get("protocol_version") != PROTOCOL_VERSION:
            raise RpcError(VERSION_MISMATCH, f"host speaks {PROTOCOL_VERSION}, client sent {p.get('protocol_version')}")
        self.ring = FrameRing(p["ring_path"])
        return {
            "session_id": uuid.uuid4().hex,
            "protocol_version": PROTOCOL_VERSION,
            "host_name": "atd-ai-host",
            "host_version": __version__,
        }

    def rpc_host_capabilities(self, _p: dict) -> dict:
        return {
            "vendor": detect_vendor(),
            "vram_mb": None,
            "runtimes": ["cpu-reference"],
            "transports": ["shm"],
            "models": sorted(BUILTIN),
        }

    def rpc_host_health(self, _p: dict) -> dict:
        return {
            "uptime_s": time.monotonic() - self.started,
            "frames_processed": self.frames,
            "loaded_models": sorted(self.models),
        }

    def rpc_model_load(self, p: dict) -> dict:
        model_id = p.get("model_id")
        if model_id not in BUILTIN:
            raise RpcError(UNKNOWN_MODEL, f"unknown model {model_id!r}")
        t0 = time.perf_counter()
        self.models.setdefault(model_id, BUILTIN[model_id]())
        return {"model_id": model_id, "load_ms": (time.perf_counter() - t0) * 1e3}

    def rpc_model_unload(self, p: dict) -> None:
        self.models.pop(p.get("model_id"), None)

    def rpc_frame_submit(self, p: dict) -> dict:
        t0 = time.perf_counter()
        if self.ring is None:
            raise RpcError(INVALID_PARAMS, "session not open")
        model = self.models.get(p.get("model_id"))
        if model is None:
            raise RpcError(UNKNOWN_MODEL, f"model {p.get('model_id')!r} not loaded")
        seq, src, dst = p["seq"], p["input_slot"], p["output_slot"]
        try:
            if self.ring.seq(src) != seq:
                raise RpcError(BAD_SLOT, f"slot {src} holds frame {self.ring.seq(src)}, not {seq}")
            self.ring.write(dst, seq, model.process(self.ring.frame(src)))
        except IndexError as e:
            raise RpcError(BAD_SLOT, str(e)) from None
        self.frames += 1
        return {"seq": seq, "output_slot": dst, "host_ms": (time.perf_counter() - t0) * 1e3}

    def rpc_frame_cancel(self, _p: dict) -> None:
        # Frames are processed synchronously in the prototype; nothing to cancel.
        return None

    def rpc_session_close(self, _p: dict) -> None:
        self.closed = True
