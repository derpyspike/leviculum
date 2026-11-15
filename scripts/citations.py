#!/usr/bin/env python3
"""Citations by name: the resolver every citation tool shares.

A living text (a page under docs/src, a code comment, a README, a comment in
the Justfile or a script) cites code by NAME, never by line number
(docs/src/concepts/checks-and-citations.md, "Citations name items"). The
written form is

    <path>#<Item>                 an item of the file
    <path>#<Type>::<method>       a qualified item
    <path>#anchor:<name>          a named span (// ANCHOR: <name>)

with `path` relative to the repository root. This module answers, for such a
citation, whether the item exists exactly once and on which lines it stands
today; the guard (scripts/check-name-citations.py), the book preprocessor
(scripts/mdbook-citations.py) and the converter
(scripts/convert-line-citations.py) all ask it.

HOW ITEMS ARE FOUND. No parser dependency: each language is read by a small
scanner over the text, tested in scripts/citations-selftest.py.

- Rust, Java, Kotlin, C: comments and literals are blanked first (the
  `mask` of scripts/check-python-test-prerequisites.py, loaded from there so
  there is one copy), then every brace pair is classified by the header in
  front of it. Rust items: fn, struct, enum, union, trait, mod, macro_rules,
  and the brace-less const, static, type, unit or tuple struct, `mod x;` and
  trait method declaration. Methods are `Type::method`; a method of a trait
  impl is also `<Type as Trait>::method`, which tells two `fmt`s apart. Enum
  variants and named struct fields are `Enum::Variant` and `Struct::field`.
  Items inside a fn body are not items: a citation of one names the fn.
  Java and Kotlin: class, interface, enum, object, record, and methods
  (`Class.method` or `Class::method`, the two separators are one).
- Python: def and class by indentation (`Class.method`), and module-level
  assignments (`NAME`).
- Shell: functions, and top-level `NAME=` assignments.
- Justfile: recipes and `name := ...` variables. A recipe's range begins at
  the comment block and attributes directly above it, which is where its
  documentation stands.
- Markdown: headings, by the slug mdBook gives them.
- TOML: tables (`dependencies`, `workspace.dependencies`) and their keys
  (`package.version`).
- Any file: `ANCHOR: name` ... `ANCHOR_END: name` in a comment, mdBook's
  own anchor syntax, cited as `anchor:name`.

An item's range starts at its doc comments and attributes, so a citation that
pointed at the doc comment of a function names that function.

A citation `A::B` matches an item when `A::B` is a suffix, segment by
segment, of one of the item's qualified names. It must match exactly one.
"""

import importlib.util
import os
import re
import subprocess
from dataclasses import dataclass, field

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

_SPEC = importlib.util.spec_from_file_location(
    "check_python_test_prerequisites",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "check-python-test-prerequisites.py"),
)
_PREREQ = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_PREREQ)
mask = _PREREQ.mask
header_start = _PREREQ.header_start


@dataclass
class Item:
    names: list  # each a list of segments, outermost first
    kind: str
    start: int  # first line (1-based), doc comments and attributes included
    end: int  # last line
    head: int  # the line the item's keyword or name stands on
    children: list = field(default_factory=list)
    # A member (method, field, variant) is always cited with its owner,
    # `Type::method`: a bare `new` would turn ambiguous with the next
    # type's `new` in the same file, and the guard would ask for a rewrite.
    min_segs: int = 1

    def label(self):
        return "::".join(self.names[0])


# --------------------------------------------------------------------------
# Citation spelling

NAME_CITE = re.compile(
    r"(?<![\w./-])((?:\.githooks/[A-Za-z0-9_-]+)|(?:[A-Za-z0-9_.][A-Za-z0-9_.-]*/)*"
    r"(?:[A-Za-z0-9_][A-Za-z0-9_.-]*\.[A-Za-z]+|Justfile))"
    r"#((?:[a-z]+:)?(?:<[A-Za-z0-9_ ]+>|[A-Za-z_])(?:[A-Za-z0-9_/-]|::|\.(?=[A-Za-z_<])|<[A-Za-z0-9_ ]+>)*)"
)


def split_item(spec):
    """`Type::method`, `Class.method`, `<T as Tr>::f` -> segments."""
    if spec.startswith("anchor:"):
        return [spec]
    out, depth, cur, i = [], 0, "", 0
    while i < len(spec):
        c = spec[i]
        if c == "<":
            depth += 1
        elif c == ">":
            depth -= 1
        if depth == 0 and spec.startswith("::", i):
            out.append(cur)
            cur, i = "", i + 2
            continue
        if depth == 0 and c == ".":
            out.append(cur)
            cur, i = "", i + 1
            continue
        cur += c
        i += 1
    out.append(cur)
    return [s.strip() for s in out if s.strip()]


# --------------------------------------------------------------------------
# Shared helpers

def _line_starts(text):
    starts = [0]
    for i, c in enumerate(text):
        if c == "\n":
            starts.append(i + 1)
    return starts


def _line_of(starts, off):
    lo, hi = 0, len(starts) - 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if starts[mid] <= off:
            lo = mid
        else:
            hi = mid - 1
    return lo + 1


def _extend_up(lines, head, prefixes):
    """The first line of the doc-comment and attribute block above `head`."""
    start = head
    k = head - 1
    depth = 0
    while k >= 1:
        s = lines[k - 1].strip()
        if depth > 0:
            # inside a multi-line attribute, walking up to its `#[`
            depth += s.count("]") - s.count("[")
            start = k
            if s.startswith("#[") or s.startswith("#!["):
                depth = 0
            k -= 1
            continue
        if any(s.startswith(p) for p in prefixes):
            start = k
            if s.startswith("#[") and s.count("[") > s.count("]"):
                pass
            k -= 1
            continue
        if s.endswith(")]") or s == "]" or s.endswith(")]\n"):
            # end of a multi-line attribute
            depth = s.count("]") - s.count("[")
            if depth <= 0:
                break
            start = k
            k -= 1
            continue
        break
    return start


# --------------------------------------------------------------------------
# Brace languages (Rust, Java, Kotlin, C)

RUST_PREFIX = r"^[ \t]*(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:const|async|unsafe|default|extern(?:\s+\"[^\"]*\")?)\s+)*"
R_FN = re.compile(r"\bfn\s+(\w+)")
R_TYPE = re.compile(r"\b(struct|enum|union|trait)\s+(\w+)")
R_MOD = re.compile(r"\bmod\s+(\w+)\s*$")
R_IMPL = re.compile(r"(?:^|[\s;])impl\b")
R_MACRO = re.compile(r"\bmacro_rules!\s*(\w+)")
R_BRACELESS = re.compile(
    RUST_PREFIX + r"(?:(const|static)\s+(?:mut\s+)?([A-Za-z_]\w*)\s*:"
    r"|(type)\s+(\w+)"
    r"|(struct)\s+(\w+)"
    r"|(mod)\s+(\w+)\s*;"
    r"|(fn)\s+(\w+))",
    re.M,
)
J_TYPE = re.compile(r"\b(class|interface|enum|object|record)\s+(\w+)")
J_METHOD = re.compile(r"(?:\bfun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?(\w+)\s*\(|\b(\w+)\s*\([^;{}]*\)[^;{}()=]*$)")
J_NOT_METHOD = {"if", "for", "while", "switch", "catch", "synchronized", "when", "try", "else",
                "do", "return", "new", "throw", "super", "this", "init", "sizeof", "foreach"}


def _impl_names(header):
    """(self type, trait or None) of an impl header."""
    body = header[R_IMPL.search(header).end():]
    body = re.split(r"\bwhere\b", body)[0]
    prev = None
    while prev != body:
        prev, body = body, re.sub(r"<[^<>]*>", "", body)
    trait = None
    if re.search(r"\bfor\b", body):
        tr, body = re.split(r"\bfor\b", body, maxsplit=1)
        tnames = re.findall(r"\w+", tr.replace("dyn", ""))
        trait = tnames[-1] if tnames else None
    names = re.findall(r"\w+", body.replace("dyn ", ""))
    return (names[-1] if names else "?"), trait


def _brace_scopes(masked):
    stack, scopes = [], []
    for i, c in enumerate(masked):
        if c == "{":
            stack.append([header_start(masked, i), i, None, len(stack)])
        elif c == "}" and stack:
            e = stack.pop()
            e[2] = i
            scopes.append(e)
    return scopes


def _rust_items(text):
    masked = mask(text)
    lines = text.split("\n")
    starts = _line_starts(text)
    line = lambda off: _line_of(starts, off)  # noqa: E731
    scopes = _brace_scopes(masked)
    # classify
    nodes = []  # (open, close, kind, name, trait, h0)
    for h0, op, cl, _ in scopes:
        header = masked[h0:op]
        hs = header.strip()
        kind = name = trait = None
        m = R_FN.search(header)
        if header.count("(") != header.count(")") or header.count("[") != header.count("]"):
            # a `{` inside an argument list (a const-generic block, a
            # closure argument): no item opens here
            kind = "block"
        elif m and not re.search(r"\|[^|]*\|\s*$", header) and "=" not in header[:m.start()].split("\n")[-1]:
            kind, name = "fn", m.group(1)
        elif R_MACRO.search(header):
            kind, name = "macro", R_MACRO.search(header).group(1)
        elif R_TYPE.search(header) and not R_IMPL.search(header):
            mt = R_TYPE.search(header)
            kind, name = mt.group(1), mt.group(2)
        elif R_MOD.search(hs):
            kind, name = "mod", R_MOD.search(hs).group(1)
        elif R_IMPL.search(header) and not re.search(r"[=(]\s*impl\b", header):
            kind = "impl"
            name, trait = _impl_names(header)
        elif re.search(r"\bextern\s+\"[^\"]*\"\s*$", hs):
            kind = "extern"
        else:
            kind = "block"
        nodes.append([op, cl, kind, name, trait, h0])
    nodes.sort()

    def chain_at(off):
        return [n for n in nodes if n[0] < off < n[1]]

    items = []
    item_level = {"mod", "impl", "trait", "extern"}

    def qualify(chain):
        """Prefixes (alternatives) for an item inside `chain`."""
        prefixes = [[]]
        for n in chain:
            if n[2] == "mod" or n[2] == "trait":
                prefixes = [p + [n[3]] for p in prefixes]
            elif n[2] == "impl":
                alt = [p + [n[3]] for p in prefixes]
                if n[4]:
                    alt += [p + [f"<{n[3]} as {n[4]}>"] for p in prefixes]
                prefixes = alt
        return prefixes

    def is_item_context(chain):
        return all(n[2] in item_level for n in chain)

    def add(name, kind, head_off, end_off, chain):
        head = line(head_off)
        end = line(end_off)
        start = _extend_up(lines, head, ("///", "//!", "#[", "/**", "*", "*/"))
        # `//!` belongs to the enclosing module, never to the item below it.
        while start < head and lines[start - 1].strip().startswith("//!"):
            start += 1
        names = [p + [name] for p in qualify(chain)]
        it = Item(names, kind, start, end, head)
        if chain and chain[-1][2] in ("impl", "trait"):
            it.min_segs = 2
        items.append(it)
        return it

    for op, cl, kind, name, trait, h0 in nodes:
        if kind == "impl" and trait:
            chain = chain_at(op)
            if is_item_context(chain):
                hdr = masked[h0:op]
                mk = R_IMPL.search(hdr)
                add(f"<{name} as {trait}>", "impl", h0 + mk.start() + (mk.group(0)[0] != "i"), cl, chain)
            continue
        if kind in ("block", "impl", "extern") or name is None:
            continue
        chain = chain_at(op)
        if not is_item_context(chain):
            continue
        # the keyword's own offset, so the head line is the signature's
        hdr = masked[h0:op]
        kw = {"fn": R_FN, "macro": R_MACRO, "mod": re.compile(r"\bmod\s+\w+")}.get(kind, R_TYPE)
        mk = kw.search(hdr)
        head_off = h0 + (mk.start() if mk else len(hdr) - len(hdr.lstrip()))
        # a tuple struct with a body after `where`? rare; struct with `;` is braceless
        it = add(name, kind, head_off, cl, chain)
        if kind in ("enum", "struct", "union"):
            _members(masked, text, op, cl, it, lines, line)

    for m in R_BRACELESS.finditer(masked):
        groups = [g for g in m.groups()]
        pairs = [(groups[i], groups[i + 1]) for i in range(0, len(groups), 2)]
        kw, name = next((k, n) for k, n in pairs if k)
        head_off = m.start() + len(m.group(0)) - len(m.group(0).lstrip())
        chain = chain_at(head_off)
        if not is_item_context(chain):
            continue
        if kw in ("fn", "struct"):
            # a declaration only when a `;` comes before any `{` outside
            # parentheses and brackets (`[u8; 16]` is not its end)
            depth, j = 0, m.end()
            while j < len(masked) and not (depth == 0 and masked[j] in "{;"):
                depth += {"(": 1, "[": 1, ")": -1, "]": -1}.get(masked[j], 0)
                j += 1
            if j >= len(masked) or masked[j] == "{":
                continue
        # end: the `;` at bracket depth 0 from the keyword on
        depth, j = 0, head_off
        while j < len(masked):
            c = masked[j]
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
                if depth < 0:
                    break
            elif c == ";" and depth == 0:
                break
            j += 1
        kind = {"const": "const", "static": "static", "type": "type", "struct": "struct",
                "mod": "mod", "fn": "fn"}[kw]
        add(name, kind, head_off, j, chain)

    anchors(lines, items)
    return _nest(items)


def _members(masked, text, op, cl, parent, lines, line):
    """Variants of an enum, fields of a struct: entries at depth 1."""
    depth, j, entry_start = 0, op + 1, op + 1
    entries = []
    while j < cl:
        c = masked[j]
        if c in "([{<" and not (c == "<" and masked[j - 1] == "-"):
            depth += 1
        elif c in ")]}>" and not (c == ">" and masked[j - 1] in "-="):
            depth -= 1
        elif c == "," and depth == 0:
            entries.append((entry_start, j))
            entry_start = j + 1
        j += 1
    if masked[entry_start:cl].strip():
        entries.append((entry_start, cl - 1))
    for a, b in entries:
        seg = masked[a:b + 1]
        # skip attributes, find the name
        body = re.sub(r"#\s*\[[^\]]*\]", lambda m: " " * len(m.group(0)), seg)
        m = re.search(r"(?:pub(?:\s*\([^)]*\))?\s+)?([A-Za-z_]\w*)", body)
        if not m:
            continue
        name = m.group(1)
        if parent.kind != "enum":
            after = body[m.end():].lstrip()
            if not after.startswith(":"):
                continue  # tuple struct field
        head = line(a + m.start(1))
        end = line(b)
        start = _extend_up(lines, head, ("///", "#[", "/**", "*", "*/"))
        names = [n + [name] for n in parent.names]
        parent.children.append(Item(names, "variant" if parent.kind == "enum" else "field",
                                    max(start, parent.head + 1), end, head, min_segs=2))


def _arity(header):
    """Parameters of the first argument list in `header`."""
    i = header.find("(")
    if i < 0:
        return 0
    depth, j, commas, body = 0, i, 0, ""
    while j < len(header):
        c = header[j]
        if c in "(<[":
            depth += 1
        elif c in ")>]":
            depth -= 1
            if depth == 0:
                break
        elif c == "," and depth == 1:
            commas += 1
        if depth >= 1 and j > i:
            body += c
        j += 1
    return commas + 1 if body.strip() else 0


def _java_items(text):
    masked = mask(text)
    lines = text.split("\n")
    starts = _line_starts(text)
    line = lambda off: _line_of(starts, off)  # noqa: E731
    scopes = sorted(_brace_scopes(masked), key=lambda s: s[1])
    nodes = []
    for h0, op, cl, _ in scopes:
        header = masked[h0:op]
        if header.count("(") != header.count(")"):
            nodes.append([op, cl, "block", None, h0])
            continue
        last = header.strip().split("\n")
        tail = " ".join(x.strip() for x in last)
        kind = name = None
        mt = J_TYPE.search(tail)
        if mt and not re.search(r"\bnew\b", tail):
            kind, name = "type", mt.group(2)
        else:
            mm = J_METHOD.search(tail)
            if mm:
                name = mm.group(1) or mm.group(2)
                if name and name not in J_NOT_METHOD and not re.search(r"(^|[^\w])(=|return)\s", tail.split(name)[0][-60:] if name in tail else ""):
                    kind = "fn"
                else:
                    name = None
        if kind is None:
            kind = "block"
        nodes.append([op, cl, kind, name, h0])
    items = []
    for op, cl, kind, name, h0 in nodes:
        if kind == "block":
            continue
        chain = [n for n in nodes if n[0] < op < n[1]]
        if any(n[2] != "type" for n in chain):
            continue
        prefix = [n[3] for n in chain]
        hdr = masked[h0:op]
        off = hdr.find(name)
        head = line(h0 + (off if off >= 0 else 0))
        start = _extend_up(lines, head, ("@", "/**", "*", "*/", "///"))
        names = [prefix + [name]]
        if kind == "fn":
            # Java overloads: `verb/7` is the `verb` that takes seven
            names.append(prefix + [f"{name}/{_arity(hdr[off:])}"])
        items.append(Item(names, kind, start, line(cl), head, min_segs=min(2, len(prefix) + 1)))
    anchors(lines, items)
    return _nest(items)


# --------------------------------------------------------------------------
# Indentation and line languages

def _python_items(text):
    """def, class and module-level assignments, by Python's own parser; the
    indentation scanner below only for a file that does not parse."""
    import ast
    try:
        tree = ast.parse(text)
    except (SyntaxError, ValueError):
        return _python_items_by_indent(text)
    items = []

    def visit(body, prefix):
        for node in body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                start = min([node.lineno] + [d.lineno for d in node.decorator_list])
                kind = "class" if isinstance(node, ast.ClassDef) else "def"
                items.append(Item([prefix + [node.name]], kind, start, node.end_lineno, node.lineno,
                                  min_segs=min(2, len(prefix) + 1)))
                visit(node.body, prefix + [node.name])
            elif not prefix and isinstance(node, (ast.Assign, ast.AnnAssign)):
                targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                for t in targets:
                    if isinstance(t, ast.Name):
                        items.append(Item([[t.id]], "const", node.lineno, node.end_lineno, node.lineno))

    visit(tree.body, [])
    anchors(text.split("\n"), items)
    return _nest(items)


def _python_items_by_indent(text):
    lines = text.split("\n")
    items = []
    stack = []  # (indent, item)
    sig = [(i + 1, len(l) - len(l.lstrip())) for i, l in enumerate(lines)
           if l.strip() and not l.strip().startswith("#")]

    def block_end(idx, indent):
        """Last significant line after sig[idx] that is indented deeper."""
        end = sig[idx][0]
        for n, ind in sig[idx + 1:]:
            s = lines[n - 1].lstrip()
            if ind <= indent and not s.startswith((")", "]", "}")):
                break
            end = n
        return end

    in_str = None
    for idx, (n, ind) in enumerate(sig):
        s = lines[n - 1].strip()
        if in_str:
            if in_str in s:
                in_str = None
            continue
        m = re.match(r"(?:async\s+)?(def|class)\s+(\w+)", s)
        while stack and stack[-1][0] >= ind:
            stack.pop()
        if m:
            name = m.group(2)
            prefix = [st[1].names[0][-1] for st in stack]
            head = n
            start = n
            k = n - 1
            while k >= 1 and lines[k - 1].strip().startswith("@"):
                start = k
                k -= 1
            it = Item([prefix + [name]], m.group(1), start, block_end(idx, ind), head,
                      min_segs=min(2, len(prefix) + 1))
            items.append(it)
            stack.append((ind, it))
            continue
        if ind == 0:
            a = re.match(r"([A-Za-z_]\w*)\s*(?::[^=]+)?=(?!=)", s)
            if a:
                items.append(Item([[a.group(1)]], "const", n, block_end(idx, 0), n))
        for q in ('"""', "'''"):
            if s.count(q) == 1:
                in_str = q
    anchors(lines, items)
    return _nest(items)


def _shell_items(text):
    lines = text.split("\n")
    items = []
    for i, l in enumerate(lines):
        m = re.match(r"^(\s*)(?:function\s+)?([A-Za-z_][\w-]*)\s*\(\)\s*\{?\s*$", l) or \
            re.match(r"^(\s*)function\s+([A-Za-z_][\w-]*)\s*\{?\s*$", l)
        if m:
            indent = m.group(1)
            end = i + 1
            for j in range(i + 1, len(lines)):
                if re.match(re.escape(indent) + r"\}", lines[j]) and \
                        len(lines[j]) - len(lines[j].lstrip()) == len(indent):
                    end = j + 1
                    break
            start = i + 1
            items.append(Item([[m.group(2)]], "fn", start, end, i + 1))
            continue
        a = re.match(r"^(?:readonly\s+|export\s+|declare\s+-\w+\s+)?([A-Z_][A-Z0-9_]*)=", l)
        if a:
            end = i + 1
            if l.rstrip().endswith("(") or l.count("(") > l.count(")"):
                for j in range(i + 1, len(lines)):
                    if lines[j].strip().startswith(")"):
                        end = j + 1
                        break
            items.append(Item([[a.group(1)]], "const", i + 1, end, i + 1))
    anchors(lines, items)
    return _nest(items)


def _justfile_items(text):
    lines = text.split("\n")
    items = []
    for i, l in enumerate(lines):
        v = re.match(r"^(?:export\s+)?([A-Za-z_][\w-]*)\s*:=", l)
        r = re.match(r"^@?([A-Za-z_][\w-]*)(?:\s+[^:]*)?:(?!=)", l)
        if v:
            items.append(Item([[v.group(1)]], "var", i + 1, i + 1, i + 1))
            continue
        if not r or l.startswith(("set ", "import ", "mod ", "alias ")):
            continue
        end = i + 1
        for j in range(i + 1, len(lines)):
            if lines[j].startswith((" ", "\t")):
                end = j + 1
            elif lines[j].strip() == "":
                continue
            else:
                break
        start = i + 1
        k = i
        while k >= 1 and lines[k - 1].strip() and lines[k - 1].lstrip()[0] in "#[":
            start = k
            k -= 1
        items.append(Item([[r.group(1)]], "recipe", start, end, i + 1))
    anchors(lines, items)
    return _nest(items)


def slug(heading):
    """mdBook's heading id."""
    s = re.sub(r"<[^>]*>", "", heading).strip().lower()
    s = "".join(c for c in s if c.isalnum() or c in " -_")
    return s.replace(" ", "-")


def _markdown_items(text):
    lines = text.split("\n")
    heads = []
    fence = False
    for i, l in enumerate(lines):
        if l.lstrip().startswith(("```", "~~~")):
            fence = not fence
            continue
        m = None if fence else re.match(r"^(#{1,6})\s+(.*?)\s*#*\s*$", l)
        if m:
            heads.append((i + 1, len(m.group(1)), slug(m.group(2))))
    items = []
    seen = {}
    for k, (n, level, sl) in enumerate(heads):
        end = len(lines)
        for n2, level2, _ in heads[k + 1:]:
            if level2 <= level:
                end = n2 - 1
                break
        name = sl if sl not in seen else f"{sl}-{seen[sl]}"
        seen[sl] = seen.get(sl, 0) + 1
        items.append(Item([[name]], "heading", n, end, n))
    anchors(lines, items)
    return _nest(items, nest_children=False)


def _toml_items(text):
    lines = text.split("\n")
    items = []
    table, table_item = [], None
    sig = [i for i, l in enumerate(lines) if l.strip() and not l.strip().startswith("#")]

    def until_next(i, pred):
        end = i + 1
        for j in sig:
            if j <= i:
                continue
            if pred(lines[j]):
                break
            end = j + 1
        return end

    for i in sig:
        l = lines[i]
        t = re.match(r"^\s*\[\[?\s*([^\]]+?)\s*\]\]?\s*(#.*)?$", l)
        if t:
            table = [p.strip().strip('"') for p in t.group(1).split(".")]
            end = until_next(i, lambda x: re.match(r"^\s*\[", x))
            table_item = Item([table[:]], "table", i + 1, end, i + 1)
            items.append(table_item)
            continue
        k = re.match(r"^\s*([A-Za-z0-9_\-\"]+(?:\.[A-Za-z0-9_\-\"]+)*)\s*=", l)
        if k:
            key = [p.strip('"') for p in k.group(1).split(".")]
            end = until_next(i, lambda x: re.match(r"^\s*(\[|[A-Za-z0-9_\-\"]+(\.[A-Za-z0-9_\-\"]+)*\s*=)", x))
            it = Item([table + key], "key", i + 1, end, i + 1, min_segs=min(2, len(table) + len(key)))
            if table_item:
                table_item.children.append(it)
            else:
                items.append(it)
    anchors(lines, items)
    return _nest(items)


ANCHOR = re.compile(r"\bANCHOR:\s*([A-Za-z0-9_-]+)")
ANCHOR_END = re.compile(r"\bANCHOR_END:\s*([A-Za-z0-9_-]+)")


def anchors(lines, items):
    open_ = {}
    for i, l in enumerate(lines):
        m = ANCHOR.search(l)
        if m:
            open_.setdefault(m.group(1), []).append(i + 1)
        m = ANCHOR_END.search(l)
        if m and open_.get(m.group(1)):
            a = open_[m.group(1)].pop()
            items.append(Item([["anchor:" + m.group(1)]], "anchor", a, i + 1, a))


def _nest(items, nest_children=True):
    """Every item, children of struct/enum/table flattened in, and each
    item's `children` set to the items nested directly inside it."""
    flat = []
    for it in items:
        flat.append(it)
        flat.extend(it.children)
    if nest_children:
        for it in flat:
            if it.kind in ("variant", "field", "key", "anchor"):
                continue
            if it.kind not in ("struct", "enum", "union", "table"):
                it.children = [c for c in flat if c is not it and c.kind != "anchor"
                               and it.start <= c.start and c.end <= it.end
                               and (c.end - c.start) < (it.end - it.start)]
    return flat


LANGS = {
    "rs": _rust_items,
    "java": _java_items, "kt": _java_items, "kts": _java_items,
    "c": _java_items, "h": _java_items, "cpp": _java_items, "ino": _java_items,
    "py": _python_items,
    "sh": _shell_items, "bash": _shell_items,
    "md": _markdown_items,
    "toml": _toml_items,
}


def language(path):
    base = os.path.basename(path)
    if base == "Justfile" or base.endswith(".just"):
        return _justfile_items
    if path.startswith(".githooks/"):
        return _shell_items
    ext = base.rsplit(".", 1)[-1] if "." in base else ""
    return LANGS.get(ext)


_CACHE = {}


def items_of(path, root=REPO_ROOT):
    """Every item of the file at repo-relative `path`, or None when the
    file is absent or its type has no scanner (then only anchors count)."""
    key = (root, path)
    if key in _CACHE:
        return _CACHE[key]
    full = os.path.join(root, path)
    try:
        with open(full, encoding="utf-8") as fh:
            text = fh.read()
    except (OSError, UnicodeDecodeError):
        _CACHE[key] = None
        return None
    scan = language(path)
    if scan is None:
        found = []
        anchors(text.split("\n"), found)
    else:
        found = scan(text)
    _CACHE[key] = found
    return found


def matches(item, segs):
    for name in item.names:
        if len(segs) <= len(name) and name[len(name) - len(segs):] == segs:
            return True
    return False


def resolve(path, spec, root=REPO_ROOT):
    """(status, items) for `path#spec`: status is "ok" (exactly one item),
    "missing-file", "unknown" (no item of that name), or "ambiguous"."""
    found = items_of(path, root)
    if found is None:
        if not os.path.isfile(os.path.join(root, path)):
            return "missing-file", []
        found = []
    kind = None
    km = re.match(r"(fn|field|variant|struct|enum|const|static|type|mod|trait|macro):(.*)", spec)
    if km:
        kind, spec = km.group(1), km.group(2)
    segs = split_item(spec)
    hits = [it for it in found if matches(it, segs) and (kind is None or it.kind == kind)]
    exact = [it for it in hits if segs in it.names]
    if exact:
        hits = exact
    # one item seen twice (the impl alias) is still one item
    uniq = []
    for h in hits:
        if not any(h is u for u in uniq):
            uniq.append(h)
    if not uniq:
        return "unknown", []
    if len(uniq) > 1:
        return "ambiguous", uniq
    return "ok", uniq


def shortest_name(item, path, root=REPO_ROOT):
    """The shortest spelling that resolves to `item` alone in `path`; a
    kind prefix (`field:Link::x`) only when the name is shared with an item
    of another kind. None when no spelling does."""
    best = None
    # Rust and C spell a path with `::`; everything else with `.`
    sep = "::" if path.endswith((".rs", ".c", ".h", ".cpp", ".ino")) else "."
    for prefix in ("", item.kind + ":"):
        for name in item.names:
            for k in range(min(item.min_segs, len(name)), len(name) + 1):
                cand = prefix + sep.join(name[len(name) - k:])
                status, hits = resolve(path, cand, root)
                if status == "ok" and hits[0] is item:
                    if best is None or (k, len(cand)) < best[0]:
                        best = ((k, len(cand)), cand)
                    break
        if best:
            return best[1]
    return None


def enclosing(path, a, b, root=REPO_ROOT):
    """The item lines a..b of `path` cite, as ("ok", item) or
    ("none" | "several", reason)."""
    found = items_of(path, root)
    if not found:
        return "none", "no item in the file"
    # a range that begins or ends on a blank line cites the text between
    with open(os.path.join(root, path), encoding="utf-8") as fh:
        text_lines = fh.read().split("\n")
    blank = lambda n: 1 <= n <= len(text_lines) and not text_lines[n - 1].strip()  # noqa: E731
    while a < b and blank(a):
        a += 1
    while b > a and blank(b):
        b -= 1
    cands = [it for it in found if it.kind != "anchor" and it.start <= a and b <= it.end]
    if not cands:
        over = [it for it in found if it.kind != "anchor" and not (it.end < a or b < it.start)]
        if over:
            return "several", "spans " + ", ".join(sorted({it.label() for it in over})[:4])
        return "none", "lines lie in no item"
    best = min(cands, key=lambda it: (it.end - it.start, -it.start))
    if best.kind in ("struct", "enum", "union", "table", "heading"):
        return "ok", best
    if a <= best.head:
        return "ok", best
    kids = [c for c in best.children if not (c.end < a or b < c.start)]
    if len(kids) >= 2:
        return "several", "spans " + ", ".join(sorted({c.label() for c in kids})[:4])
    return "ok", best


# --------------------------------------------------------------------------
# Line-number citations, the form living texts no longer use

LINE_CITE = re.compile(
    r"(?<![\w./-])((?:\.githooks/[A-Za-z0-9_-]+)|(?:[A-Za-z0-9_.][A-Za-z0-9_.-]*/)*"
    r"(?:[A-Za-z0-9_][A-Za-z0-9_.-]*\.[A-Za-z]+|Justfile))"
    r":(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)(?![\d\w])"
)

# `host.tld:port` and similar, never a file (the list of
# leviculum-std/tests/doc_citations.rs#NON_FILE_EXTENSIONS).
NON_FILE_EXTENSIONS = {"com", "net", "org", "io", "dev", "local", "onion", "i2p", "de",
                       "network", "example", "invalid", "localhost", "lan", "internal"}

# Trees outside this repository: their citations cannot be resolved from
# here (leviculum-std/tests/doc_citations.rs#EXTERNAL_PREFIXES).
EXTERNAL_PREFIXES = ("periculum/", "ble-reticulum/", "columba/", "Sideband/", "LXMF-Tools/",
                     "nrf-softdevice/", "Adafruit_nRF52_Bootloader/", "i2pd/")

# The pinned Python and firmware references keep line citations: the
# submodule is fixed at one commit, so its lines do not move until the pin
# does (rule in docs/src/concepts/checks-and-citations.md).
REFERENCE_EXTENSIONS = {"py", "rst", "cpp", "h", "ino", "c"}

SKIP_DIRS = {".git", "target", "book", "node_modules", "reference"}


class Tree:
    """The repository's files, for resolving the bare and partial paths
    line citations were written with."""

    def __init__(self, root=REPO_ROOT):
        self.root = root
        self.by_base = {}
        self.all = set(tree_files(root))
        for rel in self.all:
            self.by_base.setdefault(os.path.basename(rel), []).append(rel)

    def resolve(self, cited):
        if cited in self.all:
            return [cited]
        base = os.path.basename(cited)
        return sorted(f for f in self.by_base.get(base, []) if f.endswith("/" + cited))


def spans(spec):
    out = []
    for part in spec.split(","):
        a, _, b = part.partition("-")
        out.append((int(a), int(b or a)))
    return out


def line_citation_kind(tree, path):
    """"tree" for a citation into a file of this tree; "exempt" for the
    rest: the pinned references, other repositories (a bare
    `BleGattServer.kt:120` into Columba, an `embassy-nrf-0.9.0/src/qspi.rs`
    under ~/.cargo), and `host.tld:port`. Only a line number into this tree
    moves when this tree is edited; whether a citation into a file that no
    longer exists is wrong is leviculum-std/tests/doc_citations.rs's
    question, not this rule's."""
    ext = path.rsplit(".", 1)[-1] if "." in os.path.basename(path) else ""
    if ext in NON_FILE_EXTENSIONS:
        return "exempt"
    if path.startswith("reference/") or any(path.startswith(p) for p in EXTERNAL_PREFIXES):
        return "exempt"
    return "tree" if tree.resolve(path) else "exempt"


def inside_url(text, start):
    line_start = text.rfind("\n", 0, start) + 1
    before = text[line_start:start]
    tok = re.split(r"[\s(<\[\"'`]", before)[-1]
    return "://" in tok or tok.startswith("www.")


def fenced_regions(text):
    """(start, end) offsets of the fenced code blocks of a markdown text:
    example output, not citations."""
    out, start, off = [], None, 0
    for line in text.split("\n"):
        if line.lstrip().startswith(("```", "~~~")):
            if start is None:
                start = off
            else:
                out.append((start, off + len(line)))
                start = None
        off += len(line) + 1
    return out


def where_in_code(rel, text):
    """A function telling, for an offset of `text`, whether it stands in
    "comment", "string" or "code"; prose files are all "comment"."""
    lang = language(rel)
    if lang in (_rust_items, _java_items):
        masked = mask(text)

        def at(p):
            if masked[p] == text[p]:
                return "code"
            ls = masked.rfind("\n", 0, p) + 1
            return "string" if masked.count('"', ls, p) % 2 else "comment"
        return at
    if lang in (_python_items, _shell_items, _justfile_items) or rel.startswith(".githooks/"):
        def at(p):
            ls = text.rfind("\n", 0, p) + 1
            line = text[ls:p]
            if "#" in line:
                return "comment"
            if rel.endswith(".py") and line.count('"') % 2 == 0 and line.count("'") % 2 == 0:
                # a docstring line has no quote before the citation
                return "comment" if not re.search(r"[=(,]\s*$", line) else "string"
            return "string"
        return at
    return lambda p: "comment"


def line_citations(text, tree, rel=None):
    """Each line citation into this tree in `text`, outside fenced code
    blocks of a markdown text."""
    out = []
    fences = fenced_regions(text) if rel and rel.endswith(".md") else []
    for m in LINE_CITE.finditer(text):
        if inside_url(text, m.start()):
            continue
        if any(a <= m.start() < b for a, b in fences):
            continue
        if line_citation_kind(tree, m.group(1)) != "tree":
            continue
        out.append(m)
    return out


# File types a name citation can point into: those `language` reads, and
# those that carry only anchors (`# ANCHOR: name` in a list file, `<!--
# ANCHOR: name -->` in XML). A `#` after any other extension is something
# else: a URL fragment (`page.html#top`), a raw identifier (`f.r#type`).
CITABLE_EXTENSIONS = {"rs", "py", "sh", "bash", "java", "kt", "kts", "c", "h", "cpp", "ino",
                      "md", "toml", "txt", "xml", "yml", "yaml", "just", "x", "ld", "service",
                      "tsv", "cfg", "ini", "conf"}


def citable(path):
    base = os.path.basename(path)
    if base == "Justfile" or path.startswith(".githooks/"):
        return True
    return "." in base and base.rsplit(".", 1)[1] in CITABLE_EXTENSIONS


def name_citations(text):
    """Each `path#item` citation in `text`, outside URLs and link targets."""
    out = []
    for m in NAME_CITE.finditer(text):
        if not citable(m.group(1)):
            continue
        if inside_url(text, m.start()):
            continue
        if text[max(0, m.start() - 2):m.start()] == "](":
            continue
        out.append(m)
    return out


# Living texts: what cites code by name. Point-in-time texts (audits under
# docs/src/audits, CHANGELOG entries, reports, orders, the bug ledger,
# commit messages) keep what they said when they were written.
LIVING_EXTENSIONS = (".rs", ".py", ".sh", ".md", ".kt", ".java", ".toml")
POINT_IN_TIME = ("docs/src/audits/", "CHANGELOG", "docs/drafts/", "notizen/")
# Code this project did not write (vendor/, see THIRD-PARTY.md) is not ours
# to rewrite.
FOREIGN = ("vendor/",)


def tree_files(root=REPO_ROOT):
    """Tracked and untracked-but-not-ignored files, outside the submodules;
    a plain walk where `root` is not a git checkout (the selftests)."""
    try:
        out = subprocess.run(
            ["git", "-C", root, "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            capture_output=True, check=True,
        ).stdout.decode("utf-8", "replace")
        files = [f for f in out.split("\0") if f and not f.startswith("reference/")]
        return sorted(f for f in set(files) if os.path.isfile(os.path.join(root, f)))
    except (OSError, subprocess.CalledProcessError):
        files = []
        for dp, dn, fn in os.walk(root):
            dn[:] = [d for d in dn if d not in SKIP_DIRS]
            for f in fn:
                files.append(os.path.relpath(os.path.join(dp, f), root))
        return sorted(files)


def is_living(rel):
    """Whether the file at `rel` is a living text."""
    f = os.path.basename(rel)
    if any(rel.startswith(p) for p in POINT_IN_TIME + FOREIGN):
        return False
    if rel.startswith(".githooks/") or f == "Justfile":
        return True
    if not f.endswith(LIVING_EXTENSIONS):
        return False
    if f.endswith(".md"):
        return rel.startswith("docs/src/") or f.startswith("README")
    return True


def living_texts(root=REPO_ROOT):
    """Repo-relative paths of every living text, sorted."""
    return [f for f in tree_files(root) if is_living(f)]
