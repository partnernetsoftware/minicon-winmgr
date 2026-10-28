#!/usr/bin/env python3
"""Deterministically render the Winmgr application icon into `assets/winmgr.ico`.

The rasterizer (`Canvas`, `sd_round_rect`, source-over compositing and the box
downsample) and the navy palette are reused from the MiniCon icon generator, so
the two products share one drawing method and one colour family. Only the
artwork is new: a window frame above a taskbar, with the active task in amber.

Only the Python standard library is used (`zlib` and `struct` for PNG and ICO),
with no randomness or timestamps, so rerunning the script reproduces the same
file byte for byte.

Usage, from the repository root:

    python3 assets/icon/generate_winmgr_icon.py
"""

from __future__ import annotations

import math
import pathlib
import struct
import zlib

NAVY_TOP = (0x16, 0x24, 0x3A)
NAVY_BOTTOM = (0x0A, 0x13, 0x22)
PANEL = (0x10, 0x1C, 0x2E)
CYAN = (0x3A, 0xD1, 0xF0)
AMBER = (0xF2, 0xB0, 0x1E)

BG_HALF = 824.0 / 2.0 / 1024.0
BG_RADIUS = 185.0 / 1024.0

WINDOW = (0.19, 0.19, 0.81, 0.62)
WINDOW_STROKE = 0.034
WINDOW_RADIUS = 0.05
BAR = (0.12, 0.71, 0.88, 0.84)
BAR_RADIUS = 0.035
TASKS = (
    (0.18, 0.745, 0.33, 0.815, CYAN),
    (0.36, 0.745, 0.51, 0.815, AMBER),
    (0.54, 0.745, 0.69, 0.815, CYAN),
)
TASK_RADIUS = 0.018

SIZES = (16, 24, 32, 48, 64, 256)


def sd_round_rect(x: float, y: float, box: tuple, radius: float) -> float:
    """Distance to a rounded rectangle given as (x0, y0, x1, y1)."""
    x0, y0, x1, y1 = box
    cx, cy = (x0 + x1) / 2.0, (y0 + y1) / 2.0
    hw, hh = (x1 - x0) / 2.0 - radius, (y1 - y0) / 2.0 - radius
    qx, qy = abs(x - cx) - hw, abs(y - cy) - hh
    outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
    return outside + min(max(qx, qy), 0.0) - radius


class Canvas:
    """Premultiplied float RGBA canvas painted by source-over compositing."""

    def __init__(self, size: int) -> None:
        self.size = size
        self.px = [0.0] * (size * size * 4)

    def paint(self, bbox: tuple, distance, color, alpha: float = 1.0) -> None:
        size = self.size
        inv = 1.0 / size
        feather = inv
        x0 = max(0, int((bbox[0] - feather) * size) - 1)
        y0 = max(0, int((bbox[1] - feather) * size) - 1)
        x1 = min(size, int(math.ceil((bbox[2] + feather) * size)) + 1)
        y1 = min(size, int(math.ceil((bbox[3] + feather) * size)) + 1)
        constant_color = not callable(color)
        cr = cg = cb = 0.0
        if constant_color:
            cr, cg, cb = color[0] / 255.0, color[1] / 255.0, color[2] / 255.0
        px = self.px
        for j in range(y0, y1):
            y = (j + 0.5) * inv
            if not constant_color:
                row = color(y)
                cr, cg, cb = row[0] / 255.0, row[1] / 255.0, row[2] / 255.0
            base = j * size * 4
            for i in range(x0, x1):
                d = distance((i + 0.5) * inv, y)
                if d >= feather:
                    continue
                coverage = 0.5 - d / feather
                if coverage <= 0.0:
                    continue
                src_a = alpha * (1.0 if coverage >= 1.0 else coverage)
                o = base + i * 4
                inv_a = 1.0 - src_a
                px[o] = cr * src_a + px[o] * inv_a
                px[o + 1] = cg * src_a + px[o + 1] * inv_a
                px[o + 2] = cb * src_a + px[o + 2] * inv_a
                px[o + 3] = src_a + px[o + 3] * inv_a

    def downsample(self, target: int) -> bytes:
        """Box-filter to `target` pixels and return straight RGBA bytes."""
        size = self.size
        factor = size // target
        inv_n = 1.0 / (factor * factor)
        px = self.px
        out = bytearray(target * target * 4)
        for j in range(target):
            for i in range(target):
                r = g = b = a = 0.0
                for sj in range(j * factor, (j + 1) * factor):
                    base = sj * size * 4
                    for si in range(i * factor, (i + 1) * factor):
                        o = base + si * 4
                        r += px[o]
                        g += px[o + 1]
                        b += px[o + 2]
                        a += px[o + 3]
                r *= inv_n
                g *= inv_n
                b *= inv_n
                a *= inv_n
                if a <= 0.0:
                    continue
                o = (j * target + i) * 4
                out[o] = _byte(r / a)
                out[o + 1] = _byte(g / a)
                out[o + 2] = _byte(b / a)
                out[o + 3] = _byte(a)
        return bytes(out)


def _byte(value: float) -> int:
    return min(255, max(0, int(value * 255.0 + 0.5)))


def _navy(y: float) -> tuple:
    t = min(max(y, 0.0), 1.0)
    return tuple(
        round(NAVY_TOP[k] + (NAVY_BOTTOM[k] - NAVY_TOP[k]) * t) for k in range(3)
    )


def _outline(box: tuple, radius: float, stroke: float):
    half = stroke / 2.0

    def distance(x: float, y: float) -> float:
        return abs(sd_round_rect(x, y, box, radius)) - half

    return distance


def render(size: int) -> Canvas:
    """Render the icon at `size` pixels. Small faces drop the task detail."""
    canvas = Canvas(size)
    # Background: the same rounded navy square as the MiniCon icon.
    centre_box = (0.5 - BG_HALF, 0.5 - BG_HALF, 0.5 + BG_HALF, 0.5 + BG_HALF)
    canvas.paint(
        centre_box,
        lambda x, y: sd_round_rect(x, y, centre_box, BG_RADIUS),
        _navy,
    )
    small = size <= 32
    stroke = WINDOW_STROKE * (1.6 if small else 1.0)
    canvas.paint(
        WINDOW,
        _outline(WINDOW, WINDOW_RADIUS, stroke),
        CYAN,
    )
    canvas.paint(
        BAR,
        lambda x, y: sd_round_rect(x, y, BAR, BAR_RADIUS),
        PANEL,
    )
    tasks = TASKS[:2] if small else TASKS
    for x0, y0, x1, y1, colour in tasks:
        box = (x0, y0, x1, y1)
        canvas.paint(
            box,
            lambda x, y, b=box: sd_round_rect(x, y, b, TASK_RADIUS),
            colour,
        )
    return canvas


def png(width: int, height: int, rgba: bytes) -> bytes:
    """Encode straight RGBA as a PNG with an unfiltered scanline per row."""
    rows = b"".join(
        b"\x00" + rgba[y * width * 4 : (y + 1) * width * 4] for y in range(height)
    )

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def ico(images: list[tuple[int, bytes]]) -> bytes:
    """Build an ICO container whose entries are PNG payloads (Windows Vista and later)."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    payloads = b""
    for size, data in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        payloads += data
    return header + entries + payloads


def main() -> None:
    root = pathlib.Path(__file__).resolve().parents[2]
    images = []
    for size in SIZES:
        # Each face is drawn at its own size: the signed-distance edges are already
        # antialiased, and the small-size variant only exists at its own resolution.
        rgba = render(size).downsample(size)
        images.append((size, png(size, size, rgba)))
    target = root / "assets" / "winmgr.ico"
    target.write_bytes(ico(images))
    print(f"wrote {target} ({len(images)} faces)")


if __name__ == "__main__":
    main()
