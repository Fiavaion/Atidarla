"""Model adapters. M0 ships only a built-in reference model for protocol tests."""

from __future__ import annotations

from typing import Protocol

# Inverts RGB and keeps alpha: a deterministic, dependency-free "model" whose
# output the engine can verify byte for byte.
_INVERT_RGB = bytes(255 - b for b in range(256))


class Model(Protocol):
    model_id: str

    def process(self, frame: memoryview) -> bytes: ...


class InvertModel:
    model_id = "builtin.invert"

    def process(self, frame: memoryview) -> bytes:
        out = bytearray(bytes(frame).translate(_INVERT_RGB))
        out[3::4] = frame[3::4]
        return bytes(out)


BUILTIN: dict[str, type] = {InvertModel.model_id: InvertModel}
