#!/usr/bin/env python3
"""`just load-walk`: the map's loading walked as a user does (1811).

Lew 2026-11-13: loading the map live was "totally buggy" on the desktop
and the phone — tiles missing after a wheel step, different ones after
the next, nothing loading after a move — and no precise report was
possible. This walk runs scripted sessions (a zoom ladder rested and
rushed, in-out-in, pans slow and fast, pan while zooming, a jump away
and back, each also against a 200 kB/s source, injected faults, a cache
at its cap) on the window's real path — the same `App`, scheduler,
loader and painter the window runs, headless against a local range
server — and checks every session by named rules (view_filled_at_rest,
screen_first, nothing_lost, no_regress, same_view_same_picture, no_spin,
fault_recovers, frame_never_blank, no_busy_wake). The driver, sessions
and rules live in `loquax/tests/support/load_walk.rs`.

Writes `OUT/index.html` (the contact sheet: every pause's picture with
its rule results), `OUT/summary.tsv`, and a per-frame trace per session;
exits 1 on any failed rule. `--quick` walks the sessions that fit about
two minutes; `--selftest` injects each fault class into the walk itself
and each rule must fire.

Usage:
  python3 -I scripts/load-walk.py [--quick | --selftest] [--out DIR]
"""

import argparse
import html
import os
import shutil
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

COLUMNS = [
    "variant", "kind", "sequence", "size", "centre", "zoom", "image",
    "rule", "value", "limit", "verdict", "note",
]


def host():
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True)
    for line in out.stdout.splitlines():
        if line.startswith("host: "):
            return line.split(": ", 1)[1]
    raise RuntimeError("rustc -vV names no host")


def run_walk(out, log, mode):
    env = dict(os.environ, LOQUAX_LOAD_WALK=os.path.abspath(out))
    if mode == "selftest":
        env["LOQUAX_LOAD_WALK_SELFTEST"] = "1"
        name = "the_load_walk_selftest_fires_every_rule"
    else:
        env["LOQUAX_LOAD_WALK_SET"] = mode
        name = "the_load_walk_holds_its_rules"
    cmd = ["cargo", "test", "--release", "-p", "loquax", "--features", "gui",
           "--bins", "--target", host(), name, "--", "--nocapture"]
    done = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
    log.write(done.stdout + done.stderr)
    # A red rule fails the test after writing its rows: the rows say
    # which. Anything else that stops it is the walk's failure.
    if done.returncode != 0 and "load-walk" not in done.stdout + done.stderr:
        sys.stderr.write(done.stderr[-4000:])
        raise RuntimeError(f"{' '.join(cmd)} did not run")


def run_phone(out, log):
    """The phone's sessions (1821): the same rules on the facade's path,
    frames asked only as MapView asks them; rows and pictures beside the
    window's."""
    env = dict(os.environ, LOQUAX_LOAD_WALK=os.path.abspath(out))
    cmd = ["cargo", "test", "--release", "-p", "loquax-android", "--test", "map",
           "the_phone_load_walk", "--", "--nocapture"]
    done = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
    log.write(done.stdout + done.stderr)
    if done.returncode != 0 and "load-walk" not in done.stdout + done.stderr:
        sys.stderr.write(done.stderr[-4000:])
        raise RuntimeError(f"{' '.join(cmd)} did not run")


def read_rows(out):
    rows = []
    for name in sorted(os.listdir(out)):
        if name.startswith("rows-") and name.endswith(".tsv"):
            with open(os.path.join(out, name)) as f:
                for line in f:
                    cells = line.rstrip("\n").split("\t")
                    if len(cells) == len(COLUMNS):
                        rows.append(dict(zip(COLUMNS, cells)))
    return rows


def write_summary(out, rows):
    with open(os.path.join(out, "summary.tsv"), "w") as f:
        f.write("\t".join(COLUMNS) + "\n")
        for r in rows:
            f.write("\t".join(r[c] for c in COLUMNS) + "\n")


def write_sheet(out, rows, title, seconds):
    rules = {}
    for r in rows:
        p, f = rules.get(r["rule"], (0, 0))
        rules[r["rule"]] = (p + (r["verdict"] == "pass"), f + (r["verdict"] != "pass"))
    e = html.escape
    parts = [
        "<!doctype html><meta charset=utf-8>",
        f"<title>{e(title)}</title>",
        "<style>body{font:13px sans-serif;margin:1em}table{border-collapse:collapse}"
        "td,th{border:1px solid #ccc;padding:2px 6px;vertical-align:top}"
        ".FAIL{color:#b00;font-weight:bold}.pass{color:#070}"
        ".cell{display:inline-block;margin:4px;width:250px;vertical-align:top}"
        ".cell img{max-width:244px;border:1px solid #888}"
        ".bad img{border:3px solid #b00}</style>",
        f"<h1>{e(title)}</h1>",
        f"<p>{len(rows)} verdicts, {sum(f for _, f in rules.values())} failed, "
        f"in {seconds:.0f} s. Rules and sessions: "
        "loquax/tests/support/load_walk.rs.</p>",
        "<h2>Rules</h2><table><tr><th>rule</th><th>pass</th><th>fail</th></tr>",
    ]
    for rule, (p, f) in sorted(rules.items()):
        parts.append(f"<tr><td>{e(rule)}</td><td class=pass>{p}</td>"
                     f"<td class={'FAIL' if f else 'pass'}>{f}</td></tr>")
    parts.append("</table>")
    parts.append("<h2>Verdicts</h2><table><tr><th>session</th><th>rule</th>"
                 "<th>value</th><th>limit</th><th>verdict</th><th>note</th></tr>")
    for r in rows:
        parts.append(
            f"<tr><td>{e(r['sequence'])}</td><td>{e(r['rule'])}</td>"
            f"<td>{e(r['value'])}</td><td>{e(r['limit'])}</td>"
            f"<td class={r['verdict']}>{r['verdict']}</td><td>{e(r['note'][:200])}</td></tr>")
    parts.append("</table>")
    full = os.path.join(out, "full")
    images = sorted(os.listdir(full)) if os.path.isdir(full) else []
    by_session = {}
    for name in images:
        if not name.endswith(".png"):
            continue
        session = name[len("load-"):].rsplit("-", 2)[0]
        by_session.setdefault(session, []).append(name)
    bad_images = {r["image"] for r in rows if r["verdict"] != "pass" and r["image"]}
    for session, names in by_session.items():
        parts.append(f"<h2>{e(session)}</h2>")
        for name in names:
            bad = name[:-4] in bad_images
            parts.append(f"<div class='cell{' bad' if bad else ''}'>"
                         f"<a href='full/{e(name)}'><img loading=lazy src='full/{e(name)}'></a>"
                         f"<br>{e(name)}</div>")
    with open(os.path.join(out, "index.html"), "w") as f:
        f.write("\n".join(parts))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--out", default=os.path.join(ROOT, "target", "load-walk"))
    args = ap.parse_args()
    mode = "selftest" if args.selftest else ("quick" if args.quick else "full")
    out = args.out
    if os.path.isdir(out):
        shutil.rmtree(out)
    os.makedirs(out)
    start = time.time()
    with open(os.path.join(out, "walk.log"), "w") as log:
        run_walk(out, log, mode)
        if mode != "selftest":
            run_phone(out, log)
    rows = read_rows(out)
    write_summary(out, rows)
    write_sheet(out, rows, f"load walk ({mode})", time.time() - start)
    failed = [r for r in rows if r["verdict"] != "pass"]
    for r in rows:
        mark = "FAIL" if r["verdict"] != "pass" else "pass"
        print(f"{mark} {r['sequence']} {r['rule']}: {r['value']} (limit {r['limit']})")
    print(f"{len(rows)} verdicts, {len(failed)} failed; {out}/index.html")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
