//! The BLE interface's accepted unframed bytes, never RF delivery
//! receipts — the fork's counter table trimmed to the one interface this
//! crate owns. The index argument keeps the fork's call shape so the
//! backend files port unchanged.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use leviculum_core::interface_counters::{InterfaceCounters, InterfaceTraffic};

/// The BLE interface's index in the firmware's counter order.
pub const BLE: usize = 6;

static BLE_COUNTER: Mutex<CriticalSectionRawMutex, Cell<InterfaceCounters>> =
    Mutex::new(Cell::new(InterfaceCounters::new()));

fn update(iface: usize, f: impl FnOnce(&mut InterfaceCounters)) {
    if iface != BLE {
        return;
    }
    BLE_COUNTER.lock(|cell| {
        let mut value = cell.get();
        f(&mut value);
        cell.set(value);
    });
}

/// Live totals and previous speeds, without sampling.
pub fn snapshot(iface: usize) -> InterfaceTraffic {
    if iface != BLE {
        return InterfaceTraffic::default();
    }
    BLE_COUNTER.lock(|cell| cell.get().snapshot())
}

pub fn note_rx(iface: usize, bytes: usize) {
    update(iface, |counter| counter.note_rx(bytes));
}

pub fn note_tx(iface: usize, bytes: usize) {
    update(iface, |counter| counter.note_tx(bytes));
}

/// Close a speed window; call once before a periodic transport line.
pub fn sample(now_ms: u64) {
    update(BLE, |counter| counter.sample(now_ms));
}
