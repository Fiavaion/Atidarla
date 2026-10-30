"""Built-in reference models: deterministic, dependency-free, verifiable byte for byte."""

from __future__ import annotations

from ..errors import UNKNOWN_MODEL, RpcError
from ..frames import Frame
from . import Adapter, Progress

# Inverts RGB and keeps alpha.
_INVERT_RGB = bytes(255 - b for b in range(256))


def invert(frame: memoryview | bytes) -> bytes:
    out = bytearray(bytes(frame).translate(_INVERT_RGB))
    out[3::4] = frame[3::4]
    return bytes(out)


BUILTIN_MODELS = ("builtin.echo", "builtin.invert")


class BuiltinAdapter(Adapter):
    kind = "builtin"
    weight_options = ()

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        super().__init__(model_id, path, options, progress)
        if model_id not in BUILTIN_MODELS:
            raise RpcError(UNKNOWN_MODEL, f"unknown built-in model {model_id!r}")

    def process(self, state: None, frame: Frame, params: dict) -> tuple[Frame, bool]:
        if self.model_id == "builtin.echo":
            return frame, False
        return Frame(frame.width, frame.height, invert(frame.data)), False
