//! The one global allocator of every Leviculum program (1805).
//!
//! Every binary of the workspace, now and in future, installs this crate's
//! allocator and starts it first thing in `main`:
//!
//! ```no_run
//! #[global_allocator]
//! static ALLOC: leviculum_alloc::Global = leviculum_alloc::GLOBAL;
//!
//! fn main() {
//!     leviculum_alloc::start();
//!     // ...
//! }
//! ```
//!
//! The allocator is mimalloc v3 (mimalloc 3.3.2 through the `mimalloc` crate
//! 0.1.52), chosen by measurement: the allocator study of 2026-11-13
//! (review repository, `notizen/2026-11-13-allocator-studie.md`) compared it
//! against musl's mallocng, glibc's ptmalloc, Zig's SmpAllocator, jemalloc,
//! snmalloc and mimalloc v2 on lnsd, lnpnd and the Loquax Mailbox. It is the
//! only candidate that builds on every shipped triple, hands a spike back in
//! 0.1 s, costs the least CPU per flush and under cross-thread churn, and
//! starts in the least memory. Three settings make it so, and this crate
//! owns all three:
//!
//! - **Huge pages off at compile time.** `-DMI_DEFAULT_ALLOW_THP=0` reaches
//!   mimalloc's C build through `.cargo/config.toml`'s `[env] CFLAGS`. Under
//!   glibc mimalloc initialises in a constructor before `main`, so a prctl
//!   in `main` comes too late there (6.8 MB `--version` against 2.9 MB with
//!   the define). [`start`] sets `PR_SET_THP_DISABLE` as well, as a belt for
//!   a build the define did not reach.
//! - **A release thread.** [`start`] spawns one thread, `alloc-release`, that
//!   calls `mi_collect(true)` every [`RELEASE_INTERVAL`]. mimalloc v3 keeps a
//!   freed span for ten seconds or more before it purges it; the thread hands
//!   the start-up spike back (lnsd 20 s after start: 50 MB anon instead of
//!   191 MB) and costs no measurable CPU.
//! - **A release at the known spike sites.** [`release`] runs
//!   `mi_collect(true)` on the calling thread. Code that has just freed a
//!   structure of many megabytes calls it, so the memory is back at once and
//!   not a second later: leviculum-std's `known_destinations` flush is the
//!   case it was measured on.
//!
//! On Android the crate is the system allocator and both functions do
//! nothing: the JNI library lives in the JVM's process, whose allocator is
//! bionic's scudo, and a second allocator inside one library would hand the
//! JVM memory it cannot free. Firmware images (no libc, their own heap) do
//! not link this crate at all.
//!
//! `scripts/check-allocator.py` holds every binary to the two lines above
//! and every crate but this one off a mimalloc dependency.

#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The global allocator type every binary installs.
#[cfg(not(target_os = "android"))]
pub type Global = mimalloc::MiMalloc;

/// The global allocator type every binary installs: the system's on Android.
#[cfg(target_os = "android")]
pub type Global = std::alloc::System;

/// The value of [`Global`] a `#[global_allocator]` static is initialised with.
#[cfg(not(target_os = "android"))]
pub const GLOBAL: Global = mimalloc::MiMalloc;

/// The value of [`Global`] a `#[global_allocator]` static is initialised with.
#[cfg(target_os = "android")]
pub const GLOBAL: Global = std::alloc::System;

/// The allocator's name, as a program reports it at start (`Allocator: ...`)
/// and in the heap census.
#[cfg(not(target_os = "android"))]
pub const NAME: &str = "mimalloc";

/// The allocator's name, as a program reports it at start (`Allocator: ...`)
/// and in the heap census.
#[cfg(target_os = "android")]
pub const NAME: &str = "system";

/// How often the release thread hands freed memory back to the system.
///
/// One second is the study's measured setting (`c-mi3-collect1s`): the
/// start-up spike goes back within it, and a collect that finds nothing to
/// purge costs microseconds.
pub const RELEASE_INTERVAL: Duration = Duration::from_secs(1);

/// The release thread's name, as `/proc/<pid>/task/*/comm` shows it.
pub const RELEASE_THREAD: &str = "alloc-release";

static STARTED: AtomicBool = AtomicBool::new(false);

/// Start the allocator's housekeeping: huge pages off for this process, and
/// the release thread. The first statement of every binary's `main`.
///
/// Idempotent: a second call does nothing, so a library may call it too.
/// A failure to spawn the thread is reported on stderr and otherwise
/// ignored: the program runs as before, it only gives freed memory back
/// later.
pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    decline_huge_pages();
    if !cfg!(target_os = "android") {
        let spawned = std::thread::Builder::new()
            .name(RELEASE_THREAD.into())
            // The loop holds nothing on its stack; the smallest stack the
            // platform grants is enough.
            .stack_size(64 * 1024)
            .spawn(|| loop {
                std::thread::sleep(RELEASE_INTERVAL);
                release();
            });
        if let Err(e) = spawned {
            eprintln!("leviculum-alloc: no release thread ({e}); freed memory returns later");
        }
    }
}

/// Hand the memory this thread's heap holds free back to the system now.
///
/// For code that has just dropped a structure of many megabytes: without it
/// mimalloc v3 keeps the freed spans for ten seconds or more. Costs a few
/// microseconds when there is nothing to return. A no-op on Android.
pub fn release() {
    #[cfg(not(target_os = "android"))]
    // SAFETY: `mi_collect` takes no pointers and may be called from any
    // thread at any time; mimalloc initialises itself on first use.
    unsafe {
        libmimalloc_sys::mi_collect(true);
    }
}

/// Whether the release thread has been started in this process.
pub fn started() -> bool {
    STARTED.load(Ordering::Acquire)
}

/// mimalloc's `allow_thp` option as this build sees it: `Some(false)` when
/// huge pages are off (the define reached the C build, or the environment
/// said `MIMALLOC_ALLOW_THP=0`), `None` on Android, where mimalloc is not
/// linked (and, unreachably, when build.rs handed over no index).
pub fn huge_pages_allowed() -> Option<bool> {
    #[cfg(not(target_os = "android"))]
    {
        // libmimalloc-sys has no constant for it; build.rs counts it out of
        // the header this build compiles.
        const ALLOW_THP: &str = env!("LEVICULUM_ALLOC_MI_OPTION_ALLOW_THP");
        let Ok(option) = ALLOW_THP.parse::<libmimalloc_sys::mi_option_t>() else {
            return None;
        };
        // SAFETY: reads one option; mimalloc initialises itself on first use.
        let value = unsafe { libmimalloc_sys::mi_option_get(option) };
        Some(value != 0)
    }
    #[cfg(target_os = "android")]
    {
        None
    }
}

/// `PR_SET_THP_DISABLE` for this process: transparent huge pages off for
/// every mapping it makes from here on, whatever the host's THP setting.
fn decline_huge_pages() {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl with integer arguments only; PR_SET_THP_DISABLE takes
    // its flag in arg2 and requires the rest to be zero.
    unsafe {
        libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_pages_are_off_at_compile_time() {
        // The define in .cargo/config.toml reached mimalloc's C build. The
        // environment variable would say the same, so the test refuses to
        // read a run that set it.
        if std::env::var_os("MIMALLOC_ALLOW_THP").is_some() {
            panic!("MIMALLOC_ALLOW_THP is set; this test reads the compiled default");
        }
        assert_eq!(huge_pages_allowed(), Some(false));

        // And the answer is read, not assumed: with the option switched on
        // at run time the function says so (1850, the audit's surviving
        // constant-return mutants). In this one test, so no other test of
        // the binary reads the option while it is on.
        #[cfg(not(target_os = "android"))]
        {
            let option = env!("LEVICULUM_ALLOC_MI_OPTION_ALLOW_THP")
                .parse::<libmimalloc_sys::mi_option_t>()
                .expect("build.rs hands over the option's index");
            // SAFETY: sets one option to an integer; no pointers.
            unsafe { libmimalloc_sys::mi_option_set(option, 1) };
            let on = huge_pages_allowed();
            // SAFETY: as above.
            unsafe { libmimalloc_sys::mi_option_set(option, 0) };
            assert_eq!(on, Some(true), "the option switched on reads as on");
            assert_eq!(huge_pages_allowed(), Some(false));
        }
    }

    /// This process's resident bytes, from `/proc/self/statm`.
    ///
    /// Resident and not mimalloc's own `current_commit`: on Linux mimalloc
    /// purges a span by `MADV_DONTNEED`, which hands the pages back without
    /// decommitting them, so its commit count stays where it was (measured
    /// 69.6 MB before and after) while the resident set falls from 68.6 MB
    /// to 1.5 MB.
    #[cfg(target_os = "linux")]
    fn resident() -> usize {
        let statm = std::fs::read_to_string("/proc/self/statm").expect("procfs");
        let pages: usize = statm
            .split_whitespace()
            .nth(1)
            .and_then(|f| f.parse().ok())
            .expect("statm's second field");
        // SAFETY: sysconf with a constant name.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        pages * usize::try_from(page).expect("a page size")
    }

    /// A freed spike stays resident until something purges it, and
    /// [`release`] is what does (1850: the audit's mutant that empties
    /// `release` survived a test that only called it). The spike is
    /// allocated from mimalloc directly, because this test binary does not
    /// install [`GLOBAL`]; small blocks, so it lives in mimalloc's pages
    /// rather than in a mapping of its own that `free` unmaps at once.
    #[cfg(target_os = "linux")]
    #[test]
    fn release_hands_a_freed_spike_back() {
        const BLOCK: usize = 32 << 10;
        const SPIKE: usize = 64 << 20;
        let blocks: Vec<*mut std::ffi::c_void> = (0..SPIKE / BLOCK)
            .map(|_| {
                // SAFETY: a fresh block of BLOCK bytes, checked non-null and
                // written within its length.
                unsafe {
                    let p = libmimalloc_sys::mi_malloc(BLOCK);
                    assert!(!p.is_null(), "mi_malloc({BLOCK})");
                    std::ptr::write_bytes(p.cast::<u8>(), 1, BLOCK);
                    p
                }
            })
            .collect();
        for p in blocks {
            // SAFETY: each pointer came from mi_malloc above, freed once.
            unsafe { libmimalloc_sys::mi_free(p) };
        }
        let held = resident();
        release();
        let after = resident();
        assert!(
            held.saturating_sub(after) >= SPIKE / 2,
            "release() handed back {} of a freed {SPIKE}-byte spike (resident {held} -> {after})",
            held.saturating_sub(after)
        );
    }

    #[test]
    fn release_runs_on_any_thread() {
        let v: Vec<u8> = vec![7; 8 << 20];
        drop(v);
        release();
        std::thread::spawn(release)
            .join()
            .expect("release on a second thread");
    }
}
