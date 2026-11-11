//! Bluetooth LE on the M9: Columba over esp-radio's HCI controller (1480).
//!
//! The host stack, the Columba roles and the `[BLE ]` lines are
//! `leviculum_trouble`'s; this module names the transport and the
//! platform:
//!
//! - the radio blobs' C heap, one internal-DRAM region behind esp-alloc's
//!   `compat` symbols — the Rust global allocator stays the rlsf pools in
//!   PSRAM, where controller memory must not land,
//! - the scheduler the blobs' tasks run under (`esp_rtos::start`), which
//!   turns core 0's loop into the scheduler's main task and otherwise
//!   leaves it alone — no task of ours runs on it,
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
use esp_hal::peripherals::{BT, FROM_CPU_INTR0};
use esp_hal::timer::timg::Timer;
use esp_radio::ble::controller::BleConnector;
use leviculum_trouble::ble::{
    build_stack, channels, run_columba, BleChannels, BleInterface, Resources, StackCell,
};
pub use leviculum_trouble::ble::PeerEvent;
use leviculum_trouble::ble::columba;

use crate::handheld::BLE_IFACE;
use crate::time_driver::poll_once;
use crate::UartLog;

/// The radio blobs' internal-DRAM heap: the controller's pools and the
/// scheduler's task stacks, everything behind `malloc_internal`. 44 KiB
/// is what the internal bank has left once core 0's stack floor
/// (94,976 B, measured + reserve) and the UI core's (96 KiB) are paid —
/// the whole DRAM+IRAM bank is ~340 KiB and the blobs' code already
/// took 29 KiB of it into IRAM. The boot's `[BLE ] heap` line states
/// the real take; if the bench shows the blobs need more, the UI
/// core's 32 KiB above its own floor is the next bank to draw on, and
/// this constant follows the measurement. Only a board whose media
/// record keeps BLE on spends it at runtime — the region is handed to
/// esp-alloc either way, but nothing allocates from it with the radio
/// off.
pub const BLE_INTERNAL_HEAP_BYTES: usize = 44 * 1024;

/// The BLE carrier, as the loop drives it: the joined Columba future,
/// the node-facing receivers, and the log drain's line accumulator.
pub struct Ble {
    future: Pin<Box<dyn Future<Output = ()>>>,
    incoming_rx: IncomingRx,
    peer_event_rx: PeerEventRx,
    /// Bytes of an unfinished ring line between drains.
    line: Vec<u8>,
}

type IncomingRx = embassy_sync::channel::Receiver<
    'static,
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    (Option<[u8; 16]>, Vec<u8>),
    { leviculum_trouble::ble::QUEUE_DEPTH },
>;
type PeerEventRx = embassy_sync::channel::Receiver<
    'static,
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    PeerEvent,
    8,
>;

/// Bring the controller up and build the Columba future. `None` — with
/// its reason on the log — when the controller refused; the board runs
/// on without BLE, exactly like the nRF's `BLE_INIT_FAIL` path. The
/// caller has already decided the carrier is wanted (the media record);
/// a board that wants it off never calls this and never powers the
/// radio.
///
/// Returns the future holder and the interface the node sends through.
pub fn start(
    timer: Timer<'static>,
    int0: FROM_CPU_INTR0<'static>,
    bt: BT<'static>,
    identity_hash: [u8; 16],
    log: &mut UartLog<'_>,
) -> Option<(Ble, BleInterface)> {
    leviculum_trouble::name::note_boot_identity(identity_hash);
    // The M9's UART is open from boot: no DTR gate holds the backend's
    // runtime lines back.
    leviculum_trouble::log::open_runtime_drain();

    // The blobs' heap, before the scheduler: esp-rtos allocates its task
    // stacks through the same `malloc_internal`.
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

    // The scheduler the blobs require. Core 0 only: this loop becomes
    // its main task and runs on; the UI core keeps its own regime
    // (`corepark` parks it for flash ops exactly as before).
    esp_rtos::start(timer, int0);

    let connector = match BleConnector::new(bt, esp_radio::ble::Config::default()) {
        Ok(connector) => connector,
        Err(e) => {
            log.line(
                "[BLE ] ",
                format_args!("off reason=controller err={:?}", e),
            );
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
    let (server, hint) = columba::start(identity_hash);
    let BleChannels {
        incoming_rx,
        outgoing_tx,
        peer_event_rx,
    } = channels();
    let interface = BleInterface::new(BLE_IFACE, outgoing_tx);
    let future: Pin<Box<dyn Future<Output = ()>>> = Box::pin(async move {
        run_columba(stack, server, hint, identity_hash).await;
    });
    Some((
        Ble {
            future,
            incoming_rx,
            peer_event_rx,
            line: Vec::new(),
        },
        interface,
    ))
}

impl Ble {
    /// One poll of every Columba task, once a loop turn.
    pub fn poll(&mut self) {
        let _ = poll_once(self.future.as_mut());
    }

    /// One completed inbound packet, with its link peer where the
    /// handshake named one; `None` when the turn has taken them all.
    pub fn take_incoming(&mut self) -> Option<(Option<[u8; 16]>, Vec<u8>)> {
        self.incoming_rx.try_receive().ok()
    }

    /// One peer transition, in order; `None` when the turn is caught up.
    pub fn take_peer_event(&mut self) -> Option<PeerEvent> {
        self.peer_event_rx.try_receive().ok()
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
