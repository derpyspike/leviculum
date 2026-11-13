#!/usr/bin/env python3
"""Every Leviculum program runs on one allocator, and it is leviculum-alloc's (1805).

WHY. Lew decided on 2026-11-13, from the allocator study of that day
(review repository, notizen/2026-11-13-allocator-studie.md), that every
program runs on mimalloc v3 with three settings: huge pages off at compile
time, a release thread, and a release at the known spike sites. The rule
and its reasons are on docs/src/architecture.md, section "Memory
allocator". A binary's allocator is decided by two things nobody sees in a
review of one change: whether its source installs one, and how it was
linked. Before this rule the Loquax Mailbox shipped without one, was linked
by cargo zigbuild for arm64, ran on Zig's SmpAllocator through Zig's libc
malloc, and held 1.2 GB over about 9 MB live after 37 hours. This check
holds both, in two halves:

  SOURCE (default, in `just guards`, no build, well under a second):
    * every binary target of every workspace member outside EXEMPT_PATHS
      installs `leviculum_alloc::Global` as its `#[global_allocator]`
      (directly, or as `CountingOver<leviculum_alloc::Global>` under the
      `heap-accounting` feature beside the plain one under its negation),
      and calls `leviculum_alloc::start()` as the first statement of a
      plain `fn main`. The binaries are found by cargo's own rules
      (src/main.rs, src/bin/*.rs, src/bin/*/main.rs, [[bin]] paths), so a
      new binary without the two lines fails without anybody editing this
      file;
    * no Cargo.toml in the tree but leviculum-alloc's depends on mimalloc,
      libmimalloc-sys, tikv-jemallocator, jemallocator or snmalloc-rs;
    * leviculum-std calls `leviculum_alloc::release()` at the spike sites:
      the end of the flush closure (`begin_flush`), after the replaced table
      is dropped (`settle_flush`), and after the start-up load of
      known_destinations (`Storage::new_with_limits`);
    * `.cargo/config.toml` passes `-DMI_DEFAULT_ALLOW_THP=0` to the C build;
    * scripts/build-deb.sh links no musl triple through cargo zigbuild (Zig
      stays the C compiler only) and runs the BYTES half on what it packs,
      and scripts/collect-nightly-debs.sh runs it on every tarball binary;
    * the exemptions still hold where a file can say so: loquax-android's
      library installs no allocator, leviculum-alloc is `System` on
      Android, and the exempt test binaries carry their own allocator;
    * docs/src/architecture.md has the section this rule lives in.

  BYTES (`--binary PATH...`, run by build-deb.sh on every binary it
  packages, which are the files the nightly tarballs pack):
    * mimalloc's own message string is in the file (`mimalloc: warning: `,
      from its options code, which every allocation path links);
    * a musl binary carries musl mallocng's `__libc_malloc_impl` symbol.
      That is the witness that Zig's malloc is absent: rust-lld keeps the
      symbol table under the release profile's `strip = "debuginfo"` and
      links Rust's self-contained musl, whose malloc is mallocng; a cargo
      zigbuild link strips every symbol and brings Zig's libc, whose malloc
      is SmpAllocator. A stripped binary therefore fails too, which is the
      choice: an unstripped musl binary is the only one this check can tell
      apart, and every packaged one is unstripped. A glibc binary (its
      bytes name a `GLIBC_2.` version) gets glibc's malloc for C code and
      needs no witness;
    * a runnable binary (the host's architecture, or one a `qemu-<arch>` on
      PATH runs) prints `option 'allow_thp': 0` under `MIMALLOC_VERBOSE=1
      <bin> --version`: the define reached mimalloc's C build.
  `--android PATH...` is the exemption's side: the Android library must
  NOT carry the mimalloc string, since bionic's scudo owns the JVM's
  process.

EXEMPT_PATHS are the only exemptions (1805 item 7); one-shot tools, lnflash
included, are not exempt: mimalloc v3 with huge pages off starts in 2.7 to
4.0 MB against mallocng's 3.2 MB, and one rule beats a list.

Exit 0 = every check held. Exit 1 = one did not, named. Exit 2 = usage.

Usage:
  python3 -I scripts/check-allocator.py                  # SOURCE half
  python3 -I scripts/check-allocator.py --binary BIN...  # BYTES half
  python3 -I scripts/check-allocator.py --android SO...  # the Android library
  python3 -I scripts/check-allocator.py --selftest       # positive controls
"""

from __future__ import annotations

import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent

# mimalloc's own message prefix (src/options.c), linked by every allocation
# path; present in every mimalloc binary measured, absent without it.
MIMALLOC_MARKER = b"mimalloc: warning: "
# musl mallocng's entry point, kept in the symbol table of a rust-lld link.
MALLOCNG_WITNESS = b"__libc_malloc_impl"
GLIBC_MARKER = b"GLIBC_2."
THP_OFF = "option 'allow_thp': 0"

# Binary targets that carry their own allocator on purpose, by path, with
# the reason. check_exempt re-reads each reason where a file can say so.
EXEMPT_PATHS = {
    # Measures the counting shim over the system allocator; its allocator
    # IS the measurement (`--alloc-overhead`).
    "leviculum-std/src/bin/heap-gap-bench.rs": "bench with its own counting allocator",
}
# Exempt directories: no binary target under them is held to the rule.
EXEMPT_DIRS = {
    # The boards' heaps on the host, measured under the firmware's allocator.
    "leviculum-heap-lab": "the boards' allocators, measured on the host",
    # The JNI library inside the JVM's process (bionic's scudo); a cdylib.
    "loquax-android": "Android library, bionic's allocator owns the process",
}

FORBIDDEN_DEPS = ("mimalloc", "libmimalloc-sys", "tikv-jemallocator", "jemallocator", "snmalloc-rs")
ALLOC_CRATE = "leviculum-alloc"

GLOBAL_STATIC = re.compile(
    r"((?:#\[cfg\([^\n]*\)\]\n)?)#\[global_allocator\]\n\s*static\s+\w+\s*:\s*([^=]+?)\s*=",
)
ALLOWED_TYPES = {
    "leviculum_alloc::Global",
    "leviculum_std::heap_accounting::CountingOver<leviculum_alloc::Global>",
}
HEAPACCT_ON = '#[cfg(feature = "heap-accounting")]'
HEAPACCT_OFF = '#[cfg(not(feature = "heap-accounting"))]'
MAIN_START = re.compile(
    r"^fn main\(\)[^{]*\{\n(?:\s*//[^\n]*\n|\s*\n)*\s*leviculum_alloc::start\(\);",
    re.M,
)
MAIN_ANY = re.compile(r"^(?:pub\s+)?(?:async\s+)?fn main\(\)", re.M)


def fail(errors: list[str]) -> int:
    for error in errors:
        print(f"check-allocator: {error}", file=sys.stderr)
    return 1 if errors else 0


# ---------------------------------------------------------------- SOURCE --


def workspace_members(root: pathlib.Path) -> list[str]:
    text = (root / "Cargo.toml").read_text()
    match = re.search(r"^members\s*=\s*\[(.*?)\]", text, re.M | re.S)
    if not match:
        return []
    body = "\n".join(line.split("#", 1)[0] for line in match.group(1).splitlines())
    return re.findall(r'"([^"]+)"', body)


def bin_targets(root: pathlib.Path, member: str) -> list[pathlib.Path]:
    """Binary sources of one member, by cargo's rules."""
    crate = root / member
    manifest = (crate / "Cargo.toml").read_text()
    found: set[pathlib.Path] = set()
    autobins = not re.search(r"^autobins\s*=\s*false", manifest, re.M)
    if autobins:
        main = crate / "src/main.rs"
        if main.is_file():
            found.add(main)
        bindir = crate / "src/bin"
        if bindir.is_dir():
            for entry in sorted(bindir.iterdir()):
                if entry.suffix == ".rs" and entry.is_file():
                    found.add(entry)
                elif entry.is_dir() and (entry / "main.rs").is_file():
                    found.add(entry / "main.rs")
    for block in re.findall(r"^\[\[bin\]\]\n(.*?)(?=^\[|\Z)", manifest, re.M | re.S):
        path = re.search(r'^path\s*=\s*"([^"]+)"', block, re.M)
        if path:
            found.add(crate / path.group(1))
    return sorted(found)


def rel(root: pathlib.Path, path: pathlib.Path) -> str:
    return path.relative_to(root).as_posix()


def is_exempt(relpath: str) -> bool:
    if relpath in EXEMPT_PATHS:
        return True
    return any(relpath.startswith(d + "/") for d in EXEMPT_DIRS)


def check_binary_source(relpath: str, text: str) -> list[str]:
    errors = []
    statics = GLOBAL_STATIC.findall(text)
    if not statics:
        errors.append(
            f"{relpath}: no #[global_allocator]; install leviculum_alloc's "
            "(`static ALLOC: leviculum_alloc::Global = leviculum_alloc::GLOBAL;`)"
        )
    cfgs = set()
    for cfg, ty in statics:
        ty = re.sub(r"\s+", "", ty)
        if ty not in {re.sub(r"\s+", "", t) for t in ALLOWED_TYPES}:
            errors.append(f"{relpath}: #[global_allocator] is `{ty}`, not leviculum_alloc::Global")
        cfg = cfg.strip()
        if cfg and cfg not in (HEAPACCT_ON, HEAPACCT_OFF):
            errors.append(f"{relpath}: the allocator is behind `{cfg}`; only heap-accounting may switch it")
        cfgs.add(cfg)
    if cfgs and "" not in cfgs and cfgs != {HEAPACCT_ON, HEAPACCT_OFF}:
        errors.append(f"{relpath}: the allocator is not installed in every build ({sorted(cfgs)})")
    if not MAIN_ANY.search(text):
        errors.append(f"{relpath}: no `fn main` found")
    elif not MAIN_START.search(text):
        errors.append(
            f"{relpath}: `leviculum_alloc::start();` is not the first statement of a plain `fn main`"
        )
    return errors


def dependency_lines(manifest: str) -> list[tuple[str, str]]:
    """(section, dependency name) for every dependency a manifest declares."""
    out = []
    section = ""
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped
            continue
        if "dependencies" not in section:
            continue
        match = re.match(r'^([A-Za-z0-9_-]+)\s*(?:\.\w+\s*)?=', stripped)
        if match:
            out.append((section, match.group(1)))
        # `[dependencies.mimalloc]` style.
    for match in re.finditer(r"^\[(?:[^\]]*\.)?dependencies\.([A-Za-z0-9_-]+)\]", manifest, re.M):
        out.append(("table", match.group(1)))
    return out


def manifests(root: pathlib.Path) -> list[pathlib.Path]:
    skip = {"target", "reference", ".git", "node_modules"}
    found = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in skip and not d.startswith("target")]
        if "Cargo.toml" in filenames:
            found.append(pathlib.Path(dirpath) / "Cargo.toml")
    return sorted(found)


def fn_body(text: str, name: str) -> str:
    match = re.search(rf"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn {name}\b", text, re.M)
    if not match:
        return ""
    rest = text[match.end():]
    end = re.search(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn \w", rest, re.M)
    return rest[: end.start()] if end else rest


RELEASE = "leviculum_alloc::release();"


def check_release_sites(root: pathlib.Path) -> list[str]:
    errors = []
    driver = root / "leviculum-std/src/driver/mod.rs"
    storage = root / "leviculum-std/src/storage.rs"
    dtext = driver.read_text() if driver.is_file() else ""
    begin = fn_body(dtext, "begin_flush")
    if RELEASE not in begin:
        errors.append("leviculum-std/src/driver/mod.rs: begin_flush's closure does not call leviculum_alloc::release()")
    settle = fn_body(dtext, "settle_flush")
    if not re.search(r"drop\(replaced\);\s*(?://[^\n]*\n\s*)*leviculum_alloc::release\(\);", settle):
        errors.append(
            "leviculum-std/src/driver/mod.rs: settle_flush does not call leviculum_alloc::release() "
            "after drop(replaced)"
        )
    stext = storage.read_text() if storage.is_file() else ""
    if RELEASE not in fn_body(stext, "new_with_limits"):
        errors.append(
            "leviculum-std/src/storage.rs: Storage::new_with_limits does not call "
            "leviculum_alloc::release() after the known_destinations load"
        )
    return errors


def check_packaging(root: pathlib.Path) -> list[str]:
    errors = []
    build_deb = root / "scripts/build-deb.sh"
    text = build_deb.read_text() if build_deb.is_file() else ""
    code = "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))
    for line in code.splitlines():
        if "zigbuild" in line and re.search(r"\bcargo\s+zigbuild\b|\(cargo zigbuild\)", line) and "DESKTOP_TRIPLE" not in line:
            errors.append(
                "scripts/build-deb.sh: a musl build runs through cargo zigbuild "
                f"({line.strip()!r}); link musl with rust-lld, zig as CC only"
            )
    if "check-allocator.py --binary" not in code:
        errors.append("scripts/build-deb.sh: does not run `check-allocator.py --binary` on what it packages")
    # The collect step runs in debian:bookworm-slim without python3, so it
    # reads the two byte witnesses with grep; build-deb.sh ran the whole
    # BYTES half on the same files.
    collect = root / "scripts/collect-nightly-debs.sh"
    ctext = collect.read_text() if collect.is_file() else ""
    ccode = "\n".join(l for l in ctext.splitlines() if not l.lstrip().startswith("#"))
    if MIMALLOC_MARKER.decode() not in ccode or MALLOCNG_WITNESS.decode() not in ccode:
        errors.append(
            "scripts/collect-nightly-debs.sh: does not read the mimalloc and mallocng witnesses "
            "off the tarball binaries"
        )
    config = root / ".cargo/config.toml"
    ctoml = config.read_text() if config.is_file() else ""
    if not re.search(r'^\[env\][^\[]*^CFLAGS\s*=\s*"[^"]*-DMI_DEFAULT_ALLOW_THP=0', ctoml, re.M | re.S):
        errors.append(".cargo/config.toml: [env] CFLAGS does not carry -DMI_DEFAULT_ALLOW_THP=0")
    return errors


def check_exempt(root: pathlib.Path) -> list[str]:
    errors = []
    for path, reason in EXEMPT_PATHS.items():
        f = root / path
        if f.is_file() and "#[global_allocator]" not in f.read_text():
            errors.append(f"{path}: exempt ({reason}) but declares no allocator of its own; drop the exemption")
    android = root / "loquax-android/src"
    if android.is_dir():
        for f in sorted(android.rglob("*.rs")):
            if "#[global_allocator]" in f.read_text():
                errors.append(f"{rel(root, f)}: the Android library installs an allocator; bionic owns the process")
    alloc_lib = root / "leviculum-alloc/src/lib.rs"
    atext = alloc_lib.read_text() if alloc_lib.is_file() else ""
    if not re.search(r'#\[cfg\(target_os = "android"\)\]\npub type Global = std::alloc::System;', atext):
        errors.append("leviculum-alloc/src/lib.rs: Global is not std::alloc::System on Android")
    return errors


def check_docs(root: pathlib.Path) -> list[str]:
    page = root / "docs/src/architecture.md"
    text = page.read_text() if page.is_file() else ""
    if not re.search(r"^## Memory allocator$", text, re.M) or "check-allocator.py" not in text:
        return ["docs/src/architecture.md: no section `## Memory allocator` naming check-allocator.py"]
    return []


def check_source(root: pathlib.Path) -> list[str]:
    errors: list[str] = []
    members = workspace_members(root)
    if not members:
        return ["Cargo.toml: no workspace members to read"]
    seen = 0
    for member in members:
        if not (root / member / "Cargo.toml").is_file():
            continue
        for path in bin_targets(root, member):
            relpath = rel(root, path)
            if is_exempt(relpath):
                continue
            if not path.is_file():
                errors.append(f"{relpath}: a [[bin]] path that does not exist")
                continue
            seen += 1
            errors += check_binary_source(relpath, path.read_text())
    if seen == 0:
        errors.append("no binary target found in the workspace; the enumeration is broken")
    for manifest in manifests(root):
        crate = manifest.parent.name
        if crate == ALLOC_CRATE:
            continue
        for section, dep in dependency_lines(manifest.read_text()):
            if dep in FORBIDDEN_DEPS:
                errors.append(
                    f"{rel(root, manifest)}: depends on {dep} ({section}); only {ALLOC_CRATE} may, "
                    "every program takes its allocator from there"
                )
    errors += check_release_sites(root)
    errors += check_packaging(root)
    errors += check_exempt(root)
    errors += check_docs(root)
    return errors


# ----------------------------------------------------------------- BYTES --

ELF_MACHINES = {62: "x86_64", 183: "aarch64", 40: "arm"}
HOST_MACHINES = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64", "armv7l": "arm"}
QEMU = {"x86_64": "qemu-x86_64", "aarch64": "qemu-aarch64", "arm": "qemu-arm"}


def runner(path: pathlib.Path, head: bytes) -> list[str] | None:
    """How to run `path` here, or None when nothing here can."""
    if head.startswith(b"#!"):
        return [str(path)]
    if not head.startswith(b"\x7fELF") or len(head) < 20:
        return None
    machine = ELF_MACHINES.get(int.from_bytes(head[18:20], "little"))
    if machine is None:
        return None
    if machine == HOST_MACHINES.get(platform.machine()):
        return [str(path)]
    for name in (QEMU[machine], QEMU[machine] + "-static"):
        qemu = shutil.which(name)
        if qemu:
            return [qemu, str(path)]
    return None


def check_bytes(paths: list[str]) -> list[str]:
    errors = []
    for name in paths:
        path = pathlib.Path(name)
        if not path.is_file():
            errors.append(f"{name}: no such file")
            continue
        data = path.read_bytes()
        if MIMALLOC_MARKER not in data:
            errors.append(
                f"{name}: not built on mimalloc (no {MIMALLOC_MARKER.decode()!r}); "
                "install leviculum_alloc::Global"
            )
        glibc = GLIBC_MARKER in data
        if not glibc and MALLOCNG_WITNESS not in data:
            errors.append(
                f"{name}: a musl binary without musl mallocng's {MALLOCNG_WITNESS.decode()}: "
                "linked by cargo zigbuild (Zig's malloc) or stripped; link musl with rust-lld "
                "and -C link-self-contained=yes"
            )
        command = runner(path, data[:64])
        if command is None:
            print(f"check-allocator: {name}: not runnable here, allow_thp not read")
            continue
        try:
            run = subprocess.run(
                command + ["--version"],
                env=dict(os.environ, MIMALLOC_VERBOSE="1"),
                capture_output=True,
                text=True,
                timeout=60,
            )
        except (OSError, subprocess.TimeoutExpired) as e:
            errors.append(f"{name}: `MIMALLOC_VERBOSE=1 {name} --version` did not run: {e}")
            continue
        if THP_OFF not in run.stdout + run.stderr:
            errors.append(
                f"{name}: `MIMALLOC_VERBOSE=1 --version` does not print {THP_OFF!r}; "
                "-DMI_DEFAULT_ALLOW_THP=0 did not reach mimalloc's C build"
            )
    return errors


def check_android(paths: list[str]) -> list[str]:
    errors = []
    for name in paths:
        path = pathlib.Path(name)
        if not path.is_file():
            errors.append(f"{name}: no such file")
        elif MIMALLOC_MARKER in path.read_bytes():
            errors.append(f"{name}: the Android library carries mimalloc; bionic's allocator owns the process")
    return errors


# -------------------------------------------------------------- SELFTEST --


def source_inputs(root: pathlib.Path) -> list[pathlib.Path]:
    files = [
        root / "Cargo.toml",
        root / ".cargo/config.toml",
        root / "scripts/build-deb.sh",
        root / "scripts/collect-nightly-debs.sh",
        root / "docs/src/architecture.md",
        root / "leviculum-std/src/driver/mod.rs",
        root / "leviculum-std/src/storage.rs",
        root / "leviculum-alloc/src/lib.rs",
    ]
    files += list((root / "loquax-android/src").rglob("*.rs"))
    for member in workspace_members(root):
        if not (root / member / "Cargo.toml").is_file():
            continue
        files += bin_targets(root, member)
    files += manifests(root)
    return sorted({f for f in files if f.is_file()})


def copy_tree(root: pathlib.Path, dest: pathlib.Path) -> None:
    for f in source_inputs(root):
        target = dest / f.relative_to(root)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(f, target)


def edit(path: pathlib.Path, old: str, new: str) -> None:
    text = path.read_text()
    if old not in text:
        raise SystemExit(f"selftest: {path}: fixture text {old!r} not found; the tree moved")
    path.write_text(text.replace(old, new, 1))


def selftest() -> int:
    problems = []
    clean = check_source(ROOT)
    if clean:
        problems.append("the real tree is not clean: " + "; ".join(clean))

    def case(label: str, mutate, want: str) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            dest = pathlib.Path(tmp)
            copy_tree(ROOT, dest)
            mutate(dest)
            errors = check_source(dest)
            if not any(want in e for e in errors):
                problems.append(f"{label}: not caught (want {want!r}, got {errors})")

    lnsd = "leviculum-cli/src/lnsd.rs"
    case(
        "a new binary without the two lines",
        lambda d: (d / "leviculum-cli/src/bin").mkdir(exist_ok=True)
        or (d / "leviculum-cli/src/bin/newtool.rs").write_text("fn main() {\n    println!(\"hi\");\n}\n"),
        "leviculum-cli/src/bin/newtool.rs: no #[global_allocator]",
    )
    case(
        "a binary without start()",
        lambda d: edit(d / lnsd, "    leviculum_alloc::start();\n", ""),
        f"{lnsd}: `leviculum_alloc::start();` is not the first statement",
    )
    case(
        "start() not first",
        lambda d: edit(
            d / "leviculum-cli/src/lnpath.rs",
            "    leviculum_alloc::start();\n",
            "    let _x = 1;\n    leviculum_alloc::start();\n",
        ),
        "leviculum-cli/src/lnpath.rs: `leviculum_alloc::start();` is not the first statement",
    )
    case(
        "another allocator installed",
        lambda d: edit(
            d / "lblogd/src/main.rs",
            "static ALLOC: leviculum_alloc::Global = leviculum_alloc::GLOBAL;",
            "static ALLOC: std::alloc::System = std::alloc::System;",
        ),
        "lblogd/src/main.rs: #[global_allocator] is `std::alloc::System`",
    )
    case(
        "the allocator behind a feature",
        lambda d: edit(
            d / "lblogd/src/main.rs",
            "#[global_allocator]\nstatic ALLOC: leviculum_alloc::Global",
            '#[cfg(feature = "alloc-mimalloc")]\n#[global_allocator]\nstatic ALLOC: leviculum_alloc::Global',
        ),
        "lblogd/src/main.rs: the allocator is behind",
    )
    case(
        "a second crate depends on mimalloc",
        lambda d: edit(d / "lnomad/Cargo.toml", "[dependencies]\n", '[dependencies]\nmimalloc = "0.1"\n'),
        "lnomad/Cargo.toml: depends on mimalloc",
    )
    case(
        "a crate depends on jemalloc",
        lambda d: edit(d / "lnpnd/Cargo.toml", "[dependencies]\n", '[dependencies]\ntikv-jemallocator = "0.6"\n'),
        "lnpnd/Cargo.toml: depends on tikv-jemallocator",
    )
    case(
        "no release() in the flush closure",
        lambda d: edit(
            d / "leviculum-std/src/driver/mod.rs",
            "        leviculum_alloc::release();\n        (outcome, holds)",
            "        (outcome, holds)",
        ),
        "begin_flush's closure does not call",
    )
    case(
        "no release() after drop(replaced)",
        lambda d: edit(
            d / "leviculum-std/src/driver/mod.rs",
            "                    leviculum_alloc::release();\n                });",
            "                });",
        ),
        "settle_flush does not call",
    )
    case(
        "no release() after the start-up load",
        lambda d: edit(d / "leviculum-std/src/storage.rs", "        leviculum_alloc::release();\n", ""),
        "new_with_limits does not call",
    )
    case(
        "a musl triple through cargo zigbuild",
        lambda d: edit(
            d / "scripts/build-deb.sh",
            'build_cmd=(cargo build)\n',
            'build_cmd=(cargo build)\n[ "$ARCH" != amd64 ] && build_cmd=(cargo zigbuild)\n',
        ),
        "a musl build runs through cargo zigbuild",
    )
    case(
        "build-deb.sh without the bytes half",
        lambda d: (d / "scripts/build-deb.sh").write_text(
            (d / "scripts/build-deb.sh").read_text().replace("check-allocator.py --binary", "true")
        ),
        "build-deb.sh: does not run",
    )
    case(
        "collect-nightly-debs.sh without the mallocng witness",
        lambda d: (d / "scripts/collect-nightly-debs.sh").write_text(
            (d / "scripts/collect-nightly-debs.sh").read_text().replace("__libc_malloc_impl", "nothing")
        ),
        "collect-nightly-debs.sh: does not read",
    )
    case(
        "the THP define gone",
        lambda d: edit(d / ".cargo/config.toml", "-DMI_DEFAULT_ALLOW_THP=0", ""),
        "does not carry -DMI_DEFAULT_ALLOW_THP=0",
    )
    case(
        "an exemption whose reason no longer holds",
        lambda d: (d / "leviculum-std/src/bin/heap-gap-bench.rs").write_text(
            (d / "leviculum-std/src/bin/heap-gap-bench.rs").read_text().replace("#[global_allocator]", "")
        ),
        "heap-gap-bench.rs: exempt",
    )
    case(
        "an allocator in the Android library",
        lambda d: (d / "loquax-android/src/lib.rs").write_text(
            (d / "loquax-android/src/lib.rs").read_text()
            + "\n#[global_allocator]\nstatic A: std::alloc::System = std::alloc::System;\n"
        ),
        "the Android library installs an allocator",
    )
    case(
        "the doc section gone",
        lambda d: edit(d / "docs/src/architecture.md", "## Memory allocator\n", "## Something else\n"),
        "no section `## Memory allocator`",
    )

    # BYTES: synthetic files, one rule each.
    with tempfile.TemporaryDirectory() as tmp:
        t = pathlib.Path(tmp)

        def write(name: str, data: bytes, mode: int = 0o644) -> str:
            p = t / name
            p.write_bytes(data)
            p.chmod(mode)
            return str(p)

        good = write("good-musl", b"\0" + MIMALLOC_MARKER + b"\0" + MALLOCNG_WITNESS + b"\0")
        if check_bytes([good]):
            problems.append(f"bytes: a rust-lld musl mimalloc binary was refused: {check_bytes([good])}")
        no_mi = write("no-mimalloc", b"\0" + MALLOCNG_WITNESS + b"\0")
        if not any("not built on mimalloc" in e for e in check_bytes([no_mi])):
            problems.append("bytes: a binary without mimalloc was not refused")
        zig = write("zigbuild-musl", b"\0" + MIMALLOC_MARKER + b"\0")
        if not any("without musl mallocng" in e for e in check_bytes([zig])):
            problems.append("bytes: a zigbuild-linked (or stripped) musl binary was not refused")
        gnu = write("gnu", b"\0" + MIMALLOC_MARKER + b"\0" + GLIBC_MARKER + b"31\0")
        if check_bytes([gnu]):
            problems.append(f"bytes: a glibc mimalloc binary was refused: {check_bytes([gnu])}")
        thp_on = write(
            "thp-on",
            b"#!/bin/sh\necho \"option 'allow_thp': 1\"\n# " + MIMALLOC_MARKER + MALLOCNG_WITNESS + b"\n",
            0o755,
        )
        if not any("does not print" in e for e in check_bytes([thp_on])):
            problems.append("bytes: a runnable binary with huge pages on was not refused")
        thp_off = write(
            "thp-off",
            b"#!/bin/sh\necho \"option 'allow_thp': 0\"\n# " + MIMALLOC_MARKER + MALLOCNG_WITNESS + b"\n",
            0o755,
        )
        if check_bytes([thp_off]):
            problems.append(f"bytes: a runnable binary with huge pages off was refused: {check_bytes([thp_off])}")
        so_bad = write("libloquax_android_bad.so", b"\0" + MIMALLOC_MARKER)
        so_good = write("libloquax_android.so", b"\0scudo\0")
        if not check_android([so_bad]) or check_android([so_good]):
            problems.append("android: the mimalloc-free rule did not hold both ways")

    for p in problems:
        print(f"check-allocator selftest: {p}", file=sys.stderr)
    if not problems:
        print("check-allocator selftest: every injected defect caught, the clean tree and files pass")
    return 1 if problems else 0


def main(argv: list[str]) -> int:
    if not argv:
        errors = check_source(ROOT)
        if not errors:
            print("check-allocator: every binary installs and starts leviculum-alloc")
        return fail(errors)
    if argv[0] == "--selftest" and len(argv) == 1:
        return selftest()
    if argv[0] == "--binary" and len(argv) > 1:
        errors = check_bytes(argv[1:])
        if not errors:
            print(f"check-allocator: {len(argv) - 1} binaries on mimalloc, no Zig malloc, huge pages off where run")
        return fail(errors)
    if argv[0] == "--android" and len(argv) > 1:
        errors = check_android(argv[1:])
        if not errors:
            print(f"check-allocator: {len(argv) - 1} Android libraries free of mimalloc")
        return fail(errors)
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
