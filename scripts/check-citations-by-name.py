#!/usr/bin/env python3
"""A new citation names its item; a name citation resolves to exactly one.

Two refusals, both over the living texts (scripts/citations.py, `is_living`):

1. Every name citation `<path>#<Item>` resolves: `path` is a file of this
   tree written from the repository root, and `Item` names exactly one item
   of it. Refused: a name that is gone (a renamed or deleted fn), a member
   written without its owner (`new` where `Thing::new` is meant: the
   item's `min_segs`), a file of this tree the guard cannot read (not
   UTF-8: "cannot read <path>", and not counted as checked), a name two
   items share (write it with its owner, `Type::new`, or a kind prefix,
   `field:Link::x`), a base name or a shortened path (`transport.rs` before
   the `#`: the guard never guesses which file is meant, order 1848 matched
   a citation into another repository that way), and a path into another
   repository, where no item can be checked from here.

2. No NEW `path:line` citation into this tree. Every line citation on a
   line the working tree adds or changes over the base revision (--base,
   else $LEVICULUM_CITATION_BASE, else the merge base of HEAD and
   origin/master, but never older than the commit that added this guard:
   what stood in the tree before the rule existed is not new) is refused,
   with one exception: a line the diff took away in the base held a
   citation into the same path whose numbers git's line map of the cited
   file, between base and working tree, carries to the new ones. That is
   the map the `LEVICULUM_CITATION_FIX` fixer of
   leviculum-std/tests/doc_citations.rs follows, so a citation it
   renumbered passes, and so does one whose paragraph moved while the cited
   file stayed as it was; a number changed by hand, or a new citation in
   the place of an old one, is refused. A citation into another repository
   or a pinned reference (`reference/...`, a bare `Transport.py` that names
   no file of this tree) is not refused. A bare name that one of this
   tree's files carries (`transport.rs` and a line) is refused like a full
   path: it may be ours, and the guard does not guess.

   A shallow clone does not hold the base: without --base or
   $LEVICULUM_CITATION_BASE the guard exits 1 there with a message
   instead of checking nothing.

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
    shallow = git(root, "rev-parse", "--is-shallow-repository", check=False).stdout.strip()
    if shallow == "true":
        # The only commit of a shallow clone is HEAD: counted against it,
        # nothing is ever new, and the guard would pass without looking.
        raise SystemExit("check-citations-by-name: this clone is shallow, so the base new "
                         "line citations are counted against is not in it. Name one with "
                         "--base or LEVICULUM_CITATION_BASE, or deepen the clone.")
    intro = guard_commit(root)
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
            path, spec = m.group(1), m.group(2)
            where = f"{rel}:{text.count(chr(10), 0, m.start()) + 1}: `{m.group(0)}`"
            if path not in tree.files:
                n += 1
                if any(path.startswith(p) for p in C.OTHER_TREES):
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
            if status == "missing-file":
                # Tracked, but not readable as UTF-8: no item was looked at,
                # so this citation is not checked, it is refused.
                out.append(f"{where}\n    cannot read {path}: no item of it can be checked.")
                continue
            n += 1
            if status == "needs-owner":
                desc = ", ".join(f"`{h.label()}`" for h in hits[:6])
                out.append(f"{where}\n    `{spec}` is a member; write it with its owner: {desc}.")
            elif status == "unknown":
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


def line_cites(tree, rel, text):
    """[(line in `text`, cited path, spans, match)] of the line citations of
    `text` that may point into this tree, fixture lines left out."""
    out = []
    for m in C.line_citations(text, rel):
        ls = text.rfind("\n", 0, m.start()) + 1
        le = text.find("\n", m.start())
        if FIXTURE_MARK in text[ls:le if le >= 0 else len(text)]:
            continue
        path = ours(tree, m, rel, text)
        if path is not None:
            out.append((text.count("\n", 0, m.start()) + 1, path, C.spans(m.group(2)), m))
    return out


HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", re.M)


def hunks(root, base, rel):
    """[(old_start, old_len, new_start, new_len)] of `git diff -U0 base -- rel`:
    git's line map of `rel` between base and working tree."""
    diff = git(root, "diff", "-U0", "--no-color", base, "--", rel).stdout
    return [(int(h.group(1)), int(h.group(2) or 1), int(h.group(3)), int(h.group(4) or 1))
            for h in HUNK.finditer(diff)]


def map_line(hs, line):
    """Where line `line` of the base stands in the working tree, or None when
    a hunk replaced it. The map of `map_line` in
    leviculum-std/tests/doc_citations.rs, which the fixer follows."""
    delta = 0
    for old_start, old_len, new_start, new_len in hs:
        old_end = old_start if old_len == 0 else old_start + old_len - 1
        if old_len > 0 and old_start <= line <= old_end:
            return None
        if old_end < line:
            delta += new_len - old_len
    return max(1, line + delta)


def mapped_files(tree, path):
    """The files whose line map can carry a citation into `path`: the file
    itself, or for a bare or shortened name (`transport.rs`) the files whose
    path ends in it, as the fixer resolves it (doc_citations.rs, `resolve`).
    Only an existing citation is excused this way; a new one into a bare
    name is still refused."""
    if path in tree.files:
        return [path]
    return sorted(f for f in tree.basenames.get(os.path.basename(path), [])
                  if f.endswith("/" + path))


def changed_lines(hs):
    """(lines of the base a hunk replaced or removed, lines of the working
    tree a hunk added or changed)."""
    old, new = set(), set()
    for old_start, old_len, new_start, new_len in hs:
        old.update(range(old_start, old_start + old_len))
        new.update(range(new_start, new_start + new_len))
    return old, new


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


def guard_commit(root):
    """The commit that added this guard, or None while it is uncommitted."""
    me = os.path.relpath(os.path.abspath(__file__), root)
    intro = git(root, "log", "--diff-filter=A", "--format=%H", "-1", "--", me, check=False)
    return intro.stdout.strip() or None


def merged_pre_rule(root, base):
    """The parents of the merges between `base` and HEAD that join a branch
    without this guard to one with it. Their trees are what the other branch
    held while the rule did not exist there, so they are bases as well: the
    same rule as the guard's own commit for the line it stands on."""
    intro = guard_commit(root)
    if intro is None:
        return []

    def has_rule(rev):
        return git(root, "merge-base", "--is-ancestor", intro, rev, check=False).returncode == 0

    out = []
    for m in git(root, "rev-list", "--merges", f"{base}..HEAD", check=False).stdout.split():
        parents = git(root, "rev-parse", f"{m}^@").stdout.split()
        ruled = [p for p in parents if has_rule(p)]
        if ruled:
            out += [p for p in parents if p not in ruled and p not in out]
    return out


class Base:
    """One revision new line citations are measured against: per citing
    file, the lines the diff to the working tree adds, and the citations it
    took away; per cited file, git's line map."""

    def __init__(self, tree, rev):
        self.tree, self.rev, self.root = tree, rev, tree.root
        self.changed = {f for f in git(self.root, "diff", "--name-only", "--no-renames",
                                       rev).stdout.split("\n") if f}
        self._maps, self._files = {}, {}

    def line_map(self, path):
        if path not in self._maps:
            self._maps[path] = hunks(self.root, self.rev, path)
        return self._maps[path]

    def file(self, rel):
        """(added lines or None for a file the base lacks, pool of the
        citations on the lines the diff took away)."""
        if rel not in self._files:
            old = git(self.root, "show", f"{self.rev}:{rel}", check=False)
            if old.returncode != 0:
                self._files[rel] = (None, [])
            elif rel not in self.changed:
                self._files[rel] = (set(), [])
            else:
                gone, added = changed_lines(hunks(self.root, self.rev, rel))
                pool = [(path, sp) for n, path, sp, _ in line_cites(self.tree, rel, old.stdout)
                        if n in gone]
                self._files[rel] = (added, pool)
        return self._files[rel]

    def excuses(self, rel, n, path, sp):
        """Whether this base excuses the citation `path` `sp` on line `n` of
        `rel`: the line stood here unchanged, or a citation the diff took
        away maps to it under the line map of the cited file (it is then
        used up)."""
        added, pool = self.file(rel)
        if added is not None and n not in added:
            return True
        for i, (opath, osp) in enumerate(pool):
            if opath != path or len(osp) != len(sp):
                continue
            if any(all(map_line(self.line_map(f), oa) == a and map_line(self.line_map(f), ob) == b
                       for (oa, ob), (a, b) in zip(osp, sp))
                   for f in mapped_files(self.tree, path)):
                pool.pop(i)
                return True
        return False


def check_new_lines(tree, base, also=()):
    """Refusals for the line citations on lines the working tree adds or
    changes over `base`.

    Each one is refused, with one exception: a line the diff took away in
    the base held a citation into the same path whose numbers git's line map
    of the cited file, between base and working tree, carries to the new
    ones. That is a citation the fixer renumbered, or one whose paragraph
    moved while the cited file stayed as it was. Each old citation excuses
    one new one. A number changed by hand, or a new citation in the place of
    an old one, maps to nothing and is refused.

    `also` are further bases (`merged_pre_rule`): a citation one of them
    holds on that line, or excuses by the same rule, is not new either."""
    root = tree.root
    bases = [Base(tree, base)] + [Base(tree, r) for r in also]
    changed = set(bases[0].changed)
    changed |= set(git(root, "ls-files", "--others", "--exclude-standard").stdout.split("\n"))
    out = []
    for rel in sorted(f for f in changed if f and f in tree.files and C.is_living(f)):
        text = tree.text(rel)
        if text is None:
            continue
        for n, path, sp, m in line_cites(tree, rel, text):
            if any(b.excuses(rel, n, path, sp) for b in bases):
                continue
            out.append(f"{rel}:{n}: `{m.group(0)}`\n    a new line citation: no citation the "
                       f"diff replaced maps to it under the line map of {path}. "
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
    also = merged_pre_rule(args.root, base)
    lines = check_new_lines(tree, base, also)
    for f in names + lines:
        print(f"REFUSED {f}")
    print(f"citations by name: {n_names} name citation(s) checked, {len(names)} refused; "
          f"new line citations against {base[:12]} ({why})"
          + (f" and {len(also)} merged branch(es) without the rule "
             f"({', '.join(r[:12] for r in also)})" if also else "")
          + f": {len(lines)} refused")
    if names or lines:
        print(f"See {RULE}.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
