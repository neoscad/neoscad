#!/usr/bin/env python3
"""Builds the Windows app's icon, windows/NeoSCAD.App/Assets/NeoSCAD.ico,
from the macOS app icon's art layer (apple/App/AppIcon.icon/Assets/art.png,
the threaded ring rendered on transparent; docs/icon.md).

    python3 scripts/windows/make-icon.py [OUT.ico]

Only the standard library is used, so it runs wherever Python 3 does. The
.ico is committed anyway: the art changes rarely and CI need not rebuild it.

Why not reuse the art as it is. The layer is framed at 72% of its canvas
so that macOS's rounded-square mask does not clip it. Windows draws icons
unmasked, so at 16 px that margin would leave a ring about 11 px across.
The ring is cropped to its own bounds plus a small margin first.

Why area averaging. Each output pixel is the coverage-weighted mean of the
source pixels under it (in premultiplied alpha, so transparent pixels'
colours do not bleed into edges). Point sampling or bilinear filtering at
1/64 scale would alias the ring's flat-shaded facets into noise.

Why BMP entries below 256 px. Windows reads PNG entries in icons, but a
32-bit DIB with an AND mask is the original form, which every icon reader
accepts; only the 256 px image is PNG, since as a DIB it would be 256 KB.
"""

import os
import struct
import sys
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SOURCE = os.path.join(REPO, "apple", "App", "AppIcon.icon", "Assets", "art.png")
DEFAULT_OUT = os.path.join(REPO, "windows", "NeoSCAD.App", "Assets", "NeoSCAD.ico")

# The sizes Windows asks for: 16/20/24/32/40/48/64 cover the shell's small
# and medium views at 100% to 400% scaling; 256 is the large and extra-large
# views (Explorer scales it down for 96 and 128).
SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
# The ring fills this share of the square; the rest is an even margin, about
# half a pixel at 16 px, so the edge's anti-aliasing is not cut off.
FILL = 0.94


def read_png(path):
    """Decodes an 8-bit RGBA, non-interlaced PNG into rows of floats."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos, idat, header = 8, [], None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.append(body)
    width, height, depth, colour, _, _, interlace = header
    if (depth, colour, interlace) != (8, 6, 0):
        raise SystemExit(f"{path}: expected 8-bit RGBA, non-interlaced")
    raw = zlib.decompress(b"".join(idat))
    stride = width * 4
    rows, previous = [], bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        kind = raw[start]
        line = bytearray(raw[start + 1 : start + 1 + stride])
        for i in range(stride):
            a = line[i - 4] if i >= 4 else 0
            b = previous[i]
            c = previous[i - 4] if i >= 4 else 0
            if kind == 1:
                line[i] = (line[i] + a) & 255
            elif kind == 2:
                line[i] = (line[i] + b) & 255
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 255
        rows.append(line)
        previous = line
    return width, height, rows


def premultiplied(rows, width):
    """Rows of [r*a, g*a, b*a, a] floats in 0..1."""
    out = []
    for line in rows:
        row = []
        for x in range(width):
            r, g, b, a = line[4 * x : 4 * x + 4]
            alpha = a / 255.0
            row.append((r / 255.0 * alpha, g / 255.0 * alpha, b / 255.0 * alpha, alpha))
        out.append(row)
    return out


def alpha_bounds(rows, width, height):
    xs = [x for y in range(height) for x in range(width) if rows[y][4 * x + 3]]
    ys = [y for y in range(height) if any(rows[y][3::4])]
    return min(xs), min(ys), max(xs) + 1, max(ys) + 1


def weights(start, span, n, limit):
    """For each of n output pixels over [start, start+span): the source
    pixels it covers and how much of each, normalised to sum to 1."""
    step = span / n
    table = []
    for j in range(n):
        lo, hi = start + j * step, start + (j + 1) * step
        entries = []
        for i in range(int(lo), int(hi) + 1):
            overlap = min(hi, i + 1) - max(lo, i)
            # Pixels outside the source are transparent: they count toward
            # the step (so the edge fades) but add nothing.
            if overlap > 0 and 0 <= i < limit:
                entries.append((i, overlap / step))
        table.append(entries)
    return table


def resample(pixels, width, height, x0, y0, side, n):
    columns = weights(x0, side, n, width)
    lines = weights(y0, side, n, height)
    horizontal = {}
    for entries in lines:
        for y, _ in entries:
            if y in horizontal:
                continue
            row = pixels[y]
            out = []
            for cols in columns:
                r = g = b = a = 0.0
                for x, w in cols:
                    pr, pg, pb, pa = row[x]
                    r += pr * w
                    g += pg * w
                    b += pb * w
                    a += pa * w
                out.append((r, g, b, a))
            horizontal[y] = out
    image = []
    for entries in lines:
        row = []
        for j in range(n):
            r = g = b = a = 0.0
            for y, w in entries:
                pr, pg, pb, pa = horizontal[y][j]
                r += pr * w
                g += pg * w
                b += pb * w
                a += pa * w
            row.append((r, g, b, a))
        image.append(row)
    return image


def to_bytes(pixel):
    """Straight-alpha RGBA bytes from a premultiplied float pixel."""
    r, g, b, a = pixel
    if a <= 0:
        return (0, 0, 0, 0)
    q = lambda v: max(0, min(255, int(round(v * 255.0))))
    return (q(r / a), q(g / a), q(b / a), q(a))


def png(image, n):
    raw = bytearray()
    for row in image:
        raw.append(0)
        for pixel in row:
            raw.extend(to_bytes(pixel))

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def dib(image, n):
    """A 32-bit BGRA DIB, bottom-up, with the AND mask icons also carry
    (height doubled in the header to count it)."""
    header = struct.pack("<IiiHHIIiiII", 40, n, 2 * n, 1, 32, 0, 0, 0, 0, 0, 0)
    colour = bytearray()
    mask = bytearray()
    mask_stride = ((n + 31) // 32) * 4
    for row in reversed(image):
        bits = bytearray(mask_stride)
        for x, pixel in enumerate(row):
            r, g, b, a = to_bytes(pixel)
            colour.extend((b, g, r, a))
            if a == 0:
                bits[x // 8] |= 0x80 >> (x % 8)
        mask.extend(bits)
    return header + bytes(colour) + bytes(mask)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    width, height, rows = read_png(SOURCE)
    left, top, right, bottom = alpha_bounds(rows, width, height)
    side = max(right - left, bottom - top) / FILL
    x0 = (left + right) / 2 - side / 2
    y0 = (top + bottom) / 2 - side / 2
    pixels = premultiplied(rows, width)
    images = []
    for n in SIZES:
        image = resample(pixels, width, height, x0, y0, side, n)
        images.append((n, png(image, n) if n >= 256 else dib(image, n)))
    # ICONDIR, then one ICONDIRENTRY per image (0 in a size byte means 256).
    directory = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    blobs = b""
    for n, blob in images:
        directory += struct.pack("<BBBBHHII", n % 256, n % 256, 0, 0, 1, 32, len(blob), offset + len(blobs))
        blobs += blob
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "wb") as f:
        f.write(directory + blobs)
    print(f"{out}: {', '.join(str(n) for n in SIZES)} px, crop {side:.0f} px square from {width}x{height}")


if __name__ == "__main__":
    main()
