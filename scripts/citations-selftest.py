#!/usr/bin/env python3
"""Positive controls of the citation tools: each control breaks what a tool
guards and expects the tool to say so.

scripts/check-citations-by-name.py (the guard) on a scratch repository:
  a renamed fn, an ambiguous name, a path into another repository and a base
  name are refused; a touched line citation pairs by occurrence with the
  base citation it stands in for and must agree, or, when that partner did
  not agree, stay on the item the partner resolved (the reviewer's decision
  of 2026-11-16 21:05, order 1959): a stale or label-less new number, an
  agreeing citation moved by one line onto another item (also beside a
  drifting sibling of its label), a new drifting or label-less citation
  beside an untouched one, a drift renumbered onto a third item, a drift on
  an untouched line whose file moved a third item under it, a label-less
  citation renumbered onto another item, a renumbered bare name and a file
  moved under an agreeing citation are refused; a citation fixed onto its
  item, one that agrees, a drift renumbered with its item, a drift carried
  word for word into a rewritten line, a drift that became nolabel on its
  base's item, a label-less citation that follows its item, a citation
  turned into a name (its neighbours keep their partners), a moved
  paragraph, a merge's untouched drift, a new citation into another
  repository and a marked fixture line pass.
scripts/citation-census.py (the drift census) on the same repository:
  agree, drift, nolabel, each foreign reason, a shortened path the page spells
  out, the table-row rule, and the `//` comment-block rule (a `cfg(test)`
  is no test attribute); and on this
  repository's own history, the 64 citations commit 8482638d6 renumbered
  (scripts/citations-control-1908.tsv). A clone without that commit FAILS the
  control, unless LEVICULUM_CITATION_CONTROL_WAIVE is set, which turns the
  failure into a printed WAIVED line.

The fixture texts are put together at run time (`L`, `N`), so this file
holds no citation-shaped string the guard would read as one.
"""

import argparse
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
FOREIGN = os.path.join(HERE, "check-citation-foreign.py")

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

pub mod one {
    pub fn twin() {}
}

pub mod two {
    pub fn twin() {}
}
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

# A second lib.rs, so a bare `lib.rs` has two candidates; `alpha` here is
# at line 6, where LIB holds `Thing`.
LIB2 = "crate/src/extra/lib.rs"
LIB2_RS = "//! Extra.\n//\n//\n//\n\npub fn alpha() -> u32 {\n    2\n}\n"

# A page whose citation drifts at the base on purpose: `beta` is at line
# 22 of LIB, not at line 2.
DRIFTY = "docs/src/drifty.md"
DRIFTY_PAGE = page(f"Drifting: `beta` (`{L(LIB, 2)}`).")

# A page with a bare name no spelled path places: the census cannot judge
# it, so the guard only holds it to not getting worse.
BARE = "docs/src/bare.md"
BARE_PAGE = page(f"Bare: `beta` (`{L('lib.rs', 2)}`).")


def scratch():
    repo = tempfile.mkdtemp(prefix="citations-selftest-")
    git(repo, "init", "-q")
    write(repo, "crate/Cargo.toml", "[package]\nname = \"c\"\n")
    write(repo, LIB, LIB_RS)
    write(repo, LIB2, LIB2_RS)
    write(repo, "docs/src/page.md", BASE_PAGE)
    write(repo, DRIFTY, DRIFTY_PAGE)
    write(repo, BARE, BARE_PAGE)
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

    write(repo, "docs/src/page.md", BASE_PAGE + f"\nBoth `{N(LIB, 'twin')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: an ambiguous name is refused", rc == 1 and "names 2 items" in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nOne `{N(LIB, 'one::twin')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: the module makes it one", rc == 0, out)
    reset(repo)

    write(repo, "docs/src/page.md", BASE_PAGE + f"\nBare `{N(LIB, 'new')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a member without its owner is refused",
            rc == 1 and "is a member; write it with its owner" in out
            and "`Thing::new`" in out and "`Other::new`" in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nOne `{N(LIB, 'Thing::new')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: the owner makes a member one", rc == 0, out)
    reset(repo)

    bad = "crate/src/bad.rs"
    with open(os.path.join(repo, bad), "wb") as fh:
        fh.write(b"pub fn x() {}\n// \xff\xfe not UTF-8\n")
    git(repo, "add", bad)
    git(repo, "commit", "-q", "-m", "a tracked file that is not UTF-8")
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nUnreadable `{N(bad, 'x')}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a name into a file it cannot read is refused and not counted",
            rc == 1 and f"cannot read {bad}" in out and "1 name citation(s) checked" in out, out)
    reset(repo)
    git(repo, "reset", "-q", "--hard", "HEAD~1")

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
    control("guard: a new line citation without a label is refused",
            rc == 1 and "a new line citation" in out and N(LIB, "beta") in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nNew: `beta` (`{L(LIB, 22)}`).\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new line citation on the item its text names passes", rc == 0, out)
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nNew: `{L('lib.rs', 22)}`.\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new bare-name line citation is refused",
            rc == 1 and "a new line citation" in out, out)
    reset(repo)

    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 6)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: an agreeing citation moved onto another item is refused",
            rc == 1 and "a new line citation" in out, out)
    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 3)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a changed number that stays on its item passes", rc == 0, out)
    write(repo, DRIFTY, DRIFTY_PAGE.replace(L(LIB, 2), L(LIB, 22)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a drifting citation fixed onto its item passes", rc == 0, out)
    write(repo, DRIFTY, DRIFTY_PAGE.replace(L(LIB, 2), L(LIB, 6)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a drifting citation renumbered onto a third item is refused",
            rc == 1 and "a new line citation" in out, out)
    reset(repo)
    # The sentence is rewritten, so the citing line is one the diff
    # changes; the citation in it is written as before.
    rewritten = page("Text before.", f"Still drifting: `beta` (`{L(LIB, 2)}`).")
    write(repo, DRIFTY, rewritten)
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a drifting citation carried word for word into a rewritten line passes",
            rc == 0, out)
    write(repo, LIB, LIB_RS.replace("fn beta", "fn gamma"))
    write(repo, "docs/src/page.md", page(f"The first is `alpha` (`{L(LIB, 2)}`)."))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a drift that became nolabel on the item its base resolved passes",
            rc == 0, out)
    reset(repo)
    sibling_controls(repo)
    write(repo, "docs/src/page.md", page(f"The last is `beta` (`{L(LIB, 20)}`).",
                                         f"Beta is `{N(LIB, 'beta')}`."))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new citation in the place of an old one is refused",
            rc == 1 and "a new line citation" in out, out)
    write(repo, LIB, "// one\n// two\n// three\n" + LIB_RS)
    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 5)))
    write(repo, DRIFTY, DRIFTY_PAGE.replace(L(LIB, 2), L(LIB, 5)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: lines inserted above the item and the number follows: passes", rc == 0, out)
    reset(repo)

    write(repo, LIB, "// one\n// two\n// three\n" + LIB_RS)
    write(repo, DRIFTY, DRIFTY_PAGE.replace(L(LIB, 2), L(LIB, 5)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a file moved under an agreeing citation is refused until renumbered",
            rc == 1 and "docs/src/page.md:3" in out and "changed under it" in out
            and "which agreed" in out, out)
    reset(repo)
    write(repo, LIB, LIB_RS + "\npub fn gamma() {}\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a file change that leaves every verdict alone passes", rc == 0, out)
    reset(repo)

    # `alpha` is at line 6 of the OTHER lib.rs: a guess through the base
    # name would call this agree. The page spells only LIB in full, so the
    # citation resolves there, where line 6 is `Thing`.
    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L("lib.rs", 6)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a shortened path is judged through the file it resolves to, "
            "not a base-name match", rc == 1 and "a new line citation" in out, out)
    reset(repo)
    write(repo, BARE, BARE_PAGE.replace(L("lib.rs", 2), L("lib.rs", 3)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a bare name the tree cannot place, renumbered, is refused (no item "
            "to pair it by)", rc == 1 and "a new line citation" in out, out)
    write(repo, BARE, page(f"Bare, reworded: `beta` (`{L('lib.rs', 2)}`)."))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a bare name carried word for word into a rewritten line passes",
            rc == 0, out)
    reset(repo)

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


def sibling_controls(repo):
    """An untouched citation keeps its own base entry: it never excuses a
    touched one of the same cited file and label beside it (order 1955)."""
    write(repo, "docs/src/page.md", BASE_PAGE + f"\nAgain `alpha` (`{L(LIB, 22)}`).\n")
    git(repo, "commit", "-qam", "an agreeing and a drifting `alpha`")
    with_sibling = BASE_PAGE + f"\nAgain `alpha` (`{L(LIB, 22)}`).\n"
    write(repo, "docs/src/page.md", with_sibling.replace(L(LIB, 2), L(LIB, 6), 1))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: an agreeing citation moved beside an untouched drifting sibling "
            "is refused", rc == 1 and "a new line citation" in out, out)
    reset(repo)
    git(repo, "reset", "-q", "--hard", "HEAD~1")

    write(repo, DRIFTY, DRIFTY_PAGE + f"\nAlso drifting: `beta` (`{L(LIB, 2)}`).\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new drifting citation beside an untouched one is refused",
            rc == 1 and "a new line citation" in out, out)
    write(repo, DRIFTY, DRIFTY_PAGE + f"\nAnd `beta` (`{L(LIB, 9)}`).\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: the base keeps a drift word for word, a second one of its label "
            "on another item is refused", rc == 1 and "a new line citation" in out, out)
    reset(repo)

    write(repo, DRIFTY, DRIFTY_PAGE + f"\nSomewhere (`{L(LIB, 6)}`).\n")
    git(repo, "commit", "-qam", "a label-less citation")
    write(repo, DRIFTY, DRIFTY_PAGE + f"\nSomewhere (`{L(LIB, 6)}`).\n"
          f"\nSomewhere else (`{L(LIB, 9)}`).\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a new label-less citation beside an untouched one is refused",
            rc == 1 and "a new line citation" in out, out)
    write(repo, DRIFTY, DRIFTY_PAGE + f"\nSomewhere (`{L(LIB, 9)}`).\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a label-less citation renumbered onto another item is refused",
            rc == 1 and "a new line citation" in out, out)
    write(repo, LIB, LIB_RS.replace("pub struct Thing;", "\n\n\npub struct Thing;"))
    write(repo, DRIFTY, DRIFTY_PAGE + f"\nSomewhere (`{L(LIB, 9)}`).\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a label-less citation that follows its item passes", rc == 0, out)
    reset(repo)
    git(repo, "reset", "-q", "--hard", "HEAD~1")
    pairing_controls(repo)


def pairing_controls(repo):
    """Each touched citation pairs with the one base citation it stands in
    for through its text's own diff, and answers to the item that one
    resolved (the reviewer's decision of 2026-11-16 21:05)."""
    # `Thing::new` (lines 9-11) agrees at 9; `Thing::new` drifts at 2.
    two = page(f"Made by `Thing::new` (`{L(LIB, 9)}`).",
               f"Again `Thing::new` (`{L(LIB, 2)}`).")
    write(repo, "docs/src/two.md", two)
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "an agreeing and a drifting `Thing::new`")
    write(repo, "docs/src/two.md", two.replace(L(LIB, 9), L(LIB, 8), 1))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: an agreeing citation moved by one line onto another item is refused "
            "beside a same-label drift into the same file",
            rc == 1 and "a new line citation" in out, out)
    reset(repo)
    git(repo, "reset", "-q", "--hard", "HEAD~1")

    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 5)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: an agreeing citation moved by one line onto another item is refused",
            rc == 1 and "a new line citation" in out, out)
    reset(repo)

    # Three lines above `alpha`: it moves from 2 to 5, `beta` from 22 to 25.
    shifted = "// one\n// two\n// three\n" + LIB_RS
    write(repo, LIB, shifted)
    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 5)))
    write(repo, DRIFTY, DRIFTY_PAGE.replace(L(LIB, 2), L(LIB, 5)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a drifting citation renumbered with its item passes", rc == 0, out)
    write(repo, DRIFTY, DRIFTY_PAGE.replace(L(LIB, 2), L(LIB, 25)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a repaired citation (now agree) passes", rc == 0, out)
    reset(repo)

    # `gamma` takes lines 1-2 above `alpha`; the drifting page stands.
    write(repo, LIB, "pub fn gamma() {\n}\n\n" + LIB_RS)
    write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 5)))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a drift on an untouched line is refused when its file moves a third "
            "item under it", rc == 1 and "gamma" in out and DRIFTY in out, out)
    reset(repo)

    # Two adjacent lines rewritten in one hunk: the first drops a citation,
    # the second keeps its drift on `Thing`. Paired line by line, the drift
    # keeps its partner; paired over the hunk, it would meet the dropped one.
    two_lines = page(f"Here `alpha` (`{L(LIB, 2)}`) and `alpha` (`{L(LIB, 3)}`),\n"
                     f"and `beta` (`{L(LIB, 6)}`).")
    write(repo, "docs/src/lines.md", two_lines)
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "two adjacent lines")
    write(repo, "docs/src/lines.md", page(f"Here `alpha` (`{L(LIB, 2)}`) alone,\n"
                                          f"and then `beta` (`{L(LIB, 6)}`)."))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a hunk that rewrites as many lines as it replaces pairs line by line",
            rc == 0, out)
    reset(repo)
    git(repo, "reset", "-q", "--hard", "HEAD~1")

    # One line, a drift into `alpha` then a drift into `Thing`; the first
    # becomes a name. The second keeps its place, so it keeps its partner
    # and passes; counted without the name it would pair with the first.
    row = page(f"Both `beta` (`{L(LIB, 2)}`) and `Other` (`{L(LIB, 6)}`).")
    write(repo, "docs/src/row.md", row)
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "two drifts on one line")
    write(repo, "docs/src/row.md", row.replace(f"`{L(LIB, 2)}`", f"`{N(LIB, 'beta')}`"))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a citation turned into a name holds its place in the pairing",
            rc == 0, out)
    write(repo, "docs/src/row.md", page(f"Both `beta` (`{N(LIB, 'beta')}`) and `Other` "
                                        f"(`{L(LIB, 2)}`)."))
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: a line citation where a name stood pairs with the one at its own place",
            rc == 1 and "a new line citation" in out, out)
    reset(repo)
    git(repo, "reset", "-q", "--hard", "HEAD~1")

    write(repo, DRIFTY, DRIFTY_PAGE + f"\nAlso drifting: `beta` (`{L(LIB, 6)}`).\n")
    rc, out = run(GUARD, repo, "--base", "HEAD")
    control("guard: an added drifting citation is refused although a same-label base "
            "citation stands", rc == 1 and "a new line citation" in out, out)
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
    write(repo, "crate/src/pins.rs",
          "fn first() {}\n\n// What the test below pins.\n#[test]\nfn t_pin() {}\n")
    write(repo, "docs/src/pins.md", page(f"Pinned at `t_pin` (`{L('crate/src/pins.rs', 3)}`)."))
    stray = "crate/src/stray.rs"
    write(repo, stray,
          "fn first() {}\n\n// other_thing: migrated to Storage trait\nfn second() {}\n\n"
          "struct S {\n    a: u32,\n\n    // b_field: moved away\n    c: u32,\n}\n\n"
          "// third returns nothing.\nfn third() {}\n\n"
          "// other_name: kept for the tests\n#[cfg(test)]\nfn helper() {}\n")
    write(repo, "docs/src/stray.md", page(
        f"Stray: `second` (`{L(stray, 3)}`).",
        f"Field: `S::c` (`{L(stray, 9)}`).",
        f"Named: `third` (`{L(stray, 13)}`).",
        f"Cfg: `helper` (`{L(stray, 16)}`)."))
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
    control("census: a // block directly above a test belongs to that test",
            v("docs/src/pins.md", 0).get("verdict") == "agree"
            and v("docs/src/pins.md", 0).get("item") == "t_pin", out)
    st = "docs/src/stray.md"
    control("census: a comment naming another identifier does not join the item below",
            v(st, 0).get("verdict") in ("drift", "nolabel"), out)
    control("census: a comment naming another identifier does not join the field below",
            v(st, 1).get("verdict") in ("drift", "nolabel"), out)
    control("census: a comment that names its item belongs to it",
            v(st, 2).get("verdict") == "agree", out)
    control("census: `cfg(test)` is no test attribute: a stray comment above it "
            "does not join the item", v(st, 3).get("verdict") in ("drift", "nolabel"), out)
    s = "docs/src/spelled.md"
    control("census: a shortened path the page spells out is placed",
            v(s, 0).get("verdict") == "agree" and v(s, 0).get("how") == "spelled", out)
    f = "docs/src/foreign.md"
    whys = [r.get("why") for r in recs.get(f, [])]
    control("census: foreign absent, other-repo, unplaced; no host:port",
            whys == ["absent", "other-repo", "unplaced"]
            and all(r.get("verdict") == "foreign" for r in recs.get(f, [])), out)
    control("census: exits 0", rc == 0, out)


WAIVE = "LEVICULUM_CITATION_CONTROL_WAIVE"


def history_control(root):
    """The 64 citations of 8482638d6 keep their verdicts. A clone without
    that commit cannot run this control, and a control that cannot look
    fails: only an explicit waiver (`LEVICULUM_CITATION_CONTROL_WAIVE`, for
    a host that knows it holds a shallow clone) turns it into a printed
    WAIVED line."""
    have = subprocess.run(["git", "-C", root, "cat-file", "-e", CONTROL_REV + "^{commit}"],
                          capture_output=True).returncode == 0
    name = "census: the 64 citations 8482638d6 renumbered keep their verdicts"
    if not have:
        why = (f"control 1908 needs commit {CONTROL_REV}, which the clone at {root} does "
               "not have (a shallow clone?)")
        if os.environ.get(WAIVE):
            print(f"WAIVED  {name}: {why}; waived by {WAIVE}={os.environ[WAIVE]}")
            return
        control(name, False, f"{why}. Deepen the clone, or set {WAIVE} to waive it.")
        return
    r = subprocess.run([sys.executable, "-I", CENSUS, "--root", root, "--rev", CONTROL_REV,
                        "--quiet", "--control", CONTROL], capture_output=True, text=True)
    last = [ln for ln in r.stdout.splitlines() if ln.startswith("CONTROL rows=")]
    control(name, r.returncode == 0 and last and "rows=64 held=64" in last[0],
            r.stdout + r.stderr)


def shallow_controls(repo):
    """Both tools in a `git clone --depth 1` of the scratch repository: the
    guard has no base there and the census control no history, and each
    must say so with exit 1 rather than pass having looked at nothing."""
    git(repo, "commit", "-q", "--allow-empty", "-m", "second, so depth 1 cuts something")
    shallow = tempfile.mkdtemp(prefix="citations-selftest-shallow-")
    try:
        subprocess.run(["git", "clone", "-q", "--depth", "1", "file://" + repo, shallow],
                       check=True, capture_output=True)
        env = {k: v for k, v in os.environ.items()
               if k not in ("LEVICULUM_CITATION_BASE", WAIVE)}
        r = subprocess.run([sys.executable, "-I", GUARD, "--root", shallow],
                           capture_output=True, text=True, env=env)
        control("shallow: the guard without a base exits 1 and says why",
                r.returncode == 1 and "shallow" in r.stdout + r.stderr, r.stdout + r.stderr)
        r = subprocess.run([sys.executable, "-I", GUARD, "--root", shallow, "--base", "HEAD"],
                           capture_output=True, text=True, env=env)
        control("shallow: the guard with --base runs", r.returncode == 0, r.stdout + r.stderr)
        me = os.path.abspath(__file__)
        r = subprocess.run([sys.executable, "-I", me, "--history-only", "--root", shallow],
                           capture_output=True, text=True, env=env)
        control("shallow: the selftest without the 1908 commit exits 1",
                r.returncode == 1 and "FAILED" in r.stdout, r.stdout + r.stderr)
        r = subprocess.run([sys.executable, "-I", me, "--history-only", "--root", shallow],
                           capture_output=True, text=True, env={**env, WAIVE: "selftest"})
        control("shallow: the waiver turns it into a printed WAIVED line",
                r.returncode == 0 and f"waived by {WAIVE}=selftest" in r.stdout,
                r.stdout + r.stderr)
    finally:
        subprocess.run(["rm", "-rf", shallow], check=False)


# What the merged-branch control commits into its scratch repository, so
# the guard under test is the TREE'S OWN copy: the merged-parent question
# only exists where the guard's own commit is in the history.
TOOL_FILES = ("citations.py", "check-citations-by-name.py", "citation-census.py",
              "check-python-test-prerequisites.py", "citation-foreign.txt")


def merge_controls():
    """Merges: an untouched drifting citation rides through a merge, and a
    pre-guard branch's stale number does not (order 1940)."""
    repo = tempfile.mkdtemp(prefix="citations-selftest-mergea-")
    try:
        git(repo, "init", "-q")
        write(repo, "crate/Cargo.toml", "[package]\nname = \"c\"\n")
        write(repo, LIB, LIB_RS)
        write(repo, DRIFTY, DRIFTY_PAGE)
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", "base")
        base = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"],
                              capture_output=True, text=True).stdout.strip()
        git(repo, "checkout", "-q", "-b", "side")
        write(repo, LIB, LIB_RS + "\npub fn gamma() {}\n")
        git(repo, "commit", "-qam", "side grows the lib below every item")
        git(repo, "checkout", "-q", "-")
        write(repo, "docs/src/other.md", page("Nothing cited."))
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", "the first parent moves")
        git(repo, "merge", "-q", "--no-edit", "side")
        rc, out = run(GUARD, repo, "--base", base)
        control("guard: a merge with an untouched drifting citation passes", rc == 0, out)
    finally:
        subprocess.run(["rm", "-rf", repo], check=False)

    repo = tempfile.mkdtemp(prefix="citations-selftest-mergeb-")
    try:
        git(repo, "init", "-q")
        write(repo, "crate/Cargo.toml", "[package]\nname = \"c\"\n")
        write(repo, LIB, LIB_RS)
        write(repo, "docs/src/page.md", BASE_PAGE)
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", "base, before the rule exists")
        git(repo, "checkout", "-q", "-b", "pre")
        write(repo, "docs/src/page.md",
              BASE_PAGE + f"\nAlso the first: `alpha` (`{L(LIB, 2)}`).\n")
        git(repo, "commit", "-qam", "a citation written before the rule")
        git(repo, "checkout", "-q", "-")
        for name in TOOL_FILES:
            with open(os.path.join(HERE, name), encoding="utf-8") as fh:
                write(repo, "scripts/" + name, fh.read())
        write(repo, LIB, "// one\n// two\n// three\n" + LIB_RS)
        write(repo, "docs/src/page.md", BASE_PAGE.replace(L(LIB, 2), L(LIB, 5)))
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", "the guard arrives and the lib grows")
        git(repo, "merge", "-q", "--no-edit", "pre")
        r = subprocess.run(
            [sys.executable, "-I", os.path.join(repo, "scripts", "check-citations-by-name.py"),
             "--root", repo], capture_output=True, text=True)
        out = r.stdout + r.stderr
        control("guard: a merged pre-guard branch's stale number is refused",
                r.returncode == 1 and "a new line citation" in out, out)
    finally:
        subprocess.run(["rm", "-rf", repo], check=False)


def foreign_controls():
    """scripts/check-citation-foreign.py refuses a guard that holds a set of
    its own beside the shared file."""
    rc, out = run_plain(FOREIGN)
    control("sets: both guards read the shared file", rc == 0, out)
    rust = os.path.join(ROOT, "leviculum-std", "tests", "doc_citations.rs")
    with open(rust, encoding="utf-8") as fh:
        text = fh.read()
    scratch_dir = tempfile.mkdtemp(prefix="citations-selftest-sets-")
    try:
        own = os.path.join(scratch_dir, "own.rs")
        with open(own, "w", encoding="utf-8") as fh:
            fh.write(text + '\nconst MORE: &[&str] = &["onion"];\n')
        rc, out = run_plain(FOREIGN, "--rust", own)
        control("sets: a literal set beside the shared file is refused",
                rc == 1 and 'holds the literal "onion"' in out, out)
        cut = os.path.join(scratch_dir, "cut.rs")
        with open(cut, "w", encoding="utf-8") as fh:
            fh.write(text.replace("include_str!", "include_bytes!"))
        rc, out = run_plain(FOREIGN, "--rust", cut)
        control("sets: a Rust guard that does not read the shared file is refused",
                rc == 1 and "does not read the shared file" in out, out)
        py = os.path.join(scratch_dir, "own.py")
        with open(os.path.join(HERE, "citations.py"), encoding="utf-8") as fh:
            ptext = fh.read()
        with open(py, "w", encoding="utf-8") as fh:
            fh.write(ptext + '\nMORE = {"lan"}\n')
        rc, out = run_plain(FOREIGN, "--python", py)
        control("sets: a Python literal beside the shared file is refused",
                rc == 1 and 'holds the literal "lan"' in out, out)
    finally:
        subprocess.run(["rm", "-rf", scratch_dir], check=False)


def run_plain(script, *args):
    r = subprocess.run([sys.executable, "-I", script, *args], capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--history-only", action="store_true",
                    help="run only the 1908 history control (the shallow controls use it)")
    ap.add_argument("--root", default=ROOT, help="the clone the history control reads")
    args = ap.parse_args(argv)
    if not args.history_only:
        repo = scratch()
        try:
            guard_controls(repo)
            language_controls(repo)
            census_controls(repo)
            shallow_controls(repo)
        finally:
            subprocess.run(["rm", "-rf", repo], check=False)
        merge_controls()
        foreign_controls()
    history_control(args.root)
    if FAILED:
        print(f"citations selftest: {len(FAILED)} control(s) FAILED: {', '.join(FAILED)}")
        return 1
    print(f"citations selftest: every control holds ({len(HELD)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
