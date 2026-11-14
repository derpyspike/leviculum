//! `start()` in a process of its own: one release thread, huge pages off,
//! and a second call that changes nothing (1805).

#[global_allocator]
static ALLOC: leviculum_alloc::Global = leviculum_alloc::GLOBAL;

/// The names of this process's threads, from `/proc/self/task/*/comm`.
fn thread_names() -> Vec<String> {
    let mut names = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").expect("procfs") {
        let comm = task.expect("task").path().join("comm");
        if let Ok(name) = std::fs::read_to_string(comm) {
            names.push(name.trim().to_owned());
        }
    }
    names
}

fn release_threads() -> usize {
    thread_names()
        .iter()
        .filter(|n| n.as_str() == leviculum_alloc::RELEASE_THREAD)
        .count()
}

/// The number of release threads once it reaches `want`, or what it is at a
/// five-second deadline.
///
/// A thread names itself: std's spawn returns before the new thread has
/// written its `comm`, so a count read straight after `start()` can see the
/// thread still under the parent's name. The baseline of a cargo-mutants
/// run on 2026-11-14 caught it that way, `left: 0` (1850).
fn release_threads_reaching(want: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let n = release_threads();
        if n == want || std::time::Instant::now() > deadline {
            return n;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// One test function, so libtest's parallel threads cannot interleave two
/// `start()` calls of this process.
#[test]
fn start_spawns_one_release_thread_and_declines_huge_pages() {
    assert_eq!(release_threads(), 0, "no release thread before start()");
    assert!(!leviculum_alloc::started());

    leviculum_alloc::start();
    assert!(leviculum_alloc::started());
    assert_eq!(
        release_threads_reaching(1),
        1,
        "start() spawns exactly one release thread"
    );

    leviculum_alloc::start();
    // A second thread would have to name itself too: give it the time the
    // first one was given before counting.
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        release_threads(),
        1,
        "a second start() spawns no second thread"
    );

    // SAFETY: PR_GET_THP_DISABLE takes no further arguments.
    let thp_disabled = unsafe { libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) };
    assert_eq!(thp_disabled, 1, "start() sets PR_SET_THP_DISABLE");
}
