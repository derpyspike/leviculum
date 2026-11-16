#!/usr/bin/env python3
"""A new citation names its item; a name citation resolves to exactly one.

Two refusals, both over the living texts (scripts/citations.py, `is_living`):

1. Every name citation `<path>#<Item>` resolves: `path` is a file of this
   tree written from the repository root, and `Item` names exactly one item
   of it. Refused: a name that is gone (a renamed or deleted fn), a name two
   items share (write it with its owner, `Type::new`, or a kind prefix,
   `field:Link::x`), a base name or a shortened path (`transport.rs` before
   the `#`: the guard never guesses which file is meant, order 1848 matched
   a citation into another repository that way), and a path into another
   repository, where no item can be checked from here.

2. No NEW `path:line` citation into this tree. "New" is counted against the
   base revision (--base, else $LEVICULUM_CITATION_BASE, else the merge base
   of HEAD and origin/master, but never older than the commit that added
   this guard: what stood in the tree before the rule existed is not new):
   per living text and per cited path as written,
   the line citations of the working tree may not outnumber those of the
   base. A citation that only moved, or whose number the
   `LEVICULUM_CITATION_FIX` fixer of leviculum-std/tests/doc_citations.rs
   renumbered, keeps the count and passes; the existing ones are left alone
   until a conversion wave turns them into names. A citation into another
   repository or a pinned reference (`reference/...`, a bare `Transport.py`
   that names no file of this tree) is not refused. A bare name that one of
   this tree's files carries (`transport.rs` and a line) is refused like a
   full path: it may be ours, and the guard does not guess.

   A line that must hold a citation-shaped string as data (a fixture of a
   citation checker) carries the marker `citation-guard: line-fixture`.

Exit 0 when nothing is refused, 1 otherwise. scripts/citations-selftest.py
holds the positive controls.
"""

import argparse
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import citations as C  # noqa: E402

RULE = "docs/src/concepts/checks-and-citations.md, \"A new citation names its item\""
FIXTURE_MARK = "citation-guard: line-fixture"


def git(root, *args, check=True):
    r = subprocess.run(["git", "-C", root, *args], capture_output=True, text=True)
    if check and r.returncode != 0:
        raise SystemExit(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r


def default_base(root):
    """(revision, how it was chosen) that new line citations count against."""
    env = os.environ.get("LEVICULUM_CITATION_BASE")
    if env:
        return env, "LEVICULUM_CITATION_BASE"
    me = os.path.relpath(os.path.abspath(__file__), root)
    intro = git(root, "log", "--diff-filter=A", "--format=%H", "-1", "--", me, check=False)
    intro = intro.stdout.strip()
    if not intro:
        return "HEAD", "HEAD: this guard is not committed yet"
    mb = git(root, "merge-base", "HEAD", "origin/master", check=False).stdout.strip()
    if mb and git(root, "merge-base", "--is-ancestor", intro, mb, check=False).returncode == 0:
        return mb, "merge base with origin/master"
    return intro, "the commit that added this guard"


def check_names(tree):
    """(count, refusals) for the name citations of the living texts: each
    must resolve to exactly one item."""
    out, n = [], 0
    for rel in sorted(f for f in tree.files if C.is_living(f)):
        text = tree.text(rel)
        if text is None:
            continue
        for m in C.name_citations(text, rel):
            n += 1
            path, spec = m.group(1), m.group(2)
            where = f"{rel}:{text.count(chr(10), 0, m.start()) + 1}: `{m.group(0)}`"
            if path not in tree.files:
                if any(path.startswith(p) for p in C.EXTERNAL_PREFIXES):
                    out.append(f"{where}\n    names another repository; no item can be checked "
                               "there. Cite it by line with its prefix, or by URL.")
                elif os.path.basename(path) in tree.basenames:
                    cands = sorted(f for f in tree.basenames[os.path.basename(path)]
                                   if f.endswith("/" + path))
                    out.append(f"{where}\n    is not a path from the repository root, and the "
                               "guard does not guess which file is meant"
                               + (f" (files ending so: {', '.join(cands)})" if cands else "") + ".")
                else:
                    out.append(f"{where}\n    no such file in this tree.")
                continue
            status, hits = C.resolve_in(tree, path, spec)
            if status == "unknown":
                out.append(f"{where}\n    {path} has no item `{spec}`: renamed or deleted? "
                           "Cite what the text means now.")
            elif status == "ambiguous":
                desc = ", ".join(f"{h.label()} ({h.kind}, line {h.head})" for h in hits[:6])
                out.append(f"{where}\n    `{spec}` names {len(hits)} items: {desc}. Write it "
                           "with its owner (`Type::name`) or a kind prefix (`fn:`, `field:`).")
    return n, out


def ours(tree, m, rel, text):
    """The located path when a line citation may point into this tree, else
    None. A bare name that a file here carries counts as ours."""
    kind, path, why = C.locate(tree, m.group(1), rel, text)
    if kind == "tree":
        return path
    if kind == "foreign" and why == "unplaced":
        return m.group(1)
    return None


def counts(tree, rel, text):
    """{cited path as written: [match, ...]} of the line citations of `text`
    that may point into this tree."""
    out = {}
    for m in C.line_citations(text, rel):
        ls = text.rfind("\n", 0, m.start()) + 1
        le = text.find("\n", m.start())
        if FIXTURE_MARK in text[ls:le if le >= 0 else len(text)]:
            continue
        if ours(tree, m, rel, text) is not None:
            out.setdefault(m.group(1), []).append(m)
    return out


def added_lines(root, base, rel):
    """Line numbers of `rel` in the working tree that the diff from `base`
    adds; None when the file is new (every line is added)."""
    if git(root, "cat-file", "-e", f"{base}:{rel}", check=False).returncode != 0:
        return None
    diff = git(root, "diff", "-U0", "--no-color", base, "--", rel).stdout
    lines = set()
    for h in re.finditer(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", diff, re.M):
        start, n = int(h.group(1)), int(h.group(2) or 1)
        lines.update(range(start, start + n))
    return lines


def suggestion(tree, rel, m, text):
    path = ours(tree, m, rel, text)
    found = tree.items(path) if path in tree.files else None
    if not found:
        return "`<path>#<Item>`"
    a = C.spans(m.group(2))[0][0]
    holding = [it for it in found if it.kind != "anchor" and it.start <= a <= it.end]
    if not holding:
        return "`<path>#<Item>`"
    inner = min(holding, key=lambda it: (it.end - it.start, -it.start))
    return f"`{path}#{inner.label()}` if that is the item meant"


def check_new_lines(tree, base):
    """Refusals for line citations the working tree adds over `base`."""
    root = tree.root
    changed = set(git(root, "diff", "--name-only", "--no-renames", base).stdout.split("\n"))
    changed |= set(git(root, "ls-files", "--others", "--exclude-standard").stdout.split("\n"))
    out = []
    for rel in sorted(f for f in changed if f and f in tree.files and C.is_living(f)):
        text = tree.text(rel)
        if text is None:
            continue
        old = git(root, "show", f"{base}:{rel}", check=False)
        before = counts(tree, rel, old.stdout) if old.returncode == 0 else {}
        after = counts(tree, rel, text)
        added = added_lines(root, base, rel)
        for cited, ms in sorted(after.items()):
            grown = len(ms) - len(before.get(cited, []))
            if grown <= 0:
                continue
            for m in ms:
                n = text.count("\n", 0, m.start()) + 1
                if added is not None and n not in added:
                    continue
                out.append(f"{rel}:{n}: `{m.group(0)}`\n    a new line citation ({cited} is cited "
                           f"{len(ms)} times here, {len(before.get(cited, []))} at the base). "
                           f"Cite the item by name: {suggestion(tree, rel, m, text)}.")
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=C.REPO_ROOT)
    ap.add_argument("--base", help="revision new line citations are counted against")
    args = ap.parse_args(argv)
    tree = C.Tree(args.root)
    if args.base:
        base, why = args.base, "--base"
    else:
        base, why = default_base(args.root)
    n_names, names = check_names(tree)
    lines = check_new_lines(tree, base)
    for f in names + lines:
        print(f"REFUSED {f}")
    print(f"citations by name: {n_names} name citation(s) checked, {len(names)} refused; "
          f"new line citations against {base[:12]} ({why}): {len(lines)} refused")
    if names or lines:
        print(f"See {RULE}.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
