"""Process-wide model cache and per-model workers.

The engine runs one host process and opens a session per AI node, so a
model loaded by several sessions is loaded once: instances are shared by
(adapter, model_id, path, weight options) and ref-counted by `model.load`,
`model.unload` and session close. Each shared model has one worker thread
that runs frames from every session in arrival order (one GPU stream).
"""

from __future__ import annotations

import json
import logging
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from .adapters import Adapter, Audio, Progress, adapter_class
from .errors import MODEL_LOAD_FAILED, UNSUPPORTED, RpcError
from .frames import Frame

if TYPE_CHECKING:
    from .session import Session

log = logging.getLogger("lsr_ai_host.models")

# Frames a session may have waiting (not yet running) on one model before
# `frame.submit` answers BUSY.
MAX_QUEUED = 4


@dataclass(eq=False)
class Job:
    session: Session
    request_id: Any
    model_id: str
    seq: int
    # A frame (`frame.submit`) or audio (`audio.transcribe`).
    frame: Frame | None
    params: dict
    output_slot: int | None
    audio: Audio | None = None
    received: float = field(default_factory=time.perf_counter)
    cancelled: bool = False


class SharedModel:
    def __init__(self, key: tuple, adapter: Adapter, load_ms: float) -> None:
        self.key = key
        self.adapter = adapter
        self.load_ms = load_ms
        self.refs = 0
        self._queue: deque[Job] = deque()
        self._cond = threading.Condition()
        self._running: Job | None = None
        self._stopping = False
        self._thread = threading.Thread(target=self._run, name=f"model:{adapter.model_id}", daemon=True)
        self._thread.start()

    def submit(self, job: Job) -> bool:
        """Queues a job; False if the session already has MAX_QUEUED frames waiting."""
        with self._cond:
            if sum(1 for j in self._queue if j.session is job.session) >= MAX_QUEUED:
                return False
            self._queue.append(job)
            self._cond.notify()
            return True

    def cancel(self, session: Session, seq: int) -> Job | None:
        """Removes a queued frame, or marks the running one cancelled. Returns the job."""
        with self._cond:
            for job in self._queue:
                if job.session is session and job.seq == seq:
                    self._queue.remove(job)
                    job.cancelled = True
                    return job
            running = self._running
            if running is not None and running.session is session and running.seq == seq:
                running.cancelled = True
            return None

    def drop_session(self, session: Session) -> list[Job]:
        """Removes the session's queued frames (returned) and waits for its running one."""
        with self._cond:
            dropped = [j for j in self._queue if j.session is session]
            self._queue = deque(j for j in self._queue if j.session is not session)
            while self._running is not None and self._running.session is session:
                self._cond.wait()
            return dropped

    def depth(self, session: Session) -> int:
        with self._cond:
            n = sum(1 for j in self._queue if j.session is session)
            return n + (self._running is not None and self._running.session is session)

    def stop(self) -> None:
        with self._cond:
            self._stopping = True
            self._cond.notify_all()

    def _run(self) -> None:
        while True:
            with self._cond:
                while not self._queue and not self._stopping:
                    self._cond.wait()
                if self._stopping and not self._queue:
                    break
                job = self._running = self._queue.popleft()
            try:
                job.session.execute(job, self.adapter)
            except Exception:  # execute answers the request itself; this is a last resort
                log.exception("frame %s failed outside its handler", job.seq)
            finally:
                with self._cond:
                    self._running = None
                    self._cond.notify_all()
        try:
            self.adapter.close()
        except Exception:
            log.exception("closing %s failed", self.adapter.model_id)


@dataclass(eq=False)
class _Loading:
    done: threading.Event = field(default_factory=threading.Event)
    model: SharedModel | None = None
    error: RpcError | None = None


class ModelCache:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._entries: dict[tuple, _Loading] = {}

    @staticmethod
    def key(kind: str, model_id: str, path: str | None, options: dict) -> tuple:
        cls = adapter_class(kind)
        if cls.weight_options is None:
            weights = options
        else:
            weights = {k: options[k] for k in cls.weight_options if k in options}
        return kind, model_id, path, json.dumps(weights, sort_keys=True, default=str)

    def acquire(self, kind: str, model_id: str, path: str | None, options: dict, progress: Progress) -> SharedModel:
        cls = adapter_class(kind)
        if not cls.available():
            raise RpcError(UNSUPPORTED, f"adapter {kind!r} is not installed on this host")
        key = self.key(kind, model_id, path, options)
        with self._lock:
            entry = self._entries.get(key)
            loader = entry is None
            if loader:
                entry = self._entries[key] = _Loading()
        if loader:
            t0 = time.perf_counter()
            try:
                adapter = cls(model_id, path, options, progress)
                entry.model = SharedModel(key, adapter, (time.perf_counter() - t0) * 1e3)
            except RpcError as e:
                entry.error = e
            except MemoryError:
                entry.error = RpcError(MODEL_LOAD_FAILED, f"out of memory loading {model_id!r}")
            except Exception as e:  # weights unreadable, runtime errors, OOM from the GPU runtime
                log.exception("loading %s failed", model_id)
                entry.error = RpcError(MODEL_LOAD_FAILED, f"{type(e).__name__}: {e}")
            if entry.error is not None:
                with self._lock:
                    self._entries.pop(key, None)
            entry.done.set()
        else:
            entry.done.wait()
        if entry.error is not None:
            raise entry.error
        with self._lock:
            entry.model.refs += 1
        return entry.model

    def release(self, model: SharedModel) -> None:
        with self._lock:
            model.refs -= 1
            if model.refs > 0:
                return
            self._entries.pop(model.key, None)
        model.stop()

    def loaded(self) -> list[SharedModel]:
        with self._lock:
            return [e.model for e in self._entries.values() if e.model is not None]


CACHE = ModelCache()
