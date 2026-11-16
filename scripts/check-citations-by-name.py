#!/usr/bin/env python3
"""A new citation names its item; a touched line citation agrees with it.

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

2. The census is the judge of every citation move (order 1940): a change
   may not turn a line citation's verdict (scripts/citation-census.py,
   `judge`) from `agree` to `drift` or `nolabel`, and a citation it adds
   or moves must be `agree` unless the pairing below excuses it. The guard computes the census for the touched
   citations at the base revision (--base, else $LEVICULUM_CITATION_BASE,
   else the merge base of HEAD and origin/master, but never older than the
   commit that added this guard nor than RULE_FLOOR) and in the working tree. There is no
   "unchanged since a merged parent" exemption and no line map to trust: a
   renumbered citation is right because the line it points at now holds
   the item its text names, not because a map carried it there.

   Touched means: the citing line is one the diff adds or changes, or the
   cited file changed. A touched citation that is not `agree` is paired
   with the base so that no citation stands in for another (order 1955):

   - Its citing line untouched, its cited file changed: it answers to its
     own base self (the citation at the same place of the base line git's
     diff maps it to) and passes when that verdict is no better in the
     order agree, drift, nolabel; a drifting citation rides through a
     merge, and a file that moved under an agreeing citation refuses until
     the citation is renumbered onto its item.
   - Its citing line added or changed: excused only by a base citation
     the change replaced or moved, one on a line the diff removed or
     changed, of the same cited file and label (`_key`), with a verdict no
     better (a base drift never excuses a head nolabel) and written the
     same, each excusing one. A base citation that still stands untouched
     keeps its own entry and excuses nothing else: an agreeing citation
     moved onto another item is refused even beside an untouched drifting
     sibling of its label, and a new drifting or label-less citation
     beside an untouched one of its key is refused. "Written the same"
     makes a renumbered drift a move that must agree (the reviewer's
     decision of 2026-11-16 13:55). A label-less citation (`nolabel`)
     makes no claim to judge and may be renumbered in place of a replaced
     label-less one of its file; a bare name the tree cannot place
     (`foreign`, `unplaced`) in place of a replaced one of the same name
     and label. A label the base never cited this file under is new and
     must agree.

   Citations into other repositories and pinned references
   (`reference/...`, a bare `Transport.py` that names no file of this
   tree) are not checked.

   A shallow clone does not hold the base: without --base or
   $LEVICULUM_CITATION_BASE the guard exits 1 there with a message
   instead of checking nothing.

   A line that must hold a citation-shaped string as data (a fixture of a
   citation checker) carries the marker `citation-guard: line-fixture`.

Exit 0 when nothing is refused, 1 otherwise. scripts/citations-selftest.py
holds the positive controls.
"""

import argparse
import importlib.util
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import citations as C  # noqa: E402

_CSPEC = importlib.util.spec_from_file_location(
    "citation_census",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "citation-census.py"),
)
CEN = importlib.util.module_from_spec(_CSPEC)
_CSPEC.loader.exec_module(CEN)

RULE = "docs/src/concepts/checks-and-citations.md, \"A new citation names its item\""
# The oldest commit the default base reaches back to under the occurrence
# rule (the reviewer's decision of 2026-11-16 21:05): ac86baa29, the base of
# the range that decision was ordered for (order 1959). The landings between
# the guard's own commit and this one were written under the rule of 13:55;
# order 1959 counts what the new rule finds in them and forbids fixing the
# tree for that count, so they are measured, not refused. A clone without
# the commit (another repository, a scratch test) falls back to the commit
# that added this guard.
RULE_FLOOR = "ac86baa296a552909324bf63db1bcc877c6dfda6"
FIXTURE_MARK = "citation-guard: line-fixture"


def git(root, *args, check=True):
    r = subprocess.run(["git", "-C", root, *args], capture_output=True, text=True)
    if check and r.returncode != 0:
        raise SystemExit(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r


def default_base(root):
    """(revision, how it was chosen) that touched line citations are judged
    against."""
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
    floor, how = intro, "the commit that added this guard"
    if (git(root, "cat-file", "-e", f"{RULE_FLOOR}^{{commit}}", check=False).returncode == 0
            and git(root, "merge-base", "--is-ancestor", intro, RULE_FLOOR,
                    check=False).returncode == 0
            and git(root, "merge-base", "--is-ancestor", RULE_FLOOR, "HEAD",
                    check=False).returncode == 0):
        floor, how = RULE_FLOOR, "the floor of the occurrence rule (RULE_FLOOR)"
    mb = git(root, "merge-base", "HEAD", "origin/master", check=False).stdout.strip()
    if mb and git(root, "merge-base", "--is-ancestor", floor, mb, check=False).returncode == 0:
        return mb, "merge base with origin/master"
    return floor, how


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


HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", re.M)


def hunks(root, base, rel):
    """[(old_start, old_len, new_start, new_len)] of `git diff -U0 base -- rel`."""
    diff = git(root, "diff", "-U0", "--no-color", base, "--", rel).stdout
    return [(int(h.group(1)), int(h.group(2) or 1), int(h.group(3)), int(h.group(4) or 1))
            for h in HUNK.finditer(diff)]


def changed_lines(hs):
    """(lines of the base a hunk replaced or removed, lines of the working
    tree a hunk added or changed)."""
    old, new = set(), set()
    for old_start, old_len, new_start, new_len in hs:
        old.update(range(old_start, old_start + old_len))
        new.update(range(new_start, new_start + new_len))
    return old, new


def suggestion(tree, path, label, item="-"):
    """The name citation a refusal offers: the label where it resolves,
    else the item standing at the cited line."""
    if label and path in tree.files:
        status, hits = C.resolve_in(tree, path, label)
        if status == "ok":
            it = hits[0]
            return f"`{path}#{it.label()}` (its lines are {it.start}-{it.end})"
    if item not in ("-", "past-end") and path in tree.files:
        return f"`{path}#{item}` if that is the item meant"
    return "`<path>#<Item>`"


def guard_commit(root):
    """The commit that added this guard, or None while it is uncommitted."""
    me = os.path.relpath(os.path.abspath(__file__), root)
    intro = git(root, "log", "--diff-filter=A", "--format=%H", "-1", "--", me, check=False)
    return intro.stdout.strip() or None


def _key(rec):
    """What pairs a citation of the working tree with one of the base: the
    cited file (for an unplaceable bare name, the name as written) and the
    label the text gives it."""
    if rec["verdict"] == "foreign":
        return (rec["cite"].rsplit(":", 1)[0], rec["label"])
    return (rec["at"].rsplit(":", 1)[0], rec["label"])


# Better first. A base verdict excuses a touched one only when it is no
# better: a base drift never excuses a head nolabel. `foreign` (an
# unplaceable bare name) is excused by `foreign` alone.
RANK = {"agree": 0, "drift": 1, "nolabel": 2, "foreign": 3}


def base_line(hs, n):
    """The base line of working-tree line `n`, which no hunk of `hs` (the
    `-U0` hunks of its file) added or changed."""
    shift = 0
    for old_start, old_len, new_start, new_len in hs:
        if new_start + max(new_len, 1) - 1 < n:
            shift += old_len - new_len
    return n + shift


def check_new_lines(tree, base):
    """Refusals for the touched line citations, by the census (the module
    docstring, refusal 2, has the full rule)."""
    root = tree.root
    changed = {f for f in git(root, "diff", "--name-only", "--no-renames",
                              base).stdout.split("\n") if f}
    changed |= {f for f in git(root, "ls-files", "--others",
                               "--exclude-standard").stdout.split("\n") if f}
    living = {f for f in tree.files if C.is_living(f)}
    recs = CEN.census(tree, living & changed)
    recs += CEN.census(tree, living - changed, into=changed)
    base_tree = C.Tree(root, base)
    sides, lines_cache, head_at = {}, {}, {}
    for r in recs:
        head_at.setdefault((r["file"], r["line"]), []).append(r)

    def side(rel):
        """(hunks, lines the diff adds or changes, the base's citations of
        `rel` by line, the excuse pool) of a citing file; the hunks and the
        added lines are None for a file the base does not hold (every line
        is new). The pool holds only the base citations on lines the diff
        removed or changed, by `_key`: a base citation that still stands
        untouched keeps its own entry and excuses nothing else."""
        if rel not in sides:
            if git(root, "cat-file", "-e", f"{base}:{rel}", check=False).returncode != 0:
                sides[rel] = (None, None, {}, {})
            else:
                hs = hunks(root, base, rel) if rel in changed else []
                old, new = changed_lines(hs)
                by_line, pool = {}, {}
                for b in CEN.census(base_tree, {rel}):
                    by_line.setdefault(b["line"], []).append(b)
                    if b["line"] in old:
                        pool.setdefault(_key(b), []).append(b)
                sides[rel] = (hs, new, by_line, pool)
        return sides[rel]

    def own_base(r):
        """The base citation an untouched citing line held at the same
        place: the same key, in the same order on its line."""
        hs, _, by_line, _ = side(r["file"])
        if hs is None:
            return None
        mine = [h for h in head_at[(r["file"], r["line"])] if _key(h) == _key(r)]
        theirs = [b for b in by_line.get(base_line(hs, r["line"]), []) if _key(b) == _key(r)]
        k = next(i for i, h in enumerate(mine) if h is r)
        return theirs[k] if k < len(theirs) else None

    def line_text(rel, n):
        if rel not in lines_cache:
            lines_cache[rel] = (tree.text(rel) or "").split("\n")
        ls = lines_cache[rel]
        return ls[n - 1] if 0 < n <= len(ls) else ""

    out = []
    for r in recs:
        if r["verdict"] == "agree":
            continue
        rel, n = r["file"], r["line"]
        added = side(rel)[1]
        new_line = rel in changed and (added is None or n in added)
        if r["verdict"] == "foreign":
            if r["why"] != "unplaced" or not new_line:
                continue
            path = r["cite"].rsplit(":", 1)[0]
        else:
            path = r["at"].rsplit(":", 1)[0]
            if not new_line and path not in changed:
                continue
        if FIXTURE_MARK in line_text(rel, n):
            continue
        rank = RANK[r["verdict"]]
        if not new_line:
            # The citing line stands; its cited file changed under it. It
            # answers to its own base self and to nothing else.
            mine = own_base(r)
            if mine is not None and RANK[mine["verdict"]] >= rank:
                continue
            was = mine["verdict"] if mine is not None else "no citation here"
            out.append(f"{rel}:{n}: `{r['cite']}`\n    fell from {was} to {r['verdict']}: "
                       f"{path} changed under it. Renumber it onto the lines of "
                       f"`{r['label']}`, or cite it by name: "
                       f"{suggestion(tree, path, r['label'])}.")
            continue
        # A citation on a line the diff adds or changes: excused only by a
        # base citation the change replaced or moved, no better, and
        # written the same: a renumbered drift is a move and must agree. A
        # citation with no label (`nolabel`) or an unplaceable bare name
        # (`foreign`) makes no claim the census can check, so it may be
        # renumbered in place of a replaced one of its kind.
        cands = side(rel)[3].get(_key(r), [])
        k = next((i for i, b in enumerate(cands)
                  if RANK[b["verdict"]] >= rank
                  and (b["cite"] == r["cite"] or r["verdict"] in ("nolabel", "foreign"))),
                 None)
        if k is not None:
            cands.pop(k)
            continue
        holder = f"; the line belongs to `{r['item']}`" if r.get("item", "-") != "-" else ""
        out.append(f"{rel}:{n}: `{r['cite']}`\n    a new line citation (verdict "
                   f"{r['verdict']}{holder}): a citation this change adds or moves must "
                   "agree with its label. Cite the item by name: "
                   f"{suggestion(tree, path, r['label'], r['item'])}.")
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=C.REPO_ROOT)
    ap.add_argument("--base", help="revision touched line citations are judged against")
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
          f"touched line citations against {base[:12]} ({why}): {len(lines)} refused")
    if names or lines:
        print(f"See {RULE}.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
