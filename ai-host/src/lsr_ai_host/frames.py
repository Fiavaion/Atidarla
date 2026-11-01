"""Frames and the inline payload codecs."""

from __future__ import annotations

import importlib.util
import io
from dataclasses import dataclass, field

from .errors import INVALID_PARAMS, RpcError


@dataclass
class Frame:
    """RGBA8, unpremultiplied, rows top to bottom, tightly packed.

    `data` may be a view into shared memory: adapters must copy anything they
    keep beyond one `process` call.
    """

    width: int
    height: int
    data: bytes | bytearray | memoryview
    # Extra inputs by name (e.g. "control"); shm transport only.
    aux: dict[str, Frame] = field(default_factory=dict)

    @property
    def nbytes(self) -> int:
        return self.width * self.height * 4


def available_codecs() -> list[str]:
    codecs = ["rgba8"]
    if importlib.util.find_spec("PIL") is not None:
        codecs.append("jpeg")
    return codecs


def negotiate_codec(client_codecs: list[str]) -> str | None:
    """The first codec in the client's preference list that this host supports."""
    ours = available_codecs()
    return next((c for c in client_codecs if c in ours), None)


def decode(codec: str, payload: bytes, width: int, height: int) -> Frame:
    if codec == "rgba8":
        if len(payload) != width * height * 4:
            raise RpcError(
                INVALID_PARAMS, f"payload is {len(payload)} bytes, a {width}x{height} frame is {width * height * 4}"
            )
        return Frame(width, height, payload)
    from PIL import Image

    try:
        img = Image.open(io.BytesIO(payload))
        img.load()
    except (OSError, ValueError, Image.DecompressionBombError) as e:
        raise RpcError(INVALID_PARAMS, f"undecodable JPEG payload: {e}") from None
    if img.size != (width, height):
        raise RpcError(INVALID_PARAMS, f"JPEG is {img.size[0]}x{img.size[1]}, frame says {width}x{height}")
    return Frame(width, height, img.convert("RGBA").tobytes())


def encode(codec: str, frame: Frame, quality: int = 90) -> bytes | memoryview:
    if codec == "rgba8":
        return memoryview(frame.data).cast("B")
    from PIL import Image

    img = Image.frombuffer("RGBA", (frame.width, frame.height), bytes(frame.data), "raw", "RGBA", 0, 1)
    out = io.BytesIO()
    img.convert("RGB").save(out, format="JPEG", quality=quality)
    return out.getvalue()
