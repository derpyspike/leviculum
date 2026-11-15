#!/usr/bin/env python3
"""The built book links each name citation to the lines its item stands on.

Reads a rendered book (`mdbook build docs -d DIR`, from the repository
root), collects every link the citations preprocessor wrote
(scripts/mdbook-citations.py), and checks a sample of them against the
tree, independently of the resolver that wrote them: the linked range must
lie inside the file, and the item's name (its last segment, or the anchor's
`ANCHOR:` line) must stand on a line of the linked range. It also fails when
a page holds a name citation in a code span that carries no link at all,
which is what a preprocessor that did not run looks like (mdbook only warns
when a preprocessor command fails).

Exit 0 = every sampled link lands; 1 = one does not, or no link was found.

Usage:
  python3 scripts/check-book-citation-links.py --book DIR [--sample N] [--seed S]
"""

import argparse
import glob
import html
import os
import random
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LINK = re.compile(r'<a href="[^"]*/src/(?:commit/[0-9a-f]+|branch/master)/([^"#]+)#L(\d+)(?:-L(\d+))?">'
                  r"<code>([^<]+)</code></a>")
UNLINKED = re.compile(r"(?<!\">)<code>([A-Za-z0-9_./-]+\.(?:rs|py|sh|java|kt|toml|md)#[^<]+)</code>(?!</a>)")


def name_on(lines, a, b, item):
    item = html.unescape(item)
    if item.startswith("anchor:"):
        needle = "ANCHOR: " + item[len("anchor:"):]
    else:
        last = re.split(r"::|\.", item)[-1].split("/")[0]
        needle = last.strip("<>").split(" as ")[0] if last.startswith("<") else last
        if re.fullmatch(r"[a-z0-9-]+", needle) and "-" in needle:
            # a markdown heading slug or a recipe: the words, any case
            words = needle.split("-")
            return any(all(w in lines[n - 1].lower() for w in words if w) for n in range(a, b + 1))
    return any(needle in lines[n - 1] for n in range(a, b + 1))


def main(argv):
    ap = argparse.ArgumentParser()
    ap.add_argument("--book", required=True)
    ap.add_argument("--sample", type=int, default=30)
    ap.add_argument("--seed", type=int, default=1848)
    args = ap.parse_args(argv)
    links, unlinked = [], []
    for page in sorted(glob.glob(os.path.join(args.book, "**", "*.html"), recursive=True)):
        with open(page, encoding="utf-8") as fh:
            text = fh.read()
        rel = os.path.relpath(page, args.book)
        for m in LINK.finditer(text):
            links.append((rel, m.group(1), int(m.group(2)), int(m.group(3) or m.group(2)), m.group(4)))
        for m in UNLINKED.finditer(text):
            unlinked.append((rel, m.group(1)))
    print(f"book citation links: {len(links)} found, {len(unlinked)} citation span(s) without one")
    failures = [f"{rel}: `{c}` is not linked" for rel, c in unlinked]
    if not links:
        failures.append("no citation link in the book: did the preprocessor run?")
    rng = random.Random(args.seed)
    sample = rng.sample(links, min(args.sample, len(links)))
    for rel, path, a, b, cite in sample:
        try:
            with open(os.path.join(ROOT, path), encoding="utf-8") as fh:
                lines = fh.read().split("\n")
        except OSError:
            failures.append(f"{rel}: {cite} links {path}, which does not exist")
            continue
        item = cite.split("#", 1)[1]
        ok = 1 <= a <= b <= len(lines) and name_on(lines, a, b, item)
        print(f"  {'ok ' if ok else 'BAD'} {cite} -> {path}#L{a}-L{b}: {lines[a - 1].strip()[:70] if a <= len(lines) else '?'}")
        if not ok:
            failures.append(f"{rel}: {cite} links lines {a}-{b} of {path}, which do not hold it")
    for f in failures:
        print(f)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
