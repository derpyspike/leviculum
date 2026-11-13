//! Order 1805 (Refs #232): the `known_destinations` flush hands its spike
//! back to the system at once, not ten to twenty seconds later.
//!
//! mimalloc v3, the allocator of every Leviculum program, keeps a freed span
//! for ten seconds or more before it purges it. The flush builds a snapshot
//! and a folded table of the whole store on the blocking pool and frees the
//! old table there, so a node's resident set stood at the flush's peak for
//! 10 to 20 s after every flush (allocator study 2026-11-13, sections 4b and
//! 7). `leviculum_alloc::release()` at the end of the flush closure and
//! after the old table is dropped returns it in 0.1 s.
//!
//! The test runs a node in a CHILD process, so the resident set it reads
//! is that node's and no other test's: the parent re-runs this binary with
//! the test's name and `FLUSH_RELEASE_CHILD` set. The child fills a store
//! of 50 000 destinations, starts the node with a one-second flush
//! interval, lets the first flush (which writes the file and keeps its
//! table) settle, dirties every entry again, reads RssAnon before the
//! second flush and one second after its file lands, and prints both; the
//! parent asserts the second is within 4 MiB of the first.
//!
//! Its own test binary, not a module of `tests/mvr/`: the mvr binary
//! installs a counting allocator over `System`, and this one has to run on
//! the allocator the programs ship with. It installs `leviculum_alloc`'s
//! allocator but deliberately does NOT call `leviculum_alloc::start()`: the
//! release thread collects every second, so with it running the test would
//! pass without the flush-site release it exists to hold.

use std::process::Command;
use std::time::{Duration, Instant};

use leviculum_core::{DestinationHash, Identity};
use leviculum_std::ReticulumNodeBuilder;

#[global_allocator]
static ALLOC: leviculum_alloc::Global = leviculum_alloc::GLOBAL;

const CHILD_ENV: &str = "FLUSH_RELEASE_CHILD";
const ENTRIES: u64 = 50_000;
const FLUSH_INTERVAL_SECS: u64 = 1;
/// Time for the first flush's own spike to pass before the second's is
/// measured. With the flush-site release it is gone in 0.1 s; without it,
/// the second flush's `before` would carry it and the test would read the
/// first spike's slow return as the second's quick one, so the bound on
/// `after` still holds the release to account.
const SETTLE: Duration = Duration::from_millis(1200);
/// How long after the flush the resident set is read: the order's bound.
const AFTER_FLUSH: Duration = Duration::from_secs(1);
/// What the flush may leave behind: the order's bound.
const SLACK_KB: u64 = 4 * 1024;

/// RssAnon of this process in kB: anonymous memory only, so the binary's
/// text pages and file cache do not count.
fn rss_anon_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("procfs");
    status
        .lines()
        .find_map(|line| line.strip_prefix("RssAnon:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("RssAnon line")
}

/// The inode of the store file, which the flush writes beside and renames
/// over: a new inode is a finished flush.
fn inode(file: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(file).ok().map(|m| m.ino())
}

/// Wait until `file` exists with an inode other than `previous`.
async fn wait_for_flush(file: &std::path::Path, previous: Option<u64>) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(FLUSH_INTERVAL_SECS + 10);
    loop {
        if let Some(ino) = inode(file) {
            if Some(ino) != previous {
                return ino;
            }
        }
        assert!(Instant::now() < deadline, "no flush within the interval");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn dest(i: u64) -> DestinationHash {
    let mut h = [0u8; 16];
    h[0] = 0xF1;
    h[8..].copy_from_slice(&i.to_be_bytes());
    DestinationHash::new(h)
}

/// The child: a node with 50 000 known destinations, flushed once.
fn child() {
    let dir = std::env::var(CHILD_ENV).expect("child storage dir");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let storage = std::path::PathBuf::from(&dir);
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .storage_path(storage.clone())
            .flush_interval_secs(FLUSH_INTERVAL_SECS)
            // No fit to the host's RAM: every entry stays in memory.
            .host_ram_bytes(None)
            .build()
            .await
            .expect("build node");
        // One key pair for every entry: generating 50 000 in a debug build
        // takes longer than the whole test may. The table holds one entry
        // per destination hash either way.
        let identity = Identity::generate(&mut rand_core::OsRng);
        for i in 0..ENTRIES {
            node.remember_identity(dest(i), identity.clone());
        }
        node.start().await.expect("start node");

        // The first flush builds the on-disk table for the first time, which
        // stays live: it is the second flush, replacing a table of the same
        // size, whose spike must go. Wait for the first, let it settle, and
        // dirty every entry again by remembering it anew.
        let file = storage.join("known_destinations");
        let first = wait_for_flush(&file, None).await;
        tokio::time::sleep(SETTLE).await;
        for i in 0..ENTRIES {
            node.remember_identity(dest(i), identity.clone());
        }
        let before = rss_anon_kb();
        let mut peak = before;
        let deadline = Instant::now() + Duration::from_secs(FLUSH_INTERVAL_SECS + 10);
        while inode(&file) == Some(first) {
            assert!(
                Instant::now() < deadline,
                "no second flush within the interval"
            );
            peak = peak.max(rss_anon_kb());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let settled = Instant::now() + AFTER_FLUSH;
        while Instant::now() < settled {
            peak = peak.max(rss_anon_kb());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let after = rss_anon_kb();
        println!("FLUSH_RELEASE before_kb={before} peak_kb={peak} after_kb={after}");
        node.stop().await.ok();
    });
}

fn field(line: &str, key: &str) -> u64 {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {line:?}"))
}

#[test]
fn a_flush_hands_its_spike_back_within_a_second() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child();
        return;
    }
    let storage = tempfile::tempdir().expect("tempdir");
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "a_flush_hands_its_spike_back_within_a_second",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, storage.path())
        .output()
        .expect("spawn the child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child failed: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // libtest prints `test <name> ... ` before the child's own output, on
    // the same line.
    let line = stdout
        .lines()
        .find_map(|l| l.find("FLUSH_RELEASE ").map(|at| &l[at..]))
        .unwrap_or_else(|| panic!("child printed no FLUSH_RELEASE line:\n{stdout}"));
    eprintln!("{line}");
    let before = field(line, "before_kb=");
    let peak = field(line, "peak_kb=");
    let after = field(line, "after_kb=");
    // The control: the flush did make a spike worth handing back, so a
    // pass below is not a flush that freed nothing.
    assert!(
        peak > before + SLACK_KB,
        "the flush of {ENTRIES} entries made no spike above {SLACK_KB} kB \
         (before {before} kB, peak {peak} kB); the test measures nothing"
    );
    assert!(
        after <= before + SLACK_KB,
        "RssAnon {after} kB one second after the flush, {before} kB before it \
         (peak {peak} kB): the flush kept {} kB beyond the {SLACK_KB} kB allowed",
        after.saturating_sub(before)
    );
}
