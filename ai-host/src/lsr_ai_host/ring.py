"""Shared-memory frame ring; Python mirror of `lsr-ai/src/ring.rs`.

Layout (little-endian): 64-byte header ("LSRRING1", version u32, slot_count
u32, slot_bytes u64, width u32, height u32, format u32), then slots of
64-byte header (seq u64, len u64) + slot_bytes of RGBA8 payload.

The header's width and height are the ring's capacity. A frame of any size
whose `width * height * 4` fits in `slot_bytes` can use a slot; it occupies
the start of the slot and the slot's `len` records its byte length.
"""

from __future__ import annotations

import mmap
import os
import struct

MAGIC = b"LSRRING1"
VERSION = 1
FORMAT_RGBA8 = 1
HEADER = 64
SLOT_HEADER = 64


class FrameRing:
    def __init__(self, path: str) -> None:
        self._file = open(path, "r+b")  # noqa: SIM115 - stays open as long as the mapping
        try:
            self.map = mmap.mmap(self._file.fileno(), 0)
        except (OSError, ValueError):
            self._file.close()
            raise
        if self.map[0:8] != MAGIC:
            self.close()
            raise ValueError(f"{path}: not a Lasair frame ring")
        version, self.slot_count, self.slot_bytes, self.width, self.height, fmt = struct.unpack_from(
            "<IIQIII", self.map, 8
        )
        self.stride = SLOT_HEADER + self.slot_bytes
        if version != VERSION or fmt != FORMAT_RGBA8:
            self.close()
            raise ValueError(f"{path}: unsupported ring version {version} or format {fmt}")
        if len(self.map) < HEADER + self.slot_count * self.stride:
            self.close()
            raise ValueError(f"{path}: file shorter than its header declares")

    @classmethod
    def create(cls, path: str, slot_count: int, width: int, height: int) -> FrameRing:
        """Creates a ring file the way the engine does (used by tests and `bench --via-protocol`)."""
        slot_bytes = (width * height * 4 + 63) // 64 * 64
        with open(path, "wb") as f:
            header = MAGIC + struct.pack("<IIQIII", VERSION, slot_count, slot_bytes, width, height, FORMAT_RGBA8)
            f.write(header.ljust(HEADER, b"\0"))
            f.truncate(HEADER + slot_count * (SLOT_HEADER + slot_bytes))
        return cls(path)

    def _offset(self, slot: int) -> int:
        if not isinstance(slot, int) or not 0 <= slot < self.slot_count:
            raise IndexError(f"slot {slot} out of range (ring has {self.slot_count})")
        return HEADER + slot * self.stride

    def fits(self, width: int, height: int) -> bool:
        return width * height * 4 <= self.slot_bytes

    def seq(self, slot: int) -> int:
        return struct.unpack_from("<Q", self.map, self._offset(slot))[0]

    def frame(self, slot: int, width: int | None = None, height: int | None = None) -> memoryview:
        """A view of the frame in `slot` (ring-sized unless width/height are given)."""
        n = (width or self.width) * (height or self.height) * 4
        if n > self.slot_bytes:
            raise IndexError(f"a {width}x{height} frame does not fit a {self.slot_bytes}-byte slot")
        start = self._offset(slot) + SLOT_HEADER
        return memoryview(self.map)[start : start + n]

    def write(self, slot: int, seq: int, data: bytes | memoryview) -> None:
        data = memoryview(data).cast("B")
        if data.nbytes > self.slot_bytes:
            raise IndexError(f"frame is {data.nbytes} bytes, slot holds {self.slot_bytes}")
        o = self._offset(slot)
        self.map[o + SLOT_HEADER : o + SLOT_HEADER + data.nbytes] = data
        struct.pack_into("<QQ", self.map, o, seq, data.nbytes)

    def close(self) -> None:
        try:
            self.map.close()
        except BufferError:  # a view is still alive somewhere; the GC closes it later
            pass
        self._file.close()


def remove_quietly(path: str) -> None:
    try:
        os.remove(path)
    except OSError:
        pass
