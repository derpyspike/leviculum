//! `loquax alloc-probe` (hidden, 1750): the mailbox's allocation pattern,
//! run against whatever allocator this binary installed, with the resident
//! set before and after.
//!
//! The mailbox allocates on one thread and frees on another: a frame is read
//! on the driver's task, decoded, archived and dropped on a worker. Zig's
//! libc malloc, which a cargo zigbuild musl binary got before 1750, files a
//! free on the freeing thread's list and never returns a slab, so the thread
//! that allocates keeps taking fresh memory while the live set stays where it
//! is: the miauhaus mailbox held 68.5 MB on free lists over 9 MB live six
//! minutes after a restart. This probe is that pattern and nothing else: one
//! thread allocates a batch of mixed-size buffers, a second frees it, at most
//! two batches are alive, and the resident set is read from /proc before the
//! first round and after the last. `loquax/tests/alloc_retention.rs` holds
//! the bound; the line it reads is
//!
//! `ALLOC_PROBE allocator=<name> rounds=<n> live_kb=<k> rss_start_kb=<a> rss_end_kb=<b>`

use std::sync::mpsc;
use std::thread;

/// Buffers a batch holds, and their sizes, cycled: the spread of a frame,
/// its decoded fields and an archive row, small to a few KiB.
const BATCH: usize = 256;
const SIZES: [usize; 8] = [24, 64, 136, 300, 700, 1_500, 3_000, 6_000];

/// One batch's bytes, which bounds the live set at two of them (one being
/// filled, one in the channel; the freeing thread holds the third briefly).
fn batch_bytes() -> usize {
    (0..BATCH).map(|i| SIZES[i % SIZES.len()]).sum()
}

/// The resident set in KiB, from `/proc/self/status`; 0 where it cannot be
/// read, which the test reads as a probe that measured nothing.
fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kb| kb.parse().ok())
        })
        .unwrap_or(0)
}

/// Runs `rounds` batches across the two threads and prints the line.
pub fn run(allocator: &str, rounds: u32) -> std::process::ExitCode {
    let rss_start = rss_kb();
    let (tx, rx) = mpsc::sync_channel::<Vec<Vec<u8>>>(1);
    let freer = thread::spawn(move || {
        for batch in rx {
            drop(std::hint::black_box(batch));
        }
    });
    let allocator_thread = thread::spawn(move || {
        for round in 0..rounds {
            let batch: Vec<Vec<u8>> = (0..BATCH)
                .map(|i| {
                    let size = SIZES[(i + round as usize) % SIZES.len()];
                    // Written, so the pages are touched and count as resident.
                    vec![(i % 251) as u8; size]
                })
                .collect();
            if tx.send(batch).is_err() {
                break;
            }
        }
    });
    let joined = allocator_thread.join().is_ok() & freer.join().is_ok();
    let rss_end = rss_kb();
    println!(
        "ALLOC_PROBE allocator={allocator} rounds={rounds} live_kb={} rss_start_kb={rss_start} rss_end_kb={rss_end}",
        2 * batch_bytes() / 1024
    );
    if joined {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
