#!/usr/bin/env python3
"""Positive controls of the citation tools: each control breaks what a tool
guards and expects the tool to say so.

scripts/check-citations-by-name.py (the guard) on a scratch repository:
  a renamed fn, an ambiguous name, a path into another repository, a base
  name, and a new line citation are refused; a renumbered or moved existing
  line citation, a new one into another repository and a marked fixture
  line pass.
scripts/citation-census.py (the drift census) on the same repository:
  agree, drift, nolabel, each foreign reason, a shortened path the page spells
  out, and the table-row rule; and on this repository's own history, the 64
  citations commit 8482638d6 renumbered (scripts/citations-control-1908.tsv),
  skipped with a loud line only where that commit is not in the clone.

The fixture texts are put together at run time (`L`, `N`), so this file
holds no citation-shaped string the guard would read as one.
"""

import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
GUARD = os.path.join(HERE, "check-citations-by-name.py")
CENSUS = os.path.join(HERE, "citation-census.py")
CONTROL = os.path.join(HERE, "citations-control-1908.tsv")
CONTROL_REV = "8482638d6"

FAILED = []
HELD = []


def L(path, spec):
    """A line citation."""
    return f"{path}:{spec}"


def N(path, item):
    """A name citation."""
    return f"{path}#{item}"


LIB = "crate/src/lib.rs"

LIB_RS = """\
/// Alpha's doc.
pub fn alpha() -> u32 {
    1
}

pub struct Thing;

impl Thing {
    pub fn new() -> Self {
        Thing
    }
}

pub struct Other;

impl Other {
    pub fn new() -> Self {
        Other
    }
}

pub fn beta() {}
"""


def control(name, ok, detail=""):
    (HELD if ok else FAILED).append(name)
    print(("ok      " if ok else "FAILED  ") + name + ("" if ok else f"\n{detail}"))


def git(repo, *args):
    subprocess.run(["git", "-C", repo, "-c", "user.name=selftest", "-c",
                    "user.email=selftest@invalid", *args],
                   check=True, capture_output=True)


def write(repo, rel, text):
    full = os.path.join(repo, rel)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w", encoding="utf-8") as fh:
        fh.write(text)


def run(script, repo, *args):
    r = subprocess.run([sys.executable, "-I", script, "--root", repo, *args],
                       capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr


def page(*paras):
    return "# Page\n\n" + "\n\n".join(paras) + "\n"


BASE_PAGE = page(
    f"The first is `alpha` (`{L(LIB, 2)}`).",
    f"Beta is `{N(LIB, 'beta')}`.",
)


def scratch():
    repo = tempfile.mkdtemp(prefix="citations-selftest-")
    git(repo, "init", "-q")
    write(repo, "crate/Cargo.toml", "[package]\nname = \"c\"\n")
    write(repo, LIB, LIB_RS)
    write(repo, "docs/src/page.md", BASE_PAGE)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    return repo


def reset(repo):
    git(repo, "checkout", "-q", "--", ".")
    git(repo, "clean", "-qfd")


def guard_controls(repo):
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: the base tree passes", rc == 0, out)

    write(repo, LIB, LIB_RS.replace("fn beta", "fn gamma"))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a renamed fn is refused", rc == 1 and "has no item `beta`" in out, out)
    reset(repo)

    write(repo, "docs/src/page.md", BASE_PAGE + f"\nBoth `{N(LIB, 'new')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: an ambiguous name is refused", rc == 1 and "names 2 items" in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nOne `{N(LIB, 'Thing::new')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: the owner makes it one", rc == 0, out)
    reset(repo)

    write(repo, "docs/src/page.md",
          BASE_PAGE + f"\nThe phone's `{N('columba/app/src/Foo.kt', 'bar')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a name into another repository is refused",
            rc == 1 and "names another repository" in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nShort `{N('lib.rs', 'alpha')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a base name is refused, not guessed",
            rc == 1 and "not a path from the repository root" in out, out)
    reset(repo)

    write(repo, "docs/src/page.md", BASE_PAGE + f"\nNew: `{L(LIB, 22)}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new line citation is refused", rc == 1 and "a new line citation" in out
            and N(LIB, "beta") in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nNew: `{L('lib.rs', 22)}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new bare-name line citation is refused",
            rc == 1 and "a new line citation" in out, out)
    reset(repo)

    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 3)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a renumbered existing citation passes", rc == 0, out)
    moved = page(f"Beta is `{N(LIB, 'beta')}`.", "Text between.",
                 f"The first is `alpha` (`{L(LIB, 2)}`).")
    write(repo, "docs/src/page.md", moved)
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a moved existing citation passes", rc == 0, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nPython: `{L('Transport.py', 40)}`, "
          f"`{L('reference/Reticulum/RNS/Link.py', 7)}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new citation into another repository passes", rc == 0, out)
    write(repo, LIB, LIB_RS + f'\n// "{L(LIB, 9)}" citation-guard: line-fixture\n')
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a marked fixture line passes", rc == 0, out)
    reset(repo)


def language_controls(repo):
    write(repo, "tools/tool.py", "class Klass:\n    def method(self):\n        return 1\n\n\nLIMIT = 3\n")
    write(repo, "Justfile", "# Build it.\nbuild:\n    true\n")
    write(repo, "crate/Cargo.toml", "[package]\nname = \"c\"\n")
    names = [N("tools/tool.py", "Klass.method"), N("tools/tool.py", "LIMIT"), N("Justfile", "build"),
             N("crate/Cargo.toml", "package.name"), N("docs/src/page.md", "page")]
    write(repo, "docs/src/page.md", BASE_PAGE + "\n" + " ".join(f"`{n}`" for n in names) + "\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: Python, Justfile, TOML and heading names resolve",
            rc == 0 and f"{len(names) + 1} name citation(s) checked, 0 refused" in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\n`{N('tools/tool.py', 'Klass.gone')}` "
          f"`{N('Justfile', 'gone')}` `{N('docs/src/page.md', 'gone')}`\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a missing Python, recipe or heading name is refused",
            rc == 1 and out.count("has no item") == 3, out)
    reset(repo)


def census_of(repo):
    rc, out = run(CENSUS, repo)
    recs = {}
    for line in out.splitlines():
        if line.startswith("CITE "):
            kv = dict(p.split("=", 1) for p in line[5:].split(" "))
            recs.setdefault(kv["file"].rsplit(":", 1)[0], []).append(kv)
    return rc, out, recs


def census_controls(repo):
    write(repo, "docs/src/census.md", page(
        f"Agrees: `alpha` (`{L(LIB, 2)}`).",
        f"Drifts: `beta` (`{L(LIB, 2)}`).",
        f"No label (`{L(LIB, 2)}`).",
        f"A variable: `x_total` (`{L(LIB, 2)}`).",
        f"| `beta` | `alpha` (`{L(LIB, 9)}`) | `beta` |",
        f"Doc line: `alpha` (`{L(LIB, 1)}`).",
        f"| `beta` | `{L(LIB, 2)}` | `alpha` |",
    ))
    write(repo, "docs/src/spelled.md", page(
        f"The file is `{LIB}`.", f"Spelled: `alpha` (`{L('src/lib.rs', 3)}`)."))
    write(repo, "docs/src/foreign.md", page(
        f"Python `Link.teardown` (`{L('Link.py', 12)}`).",
        f"Phone `{L('columba/app/src/Foo.kt', 3)}`.",
        f"Bare `alpha` (`{L('lib.rs', 2)}`).",
        f"Host `{L('peer.example.com', 4242)}`."))
    rc, out, recs = census_of(repo)

    def v(f, k):
        got = recs.get(f, [])
        return got[k] if k < len(got) else {}
    c = "docs/src/census.md"
    control("census: label at its line agrees",
            v(c, 0).get("verdict") == "agree" and v(c, 0).get("label") == "alpha", out)
    control("census: label of another item drifts, and names the item there",
            v(c, 1).get("verdict") == "drift" and v(c, 1).get("item") == "alpha", out)
    control("census: no label is nolabel", v(c, 2).get("verdict") == "nolabel"
            and v(c, 2).get("why") == "none", out)
    control("census: a label that is no item is nolabel",
            v(c, 3).get("verdict") == "nolabel" and v(c, 3).get("why") == "not-an-item", out)
    control("census: a table row reads its own cell, not the next",
            v(c, 4).get("label") == "alpha" and v(c, 4).get("verdict") == "drift"
            and v(c, 4).get("item") == "Thing::new", out)
    control("census: a doc comment line belongs to its item",
            v(c, 5).get("verdict") == "agree", out)
    control("census: a table row reads the cell before, never the one after",
            v(c, 6).get("label") == "beta" and v(c, 6).get("verdict") == "drift", out)
    s = "docs/src/spelled.md"
    control("census: a shortened path the page spells out is placed",
            v(s, 0).get("verdict") == "agree" and v(s, 0).get("how") == "spelled", out)
    f = "docs/src/foreign.md"
    whys = [r.get("why") for r in recs.get(f, [])]
    control("census: foreign absent, other-repo, unplaced; no host:port",
            whys == ["absent", "other-repo", "unplaced"]
            and all(r.get("verdict") == "foreign" for r in recs.get(f, [])), out)
    control("census: exits 0", rc == 0, out)


def history_control():
    have = subprocess.run(["git", "-C", ROOT, "cat-file", "-e", CONTROL_REV + "^{commit}"],
                          capture_output=True).returncode == 0
    if not have:
        print(f"SKIPPED census: control 1908 needs commit {CONTROL_REV}, which this clone "
              "does not have (a shallow clone)")
        return
    r = subprocess.run([sys.executable, "-I", CENSUS, "--rev", CONTROL_REV, "--quiet",
                        "--control", CONTROL], capture_output=True, text=True)
    last = [ln for ln in r.stdout.splitlines() if ln.startswith("CONTROL rows=")]
    control("census: the 64 citations 8482638d6 renumbered keep their verdicts",
            r.returncode == 0 and last and "rows=64 held=64" in last[0],
            r.stdout + r.stderr)


def main():
    repo = scratch()
    try:
        guard_controls(repo)
        language_controls(repo)
        census_controls(repo)
    finally:
        subprocess.run(["rm", "-rf", repo], check=False)
    history_control()
    if FAILED:
        print(f"citations selftest: {len(FAILED)} control(s) FAILED: {', '.join(FAILED)}")
        return 1
    print(f"citations selftest: every control holds ({len(HELD)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
