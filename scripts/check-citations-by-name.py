#!/usr/bin/env python3
"""A new citation names its item; a touched line citation stays on its item.

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

2. The census is the judge of every citation move (order 1940). The
   guard computes the census for the touched citations at the base
   revision (--base, else $LEVICULUM_CITATION_BASE, else the merge base of
   HEAD and origin/master, but never older than the commit that added this
   guard nor than RULE_FLOOR) and in the working tree. There is no
   "unchanged since a merged parent" exemption and no line map to trust: a
   renumbered citation is right because the line it points at now holds
   the item its text names, not because a map carried it there.

   Touched means: the citing line is one the diff adds or changes, or the
   cited file changed. Each touched citation pairs with at most one base
   citation, by occurrence through the citing text's own diff (the
   reviewer's decision of 2026-11-16 21:05, which replaces the decision of
   13:55 and its pool of excuses):

   - on a line the diff left alone, the citation at the same place on the
     base line it stands on;
   - on a line the diff adds or changes, the citation it replaced, by
     position inside the hunk: the k-th citation of the hunk's new lines
     with the k-th of the base lines it replaced, line by line when the
     hunk writes as many lines as it replaces. A name citation holds its
     place in that count; a line citation where a name citation stood has
     no partner.

   Each base citation pairs once. The verdict per pair, with item(x) the
   item the census resolves at x's cited line (at head for the head
   citation, at the base for the base citation):

   - base `agree`: head must be `agree`;
   - base `drift` or `nolabel`: head passes only if it is `agree` or
     item(head) == item(base), the same item of the same file;
   - no partner: head must be `agree`.

   A cited line the census resolves to no item ("-", "past-end"), or a
   line of a markdown file (the census places it under the heading above
   it), is no item: two such lines are never the same item (audit of
   1959, order 1965). A head citation that is not `agree` and whose head
   or base partner cites such a line is refused, unless the text of its
   cited line at head is byte for byte the text of its partner's cited
   line at the base and that line is neither past the end nor blank.

   A bare name the tree cannot place (`foreign`, why `unplaced`) resolves
   no item; its own text is what it names, so one carried word for word
   keeps its partner and a renumbered one is refused.

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


def cited_path(rec):
    """The cited file of a census record: for an unplaceable bare name, the
    name as written."""
    if rec["verdict"] == "foreign":
        return rec["cite"].rsplit(":", 1)[0]
    return rec["at"].rsplit(":", 1)[0]


NO_ITEM = ("-", "past-end")


def identity(rec):
    """What a pair compares: the item the census resolves at a citation's
    cited line, with its file, or None when the census resolves no item
    there ("-", "past-end": scripts/citation-census.py, `judge`) or only
    the heading of a markdown file. A bare
    name the census cannot place in this tree (`foreign`, why `unplaced`)
    resolves no item either; the only thing it names is its own text, so
    that is its identity, and a renumbered one is another citation."""
    if rec["verdict"] == "foreign":
        return ("foreign", rec["cite"])
    if rec["item"] in NO_ITEM:
        return None
    if cited_path(rec).endswith(".md"):
        # The census places a markdown line under the heading above it, a
        # section of prose of any length, so every line of it would be
        # "the same item"; order 1965 refuses a label-less citation moved
        # onto another line of a markdown file, so a markdown line is no
        # item here either.
        return None
    return (cited_path(rec), rec["item"])


def passes(head, base, head_text=None, base_text=None):
    """The verdict of a pair (the reviewer's decision of 2026-11-16 21:05).
    `base` is the head citation's partner, or None when it has none.

    A cited line that holds no item is no item: two such lines are not the
    same item (audit of 1959, order 1965). Then the one way to stay is the
    text: `head_text`, the head citation's cited line at head, equals
    `base_text`, its partner's cited line at the base, byte for byte; None
    stands for a line past the end. A blank line has no text to recognise
    it by, and the order's control of a citation moved from one blank line
    to another must be refused, so a blank line never stays this way (a
    narrowing of order 1965's one exception, not another exception)."""
    if head["verdict"] == "agree":
        return True
    if base is None or base["verdict"] == "agree":
        return False
    mine, theirs = identity(head), identity(base)
    if mine is None or theirs is None:
        return (head_text is not None and head_text == base_text
                and head_text.strip() != "")
    return mine == theirs


def cited_line(t, rec):
    """The text of the first line `rec` cites in tree `t`, or None when it
    lies past the end of the file (or the file cannot be read)."""
    if rec["verdict"] == "foreign":
        return None
    path, spec = rec["at"].rsplit(":", 1)
    first = C.spans(spec)[0][0]
    text = t.text(path)
    if text is None:
        return None
    lines = text.split("\n")
    return lines[first - 1] if 0 < first <= len(lines) else None


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
    partners, lines_cache = {}, {}

    def occurrence(recs_on_line, r):
        """(line, text, k): `r` is the k-th citation written `r["cite"]` on
        its line. What a pair is looked up by."""
        same = [h for h in recs_on_line if h["cite"] == r["cite"]]
        return (r["line"], r["cite"], next(i for i, h in enumerate(same) if h is r))

    def slots(t, rel):
        """{line: [(kind, text)]} of the citing file `rel` in tree `t`: its
        line citations ("L") and name citations ("N") in the order they are
        written. A name citation holds its place, so a line citation that a
        change turned into a name moves no other citation's position."""
        text = t.text(rel) or ""
        ms = [(m.start(), "L", m.group(0)) for m in C.line_citations(text, rel)]
        ms += [(m.start(), "N", m.group(0)) for m in C.name_citations(text, rel)]
        out = {}
        for start, kind, cite in sorted(ms):
            out.setdefault(text.count("\n", 0, start) + 1, []).append((kind, cite))
        return out

    def record(by_line, line, slot_list, k):
        """The census record of the k-th slot of `line`, or None for a name
        citation or a line citation the census does not hold."""
        kind, cite = slot_list[k]
        if kind != "L":
            return None
        nth = sum(1 for kd, c in slot_list[:k] if kd == "L" and c == cite)
        same = [r for r in by_line.get(line, []) if r["cite"] == cite]
        return same[nth] if nth < len(same) else None

    def pair(rel):
        """{occurrence: base partner} for the citations of the citing file
        `rel` at head that have one. Pairing runs through `rel`'s own diff
        against the base. Inside a hunk, the k-th citation of the hunk's new
        lines pairs with the k-th citation of the base lines it replaced (a
        hunk that writes as many lines as it replaces does so line by line);
        a name citation counts as a citation there, and a line citation
        that stands where a name citation stood has no partner. On a line no
        hunk touches, the partner is the citation of the same text at the
        same place on the base line it stands on. Each base citation pairs
        once. A file the base does not hold has no partners."""
        if rel in partners:
            return partners[rel]
        out = {}
        partners[rel] = out
        if git(root, "cat-file", "-e", f"{base}:{rel}", check=False).returncode != 0:
            return out
        head_by_line, by_line = {}, {}
        for r in CEN.census(tree, {rel}):
            head_by_line.setdefault(r["line"], []).append(r)
        for b in CEN.census(base_tree, {rel}):
            by_line.setdefault(b["line"], []).append(b)
        head_slots, base_slots = slots(tree, rel), slots(base_tree, rel)
        hs = hunks(root, base, rel) if rel in changed else []
        in_hunk = set()
        for old_start, old_len, new_start, new_len in hs:
            if old_len == new_len:
                spans = [([old_start + i], [new_start + i]) for i in range(new_len)]
            else:
                spans = [(range(old_start, old_start + old_len),
                          range(new_start, new_start + new_len))]
            for old_ns, new_ns in spans:
                olds = [(n, k) for n in old_ns for k in range(len(base_slots.get(n, [])))]
                news = [(n, k) for n in new_ns for k in range(len(head_slots.get(n, [])))]
                for (hn, hk), (bn, bk) in zip(news, olds):
                    r = record(head_by_line, hn, head_slots[hn], hk)
                    b = record(by_line, bn, base_slots[bn], bk)
                    if r is not None and b is not None:
                        out[occurrence(head_by_line[hn], r)] = b
            in_hunk.update(range(new_start, new_start + new_len))
        for n, rs in head_by_line.items():
            if n in in_hunk:
                continue
            theirs = by_line.get(base_line(hs, n), [])
            for r in rs:
                key = occurrence(rs, r)
                cands = [b for b in theirs if b["cite"] == r["cite"]]
                if key[2] < len(cands):
                    out[key] = cands[key[2]]
        return out

    def partner(r):
        """`r`'s base partner, or None."""
        return pair(r["file"]).get(occurrence(recs_by_line[(r["file"], r["line"])], r))

    def line_text(rel, n):
        if rel not in lines_cache:
            lines_cache[rel] = (tree.text(rel) or "").split("\n")
        ls = lines_cache[rel]
        return ls[n - 1] if 0 < n <= len(ls) else ""

    def touched_lines(rel):
        """The lines of `rel` its diff adds or changes; None when the base
        does not hold the file (every line is new)."""
        if git(root, "cat-file", "-e", f"{base}:{rel}", check=False).returncode != 0:
            return None
        if rel not in changed:
            return set()
        return changed_lines(hunks(root, base, rel))[1]

    recs_by_line = {}
    for r in recs:
        recs_by_line.setdefault((r["file"], r["line"]), []).append(r)
    touched_cache = {}
    out = []
    for r in recs:
        if r["verdict"] == "agree":
            continue
        rel, n = r["file"], r["line"]
        if rel not in touched_cache:
            touched_cache[rel] = touched_lines(rel)
        added = touched_cache[rel]
        new_line = added is None or n in added
        if r["verdict"] == "foreign" and r["why"] != "unplaced":
            # Another repository or a pinned reference: not checked.
            continue
        path = cited_path(r)
        if not new_line and path not in changed:
            # Neither the citing line nor the cited file moved: the pair
            # is the same citation over the same lines.
            continue
        if FIXTURE_MARK in line_text(rel, n):
            continue
        b = partner(r)
        if passes(r, b, cited_line(tree, r), b and cited_line(base_tree, b)):
            continue
        holder = f"; the line belongs to `{r['item']}`" if r.get("item", "-") != "-" else ""
        if b is None:
            why = "it pairs with no base citation, so it must agree with its label"
        elif b["verdict"] == "agree":
            why = (f"it stands in for `{b['cite']}`, which agreed, so it must agree "
                   "with its label")
        elif identity(r) is None or identity(b) is None:
            why = (f"it stands in for `{b['cite']}` ({b['verdict']}, at "
                   f"`{b['item']}`); a line outside any item is no item, so a citation "
                   "that does not agree stays only on a non-blank line of the text its "
                   "base cited")
        else:
            why = (f"it stands in for `{b['cite']}` ({b['verdict']}, at "
                   f"`{b['item']}`), and a citation that does not agree must stay on "
                   "the item its base resolved")
        if new_line:
            what = "a new line citation"
        else:
            what = f"a citation whose file {path} changed under it"
        out.append(f"{rel}:{n}: `{r['cite']}`\n    {what} (verdict {r['verdict']}{holder}): "
                   f"{why}. Cite the item by name: "
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
