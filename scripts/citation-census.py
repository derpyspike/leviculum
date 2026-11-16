#!/usr/bin/env python3
"""Drift census of the `path:line` citations in living texts.

A line number names whatever stands on that line today. Order 1848 turned
line citations into names by trusting those numbers, and where a number had
drifted it wrote the wrong item's name; its guard stayed green, because a
wrong name resolves just as well as a right one. Before any citation is
converted, this census says which numbers still point at what their text
says they point at.

For every `path:line` citation in a living text (scripts/citations.py,
`is_living`) it reads

- the LABEL the text gives the citation (`label_for`): an identifier
  before it in its own code span (`emission_secs, <path>:<n>`), else
  the nearest code span before it in the same sentence
  (`` `emission_secs` (`<path>:<n>`) ``); in a table row, its own cell,
  then the cells before it;
- the ITEM at the cited line: the innermost item whose range (doc comments
  and attributes included) holds the first cited line;

and prints one line per citation:

    CITE file=<text>:<line> at=<path>:<lines> label=<label|-> item=<item|-> verdict=<v> why=<why> how=<how>

with verdict (`judge` has the exact rule)
    agree    the label names an item of the cited file that holds the cited
             line (a range: that the range meets);
    drift    the label names an item of the cited file that does not hold
             the line: the number points at something else;
    nolabel  the text gives no label, or one that names no item of the
             cited file (a variable, an event), so nothing can be compared;
    foreign  the path is no file of this tree as written, or names another
             repository (why=other-repo|unplaced|absent). A base name is
             never resolved to a file of this tree: see `citations.locate`.

`how` says how the path was placed (root|dir|crate|spelled), or for a
foreign one why it was not.

then the counts, and with --table N the N files with the most drift.

    scripts/citation-census.py                     the working tree
    scripts/citation-census.py --rev 8482638d6     a commit, read by git
    scripts/citation-census.py --control F.tsv     each row of F (file, line
                                                   in file, citation,
                                                   verdict) must come out
                                                   with that verdict; exit 1
                                                   otherwise

The census never rewrites anything. Conversion is a later order, in waves
that convert only `agree` lines and list `drift` lines for a hand fix.
"""

import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import citations as C  # noqa: E402

CODE_SPAN = re.compile(r"`([^`]+)`")
KEYWORDS = re.compile(
    r"^(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:async|unsafe|const|extern|static|private|public|"
    r"protected|override|suspend)\s+)*(?:fn|struct|enum|trait|type|mod|static|const|impl|union|"
    r"macro_rules!|class|def|interface|object|fun|let|val|var)\s+"
)
IDENT = re.compile(r"(?:<[A-Za-z0-9_ ]+>::)?[A-Za-z_]\w*(?:(?:::|\.)[A-Za-z_<][\w<> ]*)*")
COMMENT = re.compile(r"^\s*(///|//!|//|#|\*|/\*\*|/\*)")


def label_token(content):
    """The identifier a code span names, or None for a span that is a path,
    a citation, a command line or no identifier at all."""
    c = content.strip().strip(",;:")
    if not c or C.LINE_CITE.search(c) or "/" in c:
        return None
    c = KEYWORDS.sub("", c)
    c = re.sub(r"^(?:self|Self)(?:\.|::)", "", c)
    m = IDENT.match(c)
    if not m:
        return None
    tok = m.group(0).strip().rstrip(".:")
    rest = c[m.end():]
    if re.match(r"\s+[A-Za-z_-]", rest):
        return None  # `just citation-guard`, `cargo test`: a command, not a name
    if re.search(r"\.(?:rs|py|md|toml|sh|kt|java|txt|json|yml|yaml)$", tok):
        return None  # a file name
    return tok or None


def block_of(text, pos, rel):
    """(start, end) of the paragraph, table row or comment block around
    `pos`: the stretch of text a label is looked for in."""
    ls = text.rfind("\n", 0, pos) + 1
    le = text.find("\n", pos)
    le = len(text) if le < 0 else le
    line = text[ls:le]
    if rel.endswith(".md"):
        if line.lstrip().startswith("|"):
            return ls, le
        a = text.rfind("\n\n", 0, pos)
        b = text.find("\n\n", pos)
        return (0 if a < 0 else a + 2), (len(text) if b < 0 else b)
    m = COMMENT.match(line)
    if not m:
        return ls, le
    mark = m.group(1)

    def same(l):
        mm = COMMENT.match(l)
        return mm is not None and mm.group(1) == mark and l.strip() != mark

    a, b = ls, le
    while a > 0:
        pls = text.rfind("\n", 0, a - 1) + 1
        if not same(text[pls:a - 1]):
            break
        a = pls
    while b < len(text):
        nle = text.find("\n", b + 1)
        nle = len(text) if nle < 0 else nle
        if not same(text[b + 1:nle]):
            break
        b = nle
    return a, b


def sentence_of(block, rel_pos, spans_):
    """(start, end) inside `block` of the sentence that holds `rel_pos`; a
    full stop inside a code span does not end one."""
    def in_span(i):
        return any(s <= i < e for s, e in spans_)
    if block.lstrip().startswith("|"):
        return 0, len(block)
    stops = [m.start() for m in re.finditer(r"[.!?;](?=\s|$)", block) if not in_span(m.start())]
    a = max([s + 1 for s in stops if s < rel_pos], default=0)
    b = min([s for s in stops if s >= rel_pos], default=len(block))
    return a, b


def _cells(block):
    """(start, end) of each cell of a table row."""
    bars = [i for i, c in enumerate(block) if c == "|" and (i == 0 or block[i - 1] != "\\")]
    return [(x + 1, y) for x, y in zip(bars, bars[1:])]


def label_for(text, m, rel):
    """The label the text gives the citation `m`, or None.

    In order: an identifier before the citation in its own code span; in a
    table row, a code span of the citation's own cell (before it, then
    after it), then of the cells before it, nearest first; in prose, the
    nearest code span before it in the same sentence. A span after the
    citation in prose, or in a later cell, is not read: it is the next
    thing the text says, not the name of this one."""
    a, b = block_of(text, m.start(), rel)
    block = text[a:b]
    pos = m.start() - a
    spans_ = [(s.start(), s.end()) for s in CODE_SPAN.finditer(block)]
    own = next(((s, e) for s, e in spans_ if s <= pos < e), None)
    if own:
        content = block[own[0] + 1:own[1] - 1]
        cut = pos - own[0] - 1
        for p in reversed([p for p in re.split(r"[,(]|\s[-—–]\s", content[:cut]) if p.strip()]):
            tok = label_token(p)
            if tok:
                return tok
    others = [(s, e) for s, e in spans_ if (s, e) != own]

    def first(cands):
        for s, e in cands:
            tok = label_token(block[s + 1:e - 1])
            if tok:
                return tok
        return None

    if rel.endswith(".md") and block.lstrip().startswith("|"):
        cells = _cells(block)
        mine = next((k for k, (x, y) in enumerate(cells) if x <= pos < y), None)
        if mine is None:
            return None
        x, y = cells[mine]
        tok = first([sp for sp in reversed(others) if x <= sp[0] and sp[1] <= pos]) or \
            first([sp for sp in others if sp[0] > pos and sp[1] <= y])
        if tok:
            return tok
        for x, y in reversed(cells[:mine]):
            tok = first([sp for sp in reversed(others) if x <= sp[0] and sp[1] <= y])
            if tok:
                return tok
        return None
    s0, _ = sentence_of(block, pos, spans_)
    return first([sp for sp in reversed(others) if s0 <= sp[0] and sp[1] <= pos])


def judge(tree, path, a, b, label):
    """(item, verdict, why) for lines a..b of `path` under `label`.

    agree   the label names an item of the file that holds line a (a range:
            that the range meets);
    drift   the label names an item of the file that does not hold the line:
            the number points at something else (why=elsewhere);
    nolabel no label (why=none), or a label that names no item of the cited
            file, a variable, an event, a variant used in a match arm, an
            item of another file (why=not-an-item): there is nothing to
            compare the line with.
    A leading module path in the label (`lnomad::Intent`, `census::session`,
    `crate::x`) is dropped when its segments are the crate, the file or a
    directory on the cited path.
    A label that only stands on the line (a call of the function it names)
    does not agree: converting it would turn a pointer at the call into a
    pointer at the definition."""
    found = tree.items(path) or []
    nlines = (tree.text(path) or "").count("\n") + 1
    holding = [it for it in found if it.kind != "anchor" and it.start <= a <= it.end]
    inner = min(holding, key=lambda it: (it.end - it.start, -it.start)) if holding else None
    item = inner.label() if inner else ("past-end" if a > nlines else "-")
    if label is None:
        return item, "nolabel", "none"
    segs = C.split_item(label)
    named = [it for it in found if any(C.matches(it, sg) for sg in _label_forms(segs, path))]
    if any(it.start <= b and a <= it.end for it in named):
        return item, "agree", "holds"
    if named:
        return item, "drift", "elsewhere"
    return item, "nolabel", "not-an-item"


def _label_forms(segs, path):
    """`segs`, and `segs` without each leading module segment that the cited
    path itself names (crate directory, directories, file stem)."""
    parts = path.split("/")
    mods = {"crate", "self", "super"} | {p.replace("-", "_") for p in parts[:-1]}
    mods.add(os.path.splitext(parts[-1])[0])
    forms = [segs]
    k = 0
    while k < len(segs) - 1 and segs[k] in mods:
        k += 1
        forms.append(segs[k:])
    return forms


def census(tree, only=None):
    """One record per line citation in the living texts of `tree`."""
    out = []
    files = sorted(f for f in tree.files if C.is_living(f))
    if only:
        files = [f for f in files if f in only]
    for rel in files:
        text = tree.text(rel)
        if text is None:
            continue
        for m in C.line_citations(text, rel):
            kind, path, how = C.locate(tree, m.group(1), rel, text)
            if kind == "notfile":
                continue
            doc_line = text.count("\n", 0, m.start()) + 1
            label = label_for(text, m, rel)
            a, b = C.spans(m.group(2))[0]
            if kind == "foreign":
                rec = dict(file=rel, line=doc_line, cite=m.group(0), at=m.group(0),
                           label=label, item="-", verdict="foreign", how=how, why=how)
            else:
                item, verdict, why = judge(tree, path, a, b, label)
                rec = dict(file=rel, line=doc_line, cite=m.group(0),
                           at=f"{path}:{m.group(2)}", label=label, item=item,
                           verdict=verdict, how=how, why=why)
            out.append(rec)
    return out


def fmt(r):
    return (f"CITE file={r['file']}:{r['line']} at={r['at']} label={r['label'] or '-'} "
            f"item={r['item'].replace(' ', '')} verdict={r['verdict']} why={r['why']} how={r['how']}")


VERDICTS = ("agree", "drift", "nolabel", "foreign")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=C.REPO_ROOT)
    ap.add_argument("--rev", help="read the tree of this commit instead of the working tree")
    ap.add_argument("--only", nargs="*", help="only these living texts")
    ap.add_argument("--table", type=int, default=0, help="print the N files with most drift")
    ap.add_argument("--quiet", action="store_true", help="counts and table only")
    ap.add_argument("--control", help="TSV of file, line, citation, verdict that must hold")
    args = ap.parse_args(argv)

    tree = C.Tree(args.root, args.rev)
    only = set(args.only) if args.only else None
    rows = []
    if args.control:
        with open(args.control, encoding="utf-8") as fh:
            rows = [ln.rstrip("\n").split("\t") for ln in fh if ln.strip() and not ln.startswith("#")]
        only = {r[0] for r in rows}
    recs = census(tree, only)
    if not args.quiet:
        for r in recs:
            print(fmt(r))
    counts = {v: sum(1 for r in recs if r["verdict"] == v) for v in VERDICTS}
    print("CENSUS total=%d " % len(recs) + " ".join(f"{v}={counts[v]}" for v in VERDICTS))
    if args.table:
        per = {}
        for r in recs:
            per.setdefault(r["file"], dict.fromkeys(VERDICTS, 0))[r["verdict"]] += 1
        rows = sorted(per.items(), key=lambda kv: (-kv[1]["drift"], kv[0]))[:args.table]
        print("| file | drift | agree | nolabel | foreign |")
        print("|---|---|---|---|---|")
        for f, c in rows:
            print(f"| {f} | {c['drift']} | {c['agree']} | {c['nolabel']} | {c['foreign']} |")
    if args.control:
        by = {(r["file"], r["line"], r["cite"]): r for r in recs}
        bad = 0
        for f, line, cite, want in rows:
            r = by.get((f, int(line), cite))
            if r is None or r["verdict"] != want:
                bad += 1
                print(f"CONTROL want={want} {f}:{line} {cite} -> "
                      f"{fmt(r) if r else 'no such citation'}")
        got = {v: sum(1 for x in rows if x[3] == v) for v in VERDICTS}
        print(f"CONTROL rows={len(rows)} held={len(rows) - bad} broken={bad} want "
              + " ".join(f"{v}={got[v]}" for v in VERDICTS if got[v]))
        return 1 if bad else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
