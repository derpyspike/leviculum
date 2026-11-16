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
   or moves must be `agree`. The guard computes the census for the touched
   citations at the base revision (--base, else $LEVICULUM_CITATION_BASE,
   else the merge base of HEAD and origin/master, but never older than the
   commit that added this guard) and in the working tree. There is no
   "unchanged since a merged parent" exemption and no line map to trust: a
   renumbered citation is right because the line it points at now holds
   the item its text names, not because a map carried it there.

   Touched means: the citing line is one the diff adds or changes, or the
   cited file changed. A touched citation whose verdict is not `agree` is
   excused only by a citation of the same cited file and label the base
   held with a verdict no better (each one excuses one): a drifting
   citation may ride through a merge, a paragraph move or the fixer's
   renumbering, but a citation whose label agreed in the base must agree
   now, a label the base never cited this file under is new and must
   agree, and a file that moved under an agreeing citation refuses until
   the citation is renumbered onto its item. A bare name the tree cannot
   place (`transport.rs` and a line: census verdict `foreign`,
   `unplaced`) has no judgeable item; it is held to a citation of the
   same name and label having stood in the base, and refused as new
   otherwise. Citations into other repositories and pinned
   references (`reference/...`, a bare `Transport.py` that names no file
   of this tree) are not checked.

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
    label the text gives it. The numbers are not part of the pair: the
    fixer renumbers without touching labels, so a renumbered citation
    meets its base self here, and passes exactly when its verdict did not
    get worse."""
    if rec["verdict"] == "foreign":
        return (rec["cite"].rsplit(":", 1)[0], rec["label"])
    return (rec["at"].rsplit(":", 1)[0], rec["label"])


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
    added_cache, pools, lines_cache = {}, {}, {}

    def added(rel):
        """Lines of `rel` the diff adds or changes; None for a file the
        base does not hold (every line is new)."""
        if rel not in added_cache:
            if git(root, "cat-file", "-e", f"{base}:{rel}", check=False).returncode != 0:
                added_cache[rel] = None
            else:
                added_cache[rel] = changed_lines(hunks(root, base, rel))[1]
        return added_cache[rel]

    def pool(rel):
        """The base's citations of `rel` by `_key`: each verdict excuses one
        touched citation that did not get worse."""
        if rel not in pools:
            p = {}
            for r in CEN.census(base_tree, {rel}):
                p.setdefault(_key(r), []).append(r["verdict"])
            pools[rel] = p
        return pools[rel]

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
        new_line = rel in changed and (added(rel) is None or n in added(rel))
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
        verdicts = pool(rel).get(_key(r), [])
        k = next((i for i, v in enumerate(verdicts) if v != "agree"), None)
        if k is not None:
            verdicts.pop(k)
            continue
        where = f"{rel}:{n}: `{r['cite']}`"
        if new_line:
            holder = f"; the line belongs to `{r['item']}`" if r.get("item", "-") != "-" else ""
            out.append(f"{where}\n    a new line citation (verdict {r['verdict']}{holder}): a "
                       "citation this change adds or moves must agree with its label. Cite "
                       f"the item by name: {suggestion(tree, path, r['label'], r['item'])}.")
        else:
            out.append(f"{where}\n    fell from agree to {r['verdict']}: {path} changed under "
                       f"it. Renumber it onto the lines of `{r['label']}`, or cite it by "
                       f"name: {suggestion(tree, path, r['label'])}.")
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
