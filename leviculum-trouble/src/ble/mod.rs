//! BLE for the TrouBLE-hosted boards: the host over the board's HCI
//! transport, split along the same protocol-reuse seam as
//! `leviculum-nrf/src/ble/mod.rs`.
//!
//! - [`columba`] is **the protocol** (Columba v2.2): the GATT service and
//!   its UUIDs, the identity handshake, the keepalive, the advertisement
//!   with the v0.3.0 capability record, the scanner and the connection
//!   rule, the GATT-client central path.
//! - this module and [`notify`] are **protocol-neutral**: the host stack
//!   bring-up, the per-link outbound queues and the fragment pumps over
//!   notifications and Write Commands.
//!
//! The node side — the channels, the carrier gate, the pacing value, the
//! drain table, the link table and the BLE `Interface` — is
//! `leviculum_ble_link`'s since 1715, the same crate the nRF backend
//! stands behind (there through `leviculum_firmware::ble_link`). This
//! backend implements [`leviculum_ble_link::BleLink`] ([`TroubleLink`])
//! over its slot map and reports through
//! [`leviculum_ble_link::BleLinkEvent`]; until 1715 it carried its own
//! copy of all of it, the second layer the no-duplication rule of
//! 2026-10-23 forbids.
//!
//! What differs from the nRF module, and why:
//!
//! - **Controller completions carry the connection handle.** Each pump waits
//!   on its own drain slot. Enqueue acceptance and a real HCI completion are
//!   separate events; an exhausted shared packet pool never invents a drain.
//! - **The advertisement can be rebuilt live.** TrouBLE restarts
//!   advertising with new data whenever a peripheral task starts one, so
//!   an operator's name reaches the air at the next advertising start,
//!   not the next boot.
//! - **Own address.** A static random address derived from the identity
//!   hash: stable across resets, which is what the Identity characteristic
//!   exists to survive on phones, and what the v2.2 MAC-sorting rule
//!   compares.
//! - **No tasks of its own.** The fork spawns these futures on embassy
//!   executors; the M9 polls [`run_columba`] from its core-0 loop with a
//!   no-op waker, once a turn, as it polls its LoRa task. Everything here
//!   is plain `async fn`, and a missed wake only costs one turn of
//!   latency — the controller keeps the link-layer timing either way.
//!
//! Slots: [`PERIPH_LINKS`] incoming links (phones and boards), one
//! outgoing central link, [`MAX_LINKS`] in all, the nRF's #372 shape.

extern crate alloc;

pub mod columba;
#[cfg(test)]
mod h4_tests;
mod incoming;
pub mod notify;
mod peer_address;

use alloc::vec::Vec;

use core::convert::Infallible;

use bt_hci::controller::ExternalController;
use bt_hci::transport::Transport;
use bt_hci::ReadHciError;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use leviculum_ble_tx::{Enqueue, Fill, TxAim};
use static_cell::StaticCell;
use trouble_host::prelude::*;

pub use incoming::BLE_RX_QUEUE_FULL;
pub use notify::{BLE_TX_DRAIN_UNROUTED, BLE_TX_DRAIN_WAITS, BLE_TX_DROPPED, BLE_TX_PACKETS};

// The node side of the BLE link (1715): the shared drain table, the
// carrier gate and the pacing value, re-imported so the backend's own
// modules keep their `super::` paths — exactly the shape of
// `leviculum-nrf/src/ble/mod.rs`'s re-import block.
pub(crate) use leviculum_ble_link::{carrier_off, carrier_on, CarrierWaiter};
pub use leviculum_ble_link::{BleChannels, BleInterface, BleLinkEvent, TX_DRAIN};

/// Incoming (peripheral-role) link slots: a phone and two boards, the
/// nRF's #372 count.
pub const PERIPH_LINKS: usize = 3;
/// Outgoing (central-role) links: one at a time.
pub const CENTRAL_LINKS: usize = 1;
/// Every link at once — and the shared drain table's slot count, which
/// the heap budget pins.
pub const MAX_LINKS: usize = PERIPH_LINKS + CENTRAL_LINKS;
const _: () = assert!(MAX_LINKS == leviculum_heap_budget::MAX_LINKS);
/// Credit-based L2CAP channel slots the host reserves. ATT and the LE
/// signalling channel ride fixed CIDs the host keeps no storage for, so
/// Columba uses none of these (the scripted-controller test runs green
/// with zero, 1715); two per link is a reserve for a peer that opens a
/// credit-based channel, which the host then answers instead of
/// refusing for want of a slot.
const L2CAP_CHANNELS: usize = 2 * MAX_LINKS;
/// The depth of each PER-LINK outbound queue. 4 was the nRF's
/// SoftDevice-era bound; on this family a public relay's announce bursts
/// share the per-link outbound queue with data packets and 4 drops them
/// (the 2026-09-18 Dallas soak: 202 fan-out drops in 600 s). 16 covers a
/// burst at the observed rate without giving a stalled peer a deeper hole
/// to fill before the drop counter reports it. The node-facing channels
/// are the shared crate's and run at the heap budget's depth; the soak's
/// drops were here, between the fan-out and the pumps, not there.
pub const QUEUE_DEPTH: usize = 16;

/// An HCI transport the host can drive: what [`ExternalController`] needs
/// to answer every command TrouBLE issues (esp-radio's `BleConnector`,
/// the CYW43439's `BtDriver`).
pub trait HciTransport: Transport<Error: From<ReadHciError<Infallible>>> + 'static {}
impl<T> HciTransport for T where T: Transport<Error: From<ReadHciError<Infallible>>> + 'static {}

/// The board's HCI transport wrapped as a controller. The slot count is the
/// HCI command/event queue depth TrouBLE's own Pico 2 W example uses.
pub type Controller<T> = ExternalController<T, 10>;
/// The host's tables, allocated by the firmware: on the M9 they go into
/// PSRAM through its heap, because internal DRAM is the scarce bank the
/// radio blobs and both cores' stacks share, and nothing in here is
/// touched while the cache is off (no BLE code runs then — core 0's
/// interrupts are masked for the flash op and the blobs' tasks with
/// them).
pub type Resources = HostResources<DefaultPacketPool, MAX_LINKS, L2CAP_CHANNELS>;
/// The host stack, `'static` for the tasks.
pub type BleStack<T> = Stack<'static, Controller<T>, DefaultPacketPool>;
/// The host stack's runner, what [`run_host`] drives.
pub type BleRunner<T> = Runner<'static, Controller<T>, DefaultPacketPool>;
/// The cell the firmware's bring-up places its stack in.
pub type StackCell<T> = StaticCell<BleStack<T>>;

/// Links holding a drain slot right now (the `ble_links=` status field).
pub fn links_claimed() -> usize {
    TX_DRAIN.claimed()
}

/// One packet queue, as the per-link outbound queues use it (the
/// node-facing channels are `leviculum_ble_link`'s since 1715).
type PacketQueue = Channel<CriticalSectionRawMutex, Vec<u8>, QUEUE_DEPTH>;

/// Per-link outbound queues, indexed by link slot. One broadcast from the
/// core must reach every peer, so [`TroubleLink`] — driven by the shared
/// fan-out, the only consumer of the node-facing outbound channel —
/// copies it onto every claimed slot; a routed packet (#376) goes on the
/// addressed peer's link alone.
static LINK_OUT: [PacketQueue; MAX_LINKS] = [const { Channel::new() }; MAX_LINKS];

/// Packet bytes accepted by the interface and both receive roles.
pub use leviculum_heap_budget::INTERFACE_MTU;
/// Largest accepted assembled packet. This is enforced before payload copies.
pub const MAX_REASSEMBLED_BYTES: usize = INTERFACE_MTU;
const MIN_FRAGMENT_PAYLOAD: usize =
    leviculum_core::framing::ble::payload_per_fragment(leviculum_core::framing::ble::MIN_MTU);
/// ATT23 can carry fifteen packet bytes in one Columba fragment.
pub const MAX_PACKET_FRAGMENTS: usize = INTERFACE_MTU.div_ceil(MIN_FRAGMENT_PAYLOAD);
/// The same receive profile is used by peripheral and central sessions.
pub const REASSEMBLY_LIMITS: leviculum_core::framing::ble::BleReassemblyLimits =
    leviculum_core::framing::ble::BleReassemblyLimits::new(
        INTERFACE_MTU,
        MAX_PACKET_FRAGMENTS as u16,
    );

const _: () = {
    assert!(MIN_FRAGMENT_PAYLOAD == 15);
    assert!(MAX_PACKET_FRAGMENTS == 38);
    assert!(leviculum_ble_tx::GATT_VALUE_MAX == 253);
};

pub(crate) fn link_out(slot_index: usize) -> &'static PacketQueue {
    &LINK_OUT[slot_index]
}

/// This backend's side of [`leviculum_ble_link::BleLink`]: what the
/// orchestration asks of the links below it. The same resolution the
/// nRF's `SoftDeviceLink` performs — the registry plans
/// (`columba::plan_fanout`), the drain table says which links are
/// still claimed, `leviculum_ble_tx::resolve_fanout` decides.
///
/// `try_send`, never `send`: a link whose queue is full — a peer that
/// stopped draining — costs that link the packet and is told so in the
/// log, but must not stall delivery to the healthy links or wedge the
/// fan-out. With no live link at all a flooded packet is dropped
/// silently; a ROUTED packet whose peer has no live link is dropped
/// loudly (`BLE_TX_ROUTE_MISS`) rather than flooded — see
/// [`leviculum_ble_tx::TxFanout::NoLink`] for why.
pub struct TroubleLink;

impl leviculum_ble_link::BleLink for TroubleLink {
    fn send(&mut self, aim: TxAim, packet: Vec<u8>) {
        match leviculum_ble_tx::resolve_fanout(&TX_DRAIN, columba::plan_fanout(aim)) {
            // `Link` and `RouteMiss` are only reachable from `Peer`, so
            // the peer is known in both arms.
            leviculum_ble_tx::Delivery::Link { slot, handle } => {
                let hint = aim_peer(aim);
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_TX_ROUTE peer={:02x}{:02x}{:02x}{:02x} conn={} slot={} len={}",
                        hint[0],
                        hint[1],
                        hint[2],
                        hint[3],
                        handle,
                        slot,
                        packet.len()
                    ),
                );
                queue_on_link(slot, packet);
            }
            leviculum_ble_tx::Delivery::RouteMiss => log_route_miss(&aim_peer(aim), packet.len()),
            leviculum_ble_tx::Delivery::Flood(targets) => {
                let links = flood(&packet, targets);
                match aim {
                    // The #422 ingress link: every live link but the one
                    // these bytes came in on.
                    TxAim::FloodExcept(excluded) => crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!(
                            "BLE_TX_FLOOD links={} len={} excl_link={:02x}{:02x}{:02x}{:02x}",
                            links,
                            packet.len(),
                            excluded[0],
                            excluded[1],
                            excluded[2],
                            excluded[3]
                        ),
                    ),
                    _ => crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!("BLE_TX_FLOOD links={} len={}", links, packet.len()),
                    ),
                }
            }
        }
    }

    /// The advertised name on this family is rebuilt from
    /// [`crate::name::gap_name_now`] at every advertising start, so the
    /// seam's push updates the configured half of that rule and the next
    /// start carries it.
    fn set_advertised_name(&mut self, name: leviculum_core::node_name::NodeName) {
        crate::name::set_configured(Some(name));
    }
}

/// Copy one packet onto every link in `targets`, answering how many got
/// it.
fn flood(packet: &[u8], targets: leviculum_ble_tx::SlotMask) -> usize {
    let mut links = 0usize;
    for index in 0..LINK_OUT.len() {
        if !targets.contains(index) {
            continue;
        }
        links += 1;
        queue_on_link(index, packet.to_vec());
    }
    links
}

/// The peer an aim names, or zeroes for a plain flood (which names
/// none). Only used for logging and for the miss line.
fn aim_peer(aim: TxAim) -> [u8; 16] {
    match aim {
        TxAim::Flood => [0u8; 16],
        TxAim::Peer(peer) | TxAim::FloodExcept(peer) => peer,
    }
}

/// Put one packet in a link's outbound queue, reporting a full queue
/// ([`leviculum_ble_tx::link_queue_verdict`]).
fn queue_on_link(slot: usize, packet: Vec<u8>) {
    let len = packet.len();
    let queue = &LINK_OUT[slot];
    let fill = Fill::new(queue.len(), queue.capacity());
    let verdict = leviculum_ble_tx::link_queue_verdict(fill, leviculum_ble_tx::LinkItem::Fanout);
    if verdict != Enqueue::Queue || queue.try_send(packet).is_err() {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_TX_FANOUT_DROP slot={} len={} depth={}",
                slot, len, QUEUE_DEPTH
            ),
        );
    }
}

/// One line per routed packet whose peer holds no live link here.
fn log_route_miss(peer: &[u8; 16], len: usize) {
    BLE_TX_ROUTE_MISSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_TX_ROUTE_MISS peer={:02x}{:02x}{:02x}{:02x} len={}",
            peer[0], peer[1], peer[2], peer[3], len
        ),
    );
}

/// Routed packets dropped because the addressed peer held no live link
/// (Codeberg #376), for the `BLE_COUNTERS` line.
pub static BLE_TX_ROUTE_MISSES: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// The host's runner, with the scanner's report sink as its event
/// handler (`columba::Reports`).
pub async fn run_host<T: HciTransport>(mut runner: BleRunner<T>) -> ! {
    loop {
        if let Err(e) = runner.run_with_handler(&columba::REPORTS).await {
            crate::log::log_fmt_critical("[BLE ] ", format_args!("host runner error: {:?}", e));
            embassy_time::Timer::after_secs(1).await;
        }
    }
}

/// Periodic `BLE_COUNTERS` line, the nRF's shape.
pub async fn counters() -> ! {
    use core::sync::atomic::Ordering;
    loop {
        embassy_time::Timer::after_secs(60).await;
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_COUNTERS tx_packets={} tx_dropped={} drain_waits={} drain_unrouted={} rx_full={} route_misses={} refused={} displaced={} unidentified={} links={} peers={}",
                BLE_TX_PACKETS.load(Ordering::Relaxed),
                BLE_TX_DROPPED.load(Ordering::Relaxed),
                BLE_TX_DRAIN_WAITS.load(Ordering::Relaxed),
                BLE_TX_DRAIN_UNROUTED.load(Ordering::Relaxed),
                BLE_RX_QUEUE_FULL.load(Ordering::Relaxed),
                BLE_TX_ROUTE_MISSES.load(Ordering::Relaxed),
                columba::BLE_LINKS_REFUSED.load(Ordering::Relaxed),
                columba::BLE_LINKS_DISPLACED.load(Ordering::Relaxed),
                columba::BLE_PRE_HANDSHAKE_REFUSED.load(Ordering::Relaxed),
                links_claimed(),
                columba::live_peer_count(),
            ),
        );
    }
}

/// The static random address this node uses: the identity hash's first
/// six bytes with the two top bits set (Core Spec Vol 6 Part B 1.3.2.1).
/// Stable across resets, unlike a phone's rotating one.
pub fn own_address(identity_hash: &[u8; 16]) -> Address {
    let mut a = [0u8; 6];
    a.copy_from_slice(&identity_hash[..6]);
    // `BdAddr` is little-endian; the most significant byte is the last.
    a[5] |= 0xC0;
    Address::random(a)
}

/// Bring the host up over the board's HCI transport in `cell` and say so.
pub fn build_stack<T: HciTransport>(
    cell: &'static StackCell<T>,
    resources: &'static mut Resources,
    transport: T,
    identity_hash: &[u8; 16],
) -> &'static BleStack<T> {
    let controller: Controller<T> = ExternalController::new(transport);
    let address = own_address(identity_hash);
    let stack: &'static BleStack<T> = cell.init(
        trouble_host::new(controller, resources)
            .set_random_address(address)
            .build(),
    );
    crate::log::log_fmt_critical(
        "[BLE ] ",
        format_args!(
            "up addr={} periph_links={} central_links={}",
            address, PERIPH_LINKS, CENTRAL_LINKS
        ),
    );
    stack
}

/// Every Columba future joined: the host runner, one peripheral per
/// incoming link slot, the central half, the shared fan-out over
/// [`TroubleLink`] and the counters line. The firmware drives this one
/// future however it runs futures; none of its arms ever completes.
pub async fn run_columba<T: HciTransport>(
    stack: &'static BleStack<T>,
    server: &'static columba::ReticulumServer<'static>,
    hint: columba::AdvHint,
    identity_hash: [u8; 16],
) -> ! {
    // The three calls below are the slot count; a fourth slot would
    // advertise capacity no task serves.
    const { assert!(PERIPH_LINKS == 3) };
    let mut link = TroubleLink;
    embassy_futures::join::join4(
        run_host(stack.runner()),
        embassy_futures::join::join3(
            columba::peripheral(stack, server, 0, hint),
            columba::peripheral(stack, server, 1, hint),
            columba::peripheral(stack, server, 2, hint),
        ),
        columba::central(stack, server, identity_hash),
        embassy_futures::join::join(leviculum_ble_link::run_fanout(&mut link), counters()),
    )
    .await;
    unreachable!("every joined arm loops forever")
}

/// Teardown for the process-shared link state the host tests exercise.
///
/// `TX_DRAIN`, the session registry, the per-link queues and the node
/// channels are one set of statics per test binary, and the ring lock
/// serialises the tests that touch them without ordering them. Any test
/// that counts links or peers in absolute terms therefore starts from
/// this reset, and the scripted-controller bench calls it from its
/// `Drop` so the two live links it ends with by design never outlive it
/// (order 1976: a leftover claim turned the flood tests' counts wrong
/// in full workspace runs).
#[cfg(test)]
pub(crate) mod test_support {
    use leviculum_ble_link::session;
    use leviculum_ble_link::BleLinkEvent;

    /// Put every shared link global back to its boot state: no claimed
    /// drain slot, no registered peer or connection, empty per-link
    /// queues, empty node-facing channels, incoming held bytes refunded.
    pub(crate) fn reset_shared_links() {
        for slot in 0..super::MAX_LINKS {
            if let Some(handle) = super::TX_DRAIN.handle_at(slot) {
                session::peer_link_down(slot);
                session::conn_link_down(slot);
                super::TX_DRAIN.release(handle);
            }
            while super::link_out(slot).try_receive().is_ok() {}
        }
        let chans = leviculum_ble_link::channels();
        while chans.peer_event_rx.try_receive().is_ok() {}
        while let Ok(event) = chans.incoming_rx.try_receive() {
            if let BleLinkEvent::Inbound { packet, .. } = event {
                leviculum_ble_link::incoming_held_sub(packet.capacity());
            }
        }
    }
}
