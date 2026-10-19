"""Generate Dandelion desktop app icons from the vector master (icon.svg design).

Pure-stdlib renderer -- no Pillow, no network:
  * signed-distance-field (SDF) rasterizer with analytic anti-aliasing
  * feathered neon underglow via distance-based falloff (no convolution)
  * generic box-filter downsampling for every target size
  * PNG writer (zlib) + multi-size Windows .ico writer (PNG-compressed entries)

Outputs (written next to this repo's icons/ directory):
  apps/desktop/src-tauri/icons/32x32.png
  apps/desktop/src-tauri/icons/128x128.png
  apps/desktop/src-tauri/icons/128x128@2x.png
  apps/desktop/src-tauri/icons/512x512.png
  apps/desktop/src-tauri/icons/1024x1024.png
  apps/desktop/src-tauri/icons/icon.ico   (256/128/96/64/48/40/32/24/20/16)
  apps/desktop/src-tauri/icons/icon.icns  (32/64/128/256/512/1024)

Frames at 32px and below are rendered from a second, thicker geometry rather
than downscaled from the master -- see SMALL / render_variant().
"""

import math
import os
import struct
import sys
import zlib

MASTER = 2048          # master render resolution (4x of the 512 design space)
DESIGN = 512.0         # logical design space used by icon.svg

# ---------------------------------------------------------------- geometry --
CENTER = (256.0, 256.0)
CYCLE_R = 138.0        # radius of the node ring (nodes sit on this circle)
STROKE = 16.0          # stroke width of arcs and node rings
NODE_R = 42.0          # node ring radius
GLOW_FALLOFF = 24.0    # px of feather for the neon underglow
GLOW_ALPHA = 0.38

TILE = (12.0, 12.0, 488.0, 488.0, 110.0)     # x, y, w, h, corner radius
BORDER = (15.0, 15.0, 482.0, 482.0, 107.0, 3.0)

VIOLET = (167, 139, 250)
CYAN = (56, 189, 248)
BLUE = (96, 165, 250)
TILE_A = (20, 29, 49)
TILE_B = (10, 15, 27)
TINT = (59, 130, 246)
BORDER_C = (148, 163, 184)
BORDER_A = 0.16

TOP_ANG = -90.0
RIGHT_ANG = 30.0
LEFT_ANG = 150.0
GAP = 32.0             # angular clearance between a node and an arc endpoint


def poler(angle_deg):
    a = math.radians(angle_deg)
    return (CENTER[0] + CYCLE_R * math.cos(a), CENTER[1] + CYCLE_R * math.sin(a))


# arcs: (start_angle, end_angle, color_start, color_end) -- hue flows around the loop
ARCS = [
    (TOP_ANG + GAP, RIGHT_ANG - GAP, VIOLET, CYAN),   # top -> bottom-right
    (RIGHT_ANG + GAP, LEFT_ANG - GAP, CYAN, BLUE),    # bottom-right -> bottom-left
    (LEFT_ANG + GAP, TOP_ANG + 360.0 - GAP, BLUE, VIOLET),  # bottom-left -> top
]


def arrow_vertices(a0, a1):
    """Solid triangular arrowhead at the end of an arc, oriented along the tangent."""
    ex, ey = poler(a1)
    t = math.radians(a1)
    tx, ty = -math.sin(t), math.cos(t)          # clockwise tangent
    nx, ny = -ty, tx                            # normal
    tip = (ex + tx * 26.0, ey + ty * 26.0)
    b1 = (ex - tx * 2.0 + nx * 17.0, ey - ty * 2.0 + ny * 17.0)
    b2 = (ex - tx * 2.0 - nx * 17.0, ey - ty * 2.0 - ny * 17.0)
    return (tip, b1, b2)


NODES = [
    (TOP_ANG, VIOLET),
    (RIGHT_ANG, CYAN),
    (LEFT_ANG, BLUE),
]

# ---------------------------------------------------------------- SDF lib --


def sd_rrect(px, py, x, y, w, h, r):
    cx, cy = x + w / 2.0, y + h / 2.0
    hx, hy = w / 2.0 - r, h / 2.0 - r
    qx = abs(px - cx) - hx
    qy = abs(py - cy) - hy
    ox = max(qx, 0.0)
    oy = max(qy, 0.0)
    return math.hypot(ox, oy) + min(max(qx, qy), 0.0) - r


def sd_arc(px, py, a0, a1):
    """Ring segment SDF; angular window [a0, a1] (deg, increasing, span < 180)."""
    dx, dy = px - CENTER[0], py - CENTER[1]
    d = math.hypot(dx, dy)
    ring = abs(d - CYCLE_R) - STROKE / 2.0
    th = math.degrees(math.atan2(dy, dx))
    if a0 <= th <= a1:
        return ring
    # nearest round end cap
    best = 1e30
    for ang in (a0, a1):
        ex, ey = poler(ang)
        best = min(best, math.hypot(px - ex, py - ey))
    return min(ring, best - STROKE / 2.0)


def arc_t(px, py, a0, a1):
    """0..1 position along the arc for the gradient."""
    dx, dy = px - CENTER[0], py - CENTER[1]
    th = math.degrees(math.atan2(dy, dx))
    return max(0.0, min(1.0, (th - a0) / (a1 - a0)))


def sd_tri(px, py, verts):
    """Convex triangle SDF."""
    (ax, ay), (bx, by), (cx, cy) = verts
    d = [
        _edge(px, py, ax, ay, bx, by),
        _edge(px, py, bx, by, cx, cy),
        _edge(px, py, cx, cy, ax, ay),
    ]
    inside = all(v <= 0.0 for v in d)
    if inside:
        return max(d)
    return min(
        _seg(px, py, ax, ay, bx, by),
        _seg(px, py, bx, by, cx, cy),
        _seg(px, py, cx, cy, ax, ay),
    )


def _edge(px, py, ax, ay, bx, by):
    return (px - ax) * (by - ay) - (py - ay) * (bx - ax)


def _seg(px, py, ax, ay, bx, by):
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    t = max(0.0, min(1.0, (wx * vx + wy * vy) / (vx * vx + vy * vy)))
    return math.hypot(px - (ax + t * vx), py - (ay + t * vy))


# ------------------------------------------------------------ shape tables --
def arc_bbox(a0, a1):
    xs, ys = [], []
    for ang in (a0, a1):
        ex, ey = poler(ang)
        xs += [ex, CENTER[0], ex]
        ys += [ey, CENTER[1], ey]
    # conservative: include full circle extremes when arc spans the axis
    m = STROKE / 2.0 + GLOW_FALLOFF
    x0, x1 = min(xs) - m, max(xs) + m
    y0, y1 = min(ys) - m, max(ys) + m
    return (x0, y0, x1, y1)


SHAPES = []  # (kind, bbox, data...)


def build_shapes():
    """(Re)build the shape table from the current geometry globals.

    Small renditions need a different geometry (thicker strokes, fatter nodes, a
    wider tile) and this table is derived from those globals, so it has to be
    rebuilt whenever render_variant() swaps them in. The list is mutated in place
    -- render_master() iterates the module global.
    """
    SHAPES.clear()
    for a0, a1, c0, c1 in ARCS:
        SHAPES.append(("arc", arc_bbox(a0, a1), (a0, a1, c0, c1)))
        SHAPES.append(("arrow", None, (arrow_vertices(a0, a1), c1)))
    for ang, col in NODES:
        x, y = poler(ang)
        m = NODE_R + STROKE / 2.0 + GLOW_FALLOFF
        SHAPES.append(("node", (x - m, y - m, x + m, y + m), (x, y, col)))


build_shapes()


def render_variant(size, **overrides):
    """Render `size` with some of the geometry globals temporarily overridden."""
    g = globals()
    saved = {k: g[k] for k in overrides}
    g.update(overrides)
    build_shapes()
    try:
        return render_master(size)
    finally:
        g.update(saved)
        build_shapes()


# ------------------------------------------------------- small renditions --
# At 32px and below the master's stroke (16/512 = 1.9% of the canvas) lands on
# well under a pixel and the whole loop smears into one grey blob -- the mark
# stops reading as "three nodes on a ring" and becomes a smudge with a dot in
# the middle, which is exactly what a taskbar button shows at 100-150% DPI.
#
# So the small frames get their own geometry instead of a downscale:
#   * STROKE 16 -> 30      : ~0.9px at 16px, ~1.9px at 32px
#   * NODE_R 42 -> 44      : the node ring's hole closes up, so at 16px each
#                            node reads as a solid dot rather than a hollow hairline
#   * CYCLE_R 138 -> 152   : the ring uses more of the canvas
#   * GLOW_FALLOFF 24 -> 0 : a 24px feather is larger than the icon itself; at
#                            16px it only greys the tile
#   * TILE inset 12 -> 6   : the mark gets more of the square, so it survives
#                            the shell's own padding around a taskbar button
SMALL_MAX = 32          # sizes <= this come from the small master
SMALL_MASTER = 512      # 16x supersampling for the 32px frame is plenty
SMALL = dict(
    CYCLE_R=152.0,
    STROKE=30.0,
    NODE_R=44.0,
    GLOW_FALLOFF=0.0,
    TILE=(6.0, 6.0, 500.0, 500.0, 120.0),
    BORDER=(9.0, 9.0, 494.0, 494.0, 117.0, 3.0),
)

# ---------------------------------------------------------------- rendering --


def render_master(size=MASTER):
    ss = size / DESIGN
    buf = bytearray(size * size * 4)
    tw, th, tr = TILE[2], TILE[3], TILE[4]
    bx, by, bw, bh, br, bsw = BORDER

    idx = 0
    for py_i in range(size):
        py = (py_i + 0.5) / ss
        for px_i in range(size):
            px = (px_i + 0.5) / ss

            # ---- tile: rounded rect with diagonal gradient + center tint ----
            sd_t = sd_rrect(px, py, TILE[0], TILE[1], tw, th, tr) * ss
            cover = sd_t < 0.5
            if not cover:
                idx += 4
                continue
            ta = max(0.0, min(1.0, 0.5 - sd_t))
            g = (px + py) / (2.0 * DESIGN)
            r = TILE_A[0] + (TILE_B[0] - TILE_A[0]) * g
            gg = TILE_A[1] + (TILE_B[1] - TILE_A[1]) * g
            b = TILE_A[2] + (TILE_B[2] - TILE_A[2]) * g
            # subtle radial blue tint at the center
            dc = math.hypot(px - CENTER[0], py - CENTER[1]) / 300.0
            tint = max(0.0, 1.0 - dc) * 0.10
            r += (TINT[0] - r) * tint
            gg += (TINT[1] - gg) * tint
            b += (TINT[2] - b) * tint

            ar, ag, ab, aa = r, gg, b, ta

            def over(cr, cg, cb, ca):
                nonlocal ar, ag, ab, aa
                if ca <= 0.0:
                    return
                ar = cr * ca + ar * (1.0 - ca)
                ag = cg * ca + ag * (1.0 - ca)
                ab = cb * ca + ab * (1.0 - ca)
                aa = ca + aa * (1.0 - ca)

            for _, bbox, data in SHAPES:
                if bbox is not None and not (
                    bbox[0] <= px <= bbox[2] and bbox[1] <= py <= bbox[3]
                ):
                    continue
                kind = _
                if kind == "arc":
                    a0, a1, c0, c1 = data
                    sd = sd_arc(px, py, a0, a1) * ss
                    if sd > GLOW_FALLOFF:
                        continue
                    # feathered underglow
                    if sd > 0.0:
                        ga = GLOW_ALPHA * (1.0 - sd / GLOW_FALLOFF) ** 1.6
                        gc = tuple(
                            c0[k] + (c1[k] - c0[k]) * arc_t(px, py, a0, a1)
                            for k in range(3)
                        )
                        over(gc[0], gc[1], gc[2], ga * ta)
                    if sd < 0.5:
                        t = arc_t(px, py, a0, a1)
                        col = tuple(
                            int(c0[k] + (c1[k] - c0[k]) * t) for k in range(3)
                        )
                        over(col[0], col[1], col[2], max(0.0, min(1.0, 0.5 - sd)) * ta)
                elif kind == "arrow":
                    verts, col = data
                    sd = sd_tri(px, py, verts) * ss
                    if sd < 0.5:
                        over(col[0], col[1], col[2], max(0.0, min(1.0, 0.5 - sd)) * ta)
                elif kind == "node":
                    nx, ny, col = data
                    sd = (abs(math.hypot(px - nx, py - ny) - NODE_R) - STROKE / 2.0) * ss
                    if sd > GLOW_FALLOFF:
                        continue
                    if sd > 0.0:
                        ga = GLOW_ALPHA * (1.0 - sd / GLOW_FALLOFF) ** 1.6
                        over(col[0], col[1], col[2], ga * ta)
                    if sd < 0.5:
                        over(col[0], col[1], col[2], max(0.0, min(1.0, 0.5 - sd)) * ta)

            # inner hairline border
            sd_b = abs(sd_rrect(px, py, bx, by, bw, bh, br)) * ss - bsw / 2.0 * ss
            if sd_b < 0.5:
                over(
                    BORDER_C[0],
                    BORDER_C[1],
                    BORDER_C[2],
                    max(0.0, min(1.0, 0.5 - sd_b)) * BORDER_A,
                )

            buf[idx] = int(max(0.0, min(255.0, ar)))
            buf[idx + 1] = int(max(0.0, min(255.0, ag)))
            buf[idx + 2] = int(max(0.0, min(255.0, ab)))
            buf[idx + 3] = int(max(0.0, min(255.0, aa * 255.0)))
            idx += 4
    return buf, size


# ------------------------------------------------------------- png / ico ---


def png_bytes(w, h, rgba):
    raw = bytearray()
    stride = w * 4
    for y in range(h):
        raw.append(0)  # filter: None
        raw += rgba[y * stride : (y + 1) * stride]

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    return png


def write_png(path, w, h, rgba):
    with open(path, "wb") as f:
        f.write(png_bytes(w, h, rgba))


def box_resize(rgba, sw, sh, dw, dh):
    out = bytearray(dw * dh * 4)
    xr = [((i) * sw / dw, (i + 1) * sw / dw) for i in range(dw)]
    yr = [((j) * sh / dh, (j + 1) * sh / dh) for j in range(dh)]
    for j in range(dh):
        y0, y1 = yr[j]
        iy0, iy1 = int(y0), max(int(y0) + 1, int(math.ceil(y1)))
        iy1 = min(iy1, sh)
        for i in range(dw):
            x0, x1 = xr[i]
            ix0, ix1 = int(x0), max(int(x0) + 1, int(math.ceil(x1)))
            ix1 = min(ix1, sw)
            rs = gs = bs = as_ = wsum = 0.0
            for yy in range(iy0, iy1):
                wy = min(y1, sh) - max(y0, yy)
                if wy <= 0:
                    continue
                base = yy * sw * 4
                for xx in range(ix0, ix1):
                    wx = min(x1, sw) - max(x0, xx)
                    if wx <= 0:
                        continue
                    wgt = wx * wy
                    o = base + xx * 4
                    rs += rgba[o] * wgt
                    gs += rgba[o + 1] * wgt
                    bs += rgba[o + 2] * wgt
                    as_ += rgba[o + 3] * wgt
                    wsum += wgt
            k = i * 4 + j * dw * 4
            out[k] = int(rs / wsum + 0.5)
            out[k + 1] = int(gs / wsum + 0.5)
            out[k + 2] = int(bs / wsum + 0.5)
            out[k + 3] = int(as_ / wsum + 0.5)
    return out


def write_ico(path, images):
    """images: list of (size, rgba); stored as PNG-compressed ICO entries."""
    blobs = [(size, png_bytes(size, size, rgba)) for size, rgba in images]

    ico = struct.pack("<HHH", 0, 1, len(blobs))
    offset = 6 + 16 * len(blobs)
    for size, data in blobs:
        b = 0 if size >= 256 else size
        ico += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in blobs:
        ico += data
    with open(path, "wb") as f:
        f.write(ico)


# Apple icon types -> pixel size. Both the legacy icon sizes and the modern
# @2x variants are included so every macOS version picks a crisp rendition.
ICNS_TYPES = [
    ("ic11", 32),    # 16x16@2x
    ("ic12", 64),    # 32x32@2x
    ("ic07", 128),   # 128x128
    ("ic13", 256),   # 128x128@2x
    ("ic08", 256),   # 256x256
    ("ic14", 512),   # 256x256@2x
    ("ic09", 512),   # 512x512
    ("ic10", 1024),  # 512x512@2x
]


def write_icns(path, at):
    """ICNS container holding PNG-encoded entries (supported since macOS 10.7)."""
    body = bytearray()
    cache = {}
    for typ, size in ICNS_TYPES:
        if size not in cache:
            cache[size] = png_bytes(size, size, at(size))
        data = cache[size]
        body += typ.encode("ascii") + struct.pack(">I", len(data) + 8) + data
    with open(path, "wb") as f:
        f.write(b"icns" + struct.pack(">I", len(body) + 8) + bytes(body))


# ------------------------------------------------------------------- main --


def main():
    icons_dir = os.path.dirname(os.path.abspath(__file__))
    icons_dir = os.path.join(
        os.path.dirname(icons_dir),
        "apps",
        "desktop",
        "src-tauri",
        "icons",
    )
    os.makedirs(icons_dir, exist_ok=True)

    print(f"rendering master {MASTER}x{MASTER} ...", flush=True)
    master, mw = render_master(MASTER)
    print("master done", flush=True)

    print(f"rendering small master {SMALL_MASTER}x{SMALL_MASTER} ...", flush=True)
    small, sw = render_variant(SMALL_MASTER, **SMALL)
    print("small master done", flush=True)

    def at(size):
        """Rendition at `size`, resampled from whichever master suits it."""
        src, ssw = (small, sw) if size <= SMALL_MAX else (master, mw)
        if size == ssw:
            return bytes(src)
        return bytes(box_resize(src, ssw, ssw, size, size))

    # Windows / Tauri conventions
    for name, size in (
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("512x512.png", 512),
        ("1024x1024.png", 1024),
    ):
        write_png(os.path.join(icons_dir, name), size, size, at(size))
        print(f"wrote {name}", flush=True)

    # Windows picks a frame out of this list by DPI: 16/20/24/32 at 100-150%,
    # 40/48 at 200%, 96 for Explorer's "jumbo" views. Shipping only 16/32/48
    # forced the shell to resample for every bucket in between -- and resampling
    # a thin-stroke mark is exactly where it turns to mush.
    ico_sizes = (256, 128, 96, 64, 48, 40, 32, 24, 20, 16)
    ico_images = [(s, at(s)) for s in ico_sizes]
    write_ico(os.path.join(icons_dir, "icon.ico"), ico_images)
    print("wrote icon.ico (" + "/".join(str(s) for s in ico_sizes) + ")", flush=True)

    # macOS requires an .icns container
    write_icns(os.path.join(icons_dir, "icon.icns"), at)
    print("wrote icon.icns (32/64/128/256/512/1024)", flush=True)


if __name__ == "__main__":
    sys.exit(main())
