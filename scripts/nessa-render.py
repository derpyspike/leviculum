#!/usr/bin/env python3
"""Renders every picture of Nessa from one SVG source (1690).

    python3 -I scripts/nessa-render.py           # write
    python3 -I scripts/nessa-render.py --check   # compare with the tree

The source is `assets/nessa/nessa-R3-b.svg`, the figure Lew decided on
2026-11-11 (draft R3-b of the drawing round): one 512 x 512 canvas, the
body and the tail, the head and the face, and three signs that the source
keeps hidden — a speech bubble (`sign-loquax`), a compass (`sign-lnomad`)
and a bolt (`sign-lnflash`). The palette is hers too: honey `#f0b860` fur,
ink `#2b1d14` lines, and terracotta `#c4502f` only on the nose and the
signs. The body will be redrawn with an image model later; the head and the
signs are final enough to ship, so this script builds the pipeline around
them and a redraw costs one new source file, not a search through the tree.

Every picture is framed out of that canvas by a box in `HEAD_BOX` and
`SIGN_BOX` and rendered by resvg (0.45.1 measured; `sudo apt install
resvg`), which writes the same bytes for the same SVG, so `--check`
re-renders into `target/tmp` and fails on any committed file that differs.
The framed SVGs are written as well as the PNGs: a picture whose only form
is a PNG cannot be inspected or revised.

The products, by the thing that shows them:

  Loquax (head and speech bubble)
    assets/nessa/loquax.svg           the picture, square, transparent
    assets/nessa/favicon-32.png       a browser tab
    loquax-android/icon/loquax.svg    the adaptive icon, round, clipped
    loquax-android/icon/loquax-foreground.svg   its 108 dp foreground
    loquax-android/icon/loquax-background.svg   its ground
    loquax-android/icon/loquax-monochrome.svg   a 13+ themed launcher
    loquax-android/android/res/...    the adaptive icon, its layers and the
                                      legacy ic_launcher.png per density
    packaging/loquax/loquax.png       the Linux package's 256 px icon
    leviculum-ui-iced/assets/window-icon.png    the windows' icon

  lnomad (head and compass)
    assets/nessa/lnomad.svg
    packaging/lnomad/lnomad.png       256 px, the package's launcher icon
                                      (installed now, named by the entry the
                                      GUI brings)

  lnflash (head and bolt)
    assets/nessa/lnflash.svg
    packaging/lnflash/lnflash.png     256 px, the package's icon

  Plain head (website, docs)
    assets/nessa/head.svg
    assets/nessa/head-256.png
    docs/src/images/nessa-head.png    the handbook's and the site's picture of her
    docs/src/images/nessa-favicon.png the handbook's tab

  The whole figure (website)
    assets/nessa/figure-512.png
    docs/src/images/nessa-figure.png  the handbook's and the site's whole her;
                                      both are served at /docs/images/ because
                                      lblogd ships no static assets of its own

  The M9's power-on splash (1690, the M9 decisions of 2026-11-07)
    loquax-m9/assets/splash-head.svg  the hand-made 1-bit line drawing
    loquax-m9/assets/splash-head.png  112 px, what a person looks at
    loquax-m9/assets/splash-head.1bpp 112 x 112 bits, what the board burns

The splash is NOT a scaled copy of the colour head: thresholded, her cream
muzzle and dark nose read as a bear's snout. It is drawn for the panel —
white fur on the ground, eyes and nose cut out, the muzzle one outline —
and the script only packs what it finds: white is ink, everything else is
the panel's ground.

`assets/nessa/README.md` states what the source is and whose it is.
"""

import math
import re
import shutil
import struct
import subprocess
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "assets" / "nessa"
SOURCE = ASSETS / "nessa-R3-b.svg"
SCRATCH = ROOT / "target" / "tmp" / "nessa-render"

ICON = ROOT / "loquax-android" / "icon"
RES = ROOT / "loquax-android" / "android" / "res"

# Her palette, fixed by Lew (2026-11-11): do not derive these from the UI
# chrome the way the jackdaw's colours were — the whole point of a mascot is
# that she looks the same everywhere.
INK = "#2b1d14"
HONEY = "#f0b860"
TERRACOTTA = "#c4502f"
CREAM = "#fbe6c2"

# The disc the round pictures stand on. Ink, because she is honey and her
# sign is terracotta: on the chrome's green accent neither reads.
GROUND = INK

# What the mono layer keeps: the shapes that carry her silhouette, the ear
# rim and the skull. The eyes and the muzzle are the same colour there, so
# keeping them would only add ink to a shape that is already solid.
SILHOUETTE_FILLS = {"#b8702a", HONEY}

# Content boxes on the source's 512 canvas, (x0, y0, x1, y1). A picture is
# framed as the square around the box it names, so what is drawn sits in the
# middle of the canvas and whatever is beside it is left out.
HEAD_BOX = (147.6, 81.0, 364.4, 226.7)
SIGN_BOX = {
    "loquax": (366.4, 52.0, 450.4, 138.0),
    "lnomad": (362.4, 46.0, 454.4, 138.0),
    "lnflash": (375.4, 41.0, 441.4, 143.0),
}
SIGN_GROUP = {
    "loquax": "sign-loquax",
    "lnomad": "sign-lnomad",
    "lnflash": "sign-lnflash",
}
# Where the sign sits in an app icon. Loquax's bubble is tucked into the
# head's upper right: a bubble over the head is how a speech reads, and the
# source's own spacing makes the pair 1.7 times as wide as it is tall, so at
# arm's length in a round launcher the bubble is off the disc and an ear is
# at its edge. The compass and the bolt stay where the figure has them — they
# are square pictures, not a masked launcher icon.
SIGN_SHIFT = {
    "loquax": (-70.0, 18.0),
    "lnomad": (0.0, 0.0),
    "lnflash": (0.0, 0.0),
}

# The groups the source must hold, and the ones whose every element this
# script turns into a vector path.
GROUPS = ("body", "tail", "head", "face") + tuple(SIGN_GROUP.values())
VECTOR_GROUPS = ("head", "face") + tuple(SIGN_GROUP.values())

# Android's adaptive icon: a 108 dp canvas of which a launcher shows the
# inner 72 under its mask, keeping the circle of radius 33 around (54, 54)
# safe. A picture is scaled until its corners lie inside that circle, with
# two units to spare, so no mask shape touches an ear.
CANVAS = 108.0
SAFE = 31.0

# 48 dp at each density's scale: 1, 1.5, 2, 3 and 4.
DENSITIES = {
    "mdpi": 48,
    "hdpi": 72,
    "xhdpi": 96,
    "xxhdpi": 144,
    "xxxhdpi": 192,
}

SPLASH_SVG = ROOT / "loquax-m9" / "assets" / "splash-head.svg"
SPLASH_PNG = ROOT / "loquax-m9" / "assets" / "splash-head.png"
SPLASH_BITS = ROOT / "loquax-m9" / "assets" / "splash-head.1bpp"
# The M9's splash side in panel pixels, `loquax_m9::splash::LOGO_PX`.
SPLASH_PX = 112


def num(value):
    """A coordinate as SVG spells it: no exponent, no trailing zeroes."""
    text = f"{value:.3f}".rstrip("0").rstrip(".")
    return "0" if text in ("", "-0") else text


# --- The source -----------------------------------------------------------


def read_source():
    """The source's groups by id, as their inner markup.

    The `inkscape:label` and the `style="display:none"` of a sign are
    dropped with the group tag: a sign this script names is meant to show,
    and the label is a drawing tool's, not the file's meaning.
    """
    text = SOURCE.read_text()
    found = {
        m.group(1): m.group(2)
        for m in re.finditer(r'<g id="([^"]+)"[^>]*>(.*?)</g>\n', text, re.S)
    }
    missing = [name for name in GROUPS if name not in found]
    if missing:
        sys.exit(f"nessa-render: {SOURCE.relative_to(ROOT)} has no group {missing}")
    for name in VECTOR_GROUPS:
        rest = PATH.sub("", found[name]).strip()
        if rest:
            sys.exit(
                f"nessa-render: group {name!r} holds something other than "
                f"paths, which this script cannot render: {rest[:60]!r}"
            )
    return found


PATH = re.compile(r"<path\b([^>]*?)/>", re.S)
ATTRIBUTE = re.compile(r'([\w:-]+)="([^"]*)"')


def paths(markup):
    """Every `<path>` of a group, as attribute dictionaries."""
    return [dict(ATTRIBUTE.findall(m.group(1))) for m in PATH.finditer(markup)]


def group(name, inner):
    return f'<g id="{name}">{inner}</g>'


def square(box):
    """The square around `box`, as (x, y, side)."""
    x0, y0, x1, y1 = box
    side = max(x1 - x0, y1 - y0)
    return ((x0 + x1) / 2 - side / 2, (y0 + y1) / 2 - side / 2, side)


def union(first, second):
    return (
        min(first[0], second[0]),
        min(first[1], second[1]),
        max(first[2], second[2]),
        max(first[3], second[3]),
    )


def moved(box, shift):
    dx, dy = shift
    return (box[0] + dx, box[1] + dy, box[2] + dx, box[3] + dy)


def framed(parts, view):
    """A square picture in the source's own coordinates."""
    x, y, side = view
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" '
        f'viewBox="{num(x)} {num(y)} {num(side)} {num(side)}" '
        f'width="{num(side)}" height="{num(side)}">\n'
        + "\n".join(parts)
        + "\n</svg>\n"
    )


def head_parts(groups, sign=None):
    """The head, its face, and the sign it is drawn with, moved into the
    corner an icon can afford."""
    parts = [group("head", groups["head"]), group("face", groups["face"])]
    if sign is not None:
        dx, dy = SIGN_SHIFT[sign]
        parts.append(
            f'<g transform="translate({num(dx)} {num(dy)})">'
            + group(SIGN_GROUP[sign], groups[SIGN_GROUP[sign]])
            + "</g>"
        )
    return parts


def sign_box(sign):
    """Where the sign sits once [`head_parts`] has moved it."""
    return moved(SIGN_BOX[sign], SIGN_SHIFT[sign])


def content_box(sign=None):
    """Everything the picture draws, as (x0, y0, x1, y1)."""
    return HEAD_BOX if sign is None else union(HEAD_BOX, sign_box(sign))


def sign_view(sign):
    """The square around the head and `sign` together."""
    return square(content_box(sign))


# --- The Android icon -----------------------------------------------------


def placement(box, radius=SAFE):
    """(scale, dx, dy): `box` centred on the 108 dp canvas and scaled so its
    corners lie inside a circle of `radius` around the canvas's centre."""
    x0, y0, x1, y1 = box
    half = math.hypot((x1 - x0) / 2, (y1 - y0) / 2)
    scale = radius / half
    return (
        scale,
        CANVAS / 2 - (x0 + x1) / 2 * scale,
        CANVAS / 2 - (y0 + y1) / 2 * scale,
    )


def transform(box, attribute):
    """The canvas transform in SVG's syntax or a vector drawable's."""
    scale, dx, dy = placement(box)
    if attribute:
        return (
            f'a:translateX="{num(dx)}" a:translateY="{num(dy)}" '
            f'a:scaleX="{num(scale)}" a:scaleY="{num(scale)}"'
        )
    return f'transform="translate({num(dx)} {num(dy)}) scale({num(scale)})"'


def ground_rect():
    return f'<rect x="0" y="0" width="{num(CANVAS)}" height="{num(CANVAS)}" fill="{GROUND}"/>'


def foreground_svg(groups):
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 108 108" '
        'width="108" height="108">\n'
        f'<g {transform(content_box("loquax"), False)}>'
        + "".join(head_parts(groups, "loquax"))
        + "</g>\n</svg>\n"
    )


def background_svg():
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 108 108" '
        'width="108" height="108">\n' + ground_rect() + "\n</svg>\n"
    )


def composed_svg(groups, sign):
    """The round picture a launcher shows: the inner 72 of the canvas."""
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="18 18 72 72" '
        'width="108" height="108">\n'
        '<clipPath id="launcher"><circle cx="54" cy="54" r="36"/></clipPath>\n'
        f'<g clip-path="url(#launcher)">{ground_rect()}'
        f'<g {transform(content_box(sign), False)}>'
        + "".join(head_parts(groups, sign))
        + "</g></g>\n</svg>\n"
    )


def monochrome_svg(groups):
    """What Android 13 tints for a themed home screen: her silhouette."""
    kept = []
    for name in ("head", "face"):
        inner = "".join(
            f'<path d="{p["d"]}"/>'
            for p in paths(groups[name])
            if p.get("fill") in SILHOUETTE_FILLS
        )
        if inner:
            kept.append(f'<g fill="#000000">{inner}</g>')
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 108 108" '
        'width="108" height="108">\n'
        f'<g {transform(HEAD_BOX, False)}>' + "".join(kept) + "</g>\n</svg>\n"
    )


def vector(groups, sign, colour, keeps):
    """A vector drawable: the framed head as Android spells a path.

    `keeps(group, fill)` decides which shapes are drawn; `colour` is the
    fill and stroke every kept shape gets, or None to keep the source's own.
    """
    def shapes(name):
        out = []
        for path in paths(groups[name]):
            fill = path.get("fill", "#000000")
            if not keeps(name, fill):
                continue
            # A vector drawable has no `none`: a stroked-only shape takes a
            # transparent fill, not an absent one.
            painted = colour or ("#00000000" if fill == "none" else fill)
            attributes = [
                f'a:fillColor="{painted}"',
                f'a:pathData="{path["d"]}"',
            ]
            stroke = path.get("stroke")
            if stroke is not None:
                attributes += [
                    f'a:strokeColor="{colour or stroke}"',
                    f'a:strokeWidth="{path.get("stroke-width", "1")}"',
                ]
                for source, android in (
                    ("stroke-linecap", "strokeLineCap"),
                    ("stroke-linejoin", "strokeLineJoin"),
                ):
                    if path.get(source) is not None:
                        attributes.append(f'a:{android}="{path[source]}"')
            out.append("    <path " + " ".join(attributes) + "/>")
        return out

    body = shapes("head") + shapes("face")
    if sign is not None:
        dx, dy = SIGN_SHIFT[sign]
        body.append(
            f'    <group a:translateX="{num(dx)}" a:translateY="{num(dy)}">'
            + "".join(shapes(SIGN_GROUP[sign]))
            + "    </group>"
        )
    return (
        "<?xml version='1.0' encoding='utf-8'?>\n"
        "<!-- Written by scripts/nessa-render.py from assets/nessa/nessa-R3-b.svg;\n"
        "     edit the source, not this file. -->\n"
        "<vector xmlns:a='http://schemas.android.com/apk/res/android'\n"
        "        a:width='108dp' a:height='108dp'\n"
        "        a:viewportWidth='108' a:viewportHeight='108'>\n"
        f"  <group {transform(content_box(sign), True)}>\n"
        + "\n".join(body)
        + "\n  </group>\n</vector>\n"
    )


ADAPTIVE = """<?xml version='1.0' encoding='utf-8'?>
<!-- Written by scripts/nessa-render.py from assets/nessa/nessa-R3-b.svg;
     edit the source, not this file. Nessa on the ink; the monochrome layer
     is what Android 13 onward tints for a themed home screen. -->
<adaptive-icon xmlns:a='http://schemas.android.com/apk/res/android'>
  <background a:drawable='@color/ic_launcher_background'/>
  <foreground a:drawable='@drawable/ic_launcher_foreground'/>
  <monochrome a:drawable='@drawable/ic_launcher_monochrome'/>
</adaptive-icon>
"""


def background_colour():
    return (
        "<?xml version='1.0' encoding='utf-8'?>\n"
        "<!-- Written by scripts/nessa-render.py from assets/nessa/nessa-R3-b.svg:\n"
        "     the adaptive icon's ground, her ink. -->\n"
        "<resources>\n"
        f'    <color name="ic_launcher_background">{GROUND}</color>\n'
        "</resources>\n"
    )


def sources(groups):
    """Every SVG and XML file the script writes, by path."""
    return {
        ASSETS / "head.svg": framed(head_parts(groups), square(HEAD_BOX)),
        ASSETS / "loquax.svg": framed(head_parts(groups, "loquax"), sign_view("loquax")),
        ASSETS / "lnomad.svg": framed(head_parts(groups, "lnomad"), sign_view("lnomad")),
        ASSETS / "lnflash.svg": framed(head_parts(groups, "lnflash"), sign_view("lnflash")),
        ICON / "loquax.svg": composed_svg(groups, "loquax"),
        ICON / "loquax-foreground.svg": foreground_svg(groups),
        ICON / "loquax-background.svg": background_svg(),
        ICON / "loquax-monochrome.svg": monochrome_svg(groups),
        RES / "mipmap-anydpi-v26" / "ic_launcher.xml": ADAPTIVE,
        RES / "drawable" / "ic_launcher_foreground.xml": vector(
            groups, "loquax", None, lambda name, fill: True
        ),
        RES / "drawable" / "ic_launcher_monochrome.xml": vector(
            groups,
            None,
            "#000000",
            lambda name, fill: name == "head" and fill in SILHOUETTE_FILLS,
        ),
        RES / "values" / "ic_launcher.xml": background_colour(),
    }


def rasters(groups, round_pictures):
    """Every PNG the script renders, by path: (source SVG, size).

    `round_pictures` carries the composed disc the apps' own icons render
    from; Loquax's is checked in as the adaptive icon's SVG, and lnomad's
    and lnflash's are only ever a PNG, so they are written to the scratch
    directory first.
    """
    out = {
        ASSETS / "favicon-32.png": (ASSETS / "loquax.svg", 32),
        ASSETS / "head-256.png": (ASSETS / "head.svg", 256),
        ASSETS / "figure-512.png": (SOURCE, 512),
        ROOT / "docs" / "src" / "images" / "nessa-head.png": (ASSETS / "head.svg", 256),
        ROOT / "docs" / "src" / "images" / "nessa-favicon.png": (ASSETS / "loquax.svg", 32),
        ROOT / "docs" / "src" / "images" / "nessa-figure.png": (SOURCE, 512),
        ROOT / "packaging" / "loquax" / "loquax.png": (ICON / "loquax.svg", 256),
        ROOT / "packaging" / "lnomad" / "lnomad.png": (round_pictures["lnomad"], 256),
        ROOT / "packaging" / "lnflash" / "lnflash.png": (round_pictures["lnflash"], 256),
        ROOT / "leviculum-ui-iced" / "assets" / "window-icon.png": (ICON / "loquax.svg", 128),
    }
    for density, px in DENSITIES.items():
        out[RES / f"mipmap-{density}" / "ic_launcher.png"] = (ICON / "loquax.svg", px)
    return out


# --- The M9's splash ------------------------------------------------------


def png_pixels(path):
    """An 8-bit RGBA, non-interlaced PNG (what resvg writes) as raw RGBA
    bytes row by row: the five scanline filters undone."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"nessa-render: {path} is no PNG")
    at, idat, head = 8, b"", None
    while at < len(data):
        (n,) = struct.unpack(">I", data[at : at + 4])
        kind, body = data[at + 4 : at + 8], data[at + 8 : at + 8 + n]
        at += 12 + n
        if kind == b"IHDR":
            head = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
    if head is None or head[2:] != (8, 6, 0, 0, 0):
        sys.exit(f"nessa-render: {path} is not 8-bit RGBA non-interlaced: {head}")
    width, height = head[0], head[1]
    raw, stride, bpp = zlib.decompress(idat), head[0] * 4, 4
    out, prev = bytearray(), bytearray(stride)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1 : (y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if kind == 1:
                line[i] = (line[i] + a) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + b) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
            elif kind != 0:
                sys.exit(f"nessa-render: {path}: filter {kind} in row {y}")
        out += line
        prev = line
    assert len(out) == width * height * 4
    return width, height, bytes(out)


def pack_bits(pixels):
    """White is ink, everything else is the panel's ground: one bit a pixel,
    rows in order, the leftmost pixel of a byte in its high bit."""
    out = bytearray((SPLASH_PX * SPLASH_PX) // 8)
    for i in range(SPLASH_PX * SPLASH_PX):
        r, g, b = pixels[i * 4 : i * 4 + 3]
        if 0.2126 * r + 0.7152 * g + 0.0722 * b > 128:
            out[i // 8] |= 1 << (7 - i % 8)
    return bytes(out)


# --- Running --------------------------------------------------------------


def render(src, size, out):
    out.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["resvg", "-w", str(size), "-h", str(size), str(src), str(out)],
        check=True,
    )


def main():
    arguments = sys.argv[1:]
    if arguments not in ([], ["--check"]):
        sys.exit("usage: nessa-render.py [--check]")
    check = arguments == ["--check"]

    groups = read_source()
    if SCRATCH.exists():
        shutil.rmtree(SCRATCH)
    SCRATCH.mkdir(parents=True)
    differ = []

    texts = sources(groups)
    for path, text in texts.items():
        if check:
            if not path.exists() or path.read_text() != text:
                differ.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)

    # The forge's gate container may not carry resvg; the push path and every
    # coder pass do. Everything above this line needs no renderer at all, so
    # the SVG and vector-drawable products are still held to the source there
    # and only the pictures are skipped, with a line that says which.
    if shutil.which("resvg") is None:
        print("nessa-render: SKIP the pictures, no resvg on this host (sudo apt install resvg)")
        for path in differ:
            print(f"nessa-render: {path.relative_to(ROOT)} differs from the source's")
        sys.exit(1 if differ else 0)

    # The rendered pictures are all from files written above, so a
    # hand-edited one is what `--check` compares, not a stale copy.
    round_pictures = {}
    for sign in ("lnomad", "lnflash"):
        round_pictures[sign] = SCRATCH / f"{sign}-icon.svg"
        round_pictures[sign].write_text(composed_svg(groups, sign))
    for path, (src, size) in rasters(groups, round_pictures).items():
        out = SCRATCH / "png" / path.relative_to(ROOT)
        render(src, size, out)
        if check:
            if not path.exists() or path.read_bytes() != out.read_bytes():
                differ.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(out, path)

    splash = SCRATCH / "splash-head.png"
    render(SPLASH_SVG, SPLASH_PX, splash)
    width, height, pixels = png_pixels(splash)
    if (width, height) != (SPLASH_PX, SPLASH_PX):
        sys.exit(f"nessa-render: the splash rendered {width}x{height}")
    bits = pack_bits(pixels)
    for path, wanted in ((SPLASH_PNG, splash.read_bytes()), (SPLASH_BITS, bits)):
        if check:
            if not path.exists() or path.read_bytes() != wanted:
                differ.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(wanted)

    shutil.rmtree(SCRATCH)
    for path in differ:
        print(f"nessa-render: {path.relative_to(ROOT)} differs from the source's")
    if differ:
        sys.exit(1)


if __name__ == "__main__":
    main()
