#!/bin/sh
# Zig's archiver for a musl cross build (1805); see scripts/zig-cc.sh.
set -eu
if [ -n "${ZIG:-}" ]; then
    exec $ZIG ar "$@"
elif command -v zig >/dev/null 2>&1; then
    exec zig ar "$@"
else
    exec python3 -m ziglang ar "$@"
fi
