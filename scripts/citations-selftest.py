#!/usr/bin/env python3
"""Selftest of the name-citation tools (scripts/citations.py, the guard
scripts/check-name-citations.py, the converter
scripts/convert-line-citations.py and the book preprocessor
scripts/mdbook-citations.py).

Each control builds a small repository in a temporary directory, perturbs
it the way a real edit would, and asserts the verdict: a renamed function,
an ambiguous name, a deleted anchor and a line citation must each fail; a
function moved inside its file, or lines added above it, must stay green
with the citing page unchanged. Run by `just citation-guard`.

Exit 0 = every control holds; 1 = one does not (named).
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile

_HERE = os.path.dirname(os.path.abspath(__file__))


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(_HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


C = _load("citations", "citations.py")
G = _load("check_name_citations", "check-name-citations.py")
V = _load("convert_line_citations", "convert-line-citations.py")
B = _load("mdbook_citations", "mdbook-citations.py")
# one resolver, one cache: the tools each loaded their own copy
G.C = V.C = B.C = C

RUST = """\
//! A module.

use core::fmt;

/// The engine.
pub struct Engine {
    /// How many.
    pub count: u32,
    buf: [u8; 16],
}

pub enum Event {
    Started,
    /// Stopped, with a reason.
    Stopped { reason: u8 },
}

pub const LIMIT: usize = 4;

impl Engine {
    /// Make one.
    pub fn new() -> Self {
        Self { count: 0, buf: [0; 16] }
    }

    #[inline]
    pub fn tick<const N: usize>(
        &mut self,
        out: &mut [u8; N],
    ) -> usize {
        // ANCHOR: tick-core
        self.count += 1;
        // ANCHOR_END: tick-core
        out.len()
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine")
    }
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Engine")
    }
}

pub trait Clocked {
    fn now(&self) -> u64;
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tick_counts() {
        assert_eq!(1, 1);
    }
}
"""

PYTHON = """\
LIMIT = {
    "a": 1,
}


class Census:
    @staticmethod
    def total(rows):
        return len(rows)


def main():
    return 0
"""

JUSTFILE = """\
manifest := "python3"

# The guard.
[doc('Guard')]
citation-guard:
    echo one
    echo two

fast: citation-guard
    echo fast
"""

TOML = """\
[package]
name = "x"
version = "1.0.0"

[dependencies]
serde = "1"
"""

JAVA = """\
package x;

public class Dialogs {
    static void verb(String a) {
        say(a);
    }

    static void verb(String a, String b, Runnable after) {
        new Thread(new Runnable() {
            public void run() {
                say(b);
            }
        }).start();
    }
}
"""

PAGE = """\
# A page

The engine is `src/engine.rs#Engine`, its step `src/engine.rs#Engine::tick`,
the core of the step `src/engine.rs#anchor:tick-core`, the display
`src/engine.rs#<Engine as Display>::fmt`, a variant `src/engine.rs#Event::Stopped`,
a field `src/engine.rs#Engine::count`, the test `src/engine.rs#a_tick_counts`,
the census `tools/census.py#Census.total`, the recipe `Justfile#citation-guard`,
the version `Cargo.toml#package.version`, the overload `src/Dialogs.java#Dialogs.verb/3`.
The reference keeps its lines: `reference/Reticulum/RNS/Transport.py:1234`.

```text
example log: engine.rs:12 something
```
"""

FILES = {
    "src/engine.rs": RUST,
    "tools/census.py": PYTHON,
    "Justfile": JUSTFILE,
    "Cargo.toml": TOML,
    "src/Dialogs.java": JAVA,
    "docs/src/page.md": PAGE,
    "scripts/line-citations-pending.txt": "# none\n",
    "scripts/line-citations-exempt.txt": "# none\n",
}


def write(root, files):
    for rel, body in files.items():
        os.makedirs(os.path.dirname(os.path.join(root, rel)) or root, exist_ok=True)
        with open(os.path.join(root, rel), "w", encoding="utf-8") as fh:
            fh.write(body)


def fresh(files=None):
    tmp = tempfile.mkdtemp(prefix="citations-selftest-")
    subprocess.run(["git", "init", "-q", tmp], check=True)
    write(tmp, FILES if files is None else files)
    C._CACHE.clear()
    return tmp


def run_guard(root):
    C._CACHE.clear()
    out, _, _ = G.findings(root)
    return out


def main():
    failures = []

    def check(label, cond, detail=""):
        if not cond:
            failures.append(f"{label}\n{detail}")

    # 0. the fixture is green as written
    root = fresh()
    out = run_guard(root)
    check("the fixture is green", out == [], "\n".join(out))

    # 1. resolver: each item kind resolves to the right lines
    expect = {
        ("src/engine.rs", "Engine"): (5, 10),
        ("src/engine.rs", "Engine::count"): (7, 8),
        ("src/engine.rs", "Event::Stopped"): (14, 15),
        ("src/engine.rs", "LIMIT"): (18, 18),
        ("src/engine.rs", "Engine::new"): (21, 24),
        ("src/engine.rs", "Engine::tick"): (26, 35),
        ("src/engine.rs", "anchor:tick-core"): (31, 33),
        ("src/engine.rs", "<Engine as Display>::fmt"): (39, 41),
        ("src/engine.rs", "<Engine as Display>"): (38, 42),
        ("src/engine.rs", "Clocked::now"): (51, 51),
        ("src/engine.rs", "tests::a_tick_counts"): (56, 59),
        ("tools/census.py", "LIMIT"): (1, 3),
        ("tools/census.py", "Census.total"): (7, 9),
        ("tools/census.py", "main"): (12, 13),
        ("Justfile", "citation-guard"): (3, 7),
        ("Justfile", "manifest"): (1, 1),
        ("Cargo.toml", "package.version"): (3, 3),
        ("Cargo.toml", "dependencies"): (5, 6),
        ("src/Dialogs.java", "Dialogs.verb/1"): (4, 6),
        ("src/Dialogs.java", "Dialogs.verb/3"): (8, 14),
        ("docs/src/page.md", "a-page"): (1, 14),
    }
    for (path, spec), want in expect.items():
        status, hits = C.resolve(path, spec, root)
        got = (hits[0].start, hits[0].end) if status == "ok" else status
        check(f"resolve {path}#{spec}", got == want, f"want {want}, got {got}")
    for spec in ("fmt", "Engine::fmt", "Dialogs.verb"):
        status, _ = C.resolve("src/engine.rs" if "verb" not in spec else "src/Dialogs.java", spec, root)
        check(f"`{spec}` is ambiguous", status == "ambiguous", status)

    # 2. a function moved inside its file, and lines added above it: green,
    #    and the page is byte for byte what it was
    before = open(os.path.join(root, "docs/src/page.md")).read()
    moved = RUST.replace("pub const LIMIT: usize = 4;\n", "")
    moved = "// one\n// two\n// three\n" + moved + "\npub const LIMIT: usize = 4;\n"
    write(root, {"src/engine.rs": moved})
    out = run_guard(root)
    check("a moved item stays green", out == [], "\n".join(out))
    check("a move changes no page", open(os.path.join(root, "docs/src/page.md")).read() == before)
    status, hits = C.resolve("src/engine.rs", "Engine::tick", root)
    check("the moved item's new lines", status == "ok" and hits[0].start == 28,
          f"{status} {[h.start for h in hits]}")

    # 3. a renamed function fails, naming the citation
    write(root, {"src/engine.rs": RUST.replace("pub fn tick<", "pub fn step<")})
    out = run_guard(root)
    check("a renamed function fails", len(out) == 1 and "has no item `Engine::tick`" in out[0],
          "\n".join(out))

    # 4. an ambiguous name fails
    write(root, {"src/engine.rs": RUST,
                 "docs/src/page.md": PAGE + "\nThe formatter `src/engine.rs#Engine::fmt`.\n"})
    out = run_guard(root)
    check("an ambiguous name fails", len(out) == 1 and "names 2 items" in out[0], "\n".join(out))

    # 5. a deleted anchor fails
    write(root, {"docs/src/page.md": PAGE,
                 "src/engine.rs": RUST.replace("        // ANCHOR_END: tick-core\n", "")})
    out = run_guard(root)
    check("a deleted anchor fails", len(out) == 1 and "anchor:tick-core" in out[0], "\n".join(out))

    # 6. a line citation in a living text fails, with the name to write
    write(root, {"src/engine.rs": RUST,
                 "docs/src/page.md": PAGE + "\nThe step (`src/engine.rs:30`).\n"})
    out = run_guard(root)
    check("a line citation fails", len(out) == 1 and "`src/engine.rs#Engine::tick`" in out[0],
          "\n".join(out))
    # ... and in a code comment too
    write(root, {"docs/src/page.md": PAGE,
                 "tools/census.py": PYTHON + "# see engine.rs:22\n"})
    out = run_guard(root)
    check("a line citation in a comment fails", len(out) == 1 and "Engine::new" in out[0],
          "\n".join(out))

    # 7. pending: a listed file may carry line citations; once it carries
    #    none, it must leave the list
    write(root, {"scripts/line-citations-pending.txt": "tools/census.py\n"})
    out = run_guard(root)
    check("a pending file may carry line citations", out == [], "\n".join(out))
    write(root, {"tools/census.py": PYTHON})
    out = run_guard(root)
    check("a converted file must leave the pending list",
          len(out) == 1 and "take it off the list" in out[0], "\n".join(out))
    write(root, {"scripts/line-citations-pending.txt": "# none\n"})

    # 8. the converter writes what the guard asks for, and leaves a span
    #    across two items for a hand decision
    write(root, {"docs/src/page.md": PAGE + "\nStep (`engine.rs:30`), both (`src/engine.rs:21-30`)."
                 "\n`fmt` (`src/engine.rs:39`).\n"})
    C._CACHE.clear()
    text = open(os.path.join(root, "docs/src/page.md")).read()
    new, n, hand = V.convert_text("docs/src/page.md", text, C.Tree(root))
    check("the converter converts", n == 2 and "Step (`src/engine.rs#Engine::tick`)" in new
          and "(`src/engine.rs#<Engine as Display>::fmt`)" in new, new[-200:] + "\n" + "\n".join(hand))
    check("a span across two items is left by hand",
          len(hand) == 1 and "src/engine.rs:21-30" in hand[0], "\n".join(hand))

    # 9. the book: a citation becomes a link to the item's lines
    write(root, {"docs/src/page.md": PAGE})
    C._CACHE.clear()
    chapter = B.link_citations(PAGE, root, "https://forge/src/commit/abc")
    check("the book links a citation to its lines",
          "[`src/engine.rs#Engine::tick`](https://forge/src/commit/abc/src/engine.rs#L26-L35)" in chapter,
          chapter)
    check("the book leaves the fenced block alone", "engine.rs:12 something" in chapter, chapter)
    check("the book links a recipe",
          "(https://forge/src/commit/abc/Justfile#L3-L7)" in chapter, chapter)
    wrapped = B.link_citations("A span `that wraps\nonto a line` then (`src/engine.rs#Engine::new`).\n",
                               root, "https://forge/src/commit/abc")
    check("a code span that wraps a line does not shift the pairing",
          "(https://forge/src/commit/abc/src/engine.rs#L21-L24)" in wrapped, wrapped)
    book = {"sections": [{"Chapter": {"content": PAGE, "sub_items": []}}]}
    linked = B.process_book(json.loads(json.dumps(book)), root, "https://forge/src/commit/abc")
    check("the preprocessor walks the book",
          "#L26-L35" in linked["sections"][0]["Chapter"]["content"])

    if failures:
        print(f"citations selftest: {len(failures)} control(s) failed")
        for f in failures:
            print("  " + f.replace("\n", "\n    "))
        return 1
    print("citations selftest: every control holds")
    return 0


if __name__ == "__main__":
    sys.exit(main())
