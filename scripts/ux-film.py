#!/usr/bin/env python3
"""`just ux-film`: a way's frames as a film strip a person can read (order 1994).

A walk's rules judge numbers; Lew finds the map's bugs by watching the
sequence. This draws the sequence: the frames of one way in order, the
input marked on each frame, the frames where a rule failed framed in red
with the rule's line under them, and for two runs of the same way (before,
after) the two strips above each other with a difference strip under them
that marks where the after run moved or changed.

Input, a way's output directory (the contract stage 0a writes):

* `frames.tsv`, header row, columns `frame` (number), `t` (ms) and `image`
  (a PNG, relative to the directory); other columns are ignored.
* `inputs.tsv` (optional), header row, columns `t` (ms), `kind` (`move`,
  `press`, `release`, `wheel`, `key`, `view`, ...), `x`, `y` (window px),
  `button`, `wheel` (steps), `note`; an empty cell is "not given".
* `rules.tsv` (optional), the walk's rows (`summary.tsv` columns, `rule`,
  `verdict`, `value`, `note`) with a `frame`, `image` or `t` column that
  places a failure on its frame; a failed row with none of them is the
  whole way's and is listed above the strip.

Until stage 0a has landed, `--load-walk OUT --session NAME` reads the load
walk's output instead (`scripts/load-walk.py`): its pause pictures
(`full/load-NAME-NN-*.png`) as the frames, the `t=` its rows give each, its
trace's view changes (zoom and centre between trace lines) as `view`
inputs, and its failed rows. What that output lacks, and the film then
lacks: the frames between pauses (only one picture per pause is kept), the
pointer's position, the button and the wheel's steps (the trace holds the
view each input produced, not the input).

Thinning: a strip keeps at most `--max-frames` (default 48, eight columns
by six rows) so a 20 s way at 60 Hz (1200 frames) fits one screen. Kept
always: the first and last frame, every frame a rule failed on, and the
frame at or after each input with one frame either side; then the rest of
the budget samples the other frames evenly. When those outrank the budget,
failures go first, then inputs, then their neighbours. A cell after a gap
says how many frames were skipped.

Two runs (`--before DIR` with the run's DIR, or `--before-load-walk OUT`
with `--load-walk OUT`): frames are paired by number (the before frame
of the same number, else the nearest in time: two runs' clocks differ). The difference cell shows |after - before|
brightened, and is framed and labelled red when the after frame is the
before frame moved (a shift of at least 2 px that explains the difference
better than none) or when more than 1 % of it changed.

Writes `film.png` and `film.html` (the strip, the thinning, the failures,
each kept frame linked) into `--out` (default the way directory).

Usage:
  python3 -I scripts/ux-film.py WAY_DIR [--before DIR] [--out DIR]
  python3 -I scripts/ux-film.py --load-walk OUT --session NAME
                                [--before-load-walk OUT] [--out DIR]
  python3 -I scripts/ux-film.py --selftest [--selftest-keep DIR]
"""

import argparse
import html
import os
import random
import re
import sys
import tempfile

try:
    from PIL import Image, ImageChops, ImageDraw, ImageFont, ImageStat
except ImportError:  # pragma: no cover - a host without python3-pil
    sys.stderr.write("ux-film: needs Pillow (Debian: python3-pil)\n")
    sys.exit(2)

RED = (200, 0, 0)
INPUT = (0, 90, 220)
CANVAS_W = 1900
CANVAS_H = 1040
CAPTION = 34
PAD = 6
JUMP_MIN_PX = 2
CHANGED_MIN = 0.01


def read_tsv(path):
    if not os.path.exists(path):
        return []
    with open(path) as f:
        lines = [line.rstrip("\n") for line in f if line.strip()]
    if not lines:
        return []
    head = lines[0].split("\t")
    return [dict(zip(head, line.split("\t"))) for line in lines[1:]]


def num(text, default=None):
    try:
        return float(text)
    except (TypeError, ValueError):
        return default


class Way:
    """One run of a way: frames [(frame, t, path)], inputs [dict], failures."""

    def __init__(self, label):
        self.label = label
        self.frames = []
        self.inputs = []
        self.failures = {}  # frame index -> [line]
        self.whole = []  # failures placed on no frame
        self.missing = []  # what the source could not give

    def pair(self, frame, t):
        """The index of the frame numbered `frame` (two runs of one way count
        alike), else the one at or after t."""
        for i, (f, _, _) in enumerate(self.frames):
            if f == frame:
                return i
        return self.nearest(t) if t is not None else len(self.frames) - 1

    def nearest(self, t):
        """The index of the frame at or after t (the last frame when none is)."""
        for i, (_, ft, _) in enumerate(self.frames):
            if ft is not None and ft >= t:
                return i
        return len(self.frames) - 1

    def place_failures(self, rows):
        by_image = {os.path.splitext(os.path.basename(p))[0]: i for i, (_, _, p) in enumerate(self.frames)}
        by_frame = {f: i for i, (f, _, _) in enumerate(self.frames)}
        for r in rows:
            if r.get("verdict", "FAIL").lower() == "pass":
                continue
            line = f"{r.get('rule', '?')}: {r.get('value', '')}".strip()
            i = None
            if r.get("frame") not in (None, "") and num(r["frame"]) is not None:
                i = by_frame.get(int(num(r["frame"])))
            if i is None and r.get("image"):
                i = by_image.get(os.path.splitext(os.path.basename(r["image"]))[0])
            if i is None and num(r.get("t")) is not None:
                i = self.nearest(num(r["t"]))
            if i is None:
                self.whole.append(line + (f" ({r['note'][:160]})" if r.get("note") else ""))
            else:
                self.failures.setdefault(i, []).append(line)


def load_way(d, label):
    w = Way(label)
    for r in read_tsv(os.path.join(d, "frames.tsv")):
        f = int(num(r.get("frame"), len(w.frames)))
        w.frames.append((f, num(r.get("t")), os.path.join(d, r["image"])))
    if not w.frames:
        raise SystemExit(f"ux-film: {d}/frames.tsv names no frame")
    w.inputs = read_tsv(os.path.join(d, "inputs.tsv"))
    if not w.inputs:
        w.missing.append("no inputs.tsv: no input is marked")
    w.place_failures(read_tsv(os.path.join(d, "rules.tsv")))
    return w


PAUSE = re.compile(r"^load-(?P<session>.+)-(?P<n>[0-9]{2})-(?P<label>.+)\.png$")
T_IN = re.compile(r"\bt=([0-9]+)")


def load_walk_way(out, session, label):
    """The load walk's output as a way (until stage 0a writes frames.tsv)."""
    w = Way(label)
    rows = [r for r in read_tsv(os.path.join(out, "summary.tsv")) if r.get("sequence") == session]
    t_of = {}
    for r in rows:
        m = T_IN.search(r.get("value", "") + " " + r.get("note", ""))
        if r.get("image") and m:
            t_of.setdefault(r["image"], float(m.group(1)))
    full = os.path.join(out, "full")
    pauses = []
    for name in sorted(os.listdir(full)) if os.path.isdir(full) else []:
        m = PAUSE.match(name)
        # A session whose name is another's prefix ("in-out-in" and
        # "in-out-in-slow") is told apart by the two digits after it.
        if m and m.group("session") == session:
            pauses.append((int(m.group("n")), name))
    for n, name in sorted(pauses):
        w.frames.append((n, t_of.get(name[:-4]), os.path.join(full, name)))
    if not w.frames:
        raise SystemExit(f"ux-film: {out} holds no pause picture of session {session!r}")
    # One `view` input per movement: a run of trace lines whose view
    # changes from line to line (a wheel step's glide) is one input, from
    # the view before it to the view it rests on.
    trace = read_tsv(os.path.join(out, f"trace-{session}.tsv"))
    moves = []
    last = None
    for r in trace:
        view = (r.get("centre_x"), r.get("centre_y"), num(r.get("zoom_milli"), 0) / 1000)
        if last is not None and view != last[0]:
            if moves and moves[-1]["until"] == last[1]:
                moves[-1].update(until=r["t"], to=view)
            else:
                moves.append({"t": last[1], "until": r["t"], "from": last[0], "to": view})
        last = (view, r["t"])
    for m in moves:
        a, b = m["from"], m["to"]
        what = f"zoom {a[2]:g}->{b[2]:g}" if round(a[2], 1) != round(b[2], 1) else "pan"
        w.inputs.append({"t": m["t"], "kind": "view", "note": what})
    w.place_failures(rows)
    w.missing += [
        "frames: the load walk keeps one picture per pause, not every drawn frame",
        "inputs: the trace holds the view each input produced (marked as view), "
        "not the pointer, its button or the wheel's steps",
    ]
    return w


def thin(way, budget):
    """The kept frame indices, in order: failures, inputs, neighbours, samples."""
    n = len(way.frames)
    if n <= budget:
        return list(range(n))
    fail = sorted(way.failures)
    inputs = sorted({way.nearest(num(i.get("t"), 0)) for i in way.inputs if num(i.get("t")) is not None})
    near = sorted({j for i in fail + inputs for j in (i - 1, i + 1) if 0 <= j < n})
    keep = []
    for group in (fail, inputs, [0, n - 1], near):
        for i in group:
            if i not in keep and len(keep) < budget:
                keep.append(i)
    rest = [i for i in range(n) if i not in keep]
    room = budget - len(keep)
    if room > 0 and rest:
        step = len(rest) / room
        keep += [rest[int(k * step)] for k in range(room)]
    return sorted(set(keep))


def gray(im, scale):
    g = im.convert("L")
    if scale != 1:
        g = g.resize((max(1, int(g.width * scale)), max(1, int(g.height * scale))), Image.BILINEAR)
    return g


def err(a, b, dx, dy):
    """Mean absolute difference of b shifted by (dx, dy) against a, over the overlap."""
    w, h = a.size
    x0, y0 = max(0, dx), max(0, dy)
    x1, y1 = min(w, w + dx), min(h, h + dy)
    if x1 - x0 < w // 2 or y1 - y0 < h // 2:
        return float("inf")
    ca = a.crop((x0, y0, x1, y1))
    cb = b.crop((x0 - dx, y0 - dy, x1 - dx, y1 - dy))
    return ImageStat.Stat(ImageChops.difference(ca, cb)).mean[0]


def shift(before, after, reach=40):
    """(dx, dy, err at the shift, err unshifted): the move that best maps after onto before.

    A coarse search at a quarter of the size, then a fine one at full size
    around its best."""
    q = 0.25
    a, b = gray(before, q), gray(after, q)
    r = max(1, int(reach * q))
    best = min(((err(a, b, dx, dy), dx, dy) for dx in range(-r, r + 1) for dy in range(-r, r + 1)))
    A, B = gray(before, 1), gray(after, 1)
    cx, cy = int(best[1] / q), int(best[2] / q)
    fine = min(((err(A, B, dx, dy), dx, dy) for dx in range(cx - 4, cx + 5) for dy in range(cy - 4, cy + 5)))
    return fine[1], fine[2], fine[0], err(A, B, 0, 0)


def compare(before, after):
    """The difference picture and its mark ('' when nothing to say)."""
    if before.size != after.size:
        return Image.new("RGB", after.size, (60, 0, 0)), f"size {before.size} -> {after.size}"
    diff = ImageChops.difference(before.convert("RGB"), after.convert("RGB"))
    g = diff.convert("L")
    changed = sum(g.point(lambda v: 255 if v > 24 else 0).histogram()[255:]) / (g.width * g.height)
    pic = g.point(lambda v: min(255, v * 4)).convert("RGB")
    if changed <= 0:
        return pic, ""
    dx, dy, e_best, e0 = shift(before, after)
    if (abs(dx) >= JUMP_MIN_PX or abs(dy) >= JUMP_MIN_PX) and e_best < 0.5 * e0:
        # Positive dx: the content of after sits dx px right of before's.
        return pic, f"moved {-dx:+d},{-dy:+d} px"
    if changed > CHANGED_MIN:
        return pic, f"{changed * 100:.1f} % changed"
    return pic, ""


def font():
    try:
        return ImageFont.load_default(size=12)
    except TypeError:  # Pillow before 10.1
        return ImageFont.load_default()


def thumb_size(frame_size, cols, rows_per_band, bands):
    w, h = frame_size
    tw = (CANVAS_W - PAD) // cols - PAD
    th = int(tw * h / w)
    room = (CANVAS_H - 60) // max(1, bands * rows_per_band) - CAPTION - PAD
    if th > room:
        th = max(40, room)
        tw = int(th * w / h)
    return tw, th


def layout(frame_size, n, rows_per_band):
    """(cols, thumb width, thumb height): the column count that draws the
    largest thumbnails on one screen."""
    best = None
    for cols in range(4, 17):
        bands = (n + cols - 1) // cols
        tw, th = thumb_size(frame_size, cols, rows_per_band, bands)
        if best is None or tw * th > best[1] * best[2]:
            best = (cols, tw, th)
    return best


def fit(draw, text, width, f):
    """The text cut to `width` px."""
    while text and draw.textlength(text, font=f) > width:
        text = text[:-1]
    return text


def mark_inputs(draw, way, i, sx, sy, x, y, f, tw):
    """The inputs that fall on frame i (between the previous frame and this)."""
    t1 = way.frames[i][1]
    t0 = way.frames[i - 1][1] if i > 0 else None
    words = []
    for inp in way.inputs:
        t = num(inp.get("t"))
        if t is None or t1 is None or t > t1 or (t0 is not None and t <= t0):
            continue
        px, py = num(inp.get("x")), num(inp.get("y"))
        if px is not None and py is not None:
            cx, cy = x + px * sx, y + py * sy
            draw.ellipse((cx - 5, cy - 5, cx + 5, cy + 5), outline=INPUT, width=2)
            draw.line((cx - 8, cy, cx + 8, cy), fill=INPUT)
            draw.line((cx, cy - 8, cx, cy + 8), fill=INPUT)
        word = inp.get("kind", "")
        if inp.get("button"):
            word += f" {inp['button']}"
        if inp.get("wheel"):
            word += f" {inp['wheel']}"
        if inp.get("note"):
            word += f" {inp['note']}"
        words.append(word.strip())
    if words:
        text = fit(draw, "; ".join(dict.fromkeys(words)), tw - 4, f)
        draw.rectangle((x, y, x + draw.textlength(text, font=f) + 4, y + 14), fill=(255, 255, 255))
        draw.text((x + 2, y + 1), text, fill=INPUT, font=f)


def render(after, before, kept, out_png):
    """Draw the strip(s); returns [(index, cell box, mark)] of the kept frames."""
    f = font()
    first = Image.open(after.frames[0][2])
    rows_per_band = 3 if before else 1
    cols, tw, th = layout(first.size, len(kept), rows_per_band)
    bands = (len(kept) + cols - 1) // cols
    sx, sy = tw / first.width, th / first.height
    cell_h = th + CAPTION + PAD
    whole = [f"{w.label}: whole way: {line}" for w in (after, before) if w for line in w.whole]
    top = 40 + 16 * len(whole)
    height = top + bands * rows_per_band * cell_h + PAD
    width = PAD + cols * (tw + PAD)
    sheet = Image.new("RGB", (width, height), (245, 245, 245))
    draw = ImageDraw.Draw(sheet)
    title = f"{after.label}" + (f"  vs  {before.label} (before)" if before else "")
    rowsay = "  rows: b(efore), after, diff" if before else ""
    draw.text((PAD, 8), f"{title}: {len(kept)} of {len(after.frames)} frames{rowsay}", fill=(0, 0, 0), font=f)
    for n, line in enumerate(whole):
        draw.text((PAD, 26 + 16 * n), fit(draw, line, width - 2 * PAD, f), fill=RED, font=f)
    cells = []
    prev = None
    for k, i in enumerate(kept):
        band, col = divmod(k, cols)
        x = PAD + col * (tw + PAD)
        y0 = top + band * rows_per_band * cell_h
        fr, t, path = after.frames[i]
        lines = [(f"#{fr} {t:.0f}ms" if t is not None else f"#{fr}")
                 + (f" +{i - prev - 1} skipped" if prev is not None and i - prev > 1 else "")]
        rows = [("after" if before else None, Image.open(path).convert("RGB"), after, i)]
        mark = ""
        if before:
            j = before.pair(fr, t)
            bim = Image.open(before.frames[j][2]).convert("RGB")
            aim = rows[0][1]
            dpic, mark = compare(bim, aim)
            rows = [("before", bim, before, j), ("after", aim, after, i), ("diff", dpic, None, None)]
        for r, (name, im, way, idx) in enumerate(rows):
            y = y0 + r * cell_h
            sheet.paste(im.resize((tw, th), Image.BILINEAR), (x, y))
            bad = way is not None and idx in way.failures
            if name == "diff" and mark:
                bad = True
            if bad:
                draw.rectangle((x - 3, y - 3, x + tw + 2, y + th + 2), outline=RED, width=3)
            if way is not None:
                mark_inputs(draw, way, idx, sx, sy, x, y, f, tw)
            text = lines[0] if r == 0 else (name or "")
            if name and r == 0:
                text = f"{name[0]} {text}"
            if name == "diff":
                text = mark if mark else "diff: same"
            draw.text((x, y + th + 2), fit(draw, text, tw, f), fill=RED if name == "diff" and mark else (0, 0, 0), font=f)
            if way is not None and idx in way.failures:
                draw.text((x, y + th + 16), fit(draw, "; ".join(way.failures[idx]), tw, f), fill=RED, font=f)
        cells.append((i, (x, y0, x + tw, y0 + rows_per_band * cell_h), mark))
        prev = i
    sheet.save(out_png)
    return cells


def write_html(out, after, before, kept, cells, budget):
    e = html.escape
    parts = [
        "<!doctype html><meta charset=utf-8>",
        f"<title>film: {e(after.label)}</title>",
        "<style>body{font:13px sans-serif;margin:1em}.FAIL{color:#b00;font-weight:bold}"
        "img{max-width:100%}td,th{border:1px solid #ccc;padding:2px 6px}</style>",
        f"<h1>film: {e(after.label)}" + (f" vs {e(before.label)}" if before else "") + "</h1>",
        f"<p>{len(kept)} of {len(after.frames)} frames kept (budget {budget}): the first and the "
        "last, every frame a rule failed on, the frame at or after each input with one either "
        "side, the rest sampled evenly.</p>",
    ]
    for w in (after, before):
        if w is None:
            continue
        for m in w.missing:
            parts.append(f"<p>{e(w.label)}: missing: {e(m)}</p>")
        for line in w.whole:
            parts.append(f"<p class=FAIL>{e(w.label)}: whole way: {e(line)}</p>")
    parts.append("<p><img src='film.png'></p><table><tr><th>frame</th><th>t</th><th>failed</th>"
                 + ("<th>difference</th>" if before else "") + "</tr>")
    for i, _, mark in cells:
        fr, t, path = after.frames[i]
        fails = "; ".join(after.failures.get(i, []))
        rel = os.path.relpath(path, out)
        parts.append(f"<tr><td><a href='{e(rel)}'>{fr}</a></td><td>{'' if t is None else f'{t:.0f}'}</td>"
                     f"<td class=FAIL>{e(fails)}</td>" + (f"<td class=FAIL>{e(mark)}</td>" if before else "")
                     + "</tr>")
    parts.append("</table>")
    with open(os.path.join(out, "film.html"), "w") as fh:
        fh.write("\n".join(parts) + "\n")


def film(after, before, out, budget):
    os.makedirs(out, exist_ok=True)
    kept = thin(after, budget)
    cells = render(after, before, kept, os.path.join(out, "film.png"))
    write_html(out, after, before, kept, cells, budget)
    return kept, cells


def synthetic(d, frames, jump_at=None, jump_px=20, fail_at=None):
    """A way of `frames` frames panning 2 px a frame over a fixed random world."""
    rnd = random.Random(7)
    world = Image.new("RGB", (900, 260), (230, 225, 210))
    draw = ImageDraw.Draw(world)
    for _ in range(140):
        x, y = rnd.randrange(900), rnd.randrange(260)
        c = (rnd.randrange(256), rnd.randrange(256), rnd.randrange(256))
        draw.rectangle((x, y, x + rnd.randrange(8, 40), y + rnd.randrange(8, 40)), fill=c)
    os.makedirs(os.path.join(d, "f"), exist_ok=True)
    rows = ["frame\tt\timage"]
    for i in range(frames):
        # The pan wraps inside the world, so a long way never runs off it.
        off = (2 * i) % 400 + (jump_px if jump_at is not None and i == jump_at else 0)
        world.crop((100 + off, 30, 100 + off + 320, 230)).save(os.path.join(d, "f", f"{i:04d}.png"))
        rows.append(f"{i}\t{i * 16}\tf/{i:04d}.png")
    with open(os.path.join(d, "frames.tsv"), "w") as fh:
        fh.write("\n".join(rows) + "\n")
    with open(os.path.join(d, "inputs.tsv"), "w") as fh:
        fh.write("t\tkind\tx\ty\tbutton\twheel\n" f"{10 * 16}\tpress\t160\t100\tleft\t\n"
                 f"{25 * 16}\twheel\t80\t50\t\t-1\n")
    if fail_at is not None:
        with open(os.path.join(d, "rules.tsv"), "w") as fh:
            fh.write("frame\trule\tvalue\tverdict\n" f"{fail_at}\tnothing_jumps\t20 px\tFAIL\n")


def selftest(keep=None):
    """Synthetic ways; with `keep`, their films stay there to be looked at."""
    checks = []

    def check(what, cond):
        checks.append((what, bool(cond)))

    with tempfile.TemporaryDirectory() as scratch:
        tmp = keep or scratch
        before, after, same, longway = (os.path.join(tmp, n) for n in ("before", "after", "same", "long"))
        synthetic(before, 40)
        synthetic(after, 40, jump_at=20, fail_at=30)
        synthetic(same, 40)
        out = os.path.join(tmp, "film")
        kept, cells = film(load_way(after, "after"), load_way(before, "before"), out, 48)
        marks = {i: m for i, _, m in cells}
        check("the 20 px jump is marked in the difference strip", marks.get(20, "").startswith("moved"))
        m = re.match(r"moved ([+-][0-9]+),([+-][0-9]+) px", marks.get(20, ""))
        check(f"the mark says 20 px across ({marks.get(20)})",
              m and abs(abs(int(m.group(1))) - 20) <= 1 and abs(int(m.group(2))) <= 1)
        check("no other frame is marked", [i for i, mk in marks.items() if mk and i != 20] == [])
        png = Image.open(os.path.join(out, "film.png")).convert("RGB")
        box = next(b for i, b, _ in cells if i == 20)
        th = (box[3] - box[1]) // 3
        diff_y = box[1] + 2 * th
        check("the jump's difference cell is framed red", png.getpixel((box[0] - 2, diff_y + 10)) == RED)
        fail_box = next(b for i, b, _ in cells if i == 30)
        check("the failed frame is framed red", png.getpixel((fail_box[0] - 2, fail_box[1] + th + 10)) == RED)
        calm_box = next(b for i, b, _ in cells if i == 5)
        check("negative: a calm frame is not framed", png.getpixel((calm_box[0] - 2, calm_box[1] + 10)) != RED)
        check("film.html names the failure", "nothing_jumps" in open(os.path.join(out, "film.html")).read())
        # Negative: two equal runs mark nothing.
        _, cells = film(load_way(same, "same"), load_way(before, "before"), os.path.join(tmp, "film2"), 48)
        check("negative: two equal runs mark no difference", not any(mk for _, _, mk in cells))
        # Thinning: a 20 s way at 60 Hz fits the budget and keeps what matters.
        synthetic(longway, 1200, fail_at=700)
        w = load_way(longway, "long")
        kept = thin(w, 48)
        check(f"a 1200-frame way is thinned to the budget ({len(kept)})", len(kept) <= 48)
        check("thinning keeps the failure, the inputs' frames, the first and the last",
              all(i in kept for i in (700, 10, 25, 0, 1199)))
        _, cells = film(w, None, os.path.join(tmp, "film3"), 48)
        png = Image.open(os.path.join(tmp, "film3", "film.png"))
        check(f"its strip fits one screen ({png.width}x{png.height})", png.width <= 1920 and png.height <= 1080)
    for what, ok in checks:
        print(f"ux-film selftest: {'PASS' if ok else 'FAIL'} {what}")
    ok = all(ok for _, ok in checks)
    print(f"ux-film selftest: {'PASS' if ok else 'FAIL'} {sum(o for _, o in checks)}/{len(checks)}")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("way", nargs="?")
    ap.add_argument("--before")
    ap.add_argument("--load-walk")
    ap.add_argument("--before-load-walk")
    ap.add_argument("--session")
    ap.add_argument("--out")
    ap.add_argument("--max-frames", type=int,
                    help="frames a strip keeps (default 48, with --before 24: three rows a frame)")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--selftest-keep", help="keep the selftest's ways and films in this directory")
    args = ap.parse_args()
    if args.selftest:
        return selftest(args.selftest_keep)
    if args.load_walk:
        if not args.session:
            ap.error("--load-walk needs --session")
        after = load_walk_way(args.load_walk, args.session, f"{args.session} ({args.load_walk})")
        before = (load_walk_way(args.before_load_walk, args.session, f"{args.session} ({args.before_load_walk})")
                  if args.before_load_walk else None)
        out = args.out or os.path.join(args.load_walk, "film", args.session)
    elif args.way:
        after = load_way(args.way, args.way)
        before = load_way(args.before, args.before) if args.before else None
        out = args.out or args.way
    else:
        ap.error("name a way directory, --load-walk OUT --session NAME, or --selftest")
    budget = args.max_frames or (24 if before else 48)
    kept, cells = film(after, before, out, budget)
    marked = [m for _, _, m in cells if m]
    print(f"ux-film: {len(kept)} of {len(after.frames)} frames, {len(after.failures)} with a failed rule, "
          f"{len(marked)} marked different; {out}/film.html")
    for m in after.missing:
        print(f"ux-film: missing: {m}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
