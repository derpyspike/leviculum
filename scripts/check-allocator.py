#!/usr/bin/env python3
"""Every shipped binary declares its allocator, and it is mimalloc (1750).

WHY. A Rust binary without a `#[global_allocator]` runs on its libc's
malloc, and which malloc that is depends on how the binary was linked, not
on anything in its source:

  * `cargo build` for a musl triple (amd64's packages and tarballs) links
    Rust's self-contained musl, whose malloc is mallocng;
  * `cargo zigbuild` for a musl triple (arm64 and armhf) links Zig's libc,
    whose malloc since Zig 0.16 is `lib/c/malloc.zig` over
    `std.heap.SmpAllocator`: it never returns a 64-KiB slab to the system,
    and a free from another thread lands on the freeing thread's list, so a
    thread that allocates while another frees takes fresh slabs for ever;
  * the glibc desktop loquax gets glibc's ptmalloc at run time.

The Loquax Mailbox on miauhaus, linked the second way, held 68.5 MB on free
lists over 9 MB live six minutes after a restart and 1.2 GB after 37 hours
on a 2-GB host. lnsd and lnpnd had moved to mimalloc for the same family of
reasons (#437, 766, 1592); nothing made the next binary do so. This check
does, in two halves:

  SOURCE (default, in `just guards`, no build, well under a second): every
  binary scripts/build-deb.sh packages (its BINS line) is in MIMALLOC below;
  its main source installs `mimalloc::MiMalloc` as the global allocator under
  `feature = "alloc-mimalloc"` and turns huge pages off before its runtime
  starts (`decline_huge_pages`, see leviculum-std/src/proc_memory.rs); its
  crate makes that feature default and points it at the `mimalloc`
  dependency; and build-deb.sh runs the BYTES half on the built binaries.

  BYTES (`--binary PATH...`, called by scripts/build-deb.sh on every binary
  it packages): the file carries mimalloc's own diagnostic string, which is
  linked in only when something references the allocator. A stripped arm64
  binary cannot be run or symbolised here, so the bytes are the witness, as
  the `Allocator: mimalloc` and heap-accounting markers already are there.
  The list of allowed allocators is that one string: a build that drops the
  feature, or a new binary nobody gave one, fails before it is packaged.

EXEMPT names the shipped binaries that are not on mimalloc and why; the
SOURCE half checks their reasons still hold where a file can say so.

Exit 0 = every check held. Exit 1 = one did not, named. Exit 2 = usage.

Usage:
  python3 -I scripts/check-allocator.py                 # SOURCE half
  python3 -I scripts/check-allocator.py --binary BIN... # BYTES half
  python3 -I scripts/check-allocator.py --selftest      # positive controls
"""

from __future__ import annotations

import pathlib
import re
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent

# mimalloc's own message prefix, in its options code (src/options.c), which
# every allocation path links. Present in every mimalloc binary measured
# (lnsd amd64 and arm64, loquax zigbuild musl), absent from the same loquax
# built without the feature (2026-11-12).
MIMALLOC_MARKER = b"mimalloc: warning: "

# binary -> (crate directory, main source). Every binary build-deb.sh
# packages, which is every binary of the nightly tarballs too
# (collect-nightly-debs.sh packs from the same release directories).
MIMALLOC = {
    "lnsd": ("leviculum-cli", "leviculum-cli/src/lnsd.rs"),
    "lnstest": ("leviculum-cli", "leviculum-cli/src/lnstest.rs"),
    "lncp": ("leviculum-cli", "leviculum-cli/src/lncp.rs"),
    "lnstatus": ("leviculum-cli", "leviculum-cli/src/lnstatus.rs"),
    "lnprobe": ("leviculum-cli", "leviculum-cli/src/lnprobe.rs"),
    "lnpath": ("leviculum-cli", "leviculum-cli/src/lnpath.rs"),
    "lnmgmt": ("leviculum-cli", "leviculum-cli/src/lnmgmt.rs"),
    "lnomad": ("lnomad", "lnomad/src/main.rs"),
    "lblogd": ("lblogd", "lblogd/src/main.rs"),
    "lnpnd": ("lnpnd", "lnpnd/src/main.rs"),
    "loquax": ("loquax", "loquax/src/main.rs"),
}

# Shipped, not on mimalloc, with the reason. Each reason is one the SOURCE
# half re-reads where it can (see check_exempt).
EXEMPT = {
    # amd64 only, linked by plain cargo for x86_64-unknown-linux-musl
    # (scripts/lnflash-bundle.sh), so mallocng, not Zig's malloc; and it is
    # a one-shot tool that exits when the board is flashed.
    "lnflash": "amd64-only plain cargo musl build (mallocng), runs for one flash",
    # A library inside the app's JVM process: bionic's allocator (scudo)
    # serves the whole process, and a second allocator in one library would
    # hand the JVM memory it cannot free.
    "libloquax_android.so": "Android app library; bionic's allocator owns the process",
    # The M9 image is firmware: no libc, its heap is the board's own
    # allocator over a fixed region.
    "loquax-m9 image": "firmware, no libc malloc",
}


def fail(errors: list[str]) -> int:
    for error in errors:
        print(f"check-allocator: {error}", file=sys.stderr)
    return 1 if errors else 0


def build_deb_bins(text: str) -> list[str]:
    match = re.search(r"^BINS=\(([^)]*)\)", text, re.M)
    return match.group(1).split() if match else []


def default_features(cargo_toml: str) -> list[str]:
    section = re.search(r"^\[features\]\n(.*?)(?=^\[)", cargo_toml, re.M | re.S)
    if not section:
        return []
    match = re.search(r"^default\s*=\s*\[([^\]]*)\]", section.group(1), re.M)
    return re.findall(r'"([^"]+)"', match.group(1)) if match else []


ALLOC_STATIC = re.compile(
    r'#\[cfg\([^\n]*feature = "alloc-mimalloc"[^\n]*\)\]\n'
    r"#\[global_allocator\]\n"
    r"static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;"
    r'|#\[cfg\(all\(\s*not\(feature = "heap-accounting"\),\s*feature = "alloc-mimalloc"[^\]]*\]\n'
    r"#\[global_allocator\]\n"
    r"static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;",
    re.S,
)
HUGE_PAGES_FIRST = re.compile(
    r"^fn main\(\)[^{]*\{\n(?:\s*//[^\n]*\n)*\s*leviculum_std::proc_memory::decline_huge_pages\(\);",
    re.M,
)


def check_source(root: pathlib.Path) -> list[str]:
    errors: list[str] = []
    build_deb = (root / "scripts/build-deb.sh").read_text()
    bins = build_deb_bins(build_deb)
    if not bins:
        errors.append("scripts/build-deb.sh: no BINS=(...) line to read")
    for name in bins:
        if name not in MIMALLOC and name not in EXEMPT:
            errors.append(
                f"{name}: packaged by scripts/build-deb.sh but not in MIMALLOC or EXEMPT "
                "of scripts/check-allocator.py; give it mimalloc (as lnsd) and list it"
            )
    for name in MIMALLOC:
        if name not in bins:
            errors.append(f"{name}: in MIMALLOC but scripts/build-deb.sh no longer packages it")
    seen_crates: set[str] = set()
    for name, (crate, main) in MIMALLOC.items():
        path = root / main
        if not path.is_file():
            errors.append(f"{name}: {main} does not exist")
            continue
        source = path.read_text()
        if not ALLOC_STATIC.search(source):
            errors.append(
                f'{main}: no `#[global_allocator] static ALLOC: mimalloc::MiMalloc` under '
                f'`feature = "alloc-mimalloc"`; {name} would run on its libc\'s malloc'
            )
        if not HUGE_PAGES_FIRST.search(source):
            errors.append(
                f"{main}: `fn main` does not call leviculum_std::proc_memory::decline_huge_pages() "
                "first; mimalloc's eager arena then costs megabytes under THP `always`"
            )
        if crate in seen_crates:
            continue
        seen_crates.add(crate)
        manifest = (root / crate / "Cargo.toml").read_text()
        if "alloc-mimalloc" not in default_features(manifest):
            errors.append(f"{crate}/Cargo.toml: `alloc-mimalloc` is not a default feature")
        if not re.search(r'^alloc-mimalloc\s*=\s*\[[^\]]*"dep:mimalloc"', manifest, re.M):
            errors.append(f'{crate}/Cargo.toml: `alloc-mimalloc` does not enable "dep:mimalloc"')
    if "scripts/check-allocator.py --binary" not in build_deb:
        errors.append("scripts/build-deb.sh: does not run `check-allocator.py --binary` on what it packages")
    errors += check_exempt(root)
    return errors


def check_exempt(root: pathlib.Path) -> list[str]:
    """The exemptions' reasons that a file can confirm."""
    errors = []
    bundle = root / "scripts/lnflash-bundle.sh"
    if bundle.is_file() and "zigbuild" in bundle.read_text():
        errors.append(
            "scripts/lnflash-bundle.sh: lnflash is exempt as a plain cargo musl build, "
            "but the bundle now uses zigbuild, which links Zig's malloc; give lnflash mimalloc"
        )
    return errors


def check_bytes(paths: list[str]) -> list[str]:
    errors = []
    for name in paths:
        path = pathlib.Path(name)
        try:
            data = path.read_bytes()
        except OSError as error:
            errors.append(f"{name}: {error}")
            continue
        if MIMALLOC_MARKER not in data:
            errors.append(
                f"{name}: does not carry mimalloc ({MIMALLOC_MARKER.decode()!r} absent); "
                "it would run on its libc's malloc, which under cargo zigbuild is Zig's SmpAllocator"
            )
    return errors


def selftest() -> int:
    """Positive controls: each injected defect must be reported."""
    problems = []
    with tempfile.TemporaryDirectory() as tmp:
        base = pathlib.Path(tmp)
        good = base / "with"
        bad = base / "without"
        good.write_bytes(b"\x7fELF...mimalloc: warning: ...")
        bad.write_bytes(b"\x7fELF...no allocator string here...")
        if check_bytes([str(good)]):
            problems.append("BYTES refused a binary that carries the marker")
        if not check_bytes([str(bad)]):
            problems.append("BYTES passed a binary without the marker")

    # SOURCE against copies of this tree with one defect each.
    real = check_source(ROOT)
    if real:
        problems.append("SOURCE is not clean on this tree, so its controls prove nothing: " + real[0])
    defects = {
        "drop the static": ("loquax/src/main.rs", "static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;", ""),
        "drop the default": ("lblogd/Cargo.toml", 'default = ["alloc-mimalloc"]', "default = []"),
        "drop huge pages": (
            "lnomad/src/main.rs",
            "leviculum_std::proc_memory::decline_huge_pages();",
            "",
        ),
        "new binary": ("scripts/build-deb.sh", "BINS=(lnsd ", "BINS=(lnfoo lnsd "),
        "no bytes check": ("scripts/build-deb.sh", "scripts/check-allocator.py --binary", "true"),
    }
    for label, (rel, old, new) in defects.items():
        with tempfile.TemporaryDirectory() as tmp:
            copy = pathlib.Path(tmp)
            files = {main for _, main in MIMALLOC.values()}
            files |= {f"{crate}/Cargo.toml" for crate, _ in MIMALLOC.values()}
            files |= {"scripts/build-deb.sh", "scripts/lnflash-bundle.sh"}
            for name in files:
                src = ROOT / name
                if src.is_file():
                    dst = copy / name
                    dst.parent.mkdir(parents=True, exist_ok=True)
                    dst.write_text(src.read_text())
            target = copy / rel
            text = target.read_text()
            if old not in text:
                problems.append(f"control '{label}': {rel} no longer holds {old!r}")
                continue
            target.write_text(text.replace(old, new, 1))
            if not check_source(copy):
                problems.append(f"control '{label}': SOURCE did not catch it")
    if problems:
        return fail([f"selftest: {p}" for p in problems])
    print("check-allocator: selftest: every injected defect was caught")
    return 0


def main(argv: list[str]) -> int:
    if argv[:1] == ["--selftest"]:
        return selftest()
    if argv[:1] == ["--binary"]:
        if len(argv) < 2:
            print("usage: check-allocator.py --binary PATH...", file=sys.stderr)
            return 2
        errors = check_bytes(argv[1:])
        if not errors:
            print(f"check-allocator: {len(argv) - 1} binaries carry mimalloc")
        return fail(errors)
    if argv:
        print(__doc__.split("Usage:")[1], file=sys.stderr)
        return 2
    errors = check_source(ROOT)
    if not errors:
        print(f"check-allocator: {len(MIMALLOC)} shipped binaries declare mimalloc; "
              f"{len(EXEMPT)} exempt with a reason")
    return fail(errors)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
