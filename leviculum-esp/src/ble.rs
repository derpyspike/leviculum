//! Bluetooth LE on the M9: Columba over esp-radio's HCI controller (1480).
//!
//! The host stack, the Columba roles and the `[BLE ]` lines are
//! `leviculum_trouble`'s; this module names the transport and the
//! platform:
//!
//! - the radio blobs' C heap, one internal-DRAM region behind esp-alloc's
//!   `compat` symbols — the Rust global allocator stays the rlsf pools in
//!   PSRAM, where controller memory must not land,
//! - the scheduler the blobs' tasks run under, started by
//!   `radio::start` before this (WiFi shares it, 1479), which turns core
//!   0's loop into the scheduler's main task and otherwise leaves it
//!   alone — no task of ours runs on it,
//! - the one joined future of every Columba task, polled from the loop
//!   with a no-op waker once a turn, exactly as the LR1110's task is: a
//!   missed wake costs a turn of latency, never a link — the controller
//!   keeps the link-layer timing on its own preemptive tasks,
//! - the drain of the backend's log ring onto the UART between frames.
//!
//! Boot cost, measured where it runs: `[BLE ] heap used=… free=…` right
//! after the controller is up says what the blobs took from the internal
//! region.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use esp_alloc::{HeapRegion, MemoryCapability};
use esp_hal::peripherals::BT;
use esp_radio::ble::controller::BleConnector;
use leviculum_ble_link::{BleChannels, BleInterface, BleLinkEvent};
use leviculum_trouble::ble::columba;
use leviculum_trouble::ble::{build_stack, run_columba, Resources, StackCell};

use crate::handheld::BLE_IFACE;
use crate::time_driver::poll_once;
use crate::UartLog;

/// A peer transition as the loop consumes it, in its channel's order —
/// the role the seam reports is the backend's business, not the loop's.
pub enum PeerEvent {
    Up([u8; 16]),
    Lost([u8; 16]),
}

/// The radio blobs' internal-DRAM top-up: a region of their own beside
/// the reclaimed bootloader region `radio::start` gives esp-alloc
/// (`radio::RECLAIMED_HEAP_BYTES`, 72 KiB), both behind
/// `malloc_internal`. 44 KiB from 1480 until 1479, when WiFi's IRAM code
/// and statics (`.rwtext.wifi` 29 → 52 KiB, the WiFi blobs' .bss) took
/// 43 KiB of the bank core 0's stack is what is left of: the reclaimed
/// region costs that stack nothing, so it carries the blobs now, and
/// this static shrank to what the build's STACK_HEADROOM floor leaves.
/// The pool the blobs see grew all the same, 44 → 80 KiB. The boot's
/// `[BLE ] heap` and `[WIFI] heap` lines state the real take; if the
/// bench shows the two carriers need more, the UI core's stack
/// (`UI_STACK` in `thinknode_m9.rs`) is the next bank to draw on, and
/// this constant follows the measurement. Only a board that runs a
/// carrier spends it at runtime.
pub const BLE_INTERNAL_HEAP_BYTES: usize = 8 * 1024;

/// The BLE carrier, as the loop drives it: the joined Columba future,
/// the node-facing channel ends, and the log drain's line accumulator.
pub struct Ble {
    future: Pin<Box<dyn Future<Output = ()>>>,
    channels: BleChannels,
    /// Bytes of an unfinished ring line between drains.
    line: Vec<u8>,
}

/// Bring the controller up and build the Columba future; the scheduler
/// is already running (`radio::start`). `None` — with
/// its reason on the log — when the controller refused; the board runs
/// on without BLE, exactly like the nRF's `BLE_INIT_FAIL` path. The
/// caller has already decided the carrier is wanted (the media record);
/// a board that wants it off never calls this and never powers the
/// radio.
///
/// Returns the future holder and the interface the node sends through.
pub fn start(
    bt: BT<'static>,
    identity_hash: [u8; 16],
    log: &mut UartLog<'_>,
) -> Option<(Ble, BleInterface)> {
    leviculum_trouble::name::note_boot_identity(identity_hash);
    // The M9's UART is open from boot: no DTR gate holds the backend's
    // runtime lines back.
    leviculum_trouble::log::open_runtime_drain();
    // The shared node side's lines (1715) ride the same ring the
    // backend's do; installed before any BLE task could write one.
    leviculum_ble_link::log::set_sink(ble_link_log);

    // The blobs' heap, before the controller allocates from it.
    static mut BLE_HEAP: [u8; BLE_INTERNAL_HEAP_BYTES] = [0; BLE_INTERNAL_HEAP_BYTES];
    // SAFETY: the static is given to esp-alloc here once and never
    // touched by anything else; `start` runs once (the StaticCell below
    // panics a second call before this could alias).
    unsafe {
        esp_alloc::HEAP.add_region(HeapRegion::new(
            core::ptr::addr_of_mut!(BLE_HEAP).cast(),
            BLE_INTERNAL_HEAP_BYTES,
            MemoryCapability::Internal.into(),
        ));
    }

    let connector = match BleConnector::new(bt, esp_radio::ble::Config::default()) {
        Ok(connector) => connector,
        Err(e) => {
            log.line("[BLE ] ", format_args!("off reason=controller err={:?}", e));
            return None;
        }
    };
    static STACK: StackCell<BleConnector<'static>> = StackCell::new();
    // The host's tables in PSRAM, off the loop core's pool: internal
    // DRAM is the blobs' and the stacks' bank, and the host only ever
    // reads these with the cache on.
    let resources = Box::leak(Box::new(Resources::new()));
    let stack = build_stack(&STACK, resources, connector, &identity_hash);
    log.line(
        "[BLE ] ",
        format_args!(
            "heap used={} free={} of={}",
            esp_alloc::HEAP.used(),
            esp_alloc::HEAP.free(),
            BLE_INTERNAL_HEAP_BYTES
        ),
    );
    // A refused GATT registration keeps the board alive without BLE;
    // its reason is on the backend's ring, which nothing drains without
    // a `Ble`, so the UART line here is the one a bench reads.
    let Some((server, hint)) = columba::start(identity_hash) else {
        log.line("[BLE ] ", format_args!("off reason=gatt"));
        return None;
    };
    let channels = leviculum_ble_link::channels();
    let interface = BleInterface::new(BLE_IFACE, channels.outgoing_tx);
    // The caller has already read the media record and wants the
    // carrier: push the profile's verdict before the tasks first gate
    // on it — the shared boot state is off, and `--ble off` never
    // reaches this function at all.
    leviculum_ble_link::set_active(true);
    let future: Pin<Box<dyn Future<Output = ()>>> = Box::pin(async move {
        run_columba(stack, server, hint, identity_hash).await;
    });
    Some((
        Ble {
            future,
            channels,
            line: Vec::new(),
        },
        interface,
    ))
}

/// The shared node side's log sink: onto the backend's ring, between
/// the same frames.
fn ble_link_log(
    prefix: &'static str,
    args: core::fmt::Arguments<'_>,
    level: leviculum_ble_link::log::Level,
) {
    match level {
        leviculum_ble_link::log::Level::Runtime => leviculum_trouble::log::log_fmt(prefix, args),
        leviculum_ble_link::log::Level::Critical => {
            leviculum_trouble::log::log_fmt_critical(prefix, args)
        }
    }
}

impl Ble {
    /// One poll of every Columba task, once a loop turn.
    pub fn poll(&mut self) {
        let _ = poll_once(self.future.as_mut());
    }

    /// One completed inbound packet, with its link peer where the
    /// handshake named one; `None` when the turn has taken them all.
    /// Taking it ends the packet's census custody
    /// ([`leviculum_ble_link::incoming_held_sub`]), exactly where the
    /// nRF main loops subtract.
    pub fn take_incoming(&mut self) -> Option<(Option<[u8; 16]>, Vec<u8>)> {
        match self.channels.incoming_rx.try_receive() {
            Ok(BleLinkEvent::Inbound { link, packet }) => {
                leviculum_ble_link::incoming_held_sub(packet.capacity());
                Some((link, packet))
            }
            _ => None,
        }
    }

    /// One peer transition, in order; `None` when the turn is caught up.
    pub fn take_peer_event(&mut self) -> Option<PeerEvent> {
        match self.channels.peer_event_rx.try_receive() {
            Ok(BleLinkEvent::PeerUp { identity, .. }) => Some(PeerEvent::Up(identity)),
            Ok(BleLinkEvent::PeerLost { identity, .. }) => Some(PeerEvent::Lost(identity)),
            _ => None,
        }
    }

    /// Drain the backend's log ring onto the UART, whole lines only: the
    /// ring's lines carry their own ` t=<ms>` stamp from the same clock
    /// as the loop's, and `UartLog::raw` writes each between frames.
    pub fn drain_log(&mut self, log: &mut UartLog<'_>) {
        let mut chunk = [0u8; 256];
        loop {
            let n = leviculum_trouble::log::LOG_RING.read(&mut chunk);
            if n == 0 {
                break;
            }
            for &byte in &chunk[..n] {
                if byte == b'\n' {
                    while self.line.last() == Some(&b'\r') {
                        self.line.pop();
                    }
                    if let Ok(text) = core::str::from_utf8(&self.line) {
                        if !text.is_empty() {
                            log.raw(text);
                        }
                    }
                    self.line.clear();
                } else if self.line.len() < 1024 {
                    self.line.push(byte);
                }
            }
        }
    }
}
