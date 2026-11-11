//! The BLE carrier switch as the backend reads it.
//!
//! The fork keeps a whole media profile here; this crate needs one fact —
//! is the BLE carrier on — and the firmware owns where that fact comes
//! from (the M9: the flash record `lnflash --ble` writes, read at boot).
//! A firmware that flips it at runtime calls [`crate::ble::note_media_changed`]
//! after [`set_ble_active`], so the protocol tasks re-read it and live
//! sessions drop their links.

use core::sync::atomic::{AtomicBool, Ordering};

/// Default on: the zero-config decision (#471) — media are opt-out.
static BLE_ACTIVE: AtomicBool = AtomicBool::new(true);

/// Whether the BLE carrier is on. Every protocol task gates on this.
pub fn ble_active() -> bool {
    BLE_ACTIVE.load(Ordering::Relaxed)
}

/// Set the carrier switch; follow with [`crate::ble::note_media_changed`]
/// when tasks are already running.
pub fn set_ble_active(active: bool) {
    BLE_ACTIVE.store(active, Ordering::Relaxed);
}

/// Report a run of packets a down carrier threw away (first drop of a run
/// and every decade after it, never per packet;
/// [`leviculum_media_state::DropRun`]).
pub fn log_tx_drop(iface: &str, run: leviculum_media_state::Swallowed) {
    crate::log::log_fmt(
        "[MEDIA] ",
        format_args!(
            "MEDIA_TX_DROP iface={} packets={} bytes={} reason=carrier-off",
            iface, run.packets, run.bytes
        ),
    );
}

/// Close a drop run: the carrier took a packet again.
pub fn log_tx_resumed(iface: &str, run: leviculum_media_state::Swallowed) {
    crate::log::log_fmt(
        "[MEDIA] ",
        format_args!(
            "MEDIA_TX_RESUMED iface={} packets={} bytes={}",
            iface, run.packets, run.bytes
        ),
    );
}
