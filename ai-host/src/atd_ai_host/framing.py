"""Wire framing: one JSON object per line, optionally followed by a binary payload.

A message carrying pixels has `"payload": <n>` at the top level of its JSON,
and exactly `n` raw bytes follow the newline.
"""

from __future__ import annotations

import json
from typing import Any, BinaryIO

MAX_LINE = 1 << 20  # a control message is a few hundred bytes
MAX_PAYLOAD = 256 << 20  # an 8K RGBA8 frame is 132 MB


class FramingError(Exception):
    """The stream is unreadable; the connection must be dropped."""


def read_message(reader: BinaryIO) -> tuple[dict, bytes | None] | None:
    """Reads one message. Returns None on a clean end of stream."""
    line = reader.readline(MAX_LINE + 1)
    if not line:
        return None
    if len(line) > MAX_LINE or not line.endswith(b"\n"):
        raise FramingError("control line too long or truncated")
    try:
        msg = json.loads(line)
    except ValueError as e:
        raise FramingError(f"invalid JSON: {e}") from None
    if not isinstance(msg, dict):
        raise FramingError("message is not a JSON object")
    size = msg.get("payload")
    if size is None:
        return msg, None
    if not isinstance(size, int) or isinstance(size, bool) or not 0 <= size <= MAX_PAYLOAD:
        raise FramingError(f"bad payload size {size!r}")
    payload = reader.read(size)
    if len(payload) != size:
        raise FramingError("stream ended inside a payload")
    return msg, payload


def encode_message(msg: dict[str, Any], payload: bytes | memoryview | None = None) -> list[bytes | memoryview]:
    """Returns the chunks to send for `msg` (and its payload)."""
    if payload is not None:
        payload = memoryview(payload).cast("B")
        msg = {**msg, "payload": payload.nbytes}
    line = json.dumps(msg, separators=(",", ":")).encode() + b"\n"
    return [line] if payload is None else [line, payload]
