#!/bin/sh
# Zig as the C compiler for a musl cross build, and as nothing else (1805).
#
# The packages for arm64 and armhf link their musl binaries with rust-lld
# and Rust's self-contained musl, so the libc `malloc` that C code still
# calls (bundled SQLite, libdbus) is musl's mallocng; a cargo zigbuild link
# brought Zig's libc instead, whose malloc is SmpAllocator, which keeps
# every slab (docs/src/architecture.md "Memory allocator"). Zig stays the
# cross C compiler, because it carries the headers and builtins for every
# triple without a sysroot. scripts/build-deb.sh points CC_<triple> here.
#
# cc-rs hands a clang-like compiler `--target=<rust triple>`, which zig cc
# does not take in that spelling; this maps it to zig's `-target` and drops
# it. cc-rs's compiler-family probe (`-E` on a test file) carries no
# --target=, so the triple falls back to TARGET, which cargo sets for every
# build script and so for every compiler a build script runs. The armv7 CPU line is cargo-zigbuild's for the same triple: v7-A with
# VFPv3-D16 and Thumb-2, no NEON, as Rust's armv7-unknown-linux-musleabihf.
#
# The zig is ZIG if set, else `zig` on PATH, else the pip ziglang
# (`python3 -m ziglang`); scripts/build-deb.sh pins its version.
set -eu
target=
for arg do
    case "$arg" in
    --target=*) target="${arg#--target=}" ;;
    esac
done
target="${target:-${TARGET:-}}"
case "$target" in
aarch64-unknown-linux-musl) set -- -target aarch64-linux-musl "$@" ;;
armv7-unknown-linux-musleabihf) set -- -target arm-linux-musleabihf -mcpu=generic+v7a+vfp3-d32+thumb2-neon "$@" ;;
x86_64-unknown-linux-musl) set -- -target x86_64-linux-musl "$@" ;;
*)
    echo "zig-cc.sh: no zig target for '--target=${target}'" >&2
    exit 2
    ;;
esac
# Drop cc-rs's --target=, keep everything else in order.
n=$#
while [ "$n" -gt 0 ]; do
    arg="$1"
    shift
    n=$((n - 1))
    case "$arg" in
    --target=*) ;;
    *) set -- "$@" "$arg" ;;
    esac
done
if [ -n "${ZIG:-}" ]; then
    exec $ZIG cc "$@"
elif command -v zig >/dev/null 2>&1; then
    exec zig cc "$@"
else
    exec python3 -m ziglang cc "$@"
fi
