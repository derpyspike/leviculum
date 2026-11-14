//! The `leviculum-map-tool` binary's `main` runs end to end on its cheapest path: the
//! allocator wiring of 1805 (`leviculum_alloc::start()` as the first
//! statement, `leviculum_alloc::GLOBAL` installed) is in the process that
//! answers `--help`, and `start()` ran: under `LEVICULUM_ALLOC_SAY` it says
//! so on stderr. Until 1850 no test spawned this binary, so a `main` that
//! lost the call, or aborted in it, would have shipped green.

#[test]
fn help_runs_main_with_the_allocator_and_exits_zero() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_leviculum-map-tool"))
        .arg("--help")
        .env("LEVICULUM_ALLOC_SAY", "1")
        .output()
        .expect("spawn leviculum-map-tool");
    assert!(
        out.status.success(),
        "leviculum-map-tool --help: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("leviculum-alloc: started"),
        "leviculum-map-tool --help never started the allocator; stderr: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Usage"),
        "leviculum-map-tool --help prints its usage: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
