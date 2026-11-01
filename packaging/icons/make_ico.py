"""Packs the rendered PNGs into lasair.ico (the Windows exe, MSI and setup icon).

usage: python3 packaging/icons/make_ico.py [ICON_DIR]   (default: this folder)
Reads ICON_DIR/png/lasair-<size>.png for 16, 24, 32, 48, 64, 128 and 256 px
(render_icons.sh renders them from lasair.svg) and writes ICON_DIR/lasair.ico.
The 256 px image goes in as the PNG itself; the smaller ones as 32-bit BMP
(BGRA with an AND mask), which every Windows icon loader reads. Standard
library only and deterministic.
"""
import os
import struct
import sys
import zlib

SIZES = (16, 24, 32, 48, 64, 128, 256)
PNG_FROM = 256  # sizes from here up are stored as PNG


def read_png(data):
    """Width, height and RGBA rows of an 8-bit RGBA, non-interlaced PNG."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, w = 8, b"", None
    while pos < len(data):
        (n,) = struct.unpack(">I", data[pos:pos + 4])
        kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + n]
        pos += 12 + n
        if kind == b"IHDR":
            w, h, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert (depth, colour, interlace) == (8, 6, 0), "expected 8-bit RGBA, not interlaced"
        elif kind == b"IDAT":
            idat += body
    raw = zlib.decompress(idat)
    stride, bpp, rows, prev = w * 4, 4, [], bytearray(w * 4)
    for y in range(h):
        f = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if f == 1:
                line[i] = (line[i] + a) & 0xFF
            elif f == 2:
                line[i] = (line[i] + b) & 0xFF
            elif f == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 0xFF
        rows.append(bytes(line))
        prev = line
    return w, h, rows


def bmp_entry(w, h, rows):
    """A BITMAPINFOHEADER image: BGRA rows bottom up, then the AND mask."""
    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = b"".join(
        bytes(v for x in range(w) for v in (r[4 * x + 2], r[4 * x + 1], r[4 * x], r[4 * x + 3]))
        for r in reversed(rows)
    )
    mask_stride = ((w + 31) // 32) * 4
    mask = b""
    for r in reversed(rows):
        bits = bytearray(mask_stride)
        for x in range(w):
            if r[4 * x + 3] == 0:  # fully transparent: masked out
                bits[x // 8] |= 0x80 >> (x % 8)
        mask += bytes(bits)
    return header + pixels + mask


def main(icon_dir):
    images = []
    for s in SIZES:
        with open(os.path.join(icon_dir, "png", f"lasair-{s}.png"), "rb") as f:
            png = f.read()
        w, h, rows = read_png(png)
        assert (w, h) == (s, s), f"lasair-{s}.png is {w}x{h}"
        images.append((s, png if s >= PNG_FROM else bmp_entry(w, h, rows)))
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for s, body in images:
        dim = 0 if s >= 256 else s  # 0 means 256
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(body), offset)
        offset += len(body)
    out += b"".join(body for _, body in images)
    with open(os.path.join(icon_dir, "lasair.ico"), "wb") as f:
        f.write(out)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__)))
