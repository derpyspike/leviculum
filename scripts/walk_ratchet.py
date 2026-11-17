#!/usr/bin/env python3
"""The walks' known-failures ratchet (order 1994).

The render walk failed 1145 verdicts at 4fa1fe818, every one of them older
than any pass, so every UI pass ran red and a NEW failure was invisible
among the old ones. `scripts/walk-known-failures.tsv` lists the failures
that are known, one per line:

    walk  rule  subject  first_seen  ref

* `walk` is the walk and its set: `render-walk`, `load-walk`, `ux-walk`
  for the full set, the same with `-quick` for `--quick`. The sets are
  listed apart because one picture name can hold another picture in
  each: the phone's jump from Berlin to z5 keeps 153 overlapping street
  names on its first frame in the full walk and none in the quick one
  (4fa1fe818), so a quick run would "heal" a full-walk entry it never
  drew;
* `rule` is the rule's name as the walk's rows give it;
* `subject` is the picture (the row's `image`) or, for a row without one,
  the session `variant:sequence:size`;
* `first_seen` is the commit the entry was first measured at (the list's
  first generation could only say "failing at" that commit, not since when);
* `ref` is the issue (`#N`) or order (`order N`) that owns its repair. An
  entry without one is refused: a known failure nobody owns is a failure
  nobody repairs.

Against a walk's rows (`summary.tsv` columns) the ratchet sorts every
failure:

* known: on the list. Reported and counted; does not fail the run.
* NEW: not on the list. Fails the run.
* healed: on the list, walked, and every verdict of it passes. Fails the
  run until the line is removed: the list only shrinks.
* unwalked: on the list and absent from the rows. A run that leaves a
  part of its set out (`render-walk --no-screens`) counts it only; a
  complete run fails on it, because an entry whose picture is gone can
  never heal and would stay forever.

Wall-clock rules (`WALL_CLOCK`) are not ratcheted per picture: their value
is the host's wall clock around a draw, and the coder host is shared, so
which keys fail changes from run to run with the other lanes' load. The
phone's `frame_p95` of `globe-complete-z15-pan-1224x2197-rest` measured
14.3 ms in a full walk beside another lane's `cargo test --workspace` and
4.4 ms in the quick walk a minute later; the quick set's 13 phone gestures
failed 8, 9 and 8 times in three runs, a different 8 the third time, and
the window's `pan-loading` at 1800x1320 passed its 33.3 ms three times and
failed the fourth. Listed per key, they made the walk red on most runs
for a reason no commit had. Their failures are counted and printed, marked
`wall-clock` in the summary, and never decide the run; the list refuses a
line for one. Frame time is measured where the host is controlled
(`just map-bench`, A/B in one run).

The list file is refused as a whole (exit 2 from the walk) when a line has
the wrong number of cells, an unknown walk, no ref, or repeats a key.

Usage (the walks import it; standalone for the list's upkeep):
  python3 -I scripts/walk_ratchet.py --selftest
  python3 -I scripts/walk_ratchet.py --check
  python3 -I scripts/walk_ratchet.py --generate WALK SUMMARY.tsv --commit C --ref REF
"""

import argparse
import os
import re
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIST = os.path.join(ROOT, "scripts", "walk-known-failures.tsv")
HEADER = ["walk", "rule", "subject", "first_seen", "ref"]
WALKS = ("render-walk", "load-walk", "ux-walk",
         "render-walk-quick", "load-walk-quick", "ux-walk-quick")
# Rules whose value is a wall-clock time on the (shared) host: a listed key
# that passes is counted, not failed (see the module's doc).
WALL_CLOCK = {"frame_p95"}
REF = re.compile(r"^(#[0-9]+|order [0-9]+)$")
COMMIT = re.compile(r"^[0-9a-f]{7,40}$")


class ListError(Exception):
    """The known-failures list is malformed; the walk refuses it whole."""


def subject(row):
    """The picture, or the session for a row that names none."""
    if row.get("image"):
        return row["image"]
    return f"{row.get('variant', '')}:{row.get('sequence', '')}:{row.get('size', '')}"


def parse(text, origin="walk-known-failures.tsv"):
    """Entries {(walk, rule, subject): {first_seen, ref, line}}; ListError on any bad line."""
    entries = {}
    errors = []
    for n, line in enumerate(text.split("\n"), 1):
        if not line.strip() or line.startswith("#"):
            continue
        cells = line.split("\t")
        if cells == HEADER:
            continue
        if len(cells) != len(HEADER):
            errors.append(f"{origin}:{n}: {len(cells)} cells, want {len(HEADER)} ({' '.join(HEADER)})")
            continue
        walk, rule, subj, first, ref = (c.strip() for c in cells)
        if walk not in WALKS:
            errors.append(f"{origin}:{n}: unknown walk {walk!r}, want one of {', '.join(WALKS)}")
        if not rule or not subj:
            errors.append(f"{origin}:{n}: an empty rule or subject")
        if not COMMIT.match(first):
            errors.append(f"{origin}:{n}: first_seen {first!r} is no commit hash")
        if rule in WALL_CLOCK:
            errors.append(f"{origin}:{n}: {rule} is a wall-clock rule, counted by every run and "
                          "never listed (walk_ratchet.py's doc)")
        if not REF.match(ref):
            errors.append(f"{origin}:{n}: refused, ref {ref!r} is no issue (#N) or order (order N): "
                          "a known failure needs an owner")
        key = (walk, rule, subj)
        if key in entries:
            errors.append(f"{origin}:{n}: repeats line {entries[key]['line']}")
        entries[key] = {"first_seen": first, "ref": ref, "line": n}
    if errors:
        raise ListError("\n".join(errors))
    return entries


def load(path=LIST):
    if not os.path.exists(path):
        return {}
    with open(path) as f:
        return parse(f.read(), os.path.relpath(path, ROOT))


def judge(walk, rows, entries, complete):
    """Sort the rows' failures against the list's entries for `walk`.

    A (rule, subject) fails when any of its rows fails. Returns a dict of
    lists of keys (walk, rule, subject): known, new, healed, unwalked; and
    `ok`, false when the run must fail."""
    seen = {}
    wall = set()
    for r in rows:
        k = (walk, r["rule"], subject(r))
        if r["rule"] in WALL_CLOCK:
            if r["verdict"] != "pass":
                wall.add(k)
            continue
        seen[k] = seen.get(k, False) or r["verdict"] != "pass"
    mine = {k for k in entries if k[0] == walk}
    failed = {k for k, bad in seen.items() if bad}
    out = {
        "known": sorted(failed & mine),
        "new": sorted(failed - mine),
        "healed": sorted(k for k in mine if k in seen and not seen[k]),
        "wall_clock": sorted(wall),
        "unwalked": sorted(k for k in mine if k not in seen),
        "listed": len(mine),
    }
    out["ok"] = not out["new"] and not out["healed"] and not (complete and out["unwalked"])
    return out


def mark(walk, rows, verdict):
    """The `ratchet` cell per row: known, NEW, healed, wall-clock or empty."""
    known = set(verdict["known"])
    new = set(verdict["new"])
    healed = set(verdict["healed"])
    wall = set(verdict["wall_clock"])
    for r in rows:
        k = (walk, r["rule"], subject(r))
        r["ratchet"] = ("known" if k in known else "NEW" if k in new
                        else "healed" if k in healed
                        else "wall-clock" if k in wall and r["verdict"] != "pass" else "")


def report(name, walk, verdict, entries, complete):
    """The lines a walk prints: the counts, every NEW and healed key, the known per rule."""
    lines = []
    per_rule = {}
    for k in verdict["known"]:
        per_rule[k[1]] = per_rule.get(k[1], 0) + 1
    for rule, n in sorted(per_rule.items()):
        lines.append(f"{name}: known {rule}: {n}")
    for k in verdict["wall_clock"]:
        lines.append(f"{name}: wall-clock {k[1]} {k[2]} (counted, not judged)")
    for k in verdict["new"]:
        lines.append(f"{name}: NEW FAIL {k[1]} {k[2]}")
    for k in verdict["healed"]:
        e = entries[k]
        lines.append(f"{name}: HEALED {k[1]} {k[2]} passes: remove line {e['line']} "
                     f"of scripts/walk-known-failures.tsv ({e['ref']})")
    if complete:
        for k in verdict["unwalked"]:
            e = entries[k]
            lines.append(f"{name}: GONE {k[1]} {k[2]} was not walked by a complete run: "
                         f"remove line {e['line']} ({e['ref']})")
    lines.append(
        f"{name}: ratchet {'PASS' if verdict['ok'] else 'FAIL'}: new {len(verdict['new'])}, "
        f"known {len(verdict['known'])} (listed {verdict['listed']}), "
        f"healed {len(verdict['healed'])}, unwalked {len(verdict['unwalked'])}"
        f"{'' if complete else ' (a partial run: unwalked is not judged)'}; "
        f"wall-clock failures {len(verdict['wall_clock'])} (counted, not judged)")
    return lines


def apply(name, walk, rows, complete):
    """The walk's whole ratchet step: load the list, judge, mark the rows'
    `ratchet` cells. Returns (ok, lines). ListError when the list is refused."""
    entries = load()
    verdict = judge(walk, rows, entries, complete)
    mark(walk, rows, verdict)
    return verdict["ok"], report(name, walk, verdict, entries, complete)


def generate(walk, rows, commit, ref):
    """List lines for every failing key of `rows`, for the list's first generation."""
    v = judge(walk, rows, {}, complete=False)
    return [f"{k[0]}\t{k[1]}\t{k[2]}\t{commit}\t{ref}" for k in v["new"]]


def read_summary(path):
    with open(path) as f:
        lines = f.read().split("\n")
    head = lines[0].split("\t")
    return [dict(zip(head, line.split("\t"))) for line in lines[1:] if line.strip()]


def selftest():
    """Positive and negative controls over synthetic rows."""
    good = "\t".join(["render-walk", "label_overlap", "globe-a", "4fa1fe818", "order 1994"])
    entries = parse(good)

    def rows(*cells):
        return [{"variant": "globe", "sequence": "s", "size": "1x1", "image": img,
                 "rule": rule, "verdict": v} for img, rule, v in cells]

    checks = []

    def check(what, cond):
        checks.append((what, bool(cond)))

    v = judge("render-walk", rows(("globe-a", "label_overlap", "FAIL"),
                                  ("globe-a", "frame_p95", "FAIL"),
                                  ("globe-b", "label_overlap", "pass")), entries, True)
    check("negative: a known failure and a wall-clock one pass the run",
          v["ok"] and len(v["known"]) == 1 and len(v["wall_clock"]) == 1)
    v = judge("render-walk", rows(("globe-a", "label_overlap", "FAIL"),
                                  ("globe-b", "label_overlap", "FAIL")), entries, True)
    check("a new failure fails the run", not v["ok"] and v["new"] == [("render-walk", "label_overlap", "globe-b")])
    v = judge("render-walk", rows(("globe-a", "label_overlap", "pass")), entries, False)
    check("a healed entry left on the list fails the run", not v["ok"] and len(v["healed"]) == 1)
    v = judge("render-walk", rows(("globe-a", "label_overlap", "pass"),
                                  ("globe-a", "label_overlap", "FAIL")), entries, True)
    check("negative: a key failing in one of two rows is not healed", v["ok"] and not v["healed"])
    v = judge("render-walk", rows(("globe-b", "label_overlap", "pass")), entries, False)
    check("negative: an entry a quick run did not walk passes", v["ok"] and len(v["unwalked"]) == 1)
    v = judge("render-walk", rows(("globe-b", "label_overlap", "pass")), entries, True)
    check("an entry a complete run did not walk fails the run", not v["ok"])
    v = judge("load-walk", rows(("globe-a", "label_overlap", "FAIL")), entries, True)
    check("an entry of another walk does not cover this walk's failure", not v["ok"] and len(v["new"]) == 1)
    v = judge("render-walk-quick", rows(("globe-a", "label_overlap", "pass")), entries, True)
    check("negative: a quick run does not judge the full set's entry", v["ok"] and v["listed"] == 0)
    for what, text in [
        ("an entry without a number is refused", good.rsplit("\t", 1)[0] + "\t"),
        ("an entry with a bare word for a ref is refused", good.rsplit("\t", 1)[0] + "\tlabels"),
        ("a repeated entry is refused", good + "\n" + good),
        ("an unknown walk is refused", good.replace("render-walk", "paint-walk")),
        ("a short line is refused", "render-walk\tlabel_overlap\tglobe-a"),
        ("a wall-clock rule's line is refused", good.replace("label_overlap", "frame_p95")),
    ]:
        try:
            parse(text)
            check(what, False)
        except ListError:
            check(what, True)
    check("negative: the issue form #N is accepted", parse(good.replace("order 1994", "#232")))
    # The list on disk parses, and a list file round-trips through generate.
    try:
        load()
        check("negative: scripts/walk-known-failures.tsv parses", True)
    except ListError as e:
        check(f"negative: scripts/walk-known-failures.tsv parses ({e})", False)
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "known.tsv")
        r = rows(("globe-a", "label_overlap", "FAIL"), ("", "screen_first", "FAIL"), ("", "frame_p95", "FAIL"))
        with open(path, "w") as f:
            f.write("\t".join(HEADER) + "\n" + "\n".join(generate("render-walk", r, "4fa1fe818", "order 1994")) + "\n")
        v = judge("render-walk", r, load(path), True)
        check("negative: a generated list covers the run it came from, wall clock left out",
              v["ok"] and len(v["known"]) == 2 and len(v["wall_clock"]) == 1)
    for what, ok in checks:
        print(f"walk-ratchet selftest: {'PASS' if ok else 'FAIL'} {what}")
    ok = all(ok for _, ok in checks)
    print(f"walk-ratchet selftest: {'PASS' if ok else 'FAIL'} {sum(o for _, o in checks)}/{len(checks)}")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--check", action="store_true", help="parse the list and print its counts")
    ap.add_argument("--generate", nargs=2, metavar=("WALK", "SUMMARY"))
    ap.add_argument("--commit")
    ap.add_argument("--ref")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    if args.generate:
        walk, summary = args.generate
        if walk not in WALKS or not args.commit or not args.ref or not REF.match(args.ref):
            print("walk-ratchet: --generate needs a known WALK, --commit and --ref '#N' or 'order N'",
                  file=sys.stderr)
            return 2
        for line in generate(walk, read_summary(summary), args.commit, args.ref):
            print(line)
        return 0
    try:
        entries = load()
    except ListError as e:
        print(f"walk-ratchet: {e}", file=sys.stderr)
        return 2
    counts = {}
    for walk, rule, _ in entries:
        counts[(walk, rule)] = counts.get((walk, rule), 0) + 1
    for (walk, rule), n in sorted(counts.items()):
        print(f"walk-ratchet: {walk} {rule}: {n}")
    print(f"walk-ratchet: {len(entries)} known failures listed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
