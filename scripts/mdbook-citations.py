#!/usr/bin/env python3
"""mdBook preprocessor: a name citation becomes a link to the item's lines.

The pages cite code by name, `` `<path>#<Item>` ``, and store no line number
(docs/src/concepts/checks-and-citations.md, "Citations name items"). So the
reader still lands on the code, the build resolves each citation
(scripts/citations.py) and writes it as a link to the lines the item stands
on in the commit the book is built from:

    [`<path>#<Item>`](<forge>/src/commit/<sha>/<path>#L<start>-L<end>)

Only inline code spans are rewritten, never fenced blocks, and never a span
that is already a link's text. A citation that does not resolve is left as
written: the citation guard is what refuses it, not the book.

The forge is the book's `git-repository-url` (docs/book.toml), the commit is
`git rev-parse HEAD`, or `LEV_BOOK_COMMIT` when set; outside a git checkout
the links point at the branch `master`.

Registered in docs/book.toml as `[preprocessor.citations]`. Protocol: mdBook
calls it with `supports <renderer>` (answered yes for every renderer), then
with `[context, book]` on stdin, and reads the book back on stdout.
"""

import importlib.util
import json
import os
import re
import subprocess
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("citations", os.path.join(_HERE, "citations.py"))
C = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(C)

CODE_SPAN = re.compile(r"(?<!\[)`([^`\n]+)`(?!\]\()")


def link_citations(text, root, base):
    """`text` with every resolvable `` `path#item` `` span linked."""
    fences = C.fenced_regions(text)

    def repl(m):
        if any(a <= m.start() < b for a, b in fences):
            return m.group(0)
        inner = m.group(1)
        cite = C.NAME_CITE.fullmatch(inner)
        if not cite or not C.citable(cite.group(1)):
            return m.group(0)
        status, hits = C.resolve(cite.group(1), cite.group(2), root)
        if status != "ok":
            return m.group(0)
        it = hits[0]
        lines = f"L{it.start}" if it.start == it.end else f"L{it.start}-L{it.end}"
        return f"[`{inner}`]({base}/{cite.group(1)}#{lines})"

    return CODE_SPAN.sub(repl, text)


def _walk(items, fn):
    for item in items:
        if isinstance(item, dict) and "Chapter" in item:
            ch = item["Chapter"]
            ch["content"] = fn(ch["content"])
            _walk(ch.get("sub_items", []), fn)


def process_book(book, root, base):
    key = "sections" if "sections" in book else "items"
    _walk(book.get(key, []), lambda t: link_citations(t, root, base))
    return book


def forge_base(root, book_toml):
    url = "https://codeberg.org/Lew_Palm/leviculum"
    try:
        with open(book_toml, encoding="utf-8") as fh:
            m = re.search(r'^git-repository-url\s*=\s*"([^"]+)"', fh.read(), re.M)
            if m:
                url = m.group(1).rstrip("/")
    except OSError:
        pass
    commit = os.environ.get("LEV_BOOK_COMMIT")
    if not commit:
        try:
            commit = subprocess.run(["git", "-C", root, "rev-parse", "HEAD"], capture_output=True,
                                    check=True, text=True).stdout.strip()
        except (OSError, subprocess.CalledProcessError):
            commit = None
    return f"{url}/src/commit/{commit}" if commit else f"{url}/src/branch/master"


def main(argv):
    if len(argv) >= 1 and argv[0] == "supports":
        return 0
    context, book = json.load(sys.stdin)
    book_root = context.get("root", os.getcwd())
    root = os.path.dirname(os.path.abspath(book_root))
    base = forge_base(root, os.path.join(book_root, "book.toml"))
    json.dump(process_book(book, root, base), sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
