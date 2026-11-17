//! Order 1749 (Refs #232): the `known_destinations` flush in bounded memory,
//! and a file that cannot grow without limit.
//!
//! The Loquax Mailbox on miauhaus handed out 303 MB within five minutes while
//! its live heap stayed at 22 MB: every flush read the WHOLE file into an
//! `rmpv::Value` tree, merged it into a map, and encoded it anew, about 6.3
//! times the file size at the peak. And the file kept every destination the
//! node ever heard (52 825 entries in the mailbox's home node, 182 983 in
//! miauhaus's lnsd), so that peak grew for as long as the node ran.
//!
//! This binary installs a counting global allocator that counts only the
//! allocations of a thread that asked to be measured, so libtest's parallel
//! threads do not enter a measurement. A flush called through
//! `Storage::flush` runs inline on the calling thread, all of it.
//!
//! The tests:
//!
//! - the live-heap peak of one flush does not grow with the file: N and 8N,
//!   8N being the size of the mailbox's 9 MB file;
//! - the same for loading the store at start;
//! - disk-only entries follow a retention rule, so the file stays bounded;
//!   a destination the rule dropped comes back with its next announce;
//! - a SIGKILL during a flush leaves a complete file behind.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use leviculum_core::memory_storage::TableCaps;
use leviculum_core::traits::Storage as _;
use leviculum_core::Identity;
use leviculum_std::driver::StdStorage;
use rmpv::Value;

// ---------------------------------------------------------------------------
// Counting allocator, per thread
// ---------------------------------------------------------------------------

struct ThreadCounting;

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn note(delta: isize) {
    let _ = MEASURING.try_with(|measuring| {
        if measuring.get() {
            let _ = LIVE.try_with(|live| {
                let now = live.get() + delta;
                live.set(now);
                let _ = PEAK.try_with(|peak| {
                    if now > peak.get() {
                        peak.set(now);
                    }
                });
            });
        }
    });
}

// SAFETY: every call is forwarded to `System` unchanged; the counters are
// const-initialised thread locals, which neither allocate nor recurse.
unsafe impl GlobalAlloc for ThreadCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            note(layout.size() as isize);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            note(layout.size() as isize);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        note(-(layout.size() as isize));
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            note(new_size as isize - layout.size() as isize);
        }
        p
    }
}

#[global_allocator]
static ALLOC: ThreadCounting = ThreadCounting;

/// Run `f` on this thread and return its result with the highest live heap
/// it reached above where it started, in bytes.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    MEASURING.with(|m| m.set(true));
    let out = f();
    MEASURING.with(|m| m.set(false));
    (out, PEAK.with(|p| p.get()).max(0) as usize)
}

// ---------------------------------------------------------------------------
// Store fixtures
// ---------------------------------------------------------------------------

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The field file's shape: 9.38 MB for 52 825 entries is 178 bytes an
/// entry, which a 48-byte `app_data` gives (an LXMF delivery announce's
/// display name and stamp cost are about that).
const APP_DATA_LEN: usize = 48;

/// Python's fifth element, as `Identity.py:107, 268-292` writes it.
#[derive(Clone, Copy)]
enum Use {
    Never,
    Retained,
    At(f64),
}

fn dest(i: u64, tag: u8) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0] = tag;
    h[8..].copy_from_slice(&i.to_be_bytes());
    h
}

fn entry(public_key: &[u8; 64], timestamp: f64, use_state: Use) -> Value {
    Value::Array(vec![
        Value::F64(timestamp),
        Value::Binary(vec![0x5a; 32]),
        Value::Binary(public_key.to_vec()),
        Value::Binary(vec![0x61; APP_DATA_LEN]),
        match use_state {
            Use::Never => Value::from(0i64),
            Use::Retained => Value::from(-1i64),
            Use::At(t) => Value::F64(t),
        },
    ])
}

fn write_file(dir: &Path, entries: Vec<([u8; 16], Value)>) {
    let map = Value::Map(
        entries
            .into_iter()
            .map(|(h, v)| (Value::Binary(h.to_vec()), v))
            .collect(),
    );
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &map).unwrap();
    std::fs::write(dir.join("known_destinations"), buf).unwrap();
}

/// The file as Python's loader would see it: a map of 16-byte keys to
/// entry arrays, decoded completely with nothing left over.
fn read_file(dir: &Path) -> BTreeMap<[u8; 16], Vec<Value>> {
    let bytes = std::fs::read(dir.join("known_destinations")).unwrap();
    let mut rest = &bytes[..];
    let value = rmpv::decode::read_value(&mut rest).unwrap();
    assert!(
        rest.is_empty(),
        "{} trailing bytes after the map",
        rest.len()
    );
    value
        .as_map()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            let key: [u8; 16] = k.as_slice().unwrap().try_into().unwrap();
            (key, v.as_array().unwrap().clone())
        })
        .collect()
}

fn store_dir() -> tempfile::TempDir {
    // On disk under the target directory, not in a shared tmpfs: the kill
    // test's file is tens of megabytes.
    tempfile::Builder::new()
        .prefix("kd_flush_")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

/// A store of `n` fresh entries on disk, as a node that has run for weeks
/// has it, loaded with the mailbox's caps, and dirtied by one announce.
/// Returns the live peak of the load and of one flush, and the flush time.
fn load_and_flush(n: u64) -> (usize, usize, Duration, u64) {
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    write_file(
        dir.path(),
        (0..n)
            .map(|i| {
                (
                    dest(i, 0x10),
                    entry(&key, now - i as f64 * 1e-3, Use::Never),
                )
            })
            .collect(),
    );
    let file_len = std::fs::metadata(dir.path().join("known_destinations"))
        .unwrap()
        .len();

    let (storage, load_peak) =
        peak_of(|| StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap());
    let mut storage = storage;
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));

    let started = Instant::now();
    let ((), flush_peak) = peak_of(|| storage.flush());
    let took = started.elapsed();
    (load_peak, flush_peak, took, file_len)
}

/// Growth allowed from N to 8N, for either peak. The rule's ceiling on
/// disk-only entries is the identity cap (2 048 for the mailbox), so what a
/// flush holds is a function of the caps, and only the allocator's and the
/// buffers' rounding may differ between the two sizes.
const PEAK_SLACK: usize = 256 * 1024;

/// Live heap peak of one flush and of the load, at N and at 8N, where 8N is
/// the mailbox home node's 9 MB file (52 825 entries on miauhaus).
#[test]
fn flush_and_load_peaks_do_not_grow_with_the_file() {
    let sizes = [6_600u64, 26_400, 52_800];
    let mut rows = Vec::new();
    for n in sizes {
        let (load, flush, took, file_len) = load_and_flush(n);
        println!(
            "KD_FLUSH_PEAK entries={n} file_bytes={file_len} load_peak_bytes={load} \
             flush_peak_bytes={flush} flush_ms={}",
            took.as_millis()
        );
        rows.push((n, load, flush, file_len));
    }
    let (_, load_small, flush_small, _) = rows[0];
    let (_, load_big, flush_big, big_len) = rows[2];
    assert!(
        big_len > 9_000_000,
        "the large store is the mailbox's 9 MB file, got {big_len} bytes"
    );
    assert!(
        flush_big <= flush_small + PEAK_SLACK,
        "flush peak grew with the file: {flush_small} bytes at N, {flush_big} at 8N"
    );
    assert!(
        load_big <= load_small + PEAK_SLACK,
        "load peak grew with the file: {load_small} bytes at N, {load_big} at 8N"
    );
}

/// Disk-only entries, the ones this node no longer holds in memory, are
/// kept by the reference's own cull rule (`Identity.py:311-366`): a pinned
/// one always, a used one for `DESTINATION_TIMEOUT * 1.25` after its last
/// use, a never-used one for `UNUSED_DESTINATION_LINGER` after its announce.
/// The table the node holds in memory always goes to the file. A
/// destination the rule dropped is remembered again by its next announce,
/// and recalled after a restart.
#[test]
fn the_file_keeps_what_the_cull_keeps_and_relearns_the_rest() {
    const CAP: u64 = 2_048;
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    let day = 86_400.0;

    // The newest CAP entries are what the node holds in memory after the
    // load: a day old and never used, which the cull would drop, but the
    // node holds them, so they stay.
    let mut on_disk: Vec<([u8; 16], Value)> = (0..CAP)
        .map(|i| (dest(i, 0x01), entry(&key, now - day, Use::Never)))
        .collect();
    // Older than every held entry, so none of these is loaded.
    let stale_never = dest(0, 0x02);
    let stale_used = dest(0, 0x03);
    let pinned = dest(0, 0x04);
    let recently_used = dest(0, 0x05);
    on_disk.push((stale_never, entry(&key, now - 30.0 * day, Use::Never)));
    on_disk.push((
        stale_used,
        entry(&key, now - 30.0 * day, Use::At(now - 9.0 * day)),
    ));
    on_disk.push((pinned, entry(&key, now - 30.0 * day, Use::Retained)));
    on_disk.push((
        recently_used,
        entry(&key, now - 30.0 * day, Use::At(now - 8.0 * day)),
    ));
    // Bulk of what a node heard over weeks: never used, long silent.
    for i in 1..20_000u64 {
        on_disk.push((dest(i, 0x02), entry(&key, now - 20.0 * day, Use::Never)));
    }
    write_file(dir.path(), on_disk);

    let mut storage = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();

    // Another process (an rnsd on the same storage) remembered one a
    // minute ago, after this node loaded.
    let mut with_foreign = read_file(dir.path());
    let fresh_foreign = dest(0, 0x06);
    let Value::Array(fresh) = entry(&key, now - 60.0, Use::Never) else {
        unreachable!()
    };
    with_foreign.insert(fresh_foreign, fresh);
    write_file(
        dir.path(),
        with_foreign
            .into_iter()
            .map(|(k, v)| (k, Value::Array(v)))
            .collect(),
    );

    let announced = Identity::generate(&mut rand_core::OsRng);
    storage.set_identity([0xEE; 16], announced.clone());
    storage.flush();

    let file = read_file(dir.path());
    for i in 0..CAP {
        assert!(file.contains_key(&dest(i, 0x01)), "held entry {i} dropped");
    }
    assert!(
        file.contains_key(&[0xEE; 16]),
        "the new announce is in the file"
    );
    assert!(
        file.contains_key(&pinned),
        "a pinned entry is never dropped"
    );
    assert!(
        file.contains_key(&recently_used),
        "a use within DESTINATION_TIMEOUT * 1.25 keeps the entry"
    );
    assert!(
        file.contains_key(&fresh_foreign),
        "a never-used entry within UNUSED_DESTINATION_LINGER stays"
    );
    assert!(!file.contains_key(&stale_used), "a use 9 days ago is stale");
    assert!(
        !file.contains_key(&stale_never),
        "a never-used entry silent for 30 days is stale"
    );
    assert_eq!(
        file.len() as u64,
        CAP + 4,
        "held + new + pinned + recently used + fresh foreign, and nothing else"
    );

    // The dropped destination announces again: the core remembers it
    // through `set_identity` on the validated announce, the next flush
    // writes it, and a restarted node recalls it.
    let returning = Identity::generate(&mut rand_core::OsRng);
    storage.set_identity(stale_never, returning.clone());
    storage.flush();
    assert!(
        read_file(dir.path()).contains_key(&stale_never),
        "a re-announced destination is written again"
    );
    drop(storage);
    let restarted = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();
    assert_eq!(
        restarted
            .get_identity(&stale_never)
            .map(|id| id.public_key_bytes()),
        Some(returning.public_key_bytes()),
        "the re-learned identity is recalled after a restart"
    );
}

/// The count ceiling: however many disk-only entries the cull would keep
/// (pins and recent uses an rnsd beside us set), the file holds at most the
/// identity cap of them on top of the table in memory, the newest by
/// announce time.
#[test]
fn disk_only_entries_are_held_to_the_identity_cap() {
    const CAP: u64 = 2_048;
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    // 4 CAP pinned entries, distinct announce times, newest first.
    write_file(
        dir.path(),
        (0..4 * CAP)
            .map(|i| (dest(i, 0x07), entry(&key, now - i as f64, Use::Retained)))
            .collect(),
    );
    let mut storage = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    storage.flush();

    let file = read_file(dir.path());
    assert_eq!(
        file.len() as u64,
        2 * CAP + 1,
        "memory + cap disk-only + new"
    );
    for i in 0..2 * CAP {
        assert!(
            file.contains_key(&dest(i, 0x07)),
            "newest entry {i} dropped"
        );
    }
}

/// SIGKILL a process in the middle of its flushes: the file it leaves is
/// complete, never a truncated temp, because the new file is written beside
/// the old one and renamed over it.
#[test]
fn kill_during_flush_leaves_a_complete_file() {
    const CHILD_ENV: &str = "KD_FLUSH_CHILD_DIR";
    if let Ok(dir) = std::env::var(CHILD_ENV) {
        // Child: flush over and over until the parent kills us.
        let mut storage = StdStorage::new_with_caps(&dir, TableCaps::local_client()).unwrap();
        for i in 0u64.. {
            storage.set_identity(dest(i, 0x99), Identity::generate(&mut rand_core::OsRng));
            storage.flush();
        }
        unreachable!();
    }

    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    // Pinned, so every entry survives every flush and each one writes the
    // whole file: about 18 MB, long enough a write to be caught in.
    const N: u64 = 100_000;
    write_file(
        dir.path(),
        (0..N)
            .map(|i| (dest(i, 0x08), entry(&key, now - i as f64, Use::Retained)))
            .collect(),
    );

    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "kill_during_flush_leaves_a_complete_file",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD_ENV, dir.path())
    .stdout(std::process::Stdio::null());
    // Supervised: the child dies with this test even if it is killed itself.
    let mut child = KillOnDrop(leviculum_std::process::spawn_supervised(cmd).unwrap());

    // Kill at the first sign of a write: a temp file beside the store with
    // bytes in it, or the store itself no longer the file we wrote (a
    // writer that rewrote it in place would be caught half way here).
    let store = dir.path().join("known_destinations");
    let original_len = std::fs::metadata(&store).unwrap().len();
    let deadline = Instant::now() + Duration::from_secs(120);
    let caught = loop {
        let temp_in_flight = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name != "known_destinations"
                    && name.starts_with("known_destinations")
                    && e.metadata().map(|m| m.len() > 0).unwrap_or(false)
            });
        let store_changed = std::fs::metadata(&store)
            .map(|m| m.len() != original_len)
            .unwrap_or(true);
        if temp_in_flight || store_changed {
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_micros(200));
    };
    assert!(caught, "no flush was seen writing within the deadline");

    let file = read_file(dir.path());
    assert!(
        file.len() as u64 >= N,
        "the file after the kill is complete: {} of at least {N} entries",
        file.len()
    );
}

// ---------------------------------------------------------------------------
// The audit's tests of order 1749 (order 1767), as the two auditors ran them
// ---------------------------------------------------------------------------

/// Auditor: the file is oldest-first, so once the stamp heap is full every
/// later offer must replace its oldest stamp
/// (`leviculum-std/src/file_known_destinations_store.rs#NewestStamps::offer`).
/// Uses this file's helpers: store_dir, dest, entry, Use, write_file, read_file, now_secs.
#[test]
fn audit_disk_only_cap_keeps_the_newest_of_an_oldest_first_file() {
    let cap = TableCaps::local_client().identity_cap as u64;
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    let total = 4 * cap;
    write_file(
        dir.path(),
        (0..total)
            .map(|i| {
                (
                    dest(i, 0x0A),
                    entry(&key, now - (total - i) as f64, Use::Retained),
                )
            })
            .collect(),
    );
    let mut storage = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    storage.flush();

    let file = read_file(dir.path());
    assert!(
        file.contains_key(&[0xEE; 16]),
        "the flush was abandoned: the new entry is not in the file"
    );
    assert_eq!(
        file.len() as u64,
        2 * cap + 1,
        "memory + cap disk-only + new"
    );
    for i in 2 * cap..total {
        assert!(
            file.contains_key(&dest(i, 0x0A)),
            "newest entry {i} dropped"
        );
    }
}

/// Auditor: neither newest- nor oldest-first, three entries per announce
/// time, the cap boundary inside a tie group. The flush must not be
/// abandoned by the count check, and no dropped entry may be newer than a kept one.
/// Uses this file's helpers: store_dir, dest, entry, Use, write_file, read_file, now_secs.
#[test]
fn audit_disk_only_cap_with_ties_in_a_shuffled_file() {
    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let cap = TableCaps::local_client().identity_cap as u64;
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    let total = 4 * cap + 7;
    let mut stride = 7_919u64;
    while gcd(stride, total) != 1 {
        stride += 1;
    }
    let stamp = |i: u64| now - (i / 3) as f64;
    write_file(
        dir.path(),
        (0..total)
            .map(|j| {
                let i = (j * stride) % total;
                (dest(i, 0x0B), entry(&key, stamp(i), Use::Retained))
            })
            .collect(),
    );
    let mut storage = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    storage.flush();

    let file = read_file(dir.path());
    assert!(
        file.contains_key(&[0xEE; 16]),
        "the flush was abandoned: the new entry is not in the file"
    );
    assert_eq!(
        file.len() as u64,
        2 * cap + 1,
        "memory + cap disk-only + new"
    );
    let kept_oldest = (0..total)
        .filter(|i| file.contains_key(&dest(*i, 0x0B)))
        .map(stamp)
        .fold(f64::INFINITY, f64::min);
    let dropped_newest = (0..total)
        .filter(|i| !file.contains_key(&dest(*i, 0x0B)))
        .map(stamp)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        kept_oldest >= dropped_newest,
        "a dropped entry ({dropped_newest}) is newer than a kept one ({kept_oldest})"
    );
}

/// Auditor: a file whose first key claims a 1 GiB bin32 and then ends.
/// Load and flush must stay in bounded memory and the flush must still write.
/// Uses this file's helpers: store_dir, peak_of, read_file.
#[test]
fn audit_a_corrupt_bin32_length_costs_no_large_allocation() {
    let dir = store_dir();
    let mut bytes = vec![0x81u8, 0xc6];
    bytes.extend_from_slice(&0x4000_0000u32.to_be_bytes());
    bytes.extend_from_slice(&[0u8; 16]);
    std::fs::write(dir.path().join("known_destinations"), &bytes).unwrap();

    let (storage, load_peak) =
        peak_of(|| StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap());
    assert!(
        load_peak < 32 * 1024 * 1024,
        "loading a corrupt file allocated {load_peak} bytes"
    );
    let mut storage = storage;
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    let ((), flush_peak) = peak_of(|| storage.flush());
    assert!(
        flush_peak < 32 * 1024 * 1024,
        "flushing over a corrupt file allocated {flush_peak} bytes"
    );
    assert!(read_file(dir.path()).contains_key(&[0xEE; 16]));
}

/// Auditor: a valid 16-byte key whose value claims an array32 of 2^30
/// elements and then ends. Load and flush must neither abort nor allocate
/// for the claimed length.
/// Uses this file's helpers: store_dir, peak_of, read_file.
#[test]
fn audit_a_corrupt_array32_length_costs_no_large_allocation() {
    let dir = store_dir();
    let mut bytes = vec![0x81u8, 0xc4, 0x10];
    bytes.extend_from_slice(&[0x33u8; 16]);
    bytes.push(0xdd);
    bytes.extend_from_slice(&0x4000_0000u32.to_be_bytes());
    std::fs::write(dir.path().join("known_destinations"), &bytes).unwrap();

    let (storage, load_peak) =
        peak_of(|| StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap());
    assert!(
        load_peak < 32 * 1024 * 1024,
        "loading a corrupt file allocated {load_peak} bytes"
    );
    let mut storage = storage;
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    let ((), flush_peak) = peak_of(|| storage.flush());
    assert!(
        flush_peak < 32 * 1024 * 1024,
        "flushing over a corrupt file allocated {flush_peak} bytes"
    );
    assert!(read_file(dir.path()).contains_key(&[0xEE; 16]));
}

/// Audit attack: a file (written locally, or moved in from a peer through
/// the mailbox `/move`) whose headers claim about 2^31 bytes or elements.
/// The load and the flush must stay in bounded memory and leave a valid file.
#[test]
fn audit_hostile_lengths_cost_no_memory() {
    const LIMIT: usize = 16 * 1024 * 1024;
    // fixmap(1), key bin32 claiming 0x7fff_fff0 bytes, then 4 bytes and EOF.
    let mut bin_key = vec![0x81u8, 0xc6, 0x7f, 0xff, 0xff, 0xf0];
    bin_key.extend_from_slice(&[0u8; 4]);
    // fixmap(1), a valid 16-byte key, value array32 claiming 0x7fff_ffff elements, EOF.
    let mut arr_val = vec![0x81u8, 0xc4, 0x10];
    arr_val.extend_from_slice(&[0x11u8; 16]);
    arr_val.extend_from_slice(&[0xdd, 0x7f, 0xff, 0xff, 0xff]);
    // fixmap(1), valid key, value array of 5 whose public key is bin32 claiming 0x7fff_fff0.
    let mut bin_val = vec![0x81u8, 0xc4, 0x10];
    bin_val.extend_from_slice(&[0x22u8; 16]);
    bin_val.extend_from_slice(&[0x95, 0xcb]);
    bin_val.extend_from_slice(&1_700_000_000f64.to_be_bytes());
    bin_val.extend_from_slice(&[0xc4, 0x00, 0xc6, 0x7f, 0xff, 0xff, 0xf0, 0x00]);
    for (name, bytes) in [
        ("bin key", bin_key),
        ("array value", arr_val),
        ("bin value", bin_val),
    ] {
        let dir = store_dir();
        std::fs::write(dir.path().join("known_destinations"), &bytes).unwrap();
        let (storage, load_peak) =
            peak_of(|| StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap());
        assert!(load_peak < LIMIT, "{name}: load peak {load_peak} bytes");
        let mut storage = storage;
        storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
        let ((), flush_peak) = peak_of(|| storage.flush());
        assert!(flush_peak < LIMIT, "{name}: flush peak {flush_peak} bytes");
        let file = read_file(dir.path());
        assert!(
            file.contains_key(&[0xEE; 16]),
            "{name}: the flush rewrote a valid file"
        );
    }
}

/// Audit: the disk-only ceiling keeps the NEWEST entries when the file holds
/// them oldest first, so the heap must replace
/// (`leviculum-std/src/file_known_destinations_store.rs#NewestStamps::offer`).
#[test]
fn audit_disk_only_newest_whatever_the_file_order() {
    const CAP: u64 = 2_048;
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    // 4 CAP pinned entries, OLDEST first in file order.
    write_file(
        dir.path(),
        (0..4 * CAP)
            .map(|i| {
                (
                    dest(i, 0x0A),
                    entry(&key, now - (4 * CAP - i) as f64, Use::Retained),
                )
            })
            .collect(),
    );
    let mut storage = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    storage.flush();
    let file = read_file(dir.path());
    assert!(file.contains_key(&[0xEE; 16]), "the flush wrote");
    assert_eq!(
        file.len() as u64,
        2 * CAP + 1,
        "memory + cap disk-only + new"
    );
    // Memory holds 3CAP..4CAP; the newest CAP of the disk-only rest are 2CAP..3CAP.
    for i in 2 * CAP..4 * CAP {
        assert!(
            file.contains_key(&dest(i, 0x0A)),
            "newest entry {i} dropped"
        );
    }
    for i in 0..2 * CAP {
        assert!(
            !file.contains_key(&dest(i, 0x0A)),
            "old entry {i} kept over a newer one"
        );
    }
}

/// Audit: every disk-only entry shares one announce time; the two passes
/// must agree on the count, or every flush is abandoned.
#[test]
fn audit_disk_only_ties_at_the_floor() {
    const CAP: u64 = 2_048;
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let t = now_secs() - 100.0;
    write_file(
        dir.path(),
        (0..3 * CAP)
            .map(|i| (dest(i, 0x0B), entry(&key, t, Use::Retained)))
            .collect(),
    );
    let mut storage = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    storage.flush();
    let file = read_file(dir.path());
    assert!(
        file.contains_key(&[0xEE; 16]),
        "the flush was not abandoned"
    );
    assert_eq!(
        file.len() as u64,
        2 * CAP + 1,
        "memory + cap disk-only + new"
    );
}

/// Audit: the streamed copy of an entry only the file holds keeps every
/// field, including the public key and packet hash.
#[test]
fn audit_disk_only_entries_copied_bit_for_bit() {
    const CAP: u64 = 2_048;
    let dir = store_dir();
    let key = Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let now = now_secs();
    let mut on_disk: Vec<([u8; 16], Value)> = (0..CAP)
        .map(|i| (dest(i, 0x30), entry(&key, now - 10.0, Use::Never)))
        .collect();
    let mut foreign: Vec<([u8; 16], Value)> = Vec::new();
    for i in 0..16u64 {
        let v = Value::Array(vec![
            Value::F64(now - 1000.0 - i as f64),
            Value::Binary(vec![0x70 + i as u8; 32]),
            Value::Binary(vec![0x90 + i as u8; 64]),
            if i % 2 == 0 {
                Value::Binary(vec![0x41 + i as u8; 7])
            } else {
                Value::Nil
            },
            if i % 3 == 0 {
                Value::from(-1i64)
            } else {
                Value::F64(now - 3600.0 - i as f64)
            },
        ]);
        foreign.push((dest(i, 0x31), v));
    }
    on_disk.extend(foreign.iter().cloned());
    write_file(dir.path(), on_disk);
    let mut storage = StdStorage::new_with_caps(dir.path(), TableCaps::local_client()).unwrap();
    storage.set_identity([0xEE; 16], Identity::generate(&mut rand_core::OsRng));
    storage.flush();
    let file = read_file(dir.path());
    assert!(file.contains_key(&[0xEE; 16]), "the flush wrote");
    for (hash, value) in &foreign {
        let Value::Array(want) = value else {
            unreachable!()
        };
        assert!(
            file.get(hash) == Some(want),
            "disk-only entry {:02x?} changed: {:?} != {:?}",
            hash,
            file.get(hash),
            want
        );
    }
}
