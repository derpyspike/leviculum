#!/usr/bin/env python3
"""Two branches that each add a registry line merge cleanly (order 1848).

The append-only registries (the mvr module list in
leviculum-std/tests/mvr/main.rs, scripts/source-invariant-targets.txt) carry
git's `union` merge driver (.gitattributes). This builds a scratch
repository from the tree's own copies of those files, adds one line on each
of two branches at the SAME place (the case a text merge refuses), and
merges them twice: without .gitattributes, where the merge must conflict
(the positive control: the scenario is the one that hurt), and with it,
where it must merge cleanly with both lines present exactly once and every
entry still one well-formed line.

Exit 0 = both halves hold; 1 = one does not (named).
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MVR = "leviculum-std/tests/mvr/main.rs"
CENSUS = "scripts/source-invariant-targets.txt"


def git(repo, *args, check=True):
    return subprocess.run(["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@t",
                           "-c", "commit.gpgsign=false", *args],
                          capture_output=True, text=True, check=check)


def insert_after(text, anchor_re, line):
    out, done = [], False
    for l in text.split("\n"):
        out.append(l)
        if not done and re.match(anchor_re, l):
            out.append(line)
            done = True
    assert done, anchor_re
    return "\n".join(out)


def scenario(with_attributes):
    tmp = tempfile.mkdtemp(prefix="registry-merge-")
    try:
        git(tmp, "init", "-q", "-b", "base")
        for rel in (MVR, CENSUS):
            os.makedirs(os.path.join(tmp, os.path.dirname(rel)), exist_ok=True)
            shutil.copy(os.path.join(ROOT, rel), os.path.join(tmp, rel))
        if with_attributes:
            shutil.copy(os.path.join(ROOT, ".gitattributes"), os.path.join(tmp, ".gitattributes"))
        git(tmp, "add", "-A")
        git(tmp, "commit", "-qm", "base")
        adds = {"one": ("mod aaa_lane_one_adds_an_mvr;",
                        "run   leviculum-std   aaa_lane_one  # lane one's target"),
                "two": ("mod aaa_lane_two_adds_an_mvr;",
                        "run   leviculum-std   aaa_lane_two  # lane two's target")}
        for lane, (mod, verdict) in adds.items():
            git(tmp, "checkout", "-q", "-b", lane, "base")
            for rel, anchor, line in ((MVR, r"^// the merge\. A module listed twice", mod),
                                      (CENSUS, r"^run ", verdict)):
                p = os.path.join(tmp, rel)
                with open(p, encoding="utf-8") as fh:
                    text = fh.read()
                with open(p, "w", encoding="utf-8") as fh:
                    fh.write(insert_after(text, anchor, line))
            git(tmp, "commit", "-qam", lane)
        git(tmp, "checkout", "-q", "one")
        merged = git(tmp, "merge", "-q", "--no-edit", "two", check=False)
        texts = {}
        for rel in (MVR, CENSUS):
            with open(os.path.join(tmp, rel), encoding="utf-8") as fh:
                texts[rel] = fh.read()
        return merged.returncode, texts, adds
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    failures = []
    code, texts, _ = scenario(with_attributes=False)
    if code == 0 or "<<<<<<<" not in texts[MVR] + texts[CENSUS]:
        failures.append("without .gitattributes the two additions must conflict "
                        "(the control), and they merged")
    code, texts, adds = scenario(with_attributes=True)
    if code != 0:
        failures.append("with .gitattributes the merge must be clean, and it conflicted")
    for rel, text in texts.items():
        if "<<<<<<<" in text:
            failures.append(f"{rel} holds conflict markers")
    for mod, verdict in adds.values():
        if texts[MVR].split("\n").count(mod) != 1:
            failures.append(f"{MVR}: `{mod}` is not there exactly once")
        if texts[CENSUS].split("\n").count(verdict) != 1:
            failures.append(f"{CENSUS}: `{verdict}` is not there exactly once")
    # every module line below the list's head is one well-formed `mod x;`,
    # and none is listed twice
    lines = texts[MVR].split("\n")
    start = max(i for i, l in enumerate(lines) if l.startswith("// the merge. A module listed twice"))
    mods = [l for l in lines[start + 1:] if l.strip()]
    bad = [l for l in mods if not re.fullmatch(r"mod [a-z0-9_]+;", l)]
    if bad:
        failures.append(f"{MVR}: lines in the module list that are not `mod x;`: {bad}")
    if len(set(mods)) != len(mods):
        failures.append(f"{MVR}: a module listed twice")
    if failures:
        print("registry merge selftest: FAILED")
        for f in failures:
            print("  " + f)
        return 1
    print(f"registry merge selftest: two adjacent additions conflict without the union "
          f"driver and merge cleanly with it ({len(mods)} mvr modules after the merge)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
