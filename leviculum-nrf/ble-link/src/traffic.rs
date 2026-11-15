//! The BLE interface's byte counters (Codeberg #235, #346).
//!
//! One row of the per-interface traffic table, owned where the bytes
//! are counted: [`crate::deliver`] counts a completed receive handoff,
//! the [`crate::BleInterface`] counts an accepted transmit. The nRF's
//! `leviculum_firmware::iface_bytes` keeps the table the `/status`
//! bundle and the `[TRANSPORT]` lines read, and its BLE row delegates
//! here; the semantics (unframed packet bytes, success paths only) are
//! that module's, documented there.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use leviculum_core::interface_counters::{InterfaceCounters, InterfaceTraffic};

static COUNTERS: Mutex<CriticalSectionRawMutex, Cell<InterfaceCounters>> =
    Mutex::new(Cell::new(InterfaceCounters::new()));

fn update(f: impl FnOnce(&mut InterfaceCounters)) {
    COUNTERS.lock(|cell| {
        let mut value = cell.get();
        f(&mut value);
        cell.set(value);
    });
}

/// Count a completed receive ownership handoff.
pub fn note_rx(bytes: usize) {
    update(|counter| counter.note_rx(bytes));
}

/// Count accepted unframed bytes, not framing, retries or media-off
/// discards.
pub fn note_tx(bytes: usize) {
    update(|counter| counter.note_tx(bytes));
}

/// Sample once before the periodic transport log.
pub fn sample(now_ms: u64) {
    update(|counter| counter.sample(now_ms));
}

/// Read live totals and previous sampled speeds without changing the
/// window.
pub fn snapshot() -> InterfaceTraffic {
    COUNTERS.lock(|cell| cell.get().snapshot())
}
