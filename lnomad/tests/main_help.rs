//! The `lnomad` binary's `main` runs end to end on its cheapest path: the
//! allocator wiring of 1805 (`leviculum_alloc::start()` as the first
//! statement, `leviculum_alloc::GLOBAL` installed) is in the process that
//! answers `--help`, and `start()` ran: under `LEVICULUM_ALLOC_SAY` it says
//! so on stderr. Until 1850 no test spawned this binary, so a `main` that
//! lost the call, or aborted in it, would have shipped green.
//!
//! The binary needs `tui` (its `required-features`), so the test does too.

#![cfg(feature = "tui")]

#[test]
fn help_runs_main_with_the_allocator_and_exits_zero() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_lnomad"))
        .arg("--help")
        .env("LEVICULUM_ALLOC_SAY", "1")
        .output()
        .expect("spawn lnomad");
    assert!(
        out.status.success(),
        "lnomad --help: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("leviculum-alloc: started"),
        "lnomad --help never started the allocator; stderr: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Usage"),
        "lnomad --help prints its usage: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
