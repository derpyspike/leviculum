#!/usr/bin/env python3
"""Living texts cite code by name, and every name resolves.

The rule is docs/src/concepts/checks-and-citations.md, "Citations name
items": a page under docs/src, a code comment, a README, a comment in the
Justfile or a script cites `<path>#<Item>` (or `<path>#anchor:<name>`), never
`<path>:<line>`. A line number moves whenever a line is added above it;
keeping it right took 15 of 192 commits in November 2026 and every merge of
two lanes collided in the pages that carry them (order 1848).

Checks, over every living text (scripts/citations.py, `living_texts`):

1. Every `path#item` resolves to exactly one item (scripts/citations.py,
   `resolve`): the file exists, the item exists, and the name is not shared.
   A function that moved inside its file, or lines added above it, change
   nothing here; a renamed or deleted one fails with the names the file has.
2. A `file:line` citation into this tree is a failure, with the name the
   converter would write for it. Exempt: citations into the pinned
   references under reference/ (their lines do not move while the pin
   stands), into repositories outside this one, and inside fenced code
   blocks of a page (example output).
3. Two lists hold what is not converted yet and what is data, not text:
   scripts/line-citations-pending.txt (files that still carry line
   citations; only ever shrinks: a listed file without one fails, so the
   pass that converts a page takes it off) and
   scripts/line-citations-exempt.txt (files whose `file:line` strings are
   test data or fixtures, each with its reason).

Exit 0 = all hold. Exit 1 = a failure (each with file:line and the fix).

Usage:
  python3 scripts/check-name-citations.py [--root DIR]
  python3 scripts/check-name-citations.py --selftest
"""

import argparse
import importlib.util
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("citations", os.path.join(_HERE, "citations.py"))
C = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(C)

PENDING = "scripts/line-citations-pending.txt"
EXEMPT = "scripts/line-citations-exempt.txt"


def read_list(root, rel):
    """{path: line number} of a list file; `#` starts a comment."""
    out = {}
    try:
        with open(os.path.join(root, rel), encoding="utf-8") as fh:
            for n, line in enumerate(fh, 1):
                body = line.split("#", 1)[0].strip()
                if body:
                    out[body] = n
    except OSError:
        pass
    return out


def suggestion(tree, root, m):
    """What the converter would write for line citation `m`, or a hint."""
    paths = tree.resolve(m.group(1))
    if len(paths) != 1:
        return "write the full path and the item: `<path>#<Item>`"
    a, b = C.spans(m.group(2))[0]
    status, it = C.enclosing(paths[0], a, b, root)
    if status == "ok":
        name = C.shortest_name(it, paths[0], root)
        if name:
            return f"write `{paths[0]}#{name}` (scripts/convert-line-citations.py does it)"
    return ("the lines lie in no single item: cite the item they belong to, or put "
            "`ANCHOR: name` ... `ANCHOR_END: name` comments around them and cite "
            f"`{paths[0]}#anchor:name`")


def findings(root):
    tree = C.Tree(root)
    pending = read_list(root, PENDING)
    exempt = read_list(root, EXEMPT)
    out = []
    names = 0
    carrying = set()
    for rel in C.living_texts(root):
        if rel in (PENDING, EXEMPT) or rel in exempt:
            continue
        try:
            with open(os.path.join(root, rel), encoding="utf-8") as fh:
                text = fh.read()
        except (OSError, UnicodeDecodeError):
            continue
        for m in C.name_citations(text):
            names += 1
            path, spec = m.group(1), m.group(2)
            status, hits = C.resolve(path, spec, root)
            if status == "ok":
                continue
            line = text.count("\n", 0, m.start()) + 1
            where = f"{rel}:{line}: {m.group(0)}"
            if status == "missing-file":
                why = f"no file `{path}` (the path is from the repository root)"
            elif status == "unknown":
                why = f"`{path}` has no item `{spec}`: renamed or deleted?"
            else:
                why = (f"`{spec}` names {len(hits)} items of `{path}` "
                       f"({', '.join(f'{h.label()} at {h.start}' for h in hits[:4])}); "
                       "qualify it (`Type::name`, `<Type as Trait>::name`, `field:Type::name`, "
                       "`name/N` for a Java overload) or use an anchor")
            out.append(f"{where}\n    {why}")
        lines = C.line_citations(text, tree, rel)
        if lines:
            carrying.add(rel)
        if rel in pending:
            continue
        for m in lines:
            line = text.count("\n", 0, m.start()) + 1
            out.append(
                f"{rel}:{line}: {m.group(0)}\n    a living text cites by name, not by line "
                f"number: {suggestion(tree, root, m)}"
            )
    for rel, n in sorted(pending.items()):
        if rel not in carrying:
            out.append(f"{PENDING}:{n}: {rel}\n    carries no line citation any more: "
                       "take it off the list, so it stays that way")
    for rel, n in sorted(exempt.items()):
        if not os.path.isfile(os.path.join(root, rel)):
            out.append(f"{EXEMPT}:{n}: {rel}\n    no such file: take it off the list")
    return out, names, len(pending)


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", default=C.REPO_ROOT)
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args(argv)
    if args.selftest:
        spec = importlib.util.spec_from_file_location(
            "citations_selftest", os.path.join(_HERE, "citations-selftest.py"))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod.main()
    out, names, pending = findings(os.path.abspath(args.root))
    print(f"name citations: {names} checked; {pending} file(s) still pending conversion "
          f"({PENDING})")
    for f in out:
        print(f)
    if out:
        print(f"{len(out)} failure(s)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
