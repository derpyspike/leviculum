//! BLE for the TrouBLE-hosted boards: the host over the board's HCI
//! transport, split along the same protocol-reuse seam as
//! `leviculum-nrf/src/ble/mod.rs`.
//!
//! - [`columba`] is **the protocol** (Columba v2.2): the GATT service and
//!   its UUIDs, the identity handshake, the keepalive, the advertisement
//!   with the v0.3.0 capability record, the scanner and the connection
//!   rule, the GATT-client central path.
//! - this module and [`notify`] are **protocol-neutral**: the host stack
//!   bring-up, the bounded packet channels and the [`BleInterface`], the
//!   handle-indexed drain router, the per-link outbound queues and fan-out,
//!   and the fragment pumps over notifications and Write Commands.
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
mod incoming;
pub mod notify;
mod peer_address;

use alloc::vec::Vec;

use core::convert::Infallible;

use bt_hci::controller::ExternalController;
use bt_hci::transport::Transport;
use bt_hci::ReadHciError;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_sync::signal::Signal;
use leviculum_ble_tx::{DrainRouter, SlotMask, TxAim, TxFanout};
use leviculum_core::traits::{Interface, InterfaceError, InterfaceKind};
use leviculum_core::InterfaceId;
use static_cell::StaticCell;
use trouble_host::prelude::*;

pub use incoming::BLE_RX_QUEUE_FULL;
pub use notify::{BLE_TX_DRAIN_UNROUTED, BLE_TX_DRAIN_WAITS, BLE_TX_DROPPED, BLE_TX_PACKETS};

/// Incoming (peripheral-role) link slots: a phone and two boards, the
/// nRF's #372 count.
pub const PERIPH_LINKS: usize = 3;
/// Outgoing (central-role) links: one at a time.
pub const CENTRAL_LINKS: usize = 1;
/// Every link at once.
pub const MAX_LINKS: usize = PERIPH_LINKS + CENTRAL_LINKS;
/// L2CAP channels the host reserves: one signalling and one ATT per link.
const L2CAP_CHANNELS: usize = 2 * MAX_LINKS;
/// The channel depth every BLE packet queue uses. 4 was the nRF's
/// SoftDevice-era bound; on this family a public relay's announce bursts
/// share the per-link outbound queue with data packets and 4 drops them
/// (the 2026-09-18 Dallas soak: 202 fan-out drops in 600 s). 16 covers a
/// burst at the observed rate without giving a stalled peer a deeper hole
/// to fill before the drop counter reports it.
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

/// One ownership table for session queues and handle-specific HCI drains.
pub(crate) static TX_DRAIN: DrainRouter<MAX_LINKS> = DrainRouter::new();

pub(crate) fn link_handle_at(slot: usize) -> Option<u16> {
    TX_DRAIN.handle_at(slot)
}

/// Links holding a drain slot right now (the `ble_links=` status field).
pub fn links_claimed() -> usize {
    TX_DRAIN.claimed()
}

/// One packet queue, as both the node-facing channels and the per-link
/// outbound queues use it. The outbound aim carries both the routed-peer
/// hint and the ingress peer a broadcast must not return to.
type PacketQueue = Channel<CriticalSectionRawMutex, Vec<u8>, QUEUE_DEPTH>;
type InboundQueue = Channel<CriticalSectionRawMutex, (Option<[u8; 16]>, Vec<u8>), QUEUE_DEPTH>;
type OutboundQueue = Channel<CriticalSectionRawMutex, (TxAim, Vec<u8>), QUEUE_DEPTH>;

static BLE_INCOMING: InboundQueue = Channel::new();
static BLE_OUTGOING: OutboundQueue = Channel::new();

/// A peer transition (Codeberg #365): the identity's FIRST link came up,
/// or its LAST link died. Both on one ordered channel so a link flap's
/// `Lost` lands before its `Up`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerEvent {
    Up([u8; 16]),
    Lost([u8; 16]),
}

static BLE_PEER_EVENTS: Channel<CriticalSectionRawMutex, PeerEvent, 8> = Channel::new();

pub(crate) fn report_peer_event(event: PeerEvent) {
    if BLE_PEER_EVENTS.try_send(event).is_err() {
        let (kind, identity) = match event {
            PeerEvent::Up(id) => ("up", id),
            PeerEvent::Lost(id) => ("lost", id),
        };
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_PEER_EVENT_DROPPED kind={} peer={:02x}{:02x}{:02x}{:02x}",
                kind, identity[0], identity[1], identity[2], identity[3]
            ),
        );
    }
}

/// The runtime half of the carrier switch: one latch per protocol task
/// and per live session, poked by [`note_media_changed`]; the woken task
/// re-reads [`crate::media::ble_active`] and a live session disconnects
/// its link, an advertiser or scanner drops its future.
static CARRIER_WAKES: [Signal<CriticalSectionRawMutex, ()>; PERIPH_LINKS + 1 + MAX_LINKS] =
    [const { Signal::new() }; PERIPH_LINKS + 1 + MAX_LINKS];

#[derive(Clone, Copy)]
pub(crate) enum CarrierWaiter {
    Peripheral(usize),
    Central,
    Session(usize),
}

impl CarrierWaiter {
    fn index(self) -> usize {
        match self {
            CarrierWaiter::Peripheral(i) => i,
            CarrierWaiter::Central => PERIPH_LINKS,
            CarrierWaiter::Session(slot) => PERIPH_LINKS + 1 + slot,
        }
    }
}

/// Wake every BLE protocol task to re-read the carrier switch. Called by
/// the firmware after [`crate::media::set_ble_active`].
pub fn note_media_changed() {
    for wake in &CARRIER_WAKES {
        wake.signal(());
    }
}

pub(crate) async fn carrier_off(waiter: CarrierWaiter) {
    let wake = &CARRIER_WAKES[waiter.index()];
    while crate::media::ble_active() {
        wake.wait().await;
    }
}

pub(crate) async fn carrier_on(waiter: CarrierWaiter) {
    let wake = &CARRIER_WAKES[waiter.index()];
    while !crate::media::ble_active() {
        wake.wait().await;
    }
}

/// The gap the pumps serve: the compiled default; an operator override is
/// a bench instrument this family has not needed yet.
pub(crate) fn tx_gap_ms() -> u16 {
    leviculum_ble_tx::effective_tx_gap_ms(None)
}

/// Per-link outbound queues, indexed by link slot. One broadcast from the
/// core must reach every peer, so the fan-out is the only consumer of
/// [`BLE_OUTGOING`]; a routed packet (#376) goes on the addressed peer's
/// link alone.
static LINK_OUT: [PacketQueue; MAX_LINKS] = [const { Channel::new() }; MAX_LINKS];

/// Packet bytes accepted by the interface and both receive roles.
pub const INTERFACE_MTU: usize = 564;
/// Largest accepted assembled packet. This is enforced before payload copies.
pub const MAX_REASSEMBLED_BYTES: usize = INTERFACE_MTU;
/// Exact assembled-vector capacity bound; reassembly never grows geometrically.
pub const REASSEMBLY_CAPACITY_BYTES: usize = MAX_REASSEMBLED_BYTES;
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

// An upper-bound allocation model for the census estimates below: block
// sizes rounded to a word with a two-word minimum. The fork measured it
// on Llff; the M9's rlsf rounds no worse, so the estimates stay bounds.
const ALLOCATION_ALIGNMENT: usize = core::mem::align_of::<usize>();
const MIN_ALLOCATION_BYTES: usize = 2 * core::mem::size_of::<usize>();

const fn allocation_bytes(bytes: usize) -> usize {
    if bytes == 0 {
        0
    } else if bytes < MIN_ALLOCATION_BYTES {
        MIN_ALLOCATION_BYTES
    } else {
        bytes.div_ceil(ALLOCATION_ALIGNMENT) * ALLOCATION_ALIGNMENT
    }
}

/// A queued/pumping packet's maximum modelled allocation.
pub const PACKET_HEAP_BYTES: usize = allocation_bytes(INTERFACE_MTU);

const _: () = {
    assert!(MIN_FRAGMENT_PAYLOAD == 15);
    assert!(MAX_PACKET_FRAGMENTS == 38);
    assert!(leviculum_ble_tx::GATT_VALUE_MAX == 253);
};

/// Maximum allocation bytes pinned by the ordinary packets in each queue,
/// for a census walker: per-slot outbound, then the two node-facing
/// channels together.
pub fn session_census() -> ([usize; MAX_LINKS], usize) {
    let mut slots = [0usize; MAX_LINKS];
    for (index, value) in slots.iter_mut().enumerate() {
        *value = LINK_OUT[index].len() * PACKET_HEAP_BYTES;
    }
    (
        slots,
        (BLE_OUTGOING.len() + BLE_INCOMING.len()) * PACKET_HEAP_BYTES,
    )
}

pub(crate) fn link_out(slot_index: usize) -> &'static PacketQueue {
    &LINK_OUT[slot_index]
}

/// The fan-out: the only consumer of the node-facing outbound queue,
/// resolving each aim over the link registry onto the per-link queues.
pub async fn tx_fanout() -> ! {
    loop {
        let (aim, packet) = BLE_OUTGOING.receive().await;
        match columba::plan_fanout(aim) {
            TxFanout::Route(slot) => {
                let Some(handle) = link_handle_at(slot) else {
                    if let TxAim::Peer(peer) = aim {
                        log_route_miss(&peer, packet.len());
                    }
                    continue;
                };
                if let TxAim::Peer(peer) = aim {
                    crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!(
                            "BLE_TX_ROUTE peer={:02x}{:02x}{:02x}{:02x} conn={} slot={} len={}",
                            peer[0],
                            peer[1],
                            peer[2],
                            peer[3],
                            handle,
                            slot,
                            packet.len()
                        ),
                    );
                }
                queue_on_link(slot, packet);
            }
            TxFanout::NoLink => {
                if let TxAim::Peer(peer) = aim {
                    log_route_miss(&peer, packet.len());
                }
            }
            fanout @ (TxFanout::Flood | TxFanout::FloodExcept(_)) => {
                let skip = match fanout {
                    TxFanout::FloodExcept(skip) => skip,
                    _ => SlotMask::EMPTY,
                };
                let len = packet.len();
                let mut links = 0usize;
                let mut last = None;
                for index in 0..MAX_LINKS {
                    if skip.skips(index) || link_handle_at(index).is_none() {
                        continue;
                    }
                    links += 1;
                    if let Some(previous) = last.replace(index) {
                        if LINK_OUT[previous].is_full() {
                            log_fanout_drop(previous, len);
                        } else {
                            queue_on_link(previous, packet.clone());
                        }
                    }
                }
                if let Some(last) = last {
                    queue_on_link(last, packet);
                }
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!("BLE_TX_FLOOD links={} len={}", links, len),
                );
            }
        }
    }
}

fn queue_on_link(slot: usize, packet: Vec<u8>) {
    let len = packet.len();
    if !owned_packet_fits(&packet) || LINK_OUT[slot].try_send(packet).is_err() {
        log_fanout_drop(slot, len);
    }
}

fn owned_packet_fits(packet: &Vec<u8>) -> bool {
    packet.len() <= INTERFACE_MTU && packet.capacity() <= INTERFACE_MTU
}

fn log_fanout_drop(slot: usize, len: usize) {
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_TX_FANOUT_DROP slot={} len={} depth={}",
            slot, len, QUEUE_DEPTH
        ),
    );
}

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

/// The outbound producer is opaque so every accepted packet passes the
/// interface's byte bound before its exact-length copy reaches the queue.
pub struct BleOutgoingSender {
    sender: Sender<'static, CriticalSectionRawMutex, (TxAim, Vec<u8>), QUEUE_DEPTH>,
}

pub struct BleChannels {
    pub incoming_rx:
        Receiver<'static, CriticalSectionRawMutex, (Option<[u8; 16]>, Vec<u8>), QUEUE_DEPTH>,
    pub outgoing_tx: BleOutgoingSender,
    pub peer_event_rx: Receiver<'static, CriticalSectionRawMutex, PeerEvent, 8>,
}

pub fn channels() -> BleChannels {
    BleChannels {
        incoming_rx: BLE_INCOMING.receiver(),
        outgoing_tx: BleOutgoingSender {
            sender: BLE_OUTGOING.sender(),
        },
        peer_event_rx: BLE_PEER_EVENTS.receiver(),
    }
}

/// The BLE carrier as the core sees it: the firmware names the interface
/// id its node gave this carrier.
pub struct BleInterface {
    id: InterfaceId,
    sender: BleOutgoingSender,
    drops: leviculum_media_state::DropRun,
}

impl BleInterface {
    pub fn new(id: InterfaceId, sender: BleOutgoingSender) -> Self {
        Self {
            id,
            sender,
            drops: leviculum_media_state::DropRun::new(),
        }
    }

    /// Distinct live peer identities behind this interface (#365).
    pub fn peer_count(&self) -> usize {
        if crate::media::ble_active() {
            columba::live_peer_count()
        } else {
            0
        }
    }
}

impl Interface for BleInterface {
    fn id(&self) -> InterfaceId {
        self.id
    }
    fn name(&self) -> &str {
        "ble"
    }
    fn mtu(&self) -> usize {
        INTERFACE_MTU
    }
    fn kind(&self) -> InterfaceKind {
        InterfaceKind::Ble
    }
    fn is_online(&self) -> bool {
        crate::media::ble_active() && links_claimed() > 0
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        self.try_send_to_peer(data, None, false)
    }
    fn try_send_to_peer(
        &mut self,
        data: &[u8],
        peer: Option<&[u8; 16]>,
        _high_priority: bool,
    ) -> Result<(), InterfaceError> {
        self.queue(peer.map_or(TxAim::Flood, |peer| TxAim::Peer(*peer)), data)
    }
    fn try_send_excluding_peer(
        &mut self,
        data: &[u8],
        peer: &[u8; 16],
        _high_priority: bool,
    ) -> Result<(), InterfaceError> {
        self.queue(TxAim::FloodExcept(*peer), data)
    }
}

impl BleInterface {
    fn queue(&mut self, aim: TxAim, data: &[u8]) -> Result<(), InterfaceError> {
        if data.len() > INTERFACE_MTU {
            // Dropped, not an error: the core sized the packet by `mtu()`,
            // so this is a bug said out loud, and our `InterfaceError` has
            // no oversize variant — `BufferFull` would make the core
            // retry a packet that can never fit.
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_TX_OVERSIZE len={} max={}", data.len(), INTERFACE_MTU),
            );
            return Ok(());
        }
        if !crate::media::ble_active() {
            if let Some(run) = self.drops.dropped(data.len()) {
                crate::media::log_tx_drop(self.name(), run);
            }
            return Ok(());
        }
        if let Some(run) = self.drops.resumed() {
            crate::media::log_tx_resumed(self.name(), run);
        }
        if self.sender.sender.is_full() {
            crate::log::log_fmt(
                "[IFACE_FULL] ",
                format_args!(
                    "iface={} depth={} len={}",
                    self.name(),
                    self.sender.sender.capacity(),
                    data.len()
                ),
            );
            return Err(InterfaceError::BufferFull);
        }
        let mut packet = Vec::new();
        packet
            .try_reserve_exact(data.len())
            .map_err(|_| InterfaceError::BufferFull)?;
        packet.extend_from_slice(data);
        self.sender
            .sender
            .try_send((aim, packet))
            .map(|()| {
                crate::iface_bytes::note_tx(crate::iface_bytes::BLE, data.len());
            })
            .map_err(|_| {
                crate::log::log_fmt(
                    "[IFACE_FULL] ",
                    format_args!(
                        "iface={} depth={} len={}",
                        self.name(),
                        self.sender.sender.capacity(),
                        data.len()
                    ),
                );
                InterfaceError::BufferFull
            })
    }
}

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
/// incoming link slot, the central half, the fan-out and the counters
/// line. The firmware drives this one future however it runs futures;
/// none of its arms ever completes.
pub async fn run_columba<T: HciTransport>(
    stack: &'static BleStack<T>,
    server: &'static columba::ReticulumServer<'static>,
    hint: columba::AdvHint,
    identity_hash: [u8; 16],
) -> ! {
    // The three calls below are the slot count; a fourth slot would
    // advertise capacity no task serves.
    const { assert!(PERIPH_LINKS == 3) };
    embassy_futures::join::join4(
        run_host(stack.runner()),
        embassy_futures::join::join3(
            columba::peripheral(stack, server, 0, hint),
            columba::peripheral(stack, server, 1, hint),
            columba::peripheral(stack, server, 2, hint),
        ),
        columba::central(stack, server, identity_hash),
        embassy_futures::join::join(tx_fanout(), counters()),
    )
    .await;
    unreachable!("every joined arm loops forever")
}
