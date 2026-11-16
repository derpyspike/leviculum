#!/usr/bin/env python3
"""Both citation guards read one definition of what is not a file here.

scripts/citation-foreign.txt holds the non-file extensions (`host.tld:port`)
and the external prefixes (other repositories). scripts/citations.py (the
name guard and the drift census) and leviculum-std/tests/doc_citations.rs
(the line guard) both read it; the 1928 audit found the two holding sets of
their own that had already drifted apart (three host suffixes and the
`reference/` prefix).

Refused:
  - doc_citations.rs does not `include_str!` the shared file, or does not
    read both kinds from it;
  - citations.py's sets differ from the file's;
  - either source holds a string literal equal to one of the file's values,
    which is a second definition on its way in.

Exit 0 when both read the file and nothing else, 1 otherwise. The positive
controls are in scripts/citations-selftest.py.
"""

import argparse
import ast
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SHARED = os.path.join(HERE, "citation-foreign.txt")
RUST = os.path.join(ROOT, "leviculum-std", "tests", "doc_citations.rs")
PYTHON = os.path.join(HERE, "citations.py")
INCLUDE = 'include_str!("../../scripts/citation-foreign.txt")'

sys.path.insert(0, HERE)
import citations as C  # noqa: E402


def rust_literals(text):
    """The string literals of a Rust text, comments left out."""
    text = re.sub(r"//[^\n]*", "", text)
    return set(re.findall(r'"((?:[^"\\\n]|\\.)*)"', text))


def python_literals(text):
    return {n.value for n in ast.walk(ast.parse(text))
            if isinstance(n, ast.Constant) and isinstance(n.value, str)}


def check(rust_path, python_path):
    ext, pre = C._foreign_sets(SHARED)
    values = ext | set(pre)
    out = []
    with open(rust_path, encoding="utf-8") as fh:
        rust = fh.read()
    if INCLUDE not in rust:
        out.append(f"{rust_path} does not read the shared file ({INCLUDE})")
    for kind in ("external-prefix", "non-file-extension"):
        if f'foreign("{kind}")' not in rust:
            out.append(f"{rust_path} does not read the `{kind}` lines of the shared file")
    for v in sorted(values & rust_literals(rust)):
        out.append(f"{rust_path} holds the literal \"{v}\": a second definition of "
                   "what the shared file says")
    with open(python_path, encoding="utf-8") as fh:
        py = fh.read()
    for v in sorted(values & python_literals(py)):
        out.append(f"{python_path} holds the literal \"{v}\": a second definition of "
                   "what the shared file says")
    if os.path.abspath(python_path) == os.path.abspath(PYTHON):
        if set(C.NON_FILE_EXTENSIONS) != ext:
            out.append(f"citations.NON_FILE_EXTENSIONS differs from the shared file: "
                       f"{sorted(set(C.NON_FILE_EXTENSIONS) ^ ext)}")
        if tuple(C.EXTERNAL_PREFIXES) != pre:
            out.append(f"citations.EXTERNAL_PREFIXES differs from the shared file: "
                       f"{sorted(set(C.EXTERNAL_PREFIXES) ^ set(pre))}")
    return out, len(ext), len(pre)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--rust", default=RUST)
    ap.add_argument("--python", default=PYTHON)
    args = ap.parse_args(argv)
    bad, n_ext, n_pre = check(args.rust, args.python)
    for b in bad:
        print(f"REFUSED {b}")
    if bad:
        print("citation sets: the guards do not read one definition "
              "(scripts/citation-foreign.txt)")
        return 1
    print(f"citation sets: both guards read scripts/citation-foreign.txt "
          f"({n_ext} non-file extensions, {n_pre} external prefixes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
