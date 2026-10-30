"""Speech-to-text with faster-whisper (adapter `stt.whisper`, FR-A13).

faster-whisper and CTranslate2 are MIT. The weights are the CTranslate2
conversions in the model catalogue (`whisper-base`, `whisper-small`, MIT),
downloaded by the engine's model manager into the per-user cache: this
adapter only ever loads a local folder (`local_files_only`), so it never
fetches anything itself (CLAUDE.md rule 4, Sovereign mode).

`audio.transcribe` params: `language` (ISO code, empty = detect), `task`
(`transcribe` | `translate`), `prompt` (initial prompt: names and words to
expect), `beam_size` (1-10), `vad` (faster-whisper's own VAD filter).
"""

from __future__ import annotations

from pathlib import Path

from ..capabilities import installed
from ..errors import INVALID_PARAMS, MODEL_LOAD_FAILED, RpcError
from . import Adapter, Audio, Progress

# Whisper models take 16 kHz mono.
WHISPER_RATE = 16_000
TASKS = ("transcribe", "translate")


def to_16k(audio: Audio):
    """The samples as float32 at 16 kHz (linear interpolation if the rate differs)."""
    import numpy as np

    x = np.frombuffer(audio.data[: audio.samples * 4], dtype="<f4").astype(np.float32)
    if audio.sample_rate == WHISPER_RATE or x.size == 0:
        return x
    n = max(1, round(x.size * WHISPER_RATE / audio.sample_rate))
    t = np.linspace(0.0, x.size - 1, n)
    return np.interp(t, np.arange(x.size), x).astype(np.float32)


class WhisperAdapter(Adapter):
    kind = "stt.whisper"
    weight_options = ("device", "compute_type")

    @classmethod
    def available(cls) -> bool:
        return installed("faster_whisper") and installed("numpy")

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        super().__init__(model_id, path, options, progress)
        if not path or not Path(path).is_dir():
            raise RpcError(
                MODEL_LOAD_FAILED,
                f"{model_id!r} is not downloaded: get it with the model manager (`atidarla models download {model_id}`)",
            )
        from faster_whisper import WhisperModel

        progress("weights", 0.0)
        device = options.get("device", "auto")
        self.model = WhisperModel(
            path,
            device=device,
            # CTranslate2 picks the fastest type the device supports (int8 on CPU, float16 on CUDA).
            compute_type=options.get("compute_type", "auto"),
            local_files_only=True,
        )
        self.device = getattr(self.model, "device", None) or str(device)
        progress("ready", 1.0)

    def transcribe(self, state: None, audio: Audio, params: dict) -> dict:
        task = params.get("task") or "transcribe"
        if task not in TASKS:
            raise RpcError(INVALID_PARAMS, f"task must be one of {', '.join(TASKS)}")
        language = params.get("language") or None
        beam = params.get("beam_size", 1)
        if not isinstance(beam, int) or isinstance(beam, bool) or not 1 <= beam <= 10:
            raise RpcError(INVALID_PARAMS, "beam_size must be an integer from 1 to 10")
        samples = to_16k(audio)
        if samples.size == 0:
            return {"text": "", "language": language}
        segments, info = self.model.transcribe(
            samples,
            language=language,
            task=task,
            beam_size=beam,
            initial_prompt=params.get("prompt") or None,
            vad_filter=bool(params.get("vad", False)),
            condition_on_previous_text=False,
        )
        text = " ".join(s.text.strip() for s in segments if s.text.strip())
        return {"text": text, "language": getattr(info, "language", None) or language}
