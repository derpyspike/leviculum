//! The BLE tasks' log lines, ringed until the firmware drains them.
//!
//! The fork's `leviculum-fw/src/log.rs`, trimmed to what the BLE backend
//! emits: every `log_fmt*` call formats one line — prefix, body, the
//! board-side ` t=<uptime_ms>` stamp (`leviculum-log-line`) — into
//! [`LOG_RING`], and the embedding firmware drains the ring to its debug
//! surface on its own schedule (the M9: between HDLC frames on UART0,
//! once per loop turn). Logging never awaits, and short critical sections
//! serialize producers on either core with the reader.
//!
//! Runtime lines (`log_fmt`, [`info!`](crate::info), [`warn!`](crate::warn))
//! are gated on [`RUNTIME_DRAIN_OPEN`] so a board whose drain opens late
//! keeps its boot scrollback; boot-critical lines (`log_fmt_critical`)
//! bypass the gate. A firmware whose drain is open from boot — the M9's
//! UART — calls [`open_runtime_drain`] right after it installs the drain.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex};
use embassy_sync::signal::Signal;

// 4 KiB: the BLE backend's lines alone ride this ring — the boot banner,
// the link lifecycle edges, the counters minute line — and the M9 drains
// it every loop turn, so the bound is burst depth, not scrollback.
const LOG_RING_SIZE: usize = 4 * 1024;

/// True once the firmware's debug drain accepts runtime lines. Runtime
/// log calls silently drop (and count in [`RUNTIME_DROPPED`]) until this
/// flips, so the ring is not lapped before the host can read the
/// boot-critical lines.
pub static RUNTIME_DRAIN_OPEN: AtomicBool = AtomicBool::new(false);
/// Runtime lines dropped while the gate was closed, said once at open.
pub static RUNTIME_DROPPED: AtomicUsize = AtomicUsize::new(0);

/// Ring buffer for the BLE tasks' log output.
pub struct LogRing {
    buf: core::cell::UnsafeCell<[u8; LOG_RING_SIZE]>,
    write_pos: AtomicUsize,
    read_pos: AtomicUsize,
}

// Safety: every buffer and cursor access happens under the multicore
// critical section in `write`/`read`.
unsafe impl Sync for LogRing {}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

impl LogRing {
    pub const fn new() -> Self {
        Self {
            buf: core::cell::UnsafeCell::new([0u8; LOG_RING_SIZE]),
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
        }
    }

    /// Write bytes into the ring buffer, overwriting the oldest when full.
    ///
    /// Reservation, payload and both cursors move under one critical
    /// section: reserving a range atomically before filling it would let
    /// another core read uninitialized bytes past a preempted producer.
    pub fn write(&self, data: &[u8]) {
        CriticalSectionRawMutex::new().lock(|| {
            let buf = self.buf.get().cast::<u8>();
            let start = self.write_pos.fetch_add(data.len(), Ordering::AcqRel);
            for (i, &byte) in data.iter().enumerate() {
                let idx = start.wrapping_add(i) % LOG_RING_SIZE;
                unsafe { core::ptr::write_volatile(buf.add(idx), byte) };
            }
            let wp = start.wrapping_add(data.len());
            let rp = self.read_pos.load(Ordering::Relaxed);
            if wp.wrapping_sub(rp) > LOG_RING_SIZE {
                self.read_pos
                    .store(wp.wrapping_sub(LOG_RING_SIZE), Ordering::Release);
            }
        });
    }

    /// Read up to `out.len()` bytes; returns how many were read.
    pub fn read(&self, out: &mut [u8]) -> usize {
        CriticalSectionRawMutex::new().lock(|| {
            let buf = self.buf.get().cast::<u8>();
            let wp = self.write_pos.load(Ordering::Acquire);
            let mut rp = self.read_pos.load(Ordering::Relaxed);
            let avail = wp.wrapping_sub(rp);
            let to_read = avail.min(out.len());
            for slot in out.iter_mut().take(to_read) {
                *slot = unsafe { core::ptr::read_volatile(buf.add(rp % LOG_RING_SIZE)) };
                rp = rp.wrapping_add(1);
            }
            self.read_pos.store(rp, Ordering::Release);
            to_read
        })
    }
}

/// The ring every `log_fmt*` call writes; the firmware's drain reads it.
pub static LOG_RING: LogRing = LogRing::new();

/// Signalled on every line, for a drain that waits rather than polls.
pub static LOG_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Milliseconds of board uptime, for the ` t=` stamp.
fn uptime_ms() -> u64 {
    embassy_time::Instant::now().as_millis()
}

fn fmt_into<'a>(buf: &'a mut [u8; 1024], prefix: &str, args: core::fmt::Arguments) -> &'a [u8] {
    leviculum_log_line::format_line(buf.as_mut_slice(), prefix, args, uptime_ms())
}

/// Format and ring one runtime line; dropped while the gate is closed.
pub fn log_fmt(prefix: &str, args: core::fmt::Arguments) {
    if !RUNTIME_DRAIN_OPEN.load(Ordering::Relaxed) {
        RUNTIME_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let mut buf = [0u8; 1024];
    let bytes = fmt_into(&mut buf, prefix, args);
    LOG_RING.write(bytes);
    LOG_SIGNAL.signal(());
}

/// Format and ring one line past the gate: boot banners and the lines a
/// capture is read by.
pub fn log_fmt_critical(prefix: &str, args: core::fmt::Arguments) {
    let mut buf = [0u8; 1024];
    let bytes = fmt_into(&mut buf, prefix, args);
    LOG_RING.write(bytes);
    LOG_SIGNAL.signal(());
}

/// Open the runtime gate; idempotent, says once how many lines it cost.
pub fn open_runtime_drain() {
    if !RUNTIME_DRAIN_OPEN.swap(true, Ordering::Relaxed) {
        let dropped = RUNTIME_DROPPED.load(Ordering::Relaxed);
        log_fmt_critical(
            "[LOG_GATE] ",
            format_args!("opened, dropped {} runtime lines pre-attach", dropped),
        );
    }
}

/// Log an informational line.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::log::log_fmt("[INFO] ", core::format_args!($($arg)*))
    };
}

/// Log a warning line.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::log::log_fmt("[WARN] ", core::format_args!($($arg)*))
    };
}

/// Serializes the host tests that write to the shared ring.
#[cfg(test)]
pub(crate) mod test_ring {
    pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain() -> String {
        let mut out = Vec::new();
        let mut chunk = [0u8; 256];
        loop {
            let n = LOG_RING.read(&mut chunk);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn runtime_lines_wait_for_the_drain_and_critical_lines_pass() {
        let _ring = test_ring::lock();
        // Another module's test may have logged before this one: count its
        // pre-gate drops in, and clear what it ringed after the gate.
        let _ = drain();
        let dropped = RUNTIME_DROPPED.load(Ordering::Relaxed) + 1;
        log_fmt("[INFO] ", format_args!("before"));
        log_fmt_critical("[INFO!] ", format_args!("boot"));
        assert_eq!(RUNTIME_DROPPED.load(Ordering::Relaxed), dropped);
        open_runtime_drain();
        log_fmt("[INFO] ", format_args!("after"));
        let ring = drain();
        let lines: Vec<&str> = ring.lines().collect();
        assert_eq!(lines.len(), 3, "{ring}");
        assert!(lines[0].starts_with("[INFO!] boot t="));
        assert!(lines[1].starts_with(&format!(
            "[LOG_GATE] opened, dropped {dropped} runtime lines pre-attach t="
        )));
        assert!(lines[2].starts_with("[INFO] after t="));
    }
}
