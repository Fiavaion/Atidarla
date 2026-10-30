"""Decart realtime video-to-video as a `video.v2v` backend (A59, docs/cloud.md).

Model ids are `decart/<model>` (`decart/lucy-2.6`; `decart` alone means
lucy-2.6). The engine runs this host as `cloud:decart`, so the router keeps it
out of Sovereign mode and charges it per billed second: every frame result
carries `cost_s`, the seconds Decart billed since the previous result (from
its `generation_tick` messages).

Media goes over WebRTC through Decart's MIT-licensed SDK (`decart[realtime]`,
the host's `cloud` extra). Frames are pushed into a video track as they are
submitted and results are the newest frame Decart has sent back, resized to the
input size, so a result is never waited for: until Decart's first frame
arrives the input is returned with `skipped` set.

The key is the user's own: `DECART_API_KEY` in the host's environment
(preferred), else `options.api_key` from the engine. Decart has one prompt, so
the Prompt Blend is reduced to its highest-weight text. Decart's watermark and
provenance data are passed through untouched (their terms).
"""

from __future__ import annotations

import importlib.util
import logging
import os
import threading
from typing import Protocol

from ..adapters import Adapter, Progress
from ..errors import MODEL_LOAD_FAILED, RpcError
from ..frames import Frame

log = logging.getLogger(__name__)

ENV_KEY = "DECART_API_KEY"
DEFAULT_MODEL = "lucy-2.6"
REALTIME_URL = "wss://api.decart.ai"


class Stream(Protocol):
    """One realtime session with Decart."""

    def send(self, frame: Frame) -> None: ...

    def latest(self) -> Frame | None:
        """The newest generated frame, or None before the first."""

    def set_prompt(self, text: str) -> None:
        """Changes the prompt without waiting for the acknowledgement."""

    def billed_seconds(self) -> float:
        """Seconds billed so far in this session (cumulative)."""

    def close(self) -> None: ...


def sdk_available() -> bool:
    return importlib.util.find_spec("decart") is not None and importlib.util.find_spec("livekit") is not None


class SdkStream:
    """A `Stream` over the official SDK; its asyncio loop runs on its own thread."""

    def __init__(self, api_key: str, model: str, base_url: str, prompt: str | None) -> None:
        import asyncio

        from decart import ModelState, Prompt, models
        from decart.realtime import RealtimeClient, RealtimeConnectOptions
        from livekit import rtc

        self._rtc = rtc
        self._lock = threading.Lock()
        self._latest: Frame | None = None
        self._billed = 0.0
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._loop.run_forever, name="decart", daemon=True)
        self._thread.start()
        definition = models.realtime(model)
        self._source = rtc.VideoSource(definition.width, definition.height)
        track = rtc.LocalVideoTrack.create_video_track("atidarla", self._source)

        def on_remote_stream(remote) -> None:
            self._loop.call_soon_threadsafe(lambda: self._loop.create_task(self._pump(remote)))

        initial = ModelState(prompt=Prompt(text=prompt)) if prompt else None
        options = RealtimeConnectOptions(model=definition, on_remote_stream=on_remote_stream, initial_state=initial)
        connect = RealtimeClient.connect(base_url, api_key, track, options, integration="atidarla")
        try:
            self._client = asyncio.run_coroutine_threadsafe(connect, self._loop).result(timeout=60)
        except BaseException:
            self._stop_loop()
            raise
        self._client.on("generation_tick", self._on_tick)
        self._client.on("error", lambda e: log.warning("Decart: %s", e))

    def _on_tick(self, message) -> None:
        with self._lock:
            self._billed = max(self._billed, float(message.seconds))

    async def _pump(self, remote) -> None:
        rgba = self._rtc.VideoBufferType.RGBA
        async for event in self._rtc.VideoStream(remote, format=rgba):
            f = event.frame
            if f.type != rgba:
                f = f.convert(rgba)
            frame = Frame(f.width, f.height, bytes(f.data))
            with self._lock:
                self._latest = frame

    def send(self, frame: Frame) -> None:
        rtc = self._rtc
        self._source.capture_frame(
            rtc.VideoFrame(frame.width, frame.height, rtc.VideoBufferType.RGBA, bytes(frame.data))
        )

    def latest(self) -> Frame | None:
        with self._lock:
            return self._latest

    def set_prompt(self, text: str) -> None:
        import asyncio

        fut = asyncio.run_coroutine_threadsafe(self._client.set_prompt(text, enhance=True), self._loop)
        fut.add_done_callback(lambda f: f.exception() and log.warning("Decart prompt: %s", f.exception()))

    def billed_seconds(self) -> float:
        with self._lock:
            return self._billed

    def close(self) -> None:
        import asyncio

        try:
            asyncio.run_coroutine_threadsafe(self._client.disconnect(), self._loop).result(timeout=10)
        finally:
            self._stop_loop()

    def _stop_loop(self) -> None:
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(timeout=5)


def open_stream(api_key: str, model: str, base_url: str, prompt: str | None) -> Stream:
    return SdkStream(api_key, model, base_url, prompt)


# Replaced by tests with a fake; real hosts use the SDK.
OPEN_STREAM = open_stream


def top_prompt(prompts) -> str | None:
    """The highest-weight prompt text of a Prompt Blend (Decart takes one prompt)."""
    best: tuple[float, str] | None = None
    for p in prompts or ():
        if not isinstance(p, dict):
            continue
        text = str(p.get("text") or "").strip()
        weight = float(p.get("weight", 1.0))
        if text and weight > 0 and (best is None or weight > best[0]):
            best = (weight, text)
    return best[1] if best else None


def _resize(frame: Frame, width: int, height: int) -> Frame:
    """Nearest-neighbour resize (Decart answers at its own resolution)."""
    if (frame.width, frame.height) == (width, height):
        return frame
    import numpy as np

    src = np.frombuffer(frame.data, dtype=np.uint8).reshape(frame.height, frame.width, 4)
    ys = (np.arange(height) * frame.height // height).clip(0, frame.height - 1)
    xs = (np.arange(width) * frame.width // width).clip(0, frame.width - 1)
    return Frame(width, height, src[ys][:, xs].tobytes())


class DecartBackend(Adapter):
    kind = "video.v2v"

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        super().__init__(model_id, path, options, progress)
        _, _, name = model_id.partition("/")
        self.model = name or DEFAULT_MODEL
        key = os.environ.get(ENV_KEY, "").strip() or str(options.get("api_key") or "").strip()
        if not key:
            raise RpcError(MODEL_LOAD_FAILED, f"no Decart API key: set {ENV_KEY} in the AI host's environment")
        self.device = "decart"
        self._prompt: str | None = top_prompt(options.get("prompts")) or options.get("prompt")
        self._reported = 0.0
        self._last_out: Frame | None = None
        progress("connecting", 0.0)
        self._stream = OPEN_STREAM(key, self.model, options.get("base_url") or REALTIME_URL, self._prompt)
        progress("ready", 1.0)

    def process(self, state, frame: Frame, params: dict) -> tuple[Frame, bool]:
        text = top_prompt(params.get("prompts"))
        if text and text != self._prompt:
            self._stream.set_prompt(text)
            self._prompt = text
        self._stream.send(frame)
        out = self._stream.latest()
        if out is None:
            return Frame(frame.width, frame.height, bytes(frame.data)), True
        fresh = out is not self._last_out
        self._last_out = out
        return _resize(out, frame.width, frame.height), not fresh

    def extras(self, state) -> dict:
        billed = self._stream.billed_seconds()
        delta = max(0.0, billed - self._reported)
        self._reported = max(self._reported, billed)
        return {"cost_s": delta}

    def close(self) -> None:
        self._stream.close()
