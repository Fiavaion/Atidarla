"""One protocol session: a TCP stream, its reader thread and its per-session state.

The reader thread dispatches every request as it arrives. Cheap requests
(`host.health`, `frame.cancel`, ...) are answered at once, `model.load` runs
on its own thread, and `frame.submit` is queued on the model's worker. So a
health check is answered while a frame is being processed, and responses may
leave out of order; every response is written whole under one lock.
"""

from __future__ import annotations

import hmac
import logging
import socket
import threading
import time
import uuid
from dataclasses import dataclass
from typing import Any, ClassVar

from . import PROTOCOL_VERSION, __version__
from .adapters.builtin import BUILTIN_MODELS
from .capabilities import capabilities, gpu_mem_mb
from .errors import (
    BAD_SLOT,
    BUSY,
    INTERNAL_ERROR,
    INVALID_PARAMS,
    INVALID_REQUEST,
    METHOD_NOT_FOUND,
    STALE_FRAME,
    UNAUTHORIZED,
    UNKNOWN_MODEL,
    UNSUPPORTED,
    VERSION_MISMATCH,
    RpcError,
)
from .frames import Frame, decode, encode, negotiate_codec
from .framing import FramingError, encode_message, read_message
from .models import CACHE, Job, SharedModel
from .ring import FrameRing

log = logging.getLogger("atd_ai_host.session")


class HostContext:
    """State shared by every session of one host process."""

    def __init__(self, token: str | None = None) -> None:
        self.token = token
        self.started = time.monotonic()
        self._lock = threading.Lock()
        self.sessions: set[Session] = set()

    def add(self, session: Session) -> None:
        with self._lock:
            self.sessions.add(session)

    def remove(self, session: Session) -> None:
        with self._lock:
            self.sessions.discard(session)

    def broadcast(self, method: str, params: dict) -> None:
        with self._lock:
            sessions = list(self.sessions)
        for s in sessions:
            if s.opened:
                s.notify(method, params)


class HostLogHandler(logging.Handler):
    """Forwards host warnings and errors to every open session as `host.log`."""

    def __init__(self, ctx: HostContext) -> None:
        super().__init__(logging.WARNING)
        self.ctx = ctx
        self._busy = threading.local()

    def emit(self, record: logging.LogRecord) -> None:
        if getattr(self._busy, "on", False):
            return
        self._busy.on = True
        try:
            self.ctx.broadcast("host.log", {"level": record.levelname.lower(), "message": record.getMessage()})
        finally:
            self._busy.on = False


@dataclass(eq=False)
class Loaded:
    shared: SharedModel
    state: Any


def _uint(p: dict, name: str, required: bool = True) -> int | None:
    v = p.get(name)
    if v is None and not required:
        return None
    if not isinstance(v, int) or isinstance(v, bool) or v < 0:
        raise RpcError(INVALID_PARAMS, f"{name} must be a non-negative integer")
    return v


class Session:
    def __init__(self, sock: socket.socket, ctx: HostContext) -> None:
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.sock = sock
        self.ctx = ctx
        self.session_id = uuid.uuid4().hex
        self._reader = sock.makefile("rb")
        self._wlock = threading.Lock()
        self._lock = threading.Lock()
        self.models: dict[str, Loaded] = {}
        self.ring: FrameRing | None = None
        self.transport: str | None = None
        self.codec: str | None = None
        self.opened = False
        self.closing = False
        self.frames = 0

    # ---- wire -------------------------------------------------------------

    def send(self, msg: dict, payload: bytes | memoryview | None = None) -> None:
        chunks = encode_message({"jsonrpc": "2.0", **msg}, payload)
        with self._wlock:
            try:
                for chunk in chunks:
                    self.sock.sendall(chunk)
            except OSError:
                self.closing = True

    def reply(self, request_id: Any, result: Any, payload: bytes | memoryview | None = None) -> None:
        self.send({"id": request_id, "result": result}, payload)

    def fail(self, request_id: Any, code: int, message: str) -> None:
        self.send({"id": request_id, "error": {"code": code, "message": message}})

    def notify(self, method: str, params: dict) -> None:
        self.send({"method": method, "params": params})

    # ---- reader loop ------------------------------------------------------

    def run(self) -> None:
        self.ctx.add(self)
        try:
            while not self.closing:
                msg = read_message(self._reader)
                if msg is None:
                    break
                self.dispatch(*msg)
        except FramingError as e:
            log.info("dropping session %s: %s", self.session_id, e)
            self.fail(None, INVALID_REQUEST, str(e))
        except OSError:
            pass
        finally:
            self.teardown()
            self.ctx.remove(self)
            try:
                self._reader.close()
                self.sock.close()
            except OSError:
                pass

    def dispatch(self, msg: dict, payload: bytes | None) -> None:
        request_id = msg.get("id")
        method = msg.get("method")
        if request_id is None:
            return  # a client notification; none are defined
        if not isinstance(method, str):
            self.fail(request_id, INVALID_REQUEST, "missing method")
            return
        params = msg.get("params") or {}
        try:
            if not isinstance(params, dict):
                raise RpcError(INVALID_PARAMS, "params must be an object")
            if not self.opened and method != "session.open":
                code = UNAUTHORIZED if self.ctx.token else INVALID_REQUEST
                raise RpcError(code, "open a session first")
            if payload is not None and method != "frame.submit":
                raise RpcError(INVALID_REQUEST, f"{method} takes no payload")
            if method == "frame.submit":
                self.submit(request_id, params, payload)
                return
            if method == "model.load":
                threading.Thread(target=self._load, args=(request_id, params), daemon=True).start()
                return
            handler = self._handlers.get(method)
            if handler is None:
                raise RpcError(METHOD_NOT_FOUND, f"unknown method {method!r}")
            self.reply(request_id, handler(self, params))
        except RpcError as e:
            self.fail(request_id, e.code, e.message)
            if e.code == UNAUTHORIZED:
                self.closing = True  # no second guess on the same connection
        except Exception as e:  # never let one bad request kill the host
            log.exception("%s failed", method)
            self.fail(request_id, INTERNAL_ERROR, f"{type(e).__name__}: {e}")

    # ---- methods ----------------------------------------------------------

    def session_open(self, p: dict) -> dict:
        if self.opened:
            raise RpcError(INVALID_REQUEST, "session already open")
        if p.get("protocol_version") != PROTOCOL_VERSION:
            raise RpcError(VERSION_MISMATCH, f"host speaks {PROTOCOL_VERSION}, client sent {p.get('protocol_version')}")
        if self.ctx.token is not None:
            token = p.get("token")
            if not isinstance(token, str) or not hmac.compare_digest(token.encode(), self.ctx.token.encode()):
                raise RpcError(UNAUTHORIZED, "missing or wrong token")
        transport = p.get("transport")
        if not isinstance(transport, dict):
            raise RpcError(INVALID_PARAMS, "transport must be an object")
        kind = transport.get("kind")
        if kind == "shm":
            path = transport.get("ring_path")
            if not isinstance(path, str):
                raise RpcError(INVALID_PARAMS, "shm transport needs ring_path")
            try:
                self.ring = FrameRing(path)
            except (OSError, ValueError) as e:
                raise RpcError(INVALID_PARAMS, f"cannot map ring: {e}") from None
        elif kind == "inline":
            codecs = transport.get("codecs") or ["rgba8"]
            if not isinstance(codecs, list):
                raise RpcError(INVALID_PARAMS, "codecs must be a list")
            self.codec = negotiate_codec(codecs)
            if self.codec is None:
                raise RpcError(UNSUPPORTED, f"none of the codecs {codecs} is available on this host")
        else:
            raise RpcError(UNSUPPORTED, f"transport {kind!r} is not available on this host")
        self.transport = kind
        self.opened = True
        return {
            "session_id": self.session_id,
            "protocol_version": PROTOCOL_VERSION,
            "host_name": socket.gethostname(),
            "host_version": __version__,
            "transport": kind,
            "codec": self.codec,
        }

    def host_capabilities(self, _p: dict) -> dict:
        return capabilities()

    def host_health(self, _p: dict) -> dict:
        with self._lock:
            loaded = list(self.models.items())
        return {
            "uptime_s": time.monotonic() - self.ctx.started,
            "frames_processed": self.frames,
            "loaded_models": sorted(name for name, _ in loaded),
            "queue_depth": sum(m.shared.depth(self) for _, m in loaded),
            "gpu_mem_mb": gpu_mem_mb(),
        }

    def _load(self, request_id: Any, p: dict) -> None:
        try:
            self.reply(request_id, self.model_load(p))
        except RpcError as e:
            self.fail(request_id, e.code, e.message)
        except Exception as e:
            log.exception("model.load failed")
            self.fail(request_id, INTERNAL_ERROR, f"{type(e).__name__}: {e}")

    def model_load(self, p: dict) -> dict:
        model_id = p.get("model_id")
        if not isinstance(model_id, str) or not model_id:
            raise RpcError(INVALID_PARAMS, "model_id must be a string")
        kind = p.get("adapter") or ("builtin" if model_id in BUILTIN_MODELS else None)
        if not isinstance(kind, str):
            raise RpcError(INVALID_PARAMS, "adapter must be a string")
        path = p.get("path")
        if path is not None and not isinstance(path, str):
            raise RpcError(INVALID_PARAMS, "path must be a string or null")
        options = p.get("options") or {}
        if not isinstance(options, dict):
            raise RpcError(INVALID_PARAMS, "options must be an object")

        def progress(stage: str, fraction: float) -> None:
            self.notify("model.progress", {"model_id": model_id, "stage": stage, "fraction": round(fraction, 3)})

        t0 = time.perf_counter()
        shared = CACHE.acquire(kind, model_id, path, options, progress)
        with self._lock:
            old = self.models.get(model_id)
            if self.closing or (old is not None and old.shared is shared):
                CACHE.release(shared)  # session gone, or already holding a reference
            else:
                self.models[model_id] = Loaded(shared, shared.adapter.new_state())
        if old is not None and old.shared is not shared:
            self._release(old)
        return {
            "model_id": model_id,
            "load_ms": (time.perf_counter() - t0) * 1e3,
            "adapter": kind,
            "device": shared.adapter.device,
        }

    def model_unload(self, p: dict) -> None:
        with self._lock:
            loaded = self.models.pop(p.get("model_id"), None)
        if loaded is not None:
            self._release(loaded)

    def _release(self, loaded: Loaded) -> None:
        for job in loaded.shared.drop_session(self):
            self._release_input(job)
            self.fail(job.request_id, STALE_FRAME, f"frame {job.seq} dropped: model unloaded")
        CACHE.release(loaded.shared)

    def frame_cancel(self, p: dict) -> None:
        seq = _uint(p, "seq")
        with self._lock:
            loaded = list(self.models.values())
        for m in loaded:
            job = m.shared.cancel(self, seq)
            if job is not None:
                self._release_input(job)
                self.fail(job.request_id, STALE_FRAME, f"frame {seq} cancelled")

    def session_close(self, _p: dict) -> None:
        self.teardown()
        self.closing = True

    _handlers: ClassVar[dict] = {
        "session.open": session_open,
        "host.capabilities": host_capabilities,
        "host.health": host_health,
        "model.unload": model_unload,
        "frame.cancel": frame_cancel,
        "session.close": session_close,
    }

    # ---- frames -----------------------------------------------------------

    def submit(self, request_id: Any, p: dict, payload: bytes | None) -> None:
        seq = _uint(p, "seq")
        width, height = _uint(p, "width"), _uint(p, "height")
        if width == 0 or height == 0:
            raise RpcError(INVALID_PARAMS, "width and height must be positive")
        model_id = p.get("model_id")
        with self._lock:
            loaded = self.models.get(model_id)
        if loaded is None:
            raise RpcError(UNKNOWN_MODEL, f"model {model_id!r} not loaded")
        params = p.get("params") or {}
        if not isinstance(params, dict):
            raise RpcError(INVALID_PARAMS, "params must be an object")
        aux = p.get("aux") or {}
        if not isinstance(aux, dict):
            raise RpcError(INVALID_PARAMS, "aux must be an object")
        output_slot = None
        if self.transport == "shm":
            if payload is not None:
                raise RpcError(INVALID_REQUEST, "the shm transport takes no payload")
            input_slot, output_slot = _uint(p, "input_slot"), _uint(p, "output_slot")
            frame = Frame(width, height, self._slot(input_slot, seq, width, height))
            self._slot_index(output_slot)
            for name, slot in aux.items():
                if not isinstance(slot, int) or isinstance(slot, bool):
                    raise RpcError(INVALID_PARAMS, f"aux slot {name!r} must be an integer")
                frame.aux[name] = Frame(width, height, self._slot(slot, seq, width, height))
        else:
            if payload is None:
                raise RpcError(INVALID_PARAMS, "the inline transport needs a payload")
            if aux:
                raise RpcError(UNSUPPORTED, "aux inputs are not supported on the inline transport")
            frame = decode(self.codec, payload, width, height)
        job = Job(self, request_id, model_id, seq, frame, params, output_slot)
        if not loaded.shared.submit(job):
            self._release_input(job)
            raise RpcError(BUSY, f"model {model_id!r} has too many queued frames")

    def _slot_index(self, slot: int) -> None:
        try:
            self.ring.seq(slot)
        except IndexError as e:
            raise RpcError(BAD_SLOT, str(e)) from None

    def _slot(self, slot: int, seq: int, width: int, height: int) -> memoryview:
        self._slot_index(slot)
        if self.ring.seq(slot) != seq:
            raise RpcError(BAD_SLOT, f"slot {slot} holds frame {self.ring.seq(slot)}, not {seq}")
        if not self.ring.fits(width, height):
            raise RpcError(
                BAD_SLOT, f"a {width}x{height} frame does not fit the ring's {self.ring.slot_bytes}-byte slots"
            )
        return self.ring.frame(slot, width, height)

    @staticmethod
    def _release_input(job: Job) -> None:
        """Lets go of views into the ring so it can be unmapped."""
        for f in (job.frame, *job.frame.aux.values()):
            if isinstance(f.data, memoryview):
                f.data.release()

    def execute(self, job: Job, adapter) -> None:
        """Runs one frame on the model's worker thread and answers it."""
        try:
            if job.cancelled or self.closing:
                self.fail(job.request_id, STALE_FRAME, f"frame {job.seq} cancelled")
                return
            with self._lock:
                loaded = self.models.get(job.model_id)
            if loaded is None:
                self.fail(job.request_id, UNKNOWN_MODEL, f"model {job.model_id!r} unloaded")
                return
            out, skipped = adapter.process(loaded.state, job.frame, job.params)
            if job.cancelled:
                self.fail(job.request_id, STALE_FRAME, f"frame {job.seq} cancelled")
                return
            result = {"seq": job.seq, "width": out.width, "height": out.height, "output_slot": job.output_slot}
            payload = None
            if self.transport == "shm":
                try:
                    self.ring.write(job.output_slot, job.seq, out.data)
                except IndexError as e:
                    raise RpcError(BAD_SLOT, f"output does not fit: {e}") from None
            else:
                payload = encode(self.codec, out)
            self.frames += 1
            result["host_ms"] = (time.perf_counter() - job.received) * 1e3
            result["skipped"] = skipped
            result.update(adapter.extras(loaded.state))
            self.reply(job.request_id, result, payload)
        except RpcError as e:
            self.fail(job.request_id, e.code, e.message)
        except Exception as e:
            log.exception("frame %s failed", job.seq)
            self.fail(job.request_id, INTERNAL_ERROR, f"{type(e).__name__}: {e}")
        finally:
            self._release_input(job)

    # ---- teardown ---------------------------------------------------------

    def teardown(self) -> None:
        """Answers or drops pending frames, releases models and unmaps the ring. Idempotent."""
        with self._lock:
            self.closing = True
            loaded = list(self.models.values())
            self.models.clear()
        for m in loaded:
            self._release(m)
        if self.ring is not None:
            self.ring.close()
            self.ring = None
