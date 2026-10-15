import os
import struct
import tempfile

import pytest

from atd_ai_host import PROTOCOL_VERSION
from atd_ai_host.host import BAD_SLOT, UNKNOWN_MODEL, VERSION_MISMATCH, RpcError, Session
from atd_ai_host.models import InvertModel


def make_ring(path: str, slots: int, w: int, h: int) -> None:
    """Writes a ring file the way the Rust side does (ring.rs)."""
    frame = w * h * 4
    slot_bytes = (frame + 63) // 64 * 64
    with open(path, "wb") as f:
        header = b"ATDRING1" + struct.pack("<IIQIII", 1, slots, slot_bytes, w, h, 1)
        f.write(header.ljust(64, b"\0"))
        f.write(b"\0" * slots * (64 + slot_bytes))


@pytest.fixture
def session():
    fd, path = tempfile.mkstemp()
    os.close(fd)
    make_ring(path, 2, 4, 2)
    s = Session()
    s.handle("session.open", {"protocol_version": PROTOCOL_VERSION, "ring_path": path})
    yield s
    s.ring.close()
    os.remove(path)


def test_invert_keeps_alpha():
    frame = bytes([0, 10, 255, 77] * 3)
    assert InvertModel().process(memoryview(frame)) == bytes([255, 245, 0, 77] * 3)


def test_round_trip(session):
    session.handle("model.load", {"model_id": "builtin.invert"})
    pixels = bytes(range(32))
    session.ring.write(0, 7, pixels)
    result = session.handle(
        "frame.submit", {"seq": 7, "model_id": "builtin.invert", "input_slot": 0, "output_slot": 1}
    )
    assert result["seq"] == 7
    assert session.ring.seq(1) == 7
    assert bytes(session.ring.frame(1)) == InvertModel().process(memoryview(pixels))
    assert session.handle("host.health", None)["frames_processed"] == 1


def test_errors(session):
    with pytest.raises(RpcError) as e:
        session.handle("model.load", {"model_id": "nope"})
    assert e.value.code == UNKNOWN_MODEL
    session.handle("model.load", {"model_id": "builtin.invert"})
    with pytest.raises(RpcError) as e:
        session.handle("frame.submit", {"seq": 9, "model_id": "builtin.invert", "input_slot": 0, "output_slot": 1})
    assert e.value.code == BAD_SLOT  # slot 0 does not hold frame 9
    with pytest.raises(RpcError) as e:
        Session().handle("session.open", {"protocol_version": "9.9", "ring_path": "x"})
    assert e.value.code == VERSION_MISMATCH
