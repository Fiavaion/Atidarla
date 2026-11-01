"""Decart `video.v2v` backend against a fake realtime session (no network)."""

from __future__ import annotations

import time
from typing import ClassVar

import pytest
from conftest import Client, RpcFailure, open_inline, pixels

from lsr_ai_host import capabilities
from lsr_ai_host.adapters import video
from lsr_ai_host.cloud import decart
from lsr_ai_host.errors import MODEL_LOAD_FAILED
from lsr_ai_host.frames import Frame


class FakeStream:
    """Stands in for Decart: echoes frames inverted at 2× size, bills 1 s per frame."""

    opened: ClassVar[list[FakeStream]] = []

    def __init__(self, api_key, model, base_url, prompt):
        self.api_key, self.model, self.base_url = api_key, model, base_url
        self.prompts = [prompt] if prompt else []
        self.sent = 0
        self.out: Frame | None = None
        self.closed = False
        FakeStream.opened.append(self)

    def send(self, frame):
        self.sent += 1
        # Decart's first frame arrives after the second input.
        if self.sent >= 2:
            w, h = frame.width * 2, frame.height * 2
            data = bytearray(w * h * 4)
            for y in range(h):
                for x in range(w):
                    i = ((y // 2) * frame.width + x // 2) * 4
                    o = (y * w + x) * 4
                    data[o : o + 4] = bytes(255 - b for b in frame.data[i : i + 3]) + bytes([255])
            self.out = Frame(w, h, bytes(data))

    def latest(self):
        return self.out

    def set_prompt(self, text):
        self.prompts.append(text)

    def billed_seconds(self):
        return float(self.sent)

    def close(self):
        self.closed = True


@pytest.fixture
def fake(monkeypatch):
    FakeStream.opened.clear()
    monkeypatch.setattr(decart, "OPEN_STREAM", FakeStream)
    monkeypatch.setitem(video.BACKENDS, "decart", decart.DecartBackend)
    monkeypatch.setenv(decart.ENV_KEY, "env-key")
    capabilities.detect.cache_clear()  # it lists adapters with a backend
    yield FakeStream
    capabilities.detect.cache_clear()


def test_top_prompt_takes_the_heaviest_text():
    assert decart.top_prompt(None) is None
    assert decart.top_prompt([{"text": " ", "weight": 5}]) is None
    blend = [{"text": "forest", "weight": 0.25}, {"text": "city", "weight": 0.75}, {"text": "sea", "weight": 0}]
    assert decart.top_prompt(blend) == "city"


def test_the_key_comes_from_the_environment_first(fake, monkeypatch):
    def progress(*_):
        pass

    b = decart.DecartBackend("decart/lucy-2.6", None, {"api_key": "opt-key"}, progress)
    assert fake.opened[-1].api_key == "env-key" and b.model == "lucy-2.6"
    assert fake.opened[-1].base_url == decart.REALTIME_URL
    monkeypatch.delenv(decart.ENV_KEY)
    decart.DecartBackend("decart", None, {"api_key": "opt-key"}, progress)
    assert fake.opened[-1].api_key == "opt-key" and fake.opened[-1].model == decart.DEFAULT_MODEL
    with pytest.raises(Exception) as e:
        decart.DecartBackend("decart", None, {}, progress)
    assert e.value.code == MODEL_LOAD_FAILED and "DECART_API_KEY" in e.value.message


def test_frames_prompts_and_billed_seconds_through_the_protocol(host, fake):
    c = Client(host.addr)
    open_inline(c)
    caps = c.call("host.capabilities")
    assert "video.v2v" in caps["adapters"]
    info = c.call(
        "model.load", {"model_id": "decart/lucy-2.6", "adapter": "video.v2v", "options": {"prompt": "a forest"}}
    )
    assert info["device"] == "decart"
    stream = fake.opened[-1]
    w, h = 4, 2
    prompts = [{"text": "a city", "weight": 0.7}, {"text": "a forest", "weight": 0.3}]

    def frame(seq, blend):
        params = {"prompts": blend}
        sub = {"seq": seq, "timestamp_us": seq, "model_id": "decart/lucy-2.6", "width": w, "height": h}
        return c.call("frame.submit", {**sub, "params": params}, pixels(w, h, seq))

    # No generated frame yet: the input comes back, marked skipped.
    r1, p1 = frame(1, prompts)
    assert r1["skipped"] is True and p1 == pixels(w, h, 1)
    assert r1["cost_s"] == 1.0
    # Decart's frame arrives, resized back to the input size.
    r2, p2 = frame(2, prompts)
    assert (r2["width"], r2["height"], r2["skipped"]) == (w, h, False)
    src = pixels(w, h, 2)
    assert p2[:3] == bytes(255 - b for b in src[:3]) and p2[3] == 255
    assert r2["cost_s"] == 1.0
    # The prompt went once, when the heaviest text changed.
    assert stream.prompts == ["a forest", "a city"]
    frame(3, prompts)
    assert stream.prompts == ["a forest", "a city"]
    frame(4, [{"text": "a desert", "weight": 1.0}])
    assert stream.prompts[-1] == "a desert"
    # Unloading closes the session.
    c.call("model.unload", {"model_id": "decart/lucy-2.6"})
    c.close()
    deadline = time.monotonic() + 5
    while not stream.closed and time.monotonic() < deadline:
        time.sleep(0.01)
    assert stream.closed


def test_without_a_key_the_load_fails_cleanly(host, fake, monkeypatch):
    monkeypatch.delenv(decart.ENV_KEY)
    c = Client(host.addr)
    open_inline(c)
    with pytest.raises(RpcFailure) as e:
        c.call("model.load", {"model_id": "decart/lucy-2.6", "adapter": "video.v2v", "options": {}})
    assert e.value.code == MODEL_LOAD_FAILED
    c.close()


def test_without_the_sdk_video_v2v_is_not_advertised():
    if decart.sdk_available():
        pytest.skip("the cloud extra is installed")
    assert "decart" not in video.BACKENDS
