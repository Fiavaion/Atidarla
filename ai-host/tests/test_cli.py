"""The `lsr-ai-host` command line, run as a real subprocess."""

import os
import re
import socket
import subprocess
import sys
import time

import pytest
from conftest import Client, RpcFailure, open_inline, open_shm, pixels

from lsr_ai_host.adapters.builtin import invert
from lsr_ai_host.errors import UNAUTHORIZED

LINE = re.compile(r"^lsr-ai-host listening on 127\.0\.0\.1:(\d+)$")


def spawn(*args, env=None):
    return subprocess.Popen(
        [sys.executable, "-m", "lsr_ai_host", *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={**os.environ, **(env or {})},
    )


@pytest.fixture
def listening():
    procs = []

    def start(*args, env=None):
        p = spawn("--listen", "127.0.0.1:0", *args, env=env)
        procs.append(p)
        m = LINE.match(p.stdout.readline().rstrip("\n"))
        assert m, "the first stdout line must announce the bound address"
        return ("127.0.0.1", int(m.group(1)))

    yield start
    for p in procs:
        p.kill()
        p.wait()


def test_listen_announces_the_port_and_serves_inline_rgba8(listening):
    addr = listening()
    c = Client(addr)
    assert open_inline(c)["transport"] == "inline"
    c.call("model.load", {"model_id": "builtin.invert", "adapter": "builtin", "path": None, "options": {}})
    data = pixels(16, 9, 5)
    result, out = c.call(
        "frame.submit",
        {"seq": 3, "timestamp_us": 0, "model_id": "builtin.invert", "width": 16, "height": 9, "params": {}},
        data,
    )
    assert result["seq"] == 3 and out == invert(data)
    # Sessions are accepted in a loop, concurrently.
    d = Client(addr)
    open_inline(d)
    assert d.call("host.health")["loaded_models"] == []
    c.call("session.close")
    d.call("session.close")


def test_listen_beyond_loopback_needs_a_token():
    p = spawn("--listen", "0.0.0.0:0", env={"LSR_AI_HOST_TOKEN": ""})
    out, err = p.communicate(timeout=30)
    assert p.returncode == 2 and "without --token" in err and out == ""


def test_token_from_the_environment_is_enforced(listening):
    addr = listening(env={"LSR_AI_HOST_TOKEN": "env-token"})
    c = Client(addr)
    with pytest.raises(RpcFailure) as e:
        open_inline(c, token="wrong")
    assert e.value.code == UNAUTHORIZED
    assert open_inline(Client(addr), token="env-token")["codec"] == "rgba8"


def test_fake_vendor_only_changes_reported_capabilities(listening):
    real = Client(listening())
    open_inline(real)
    fake = Client(listening("--fake-vendor", "nvidia"))
    open_inline(fake)
    r, f = real.call("host.capabilities"), fake.call("host.capabilities")
    assert f["vendor"] == "nvidia" and f["devices"][0]["vendor"] == "nvidia" and f["vram_mb"]
    assert "torch-cuda" in f["runtimes"] and "diffusion.sd" in f["adapters"]
    assert f["transports"] == r["transports"] and f["models"] == r["models"]
    # It still runs what it really can.
    fake.call("model.load", {"model_id": "builtin.invert"})
    assert (
        fake.call(
            "frame.submit", {"seq": 1, "model_id": "builtin.invert", "width": 1, "height": 1}, b"\x00\x10\x20\x30"
        )[1]
        == b"\xff\xef\xdf\x30"
    )


def test_host_exits_when_its_parent_dies():
    parent = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    host = spawn("--listen", "127.0.0.1:0", "--parent-pid", str(parent.pid))
    try:
        assert LINE.match(host.stdout.readline().rstrip("\n"))
        time.sleep(1.5)
        assert host.poll() is None  # alive while the parent is
        parent.kill()
        parent.wait()
        host.wait(timeout=5)
        assert host.returncode == 0
    finally:
        parent.kill()
        host.kill()


def test_connect_mode_with_the_shm_ring(ring_path):
    with socket.create_server(("127.0.0.1", 0)) as server:
        host = spawn("--connect", f"127.0.0.1:{server.getsockname()[1]}")
        try:
            server.settimeout(30)
            conn, _ = server.accept()
            c = Client(conn)
            ring = open_shm(c, ring_path, slots=2, w=8, h=8)
            c.call("model.load", {"model_id": "builtin.invert", "adapter": "builtin"})
            data = pixels(8, 8, 1)
            ring.write(0, 11, data)
            result = c.call(
                "frame.submit",
                {"seq": 11, "model_id": "builtin.invert", "width": 8, "height": 8, "input_slot": 0, "output_slot": 1},
            )
            assert result["output_slot"] == 1 and ring.seq(1) == 11 and bytes(ring.frame(1)) == invert(data)
            c.call("session.close")
            assert host.wait(timeout=10) == 0  # --connect serves one session
            ring.close()
        finally:
            host.kill()
