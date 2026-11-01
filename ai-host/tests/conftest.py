"""Test helpers: an in-process host, a protocol client and a slow test adapter."""

from __future__ import annotations

import os
import socket
import tempfile
import threading
import time
from typing import ClassVar

import pytest

from lsr_ai_host import PROTOCOL_VERSION
from lsr_ai_host.adapters import Adapter, register
from lsr_ai_host.frames import Frame
from lsr_ai_host.framing import encode_message, read_message
from lsr_ai_host.ring import FrameRing, remove_quietly
from lsr_ai_host.session import HostContext, Session


class SlowAdapter(Adapter):
    """Copies its input after `params.sleep_ms`; records the order frames ran in."""

    kind = "test.slow"
    weight_options = ()
    order: ClassVar[list] = []
    # Tags of frames that have begun running (before their sleep).
    started: ClassVar[list] = []

    def __init__(self, model_id, path, options, progress):
        super().__init__(model_id, path, options, progress)
        progress("weights", 0.5)
        progress("ready", 1.0)

    def process(self, state, frame, params):
        SlowAdapter.started.append(params.get("tag"))
        time.sleep(params.get("sleep_ms", 0) / 1000)
        SlowAdapter.order.append(params.get("tag"))
        return Frame(frame.width, frame.height, bytes(frame.data)), False


register("test.slow", lambda: SlowAdapter)


class RpcFailure(Exception):
    def __init__(self, code, message):
        super().__init__(f"{code}: {message}")
        self.code = code


class Client:
    """A minimal engine-side client: pipelined requests, payloads, notifications."""

    def __init__(self, addr: tuple[str, int] | socket.socket) -> None:
        self.sock = addr if isinstance(addr, socket.socket) else socket.create_connection(addr)
        self.sock.settimeout(20)
        self.reader = self.sock.makefile("rb")
        self.next_id = 1
        self.inbox: dict[int, tuple[dict, bytes | None]] = {}
        self.arrivals: list = []  # ids (and notification methods) in arrival order
        self.notifications: list[dict] = []

    def send(self, method: str, params: dict | None = None, payload: bytes | None = None) -> int:
        rid = self.next_id
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            msg["params"] = params
        for chunk in encode_message(msg, payload):
            self.sock.sendall(chunk)
        return rid

    def _read_one(self) -> None:
        got = read_message(self.reader)
        if got is None:
            raise ConnectionError("host closed the connection")
        msg, payload = got
        if "id" in msg and msg["id"] is not None:
            self.inbox[msg["id"]] = (msg, payload)
            self.arrivals.append(msg["id"])
        else:
            self.notifications.append(msg)
            self.arrivals.append(msg.get("method"))

    def wait(self, rid: int) -> tuple[dict, bytes | None]:
        while rid not in self.inbox:
            self._read_one()
        return self.inbox.pop(rid)

    def result(self, rid: int):
        msg, payload = self.wait(rid)
        if "error" in msg:
            raise RpcFailure(msg["error"]["code"], msg["error"]["message"])
        if payload is not None:
            return msg["result"], payload
        return msg["result"]

    def call(self, method: str, params: dict | None = None, payload: bytes | None = None):
        return self.result(self.send(method, params, payload))

    def close(self) -> None:
        try:
            self.reader.close()
            self.sock.close()
        except OSError:
            pass


class InProcessHost:
    def __init__(self, token: str | None = None) -> None:
        self.ctx = HostContext(token)
        self.server = socket.create_server(("127.0.0.1", 0))
        self.addr = self.server.getsockname()
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self) -> None:
        while True:
            try:
                conn, _ = self.server.accept()
            except OSError:
                return
            threading.Thread(target=Session(conn, self.ctx).run, daemon=True).start()

    def close(self) -> None:
        self.server.close()


@pytest.fixture
def host():
    h = InProcessHost()
    yield h
    h.close()


@pytest.fixture
def ring_path():
    fd, path = tempfile.mkstemp(prefix="lsr-test-ring-")
    os.close(fd)
    yield path
    remove_quietly(path)


def open_shm(client: Client, path: str, slots: int = 8, w: int = 4, h: int = 2) -> FrameRing:
    ring = FrameRing.create(path, slots, w, h)
    client.call(
        "session.open",
        {
            "protocol_version": PROTOCOL_VERSION,
            "client": "test",
            "transport": {"kind": "shm", "ring_path": path},
            "token": None,
        },
    )
    return ring


def open_inline(client: Client, codecs=("rgba8",), token=None) -> dict:
    return client.call(
        "session.open",
        {
            "protocol_version": PROTOCOL_VERSION,
            "client": "test",
            "transport": {"kind": "inline", "codecs": list(codecs)},
            "token": token,
        },
    )


def pixels(w: int, h: int, seed: int = 0) -> bytes:
    return bytes((i * 7 + seed) % 256 for i in range(w * h * 4))
