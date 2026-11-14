//! `start()`'s own `PR_SET_THP_DISABLE`, the belt for a build where
//! `-DMI_DEFAULT_ALLOW_THP=0` did not reach mimalloc (1850).
//!
//! Where the define did reach it, mimalloc's own start-up sets the same flag
//! (`prim.c`, `mi_option_allow_thp` off), which is why a test that only reads
//! the flag after `start()` cannot tell whether `start()` set it: the audit's
//! cargo-mutants run left "replace decline_huge_pages with ()" alive. Here
//! mimalloc has started first, the flag is cleared behind its back, and only
//! `start()` can set it again. A process of its own, because `start()` runs
//! once per process.

#![cfg(target_os = "linux")]

#[global_allocator]
static ALLOC: leviculum_alloc::Global = leviculum_alloc::GLOBAL;

fn thp_disabled() -> i32 {
    // SAFETY: PR_GET_THP_DISABLE takes no further arguments.
    unsafe { libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) }
}

#[test]
fn start_declines_huge_pages_where_mimalloc_did_not() {
    // mimalloc is up: the harness allocated, and so does this.
    let warm = vec![1u8; 1 << 20];
    drop(warm);
    assert!(!leviculum_alloc::started());

    // SAFETY: PR_SET_THP_DISABLE with its flag in arg2 and the rest zero.
    let cleared = unsafe { libc::prctl(libc::PR_SET_THP_DISABLE, 0, 0, 0, 0) };
    assert_eq!(cleared, 0, "prctl clears the flag");
    assert_eq!(thp_disabled(), 0, "the flag is clear before start()");

    leviculum_alloc::start();
    assert_eq!(thp_disabled(), 1, "start() sets PR_SET_THP_DISABLE itself");
}
