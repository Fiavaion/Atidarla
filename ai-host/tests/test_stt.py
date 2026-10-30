"""`audio.transcribe` and the `stt.whisper` adapter, with the model mocked (nothing is downloaded)."""

from __future__ import annotations

import importlib.machinery
import struct
import sys
import types
from types import SimpleNamespace
from typing import ClassVar

import pytest
from conftest import Client, RpcFailure, open_inline, open_shm

from atd_ai_host.adapters import Audio, available_kinds
from atd_ai_host.errors import INVALID_PARAMS, MODEL_LOAD_FAILED, UNKNOWN_MODEL, UNSUPPORTED
from atd_ai_host.models import CACHE
from atd_ai_host.paths import models_dir


def pcm(samples) -> bytes:
    return struct.pack(f"<{len(samples)}f", *samples)


def transcribe(c: Client, model: str, data: bytes, rate=16_000, seq=1, params=None):
    return c.call(
        "audio.transcribe",
        {"seq": seq, "model_id": model, "sample_rate": rate, "params": params or {}},
        data,
    )


def test_builtin_stt_echo_answers_on_both_transports(host, ring_path):
    for shm in (False, True):
        c = Client(host.addr)
        if shm:
            open_shm(c, ring_path)
        else:
            open_inline(c)
        c.call("model.load", {"model_id": "builtin.stt_echo"})
        r = transcribe(c, "builtin.stt_echo", pcm([0.0, 0.5, -0.25]), seq=4)
        assert r["seq"] == 4 and r["text"] == "3 samples at 16000 Hz, peak 0.500"
        assert r["host_ms"] >= 0
        c.call("session.close")


def test_transcribe_refuses_bad_requests(host):
    c = Client(host.addr)
    open_inline(c)
    with pytest.raises(RpcFailure) as e:
        transcribe(c, "builtin.stt_echo", pcm([0.0]))
    assert e.value.code == UNKNOWN_MODEL
    c.call("model.load", {"model_id": "builtin.stt_echo"})
    for data, rate in ((b"\x00\x00\x00", 16_000), (pcm([0.0]), 100)):
        with pytest.raises(RpcFailure) as e:
            transcribe(c, "builtin.stt_echo", data, rate=rate)
        assert e.value.code == INVALID_PARAMS
    # A frame model takes no audio, and a speech model takes no frames.
    c.call("model.load", {"model_id": "builtin.invert"})
    with pytest.raises(RpcFailure) as e:
        transcribe(c, "builtin.invert", pcm([0.0]))
    assert e.value.code == UNSUPPORTED


class FakeWhisperModel:
    """Stands in for `faster_whisper.WhisperModel`: records its arguments, returns two segments."""

    instances: ClassVar[list] = []

    def __init__(self, path, device="auto", compute_type="default", local_files_only=False):
        self.args = {"path": path, "device": device, "compute_type": compute_type, "local": local_files_only}
        self.calls = []
        self.device = "cpu"
        FakeWhisperModel.instances.append(self)

    def transcribe(self, audio, **kw):
        self.calls.append((audio, kw))
        segments = iter([SimpleNamespace(text=" hello"), SimpleNamespace(text=" world "), SimpleNamespace(text=" ")])
        return segments, SimpleNamespace(language=kw.get("language") or "en")


@pytest.fixture
def fake_whisper(monkeypatch):
    np = pytest.importorskip("numpy")
    module = types.ModuleType("faster_whisper")
    module.__spec__ = importlib.machinery.ModuleSpec("faster_whisper", None)
    module.WhisperModel = FakeWhisperModel
    monkeypatch.setitem(sys.modules, "faster_whisper", module)
    FakeWhisperModel.instances.clear()
    yield np
    for m in CACHE.loaded():
        if m.adapter.kind == "stt.whisper":
            CACHE.release(m)


def test_whisper_adapter_loads_only_a_local_folder(fake_whisper, tmp_path):
    from atd_ai_host.adapters.stt_whisper import WhisperAdapter

    assert WhisperAdapter.available()
    for path in (None, str(tmp_path / "missing")):
        with pytest.raises(Exception) as e:
            WhisperAdapter("whisper-base", path, {}, lambda *_: None)
        assert getattr(e.value, "code", None) == MODEL_LOAD_FAILED
        assert "model manager" in str(e.value)
    a = WhisperAdapter("whisper-base", str(tmp_path), {}, lambda *_: None)
    assert FakeWhisperModel.instances[-1].args == {
        "path": str(tmp_path),
        "device": "auto",
        "compute_type": "auto",
        "local": True,
    }
    assert a.device == "cpu"


def test_whisper_adapter_joins_segments_and_passes_options(fake_whisper, tmp_path):
    np = fake_whisper
    from atd_ai_host.adapters.stt_whisper import WhisperAdapter

    a = WhisperAdapter("whisper-base", str(tmp_path), {}, lambda *_: None)
    out = a.transcribe(
        None,
        Audio(16_000, pcm([0.1] * 160)),
        {"language": "de", "task": "translate", "prompt": "Atidarla", "beam_size": 3, "vad": True},
    )
    assert out == {"text": "hello world", "language": "de"}
    audio, kw = FakeWhisperModel.instances[-1].calls[-1]
    assert audio.dtype == np.float32 and audio.size == 160
    assert kw["language"] == "de" and kw["task"] == "translate" and kw["initial_prompt"] == "Atidarla"
    assert kw["beam_size"] == 3 and kw["vad_filter"] is True
    # Other rates are resampled to 16 kHz; detection when no language is given.
    out = a.transcribe(None, Audio(48_000, pcm([0.0] * 4800)), {})
    audio, kw = FakeWhisperModel.instances[-1].calls[-1]
    assert audio.size == 1600 and kw["language"] is None and kw["task"] == "transcribe"
    assert out["language"] == "en"
    # Silence in, nothing out (the model is not called).
    n = len(FakeWhisperModel.instances[-1].calls)
    assert a.transcribe(None, Audio(16_000, b""), {}) == {"text": "", "language": None}
    assert len(FakeWhisperModel.instances[-1].calls) == n
    for bad in ({"task": "summarise"}, {"beam_size": 0}, {"beam_size": True}):
        with pytest.raises(Exception) as e:
            a.transcribe(None, Audio(16_000, pcm([0.0])), bad)
        assert getattr(e.value, "code", None) == INVALID_PARAMS


def test_whisper_runs_through_the_protocol(fake_whisper, host, tmp_path):
    # Reported once faster-whisper is installed (host.capabilities caches its detection per process).
    assert "stt.whisper" in available_kinds()
    c = Client(host.addr)
    open_inline(c)
    info = c.call("model.load", {"model_id": "whisper-base", "adapter": "stt.whisper", "path": str(tmp_path)})
    assert info["adapter"] == "stt.whisper"
    r = transcribe(c, "whisper-base", pcm([0.2] * 320), seq=9, params={"language": "en"})
    assert (r["seq"], r["text"], r["language"]) == (9, "hello world", "en")
    with pytest.raises(RpcFailure) as e:
        c.call("frame.submit", {"seq": 1, "model_id": "whisper-base", "width": 1, "height": 1}, b"\x00" * 4)
    assert e.value.code == UNSUPPORTED
    c.call("session.close")


def _cached_whisper():
    folder = models_dir() / "whisper-base"
    return folder if (folder / "model.bin").is_file() else None


@pytest.mark.skipif(_cached_whisper() is None, reason="whisper-base is not in the model cache")
def test_real_whisper_base_hears_silence_as_nothing():
    pytest.importorskip("faster_whisper")
    from atd_ai_host.adapters.stt_whisper import WhisperAdapter

    a = WhisperAdapter("whisper-base", str(_cached_whisper()), {"device": "cpu"}, lambda *_: None)
    out = a.transcribe(None, Audio(16_000, pcm([0.0] * 16_000)), {"language": "en", "vad": True})
    assert out["text"] == ""
