"""Adapters: the model families a host can run.

An adapter instance holds the weights and is shared by every session that
loads the same model (see `models.ModelCache`). Anything that belongs to one
stream (latents in flight, similarity reference, previous frame) lives in the
object returned by `new_state()`, one per session.

Calls into an instance are serialised by the model's worker thread, so an
adapter never needs its own locking.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, ClassVar

from ..errors import UNSUPPORTED, RpcError
from ..frames import Frame

Progress = Callable[[str, float], None]


@dataclass
class Audio:
    """Mono audio of an `audio.transcribe` request: little-endian f32 samples."""

    sample_rate: int
    data: bytes

    @property
    def samples(self) -> int:
        return len(self.data) // 4


class Adapter:
    kind: ClassVar[str]
    # Load options that change the weights (part of the sharing key). None = all of them.
    weight_options: ClassVar[tuple[str, ...] | None] = None

    @classmethod
    def available(cls) -> bool:
        """True if this adapter's runtime is installed (must not import heavy modules)."""
        return True

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        self.model_id = model_id
        self.path = path
        self.options = options
        self.device = "cpu"

    def new_state(self) -> Any:
        return None

    def process(self, state: Any, frame: Frame, params: dict) -> tuple[Frame, bool]:
        """Returns (output frame, skipped)."""
        raise RpcError(UNSUPPORTED, f"{self.kind} takes no frames")

    def transcribe(self, state: Any, audio: Audio, params: dict) -> dict:
        """Speech-to-text (`audio.transcribe`): returns `{text, language}`."""
        raise RpcError(UNSUPPORTED, f"{self.kind} takes no audio")

    def extras(self, state: Any) -> dict:
        """Extra fields for the frame result just produced (e.g. `cost_s` from a metered service)."""
        return {}

    def close(self) -> None:
        pass


_REGISTRY: dict[str, Callable[[], type[Adapter]]] = {}


def register(kind: str, loader: Callable[[], type[Adapter]]) -> None:
    """Registers an adapter kind. `loader` imports the module lazily."""
    _REGISTRY[kind] = loader


def adapter_class(kind: str) -> type[Adapter]:
    loader = _REGISTRY.get(kind)
    if loader is None:
        raise RpcError(UNSUPPORTED, f"unknown adapter {kind!r}")
    return loader()


def available_kinds() -> list[str]:
    return sorted(kind for kind, loader in _REGISTRY.items() if loader().available())


def _builtin() -> type[Adapter]:
    from .builtin import BuiltinAdapter

    return BuiltinAdapter


def _diffusion() -> type[Adapter]:
    from .diffusion import DiffusionAdapter

    return DiffusionAdapter


def _onnx() -> type[Adapter]:
    from .onnx_vision import OnnxAdapter

    return OnnxAdapter


def _video() -> type[Adapter]:
    from .video import VideoAdapter

    return VideoAdapter


def _whisper() -> type[Adapter]:
    from .stt_whisper import WhisperAdapter

    return WhisperAdapter


register("builtin", _builtin)
register("diffusion.sd", _diffusion)
register("onnx", _onnx)
register("video.v2v", _video)
register("stt.whisper", _whisper)
