#!/usr/bin/env python3
"""Rewrite `file:line` citations in living texts into `path#item` citations.

For each line citation into this tree (not into the pinned references, not
into an external repository) it finds the item that encloses the cited
lines today (scripts/citations.py, `enclosing`) and writes the citation as
`<full path>#<Item>`, with the shortest item spelling that names that item
alone. The tree's citation guard keeps today's numbers right, so today is
the moment the numbers still say what they meant.

A citation is left for a hand decision, and listed, when

- the cited lines lie in no item (a module doc comment, a `use` block, a
  file type without items), or span several items;
- a bare file name resolves to several files and neither another citation
  on the same page nor the backticked name in front of it picks one;
- one citation names several spans that land in different items.

The hand decision is an anchor (`// ANCHOR: name` ... `// ANCHOR_END:
name` around the lines, cited `path#anchor:name`), or citing the named
constant or function the lines belong to, or extracting one.

Exit 0 when nothing is left for a hand decision; 1 when something is (the
list is on stdout); 2 on a usage error.

Usage:
  python3 scripts/convert-line-citations.py [--write] FILE...
  python3 scripts/convert-line-citations.py --all [--write] [--only-into PATH]
"""

import argparse
import importlib.util
import os
import re
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("citations", os.path.join(_HERE, "citations.py"))
C = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(C)

ATTACHED = re.compile(r"`([A-Za-z0-9_:.]+)(?:\(\))?`\s*[(,]\s*`?$")


def pick_file(tree, cited, text, before, rel=""):
    """The one file a possibly-bare `cited` means, or (None, reason)."""
    cands = tree.resolve(cited)
    if len(cands) == 1:
        return cands[0], None
    if not cands:
        return None, "names no file"
    # a bare name in a crate's own source means that crate's file
    crate = rel.split("/", 1)[0]
    if not rel.startswith("docs/"):
        same = [c for c in cands if c.split("/", 1)[0] == crate]
        if len(same) == 1:
            return same[0], None
    # another citation on the page spells the path out
    spelled = [c for c in cands if c in text]
    if len(spelled) == 1:
        return spelled[0], None
    # the page's own subject: the one candidate whose crate the page names
    # in full paths at least three times as often as any other's
    named = {c: len(re.findall(r"(?<![\w/.-])" + re.escape(c.split("/", 1)[0]) + r"/", text))
             for c in cands}
    ranked = sorted(named.items(), key=lambda kv: -kv[1])
    if ranked[0][1] >= 3 and ranked[0][1] >= 3 * ranked[1][1] and \
            sum(1 for c in cands if c.split("/", 1)[0] == ranked[0][0].split("/", 1)[0]) == 1:
        return ranked[0][0], None
    m = ATTACHED.search(before[-200:])
    if m:
        ident = m.group(1).split("::")[-1].split(".")[-1]
        holding = []
        for c in cands:
            status, _ = C.resolve(c, ident, tree.root)
            if status in ("ok", "ambiguous"):
                holding.append(c)
        if len(holding) == 1:
            return holding[0], None
    return None, "`%s` is %d files: %s" % (cited, len(cands), ", ".join(cands))


def attached_item(path, before, cited, root):
    """The item the backticked name in front of the citation names, when it
    names exactly one item of `path` and that item stands within eight
    lines of the cited span: the author's own word for what they cited."""
    m = ATTACHED.search(before[-200:])
    if not m:
        return None
    ident = m.group(1).rstrip(".")
    for spec in (ident, ident.split("::")[-1].split(".")[-1]):
        status, hits = C.resolve(path, spec, root)
        if status != "ok":
            continue
        it = hits[0]
        if all(it.start - 8 <= a and b <= it.end + 8 for a, b in cited) or \
                any(a - 8 <= it.head <= b + 8 for a, b in cited):
            return C.shortest_name(it, path, root)
    return None


def convert_text(rel, text, tree, only_into=None):
    """(new text, converted count, [hand decisions]); with `only_into`, only
    the citations into that one file are touched."""
    edits, hand = [], []
    place = C.where_in_code(rel, text)
    for m in C.line_citations(text, tree, rel):
        cited, spec = m.group(1), m.group(2)
        line_no = text.count("\n", 0, m.start()) + 1
        where = f"{rel}:{line_no}: {m.group(0)}"
        path, why = pick_file(tree, cited, text, text[: m.start()], rel)
        if only_into and path != only_into:
            continue
        if place(m.start()) != "comment":
            hand.append(f"{where}  -- in a {place(m.start())}, not a comment: by hand")
            continue
        if path is None:
            hand.append(f"{where}  -- {why}")
            continue
        names = set()
        reason = None
        named = attached_item(path, text[: m.start()], C.spans(spec), tree.root)
        if named:
            edits.append((m.start(), m.end(), f"{path}#{named}"))
            continue
        for a, b in C.spans(spec):
            status, it = C.enclosing(path, a, b, tree.root)
            if status != "ok":
                reason = it
                break
            name = C.shortest_name(it, path, tree.root)
            if name is None:
                reason = f"`{it.label()}` has no unique name in {path}"
                break
            names.add(name)
        if reason is None and len(names) != 1:
            reason = "the spans land in " + ", ".join(sorted(names))
        if reason:
            hand.append(f"{where}  -- {reason}")
            continue
        edits.append((m.start(), m.end(), f"{path}#{names.pop()}"))
    out = text
    for a, b, new in sorted(edits, reverse=True):
        out = out[:a] + new + out[b:]
    return out, len(edits), hand


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="*")
    ap.add_argument("--all", action="store_true", help="every living text")
    ap.add_argument("--write", action="store_true", help="rewrite the files in place")
    ap.add_argument("--root", default=C.REPO_ROOT)
    ap.add_argument("--only-into", metavar="PATH",
                    help="convert only the citations into this file (e.g. Justfile), in every "
                         "file given; the rest stay for a later pass")
    args = ap.parse_args(argv)
    if not args.files and not args.all:
        ap.print_usage(sys.stderr)
        return 2
    tree = C.Tree(args.root)
    exempt = set()
    try:
        with open(os.path.join(args.root, "scripts/line-citations-exempt.txt"), encoding="utf-8") as fh:
            exempt = {l.split("#", 1)[0].strip() for l in fh} - {""}
    except OSError:
        pass
    files = [f for f in C.living_texts(args.root) if f not in exempt] if args.all else [os.path.relpath(os.path.abspath(f), args.root) for f in args.files]
    total, hands = 0, []
    for rel in files:
        full = os.path.join(args.root, rel)
        try:
            with open(full, encoding="utf-8") as fh:
                text = fh.read()
        except (OSError, UnicodeDecodeError):
            continue
        new, n, hand = convert_text(rel, text, tree, args.only_into)
        total += n
        hands += hand
        if args.write and new != text:
            with open(full, "w", encoding="utf-8") as fh:
                fh.write(new)
    print(f"converted {total}, left for a hand decision {len(hands)}")
    for h in hands:
        print("  " + h)
    return 1 if hands else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
