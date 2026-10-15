"""Shared-memory frame ring; Python mirror of `atd-ai/src/ring.rs`.

Layout (little-endian): 64-byte header ("ATDRING1", version u32, slot_count
u32, slot_bytes u64, width u32, height u32, format u32), then slots of
64-byte header (seq u64, len u64) + slot_bytes of RGBA8 payload.
"""

from __future__ import annotations

import mmap
import struct

MAGIC = b"ATDRING1"
VERSION = 1
FORMAT_RGBA8 = 1
HEADER = 64
SLOT_HEADER = 64


class FrameRing:
    def __init__(self, path: str) -> None:
        self._file = open(path, "r+b")
        self.map = mmap.mmap(self._file.fileno(), 0)
        if self.map[0:8] != MAGIC:
            raise ValueError(f"{path}: not an Atidarla frame ring")
        version, self.slot_count, self.slot_bytes, self.width, self.height, fmt = struct.unpack_from(
            "<IIQIII", self.map, 8
        )
        if version != VERSION or fmt != FORMAT_RGBA8:
            raise ValueError(f"{path}: unsupported ring version {version} or format {fmt}")
        self.frame_bytes = self.width * self.height * 4
        self.stride = SLOT_HEADER + self.slot_bytes
        if len(self.map) < HEADER + self.slot_count * self.stride:
            raise ValueError(f"{path}: file shorter than its header declares")

    def _offset(self, slot: int) -> int:
        if not 0 <= slot < self.slot_count:
            raise IndexError(f"slot {slot} out of range")
        return HEADER + slot * self.stride

    def seq(self, slot: int) -> int:
        return struct.unpack_from("<Q", self.map, self._offset(slot))[0]

    def frame(self, slot: int) -> memoryview:
        start = self._offset(slot) + SLOT_HEADER
        return memoryview(self.map)[start : start + self.frame_bytes]

    def write(self, slot: int, seq: int, data: bytes) -> None:
        if len(data) != self.frame_bytes:
            raise ValueError(f"frame is {len(data)} bytes, ring expects {self.frame_bytes}")
        o = self._offset(slot)
        self.map[o + SLOT_HEADER : o + SLOT_HEADER + self.frame_bytes] = data
        struct.pack_into("<QQ", self.map, o, seq, self.frame_bytes)

    def close(self) -> None:
        self.map.close()
        self._file.close()
