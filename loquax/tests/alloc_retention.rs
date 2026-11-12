//! The mailbox's heap stays small under its own allocation pattern (1750).
//!
//! `loquax alloc-probe` allocates batches of mixed-size buffers on one
//! thread and frees them on another, the shape of a frame read on the
//! driver's task and dropped on a worker, with at most two batches alive,
//! and prints its resident set before and after (`loquax/src/alloc_probe.rs`).
//! On Zig's libc malloc, which a cargo zigbuild musl loquax ran on before
//! 1750, the freeing thread's lists grow for ever: 4.1 to 25.9 MB over 20 000
//! rounds at 0.7 MB live, measured 2026-11-12, as the miauhaus mailbox held
//! 68.5 MB of free lists over 9 MB live. On mimalloc it stayed between 4.8
//! and 6.0 MB for 1 to 100 000 rounds.
//!
//! Plain `cargo test` runs this against the host build of loquax, so a
//! build that loses the allocator is red here; `just alloc-retention` runs
//! it against the zigbuild musl binary through `LOQUAX_ALLOC_PROBE_BIN`, the
//! link the arm64 and armhf packages ship, where `alloc=system` is red.

use std::process::Command;

/// Rounds the probe runs: enough that the old link's growth (8.9 to 21.7 MB
/// over 5 000 to 20 000 rounds) is far over the bound, few enough for a debug
/// build under a CPU quota (about a second unconstrained).
const ROUNDS: u32 = 20_000;

/// The resident set may grow by this much over the run: about five times
/// the live set, and three times what mimalloc's growth measured with
/// transparent huge pages left on (2.2 MB), which the probe's binary turns
/// off first.
const BOUND_KB: u64 = 4 * 1024;

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("no {key}= in {line:?}"))
}

#[test]
fn a_cross_thread_free_does_not_grow_the_heap() {
    let bin = std::env::var("LOQUAX_ALLOC_PROBE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_loquax").to_string());
    let out = Command::new(&bin)
        .args(["alloc-probe", "--rounds", &ROUNDS.to_string()])
        .output()
        .unwrap_or_else(|error| panic!("{bin}: {error}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{bin} alloc-probe: {}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let line = stdout
        .lines()
        .find(|line| line.starts_with("ALLOC_PROBE "))
        .unwrap_or_else(|| panic!("no ALLOC_PROBE line from {bin}: {stdout}"));
    eprintln!("{bin}: {line}");
    let start: u64 = field(line, "rss_start_kb").parse().expect("rss_start_kb");
    let end: u64 = field(line, "rss_end_kb").parse().expect("rss_end_kb");
    assert!(start > 0, "the probe read no resident set: {line}");
    assert!(
        end.saturating_sub(start) <= BOUND_KB,
        "{bin}: the resident set grew by {} KiB over {ROUNDS} rounds at a constant live set, \
         more than {BOUND_KB} KiB: {line}",
        end.saturating_sub(start)
    );
    // After the bound, so a link that retains is named by what it does
    // first; then the name, so a build that loses mimalloc on a libc whose
    // malloc happens not to retain under this pattern (mallocng) is red too.
    assert_eq!(
        field(line, "allocator"),
        "mimalloc",
        "{bin} does not run on mimalloc: {line}"
    );
}
