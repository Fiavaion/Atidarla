import importlib.util

import pytest
from conftest import Client, InProcessHost, RpcFailure, SlowAdapter, open_inline, open_shm, pixels

from atd_ai_host import PROTOCOL_VERSION
from atd_ai_host.adapters.builtin import invert
from atd_ai_host.errors import (
    BAD_SLOT,
    BUSY,
    INVALID_PARAMS,
    INVALID_REQUEST,
    STALE_FRAME,
    UNAUTHORIZED,
    UNKNOWN_MODEL,
    UNSUPPORTED,
    VERSION_MISMATCH,
)
from atd_ai_host.models import CACHE, MAX_QUEUED


def submit_shm(c: Client, ring, seq, model="builtin.invert", w=4, h=2, inp=0, out=1, params=None, data=None):
    ring.write(inp, seq, data if data is not None else pixels(w, h, seq))
    return c.send(
        "frame.submit",
        {
            "seq": seq,
            "timestamp_us": seq * 1000,
            "model_id": model,
            "width": w,
            "height": h,
            "input_slot": inp,
            "output_slot": out,
            "aux": {},
            "params": params or {},
        },
    )


def error_code(c: Client, rid: int) -> int:
    with pytest.raises(RpcFailure) as e:
        c.result(rid)
    return e.value.code


def test_shm_round_trip_is_byte_exact(host, ring_path):
    c = Client(host.addr)
    ring = open_shm(c, ring_path, w=8, h=8)
    info = c.call("model.load", {"model_id": "builtin.invert", "adapter": "builtin", "path": None, "options": {}})
    assert info["adapter"] == "builtin" and info["device"] == "cpu"
    # A frame smaller than the ring's capacity.
    data = pixels(4, 2, 3)
    result = c.result(submit_shm(c, ring, 7, data=data))
    assert result["seq"] == 7 and result["output_slot"] == 1 and result["skipped"] is False
    assert (result["width"], result["height"]) == (4, 2)
    assert ring.seq(1) == 7
    assert bytes(ring.frame(1, 4, 2)) == invert(data)
    assert c.call("host.health")["frames_processed"] == 1
    c.call("session.close")
    ring.close()


def test_errors(host, ring_path):
    c = Client(host.addr)
    assert error_code(c, c.send("host.health")) == INVALID_REQUEST  # before session.open
    assert (
        error_code(c, c.send("session.open", {"protocol_version": "9.9", "transport": {"kind": "inline"}}))
        == VERSION_MISMATCH
    )
    ring = open_shm(c, ring_path)
    assert error_code(c, c.send("model.load", {"model_id": "nope", "adapter": "builtin"})) == UNKNOWN_MODEL
    assert error_code(c, c.send("model.load", {"model_id": "x", "adapter": "no.such"})) == UNSUPPORTED
    assert error_code(c, c.send("model.load", {"model_id": "v", "adapter": "video.v2v"})) == UNSUPPORTED
    assert error_code(c, submit_shm(c, ring, 1)) == UNKNOWN_MODEL
    c.call("model.load", {"model_id": "builtin.invert", "adapter": "builtin"})
    ring.write(0, 8, pixels(4, 2))
    bad = {"seq": 9, "model_id": "builtin.invert", "width": 4, "height": 2, "input_slot": 0, "output_slot": 1}
    assert error_code(c, c.send("frame.submit", bad)) == BAD_SLOT  # slot 0 holds frame 8
    assert error_code(c, c.send("frame.submit", {**bad, "seq": 8, "output_slot": 99})) == BAD_SLOT
    assert error_code(c, c.send("frame.submit", {**bad, "seq": 8, "width": 64})) == BAD_SLOT  # too big
    assert error_code(c, c.send("frame.submit", {**bad, "seq": -1})) == INVALID_PARAMS
    c.call("session.close")
    ring.close()


def test_health_is_answered_while_a_slow_frame_runs(host):
    c = Client(host.addr)
    open_inline(c)
    c.call("model.load", {"model_id": "slow", "adapter": "test.slow"})
    frame = c.send(
        "frame.submit", {"seq": 1, "model_id": "slow", "width": 1, "height": 1, "params": {"sleep_ms": 500}}, b"abcd"
    )
    health = c.send("host.health")
    assert c.result(health)["queue_depth"] == 1
    assert c.arrivals.index(health) == len(c.arrivals) - 1  # the frame has not been answered yet
    result, payload = c.result(frame)
    assert payload == b"abcd" and result["host_ms"] >= 500
    assert c.arrivals.index(health) < c.arrivals.index(frame)


def test_frames_of_one_model_run_in_submission_order(host):
    SlowAdapter.order.clear()
    c = Client(host.addr)
    open_inline(c)
    c.call("model.load", {"model_id": "slow", "adapter": "test.slow"})
    ids = [
        c.send(
            "frame.submit",
            {"seq": i, "model_id": "slow", "width": 1, "height": 1, "params": {"sleep_ms": 20, "tag": i}},
            b"rgba",
        )
        for i in range(4)
    ]
    assert [c.result(i)[0]["seq"] for i in ids] == [0, 1, 2, 3]
    assert SlowAdapter.order == [0, 1, 2, 3]


def test_cancel_drops_a_queued_frame_and_busy_bounds_the_queue(host):
    c = Client(host.addr)
    open_inline(c)
    c.call("model.load", {"model_id": "slow", "adapter": "test.slow"})

    def frame(seq, ms):
        return c.send(
            "frame.submit",
            {"seq": seq, "model_id": "slow", "width": 1, "height": 1, "params": {"sleep_ms": ms}},
            b"rgba",
        )

    running = frame(0, 400)
    while c.call("host.health")["queue_depth"] == 0:
        pass
    queued = [frame(seq, 1) for seq in range(1, MAX_QUEUED + 1)]
    assert error_code(c, frame(99, 1)) == BUSY
    assert c.call("frame.cancel", {"seq": 2}) is None
    assert error_code(c, queued[1]) == STALE_FRAME
    assert c.call("frame.cancel", {"seq": 12345}) is None  # unknown seq: nothing to do
    assert c.result(running)[0]["seq"] == 0
    assert [c.result(r)[0]["seq"] for i, r in enumerate(queued) if i != 1] == [1, 3, 4]


def test_unload_answers_queued_frames_stale(host):
    c = Client(host.addr)
    open_inline(c)
    c.call("model.load", {"model_id": "slow", "adapter": "test.slow"})
    first = c.send(
        "frame.submit", {"seq": 1, "model_id": "slow", "width": 1, "height": 1, "params": {"sleep_ms": 200}}, b"rgba"
    )
    second = c.send("frame.submit", {"seq": 2, "model_id": "slow", "width": 1, "height": 1, "params": {}}, b"rgba")
    while c.call("host.health")["queue_depth"] < 2:
        pass
    c.call("model.unload", {"model_id": "slow"})
    assert c.result(first)[0]["seq"] == 1  # the running frame finishes
    assert error_code(c, second) == STALE_FRAME
    assert c.call("host.health")["loaded_models"] == []


def test_sessions_share_one_model_instance(host, ring_path):
    a, b = Client(host.addr), Client(host.addr)
    ring = open_shm(a, ring_path)
    open_inline(b)
    a.call("model.load", {"model_id": "builtin.invert", "adapter": "builtin"})
    b.call("model.load", {"model_id": "builtin.invert", "adapter": "builtin"})
    b.call("model.load", {"model_id": "builtin.invert", "adapter": "builtin"})  # idempotent in a session
    shared = [m for m in CACHE.loaded() if m.adapter.model_id == "builtin.invert"]
    assert len(shared) == 1 and shared[0].refs == 2
    # Both sessions still get their own results.
    data = pixels(1, 1, 9)
    assert b.call("frame.submit", {"seq": 1, "model_id": "builtin.invert", "width": 1, "height": 1}, data)[1] == invert(
        data
    )
    assert a.result(submit_shm(a, ring, 5))["seq"] == 5
    b.call("model.unload", {"model_id": "builtin.invert"})
    assert shared[0].refs == 1
    a.call("session.close")
    a.close()
    assert not [m for m in CACHE.loaded() if m.adapter.model_id == "builtin.invert"]
    ring.close()


def test_a_dropped_connection_releases_its_models(host):
    a = Client(host.addr)
    open_inline(a)
    a.call("model.load", {"model_id": "builtin.echo"})
    assert [m.refs for m in CACHE.loaded() if m.adapter.model_id == "builtin.echo"] == [1]
    a.close()
    b = Client(host.addr)  # wait for the host to notice by doing a round trip on another session
    open_inline(b)
    for _ in range(200):
        if not [m for m in CACHE.loaded() if m.adapter.model_id == "builtin.echo"]:
            break
        b.call("host.health")
    assert not [m for m in CACHE.loaded() if m.adapter.model_id == "builtin.echo"]


def test_model_load_sends_progress_notifications(host):
    c = Client(host.addr)
    open_inline(c)
    c.call("model.load", {"model_id": "progress", "adapter": "test.slow"})
    progress = [n["params"] for n in c.notifications if n["method"] == "model.progress"]
    assert progress[-1] == {"model_id": "progress", "stage": "ready", "fraction": 1.0}


def test_inline_rgba8_and_codec_negotiation(host):
    c = Client(host.addr)
    info = open_inline(c, codecs=["h264", "rgba8"])
    assert info["transport"] == "inline" and info["codec"] == "rgba8"
    c.call("model.load", {"model_id": "builtin.invert"})
    data = pixels(3, 5)
    result, out = c.call("frame.submit", {"seq": 1, "model_id": "builtin.invert", "width": 3, "height": 5}, data)
    assert out == invert(data) and result["output_slot"] is None
    assert (
        error_code(
            c, c.send("frame.submit", {"seq": 2, "model_id": "builtin.invert", "width": 3, "height": 5}, b"short")
        )
        == INVALID_PARAMS
    )
    other = Client(host.addr)
    assert (
        error_code(
            other,
            other.send(
                "session.open",
                {"protocol_version": PROTOCOL_VERSION, "transport": {"kind": "inline", "codecs": ["h264"]}},
            ),
        )
        == UNSUPPORTED
    )


@pytest.mark.skipif(importlib.util.find_spec("PIL") is None, reason="needs Pillow")
def test_inline_jpeg_codec(host):
    import io

    from PIL import Image

    c = Client(host.addr)
    assert open_inline(c, codecs=["jpeg", "rgba8"])["codec"] == "jpeg"
    c.call("model.load", {"model_id": "builtin.echo"})
    src = Image.new("RGB", (16, 8), (200, 40, 90))
    buf = io.BytesIO()
    src.save(buf, format="JPEG", quality=95)
    _, out = c.call("frame.submit", {"seq": 1, "model_id": "builtin.echo", "width": 16, "height": 8}, buf.getvalue())
    img = Image.open(io.BytesIO(out))
    assert img.size == (16, 8)
    r, g, b = img.convert("RGB").getpixel((8, 4))
    assert abs(r - 200) < 8 and abs(g - 40) < 8 and abs(b - 90) < 8


def test_token_is_checked_in_session_open():
    h = InProcessHost(token="s3cret")
    try:
        c = Client(h.addr)
        assert error_code(c, c.send("host.capabilities")) == UNAUTHORIZED
        c = Client(h.addr)
        assert (
            error_code(
                c,
                c.send(
                    "session.open",
                    {"protocol_version": PROTOCOL_VERSION, "transport": {"kind": "inline"}, "token": "nope"},
                ),
            )
            == UNAUTHORIZED
        )
        with pytest.raises(ConnectionError):
            c.call("host.health")  # the host hangs up after a wrong token
        c = Client(h.addr)
        assert open_inline(c, token="s3cret")["codec"] == "rgba8"
        assert "builtin" in c.call("host.capabilities")["adapters"]
    finally:
        h.close()


def test_capabilities_shape(host):
    c = Client(host.addr)
    open_inline(c)
    caps = c.call("host.capabilities")
    assert caps["vendor"] in {"nvidia", "amd", "intel", "apple", "cpu"}
    assert caps["transports"] == ["shm", "inline"]
    assert "cpu-reference" in caps["runtimes"]
    assert {"builtin.invert", "builtin.echo"} <= set(caps["models"])
    assert "video.v2v" not in caps["adapters"]  # no backend installed
    assert all({"name", "vendor", "vram_mb"} <= set(d) for d in caps["devices"])
