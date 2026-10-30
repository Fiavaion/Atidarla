"""Built-in reference models: deterministic, dependency-free, verifiable byte for byte."""

from __future__ import annotations

import struct

from ..errors import UNKNOWN_MODEL, RpcError
from ..frames import Frame
from . import Adapter, Audio, Progress

# Inverts RGB and keeps alpha.
_INVERT_RGB = bytes(255 - b for b in range(256))


def invert(frame: memoryview | bytes) -> bytes:
    out = bytearray(bytes(frame).translate(_INVERT_RGB))
    out[3::4] = frame[3::4]
    return bytes(out)


def describe_audio(audio: Audio) -> str:
    """What `builtin.stt_echo` "hears": the sample count, rate and peak level (3 decimals)."""
    n = audio.samples
    peak = max((abs(s) for (s,) in struct.iter_unpack("<f", audio.data[: n * 4])), default=0.0)
    return f"{n} samples at {audio.sample_rate} Hz, peak {peak:.3f}"


BUILTIN_MODELS = ("builtin.echo", "builtin.invert", "builtin.stt_echo")


class BuiltinAdapter(Adapter):
    kind = "builtin"
    weight_options = ()

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        super().__init__(model_id, path, options, progress)
        if model_id not in BUILTIN_MODELS:
            raise RpcError(UNKNOWN_MODEL, f"unknown built-in model {model_id!r}")

    def process(self, state: None, frame: Frame, params: dict) -> tuple[Frame, bool]:
        if self.model_id == "builtin.invert":
            return Frame(frame.width, frame.height, invert(frame.data)), False
        return frame, False

    def transcribe(self, state: None, audio: Audio, params: dict) -> dict:
        """`builtin.stt_echo` stands in for a speech model in tests."""
        if self.model_id != "builtin.stt_echo":
            return super().transcribe(state, audio, params)
        return {"text": describe_audio(audio), "language": params.get("language") or None}
