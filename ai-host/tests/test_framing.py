import io

import pytest

from atd_ai_host.framing import MAX_LINE, FramingError, encode_message, read_message
from atd_ai_host.ring import FrameRing


def wire(*messages) -> io.BufferedReader:
    data = b"".join(bytes(c) for m in messages for c in encode_message(*m))
    return io.BufferedReader(io.BytesIO(data))


def test_payload_follows_the_line_both_ways():
    blob = bytes(range(256)) * 3 + b"\n\n{"  # payload bytes that look like framing
    r = wire(({"id": 1, "method": "frame.submit"}, blob), ({"id": 2, "method": "host.health"}, None))
    msg, payload = read_message(r)
    assert msg == {"id": 1, "method": "frame.submit", "payload": len(blob)}
    assert payload == blob
    msg, payload = read_message(r)
    assert msg == {"id": 2, "method": "host.health"} and payload is None
    assert read_message(r) is None


def test_empty_payload_is_allowed():
    msg, payload = read_message(wire(({"id": 1}, b"")))
    assert msg["payload"] == 0 and payload == b""


@pytest.mark.parametrize(
    "raw",
    [
        b'{"id":1,"payload":10}\nabc',  # stream ends inside the payload
        b'{"id":1,"payload":-1}\n',
        b'{"id":1,"payload":"x"}\n',
        b"not json\n",
        b"[1,2]\n",
        b'{"id":1}',  # no newline
        b"{" + b" " * MAX_LINE + b"}\n",
    ],
    # Short ids: pytest puts the id in an environment variable, which
    # Windows caps at 32767 characters (the last case is longer).
    ids=["short-payload", "negative-payload", "string-payload", "not-json", "not-object", "no-newline", "line-too-long"],
)
def test_bad_framing_is_rejected(raw):
    with pytest.raises(FramingError):
        read_message(io.BufferedReader(io.BytesIO(raw)))


def test_ring_takes_frames_smaller_than_its_capacity(ring_path):
    ring = FrameRing.create(ring_path, 2, 8, 8)
    try:
        assert ring.fits(4, 4) and ring.fits(8, 8) and not ring.fits(9, 8)
        ring.write(1, 42, bytes(range(64)))
        assert ring.seq(1) == 42
        assert bytes(ring.frame(1, 4, 4)) == bytes(range(64))
        with pytest.raises(IndexError):
            ring.write(1, 1, bytes(8 * 8 * 4 + 1))
        with pytest.raises(IndexError):
            ring.seq(2)
    finally:
        ring.close()
