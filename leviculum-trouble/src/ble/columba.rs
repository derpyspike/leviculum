//! The Columba BLE protocol (v2.2, advertisement records v0.3.0) on the
//! TrouBLE host: the port of `leviculum-nrf/src/ble/columba.rs`.
//!
//! Everything on this side of the seam is what a Columba peer expects to
//! see on the air and nothing else: the GATT service and its three
//! characteristics under the `37145b00-…` vendor UUID space, the 16-byte
//! identity handshake, the 1-byte keepalive, the advertisement (the
//! service UUID a scanner filters on, the name in the scan response, the
//! v0.3.0 capability record), and the MAC-sorting rule that decides which
//! of two nodes initiates. The decisions themselves are
//! `leviculum-ble-tx`'s, host-tested; this file performs them.
//!
//! What it is built on — the host stack, the packet channels, the link
//! table, the fan-out, the fragment pump — lives in [`super`] and
//! [`super::notify`] and carries no Columba knowledge.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU8, Ordering};

use alloc::vec::Vec;
use embassy_futures::select::{select, select3, select4, Either, Either3, Either4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_sync::semaphore::{FairSemaphore, Semaphore};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use leviculum_ble_tx::{
    addr_value, admit, advertise_pass_pause_ms, connect_failure_condemns, dial_preference,
    dial_stage_failure, disconnect_settle_ms, drain_table_refusal, fragment_mtu, identity_hint,
    judge_read_identity, judge_supervision_timeout, manufacturer_data_with_hint,
    parse_peer_advertisement, refused_dial, scan_pass_pause_ms, with_free_slots, AdvReport,
    AdvertisePass, CandidateTable, ChurnLedger, ChurnPolicy, ConnParams, ConnParamsAsk,
    ConnParamsLine, ConnParamsReq, ConnParamsReqLine, ConnectDecision, DeadEnds, DialBackoff,
    DialFailure, DialLedger, DialRefusedLine, DialStage, DisconnectReason, DrainSlot, Exclusion,
    LedgerPolicy, Ledgers, LinkDownLine, LinkPhase, LinkRole, LinkUp, LinkVerdict, Local, Origin,
    PeerAddr, PeerRegistry, PhaseEvent, PhaseNeighbours, ReadIdentity, ScanMode, ScanPass,
    ScanPhase, StageError, TxGap, ADV_BYTES_USED, CAP_PERIPHERAL_ONLY, DEAD_END_SLOTS,
    DEAD_END_TTL_MS, DIAL_BACKOFF_SLOTS, GATT_VALUE_MAX, IDENTITY_HINT_LEN, LEGACY_AD_CAPACITY,
    MANUFACTURER_DATA_HINT_LEN, PERIPH_SLOTS, PHASE_NEIGHBOUR_SLOTS, SCAN_WINDOW_COLLECT_MS,
    WINDOW_CANDIDATES,
};
use leviculum_core::framing::ble::{
    self as ble_framing, BleDefragmenter, DefragResult, FRAGMENT_HEADER_SIZE, KEEPALIVE_BYTE,
    KEEPALIVE_INTERVAL_MS,
};
use static_cell::StaticCell;
use trouble_host::prelude::*;

use super::incoming::offer_incoming;
use super::notify::{notify_fragments, write_fragments};
use super::peer_address::peer_addr;
use super::{BleStack, CarrierWaiter, HciTransport, MAX_LINKS, PERIPH_LINKS};

/// Bound data characteristics by the shared Columba ATT ceiling, not the
/// link-layer DLE width. Each packet still uses its negotiated ATT MTU.
type CharValue = heapless::Vec<u8, GATT_VALUE_MAX>;

/// The Columba service (v2.2 §GATT Service Structure): a peer writes
/// `rx`, is notified on `tx`, and reads `identity` once at connect.
#[gatt_service(uuid = "37145b00-442d-4a94-917f-8f42c5da28e3")]
pub struct ReticulumService {
    #[characteristic(
        uuid = "37145b00-442d-4a94-917f-8f42c5da28e5",
        write,
        write_without_response
    )]
    rx: CharValue,
    #[characteristic(uuid = "37145b00-442d-4a94-917f-8f42c5da28e4", read, notify)]
    tx: CharValue,
    #[characteristic(uuid = "37145b00-442d-4a94-917f-8f42c5da28e6", read)]
    identity: [u8; 16],
}

#[gatt_server(connections_max = 4)]
pub struct ReticulumServer {
    reticulum: ReticulumService,
}

// `Uuid::new_long` takes the wire's little-endian order, as `#[gatt_service]` emits it.
const RETICULUM_SVC_UUID_LE: [u8; 16] = 0x37145b00_442d_4a94_917f_8f42c5da28e3_u128.to_le_bytes();
const RX_UUID: Uuid = Uuid::new_long(0x37145b00_442d_4a94_917f_8f42c5da28e5_u128.to_le_bytes());
const TX_UUID: Uuid = Uuid::new_long(0x37145b00_442d_4a94_917f_8f42c5da28e4_u128.to_le_bytes());
const IDENTITY_UUID: Uuid =
    Uuid::new_long(0x37145b00_442d_4a94_917f_8f42c5da28e6_u128.to_le_bytes());
const SERVICE_UUID: Uuid = Uuid::new_long(RETICULUM_SVC_UUID_LE);

/// The FIXED capability bits this node advertises and feeds into its own
/// side of the connection decision (zero: we can initiate).
const LOCAL_CAPS: u8 = 0;

/// Peripheral link slots currently spent on a session, for the
/// advertised free-slot count (#375 item 3). Not a gate.
static PERIPH_BUSY: AtomicU8 = AtomicU8::new(0);
const _: () = assert!(PERIPH_LINKS == PERIPH_SLOTS as usize);

struct PeripheralSlot;

impl PeripheralSlot {
    fn claim() -> Self {
        PERIPH_BUSY.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for PeripheralSlot {
    fn drop(&mut self) {
        PERIPH_BUSY.fetch_sub(1, Ordering::Relaxed);
    }
}

fn free_peripheral_slots() -> u8 {
    PERIPH_SLOTS.saturating_sub(PERIPH_BUSY.load(Ordering::Relaxed))
}

/// The v0.3.0 capability record this node advertises right now (the
/// record stays with the flag cleared rather than being dropped; see the
/// nRF module for why).
fn capability_record(hint: [u8; IDENTITY_HINT_LEN]) -> [u8; MANUFACTURER_DATA_HINT_LEN] {
    manufacturer_data_with_hint(with_free_slots(LOCAL_CAPS, free_peripheral_slots()), &hint)
}

/// The advertising PDU as it stands at this instant: flags, the service
/// UUID and the live capability record; built per advertising start.
fn advertisement_now(buf: &mut [u8; LEGACY_AD_CAPACITY], hint: [u8; IDENTITY_HINT_LEN]) -> usize {
    let record = capability_record(hint);
    AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteServiceUuids128(&[RETICULUM_SVC_UUID_LE]),
            AdStructure::Unknown {
                ty: 0xFF,
                data: &record,
            },
        ],
        buf,
    )
    .expect("Columba's shared legacy advertisement budget")
}

/// The scan response: the BLE name, the operator's if set, `LN-<hex8>`
/// otherwise, rebuilt at every advertising start (unlike the nRF's, which
/// is baked at boot; hence no `BLE_PENDING` on this family).
fn scan_response_now(buf: &mut [u8; 31]) -> usize {
    let name = crate::name::gap_name_now();
    AdStructure::encode_slice(&[AdStructure::CompleteLocalName(name.as_bytes())], buf).unwrap_or(0)
}

/// The identity hint the advertisement's capability record carries.
pub type AdvHint = [u8; IDENTITY_HINT_LEN];

/// Register the GATT service and log the advertisement's budget, before
/// the firmware spawns one peripheral task per incoming link slot and
/// the central half ([`run_columba`](crate::ble::run_columba)).
pub fn start(identity_hash: [u8; 16]) -> (&'static ReticulumServer<'static>, AdvHint) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut().set_local_identity(identity_hash));
    let hint = identity_hint(&identity_hash);
    const _: () = assert!(ADV_BYTES_USED == LEGACY_AD_CAPACITY);
    static SERVER: StaticCell<ReticulumServer<'static>> = StaticCell::new();
    static GAP_NAME: StaticCell<heapless::String<22>> = StaticCell::new();
    let gap_name = GAP_NAME.init(heapless::String::new());
    let _ = gap_name.push_str(crate::name::gap_name_now().as_str());
    let server: &'static ReticulumServer<'static> = SERVER.init(
        ReticulumServer::new_with_config(GapConfig::Peripheral(PeripheralConfig {
            name: gap_name.as_str(),
            appearance: &appearance::UNKNOWN,
        }))
        .expect("GATT server"),
    );
    // Publish the identity characteristic value so a connecting peer can
    // read it before exchanging frames over rx/tx.
    let _ = server.set(&server.reticulum.identity, &identity_hash);

    let mut adv = [0u8; 31];
    let mut scan = [0u8; 31];
    let adv_len = advertisement_now(&mut adv, hint);
    let scan_len = scan_response_now(&mut scan);
    crate::log::log_fmt_critical(
        "[BLE ] ",
        format_args!(
            "ADV adv_bytes={} scan_bytes={} cap=31 peripheral_only={} periph_links={} free_slots={}",
            adv_len,
            scan_len,
            u8::from(capability_record(hint)[3] & CAP_PERIPHERAL_ONLY != 0),
            PERIPH_LINKS,
            free_peripheral_slots(),
        ),
    );
    (server, hint)
}

/// Serializes the advertise phase across the peripheral tasks: one
/// advertising set, one free task on the air at a time; a connect hands
/// the turn to the next free task, and a full board is honestly silent.
/// A waiter slot per peripheral task prevents idle advertisers from
/// replacing and waking one another in a single-waker mutex.
static ADV_LOCK: FairSemaphore<CriticalSectionRawMutex, PERIPH_LINKS> = FairSemaphore::new(1);

/// One incoming-link slot: advertise (serialized on `ADV_LOCK`),
/// accept, serve the session, repeat; the body of the firmware's
/// peripheral task.
pub async fn peripheral<T: HciTransport>(
    stack: &'static BleStack<T>,
    server: &'static ReticulumServer<'static>,
    index: usize,
    hint: AdvHint,
) {
    let incoming_tx = super::BLE_INCOMING.sender();
    let mut peripheral = stack.peripheral();
    loop {
        if !crate::media::ble_active() {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=peripheral adv={index} state=off"),
            );
            super::carrier_on(CarrierWaiter::Peripheral(index)).await;
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=peripheral adv={index} state=on"),
            );
        }
        let mut held_slot: Option<PeripheralSlot> = None;
        let conn = {
            let _adv_turn = ADV_LOCK
                .acquire(1)
                .await
                .expect("one waiter per peripheral task");
            if !crate::media::ble_active() {
                continue;
            }
            let mut adv = [0u8; 31];
            let mut scan = [0u8; 31];
            let adv_len = advertisement_now(&mut adv, hint);
            let scan_len = scan_response_now(&mut scan);
            let params = AdvertisementParameters::default();
            let advertiser = match peripheral
                .advertise(
                    &params,
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv[..adv_len],
                        scan_data: &scan[..scan_len],
                    },
                )
                .await
            {
                Ok(a) => a,
                Err(e) => {
                    crate::log::log_fmt("[BLE ] ", format_args!("advertise failed: {:?}", e));
                    pause_advertise(AdvertisePass::Failed).await;
                    continue;
                }
            };
            match select(
                advertiser.accept(),
                super::carrier_off(CarrierWaiter::Peripheral(index)),
            )
            .await
            {
                Either::First(Ok(conn)) => {
                    held_slot = Some(PeripheralSlot::claim());
                    Some(conn)
                }
                Either::First(Err(_)) => None,
                Either::Second(()) => continue,
            }
        };
        match conn {
            Some(conn) => {
                crate::info!("BLE: connected");
                let handle = conn.handle().raw();
                log_conn_params(&conn, handle, LinkRole::Peripheral, LinkPhase::Open);
                // The ATT MTU the peer and we settled on, as on the nRF
                // reported at the close rather than at connect: BlueZ sends
                // its Exchange MTU Request after the connection is up, and
                // TrouBLE forgets the value with the connection storage, so
                // `gatt_events` hands back the last value it saw live.
                let att_mtu = match conn.clone().with_attribute_server(&server.server) {
                    Ok(gatt) => {
                        // The handshake watchdog runs while the peer's
                        // parameter-update transaction is outstanding.
                        let parameters = async {
                            ask_for_a_survivable_timeout(stack, &conn, handle).await;
                            core::future::pending::<u16>().await
                        };
                        match select(gatt_events(&gatt, server, &incoming_tx), parameters).await {
                            Either::First(att_mtu) | Either::Second(att_mtu) => att_mtu,
                        }
                    }
                    Err(e) => {
                        crate::log::log_fmt(
                            "[BLE ] ",
                            format_args!("BLE: attribute server binding failed: {:?}", e),
                        );
                        conn.disconnect();
                        0
                    }
                };
                crate::info!("BLE: disconnected att_mtu={}", att_mtu);
            }
            None => {
                pause_advertise(AdvertisePass::Failed).await;
            }
        }
        drop(held_slot);
        pause_advertise(AdvertisePass::Served).await;
    }
}

async fn pause_advertise(pass: AdvertisePass) {
    if let Some(pause_ms) = advertise_pass_pause_ms(pass) {
        Timer::after_millis(pause_ms).await;
    }
}

/// Say what a link actually runs at (#385).
fn log_conn_params(
    conn: &Connection<'static, DefaultPacketPool>,
    handle: u16,
    role: LinkRole,
    phase: LinkPhase,
) {
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "{}",
            ConnParamsLine {
                conn: handle,
                role,
                params: live_conn_params(conn),
                phase,
            }
        ),
    );
}

/// The parameters this connection runs on, in the units
/// [`ConnParams`] speaks (1.25 ms and 10 ms).
fn live_conn_params(conn: &Connection<'static, DefaultPacketPool>) -> ConnParams {
    let p = conn.params();
    ConnParams {
        interval_units: (p.conn_interval.as_micros() / 1250) as u16,
        latency: p.peripheral_latency,
        timeout_units: (p.supervision_timeout.as_millis() / 10) as u16,
    }
}

/// Write `BLE_LINK_DOWN` (912); a link still closing is awaited at most `disconnect_settle_ms`.
async fn log_link_down(
    conn: &Connection<'static, DefaultPacketPool>,
    handle: u16,
    role: LinkRole,
    peer: Option<[u8; 16]>,
    seen: Option<u8>,
) {
    let status = match seen {
        Some(status) => Some(status),
        None => {
            conn.disconnect();
            let settle = Duration::from_millis(disconnect_settle_ms(live_conn_params(conn)));
            let disconnected = async {
                loop {
                    if let ConnectionEvent::Disconnected { reason } = conn.next().await {
                        break reason.into_inner();
                    }
                }
            };
            match select(disconnected, Timer::after(settle)).await {
                Either::First(status) => Some(status),
                Either::Second(()) => None,
            }
        }
    };
    crate::log::log_fmt_critical(
        "[BLE ] ",
        format_args!(
            "{}",
            LinkDownLine {
                role,
                conn: handle,
                peer,
                reason: DisconnectReason::from_hci(status),
            }
        ),
    );
}

/// Ask the central for a supervision timeout this link can survive, but
/// only if the one it came up with is too short (#385).
async fn ask_for_a_survivable_timeout<T: HciTransport>(
    stack: &'static BleStack<T>,
    conn: &Connection<'static, DefaultPacketPool>,
    handle: u16,
) {
    let (timeout_ms, result) = match judge_supervision_timeout(live_conn_params(conn)) {
        ConnParamsAsk::Keep { timeout_ms } => (timeout_ms, ConnParamsReq::Skipped),
        ConnParamsAsk::Ask(ask) => {
            let interval = Duration::from_micros(u64::from(ask.interval_units) * 1250);
            let requested = RequestedConnParams {
                min_connection_interval: interval,
                max_connection_interval: interval,
                max_latency: ask.latency,
                min_event_length: Duration::from_secs(0),
                max_event_length: Duration::from_secs(0),
                supervision_timeout: Duration::from_millis(u64::from(ask.timeout_units) * 10),
            };
            let result = match conn.update_connection_params(stack, &requested).await {
                Ok(()) => ConnParamsReq::Sent,
                Err(_) => ConnParamsReq::Refused,
            };
            (ask.timeout_ms(), result)
        }
    };
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "{}",
            ConnParamsReqLine {
                conn: handle,
                timeout_ms,
                result,
            }
        ),
    );
}

/// Run one inbound frame through a link's defragmenter, and say so when
/// that cost a partial packet (#373).
fn process_logged(
    d: &mut BleDefragmenter,
    data: &[u8],
    now_ms: u64,
    slot_index: usize,
) -> DefragResult {
    let before = d.abandoned_count();
    let result = d.process(data, now_ms);
    if matches!(result, DefragResult::Error) {
        d.abandon();
    }
    let after = d.abandoned_count();
    if after != before {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_RX_ABANDON slot={} lost={} total={}",
                slot_index,
                after.saturating_sub(before),
                after,
            ),
        );
    }
    result
}

/// Hold the next packet back until this connection's inter-packet gap
/// (#376) has elapsed.
async fn serve_tx_gap(gap: &TxGap, conn_handle: u16) {
    let wait_ms = gap.wait_ms(Instant::now().as_millis(), super::tx_gap_ms());
    if wait_ms > 0 {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_TX_GAP conn={} waited_ms={}", conn_handle, wait_ms),
        );
        Timer::after(Duration::from_millis(wait_ms)).await;
    }
}

type IncomingSender =
    Sender<'static, CriticalSectionRawMutex, (Option<[u8; 16]>, Vec<u8>), { super::QUEUE_DEPTH }>;

/// Per-connection event loop: inbound writes (handshake, keepalive,
/// fragments), outbound packets and keepalives as notifications, the
/// carrier switch and the link expiry.
async fn gatt_events(
    conn: &GattConnection<'static, 'static, DefaultPacketPool>,
    server: &'static ReticulumServer<'static>,
    incoming_tx: &IncomingSender,
) -> u16 {
    let conn_handle = conn.raw().handle().raw();
    let Some((slot_index, drain)) = super::TX_DRAIN.claim_indexed(conn_handle) else {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DRAIN_TABLE_FULL conn={} slots={}",
                conn_handle, MAX_LINKS
            ),
        );
        if drain_table_refusal(LinkRole::Peripheral).disconnects {
            conn.raw().disconnect();
        }
        return conn.raw().att_mtu();
    };
    let peer = peer_addr(&conn.raw().peer_address());
    conn_link_up(slot_index, peer);

    let defrag: Cell<BleDefragmenter> = Cell::new(BleDefragmenter::new(super::REASSEMBLY_LIMITS));
    let handshake_done: Cell<bool> = Cell::new(false);
    let last_keepalive: Cell<Instant> = Cell::new(Instant::now());
    let tx_hold: Cell<leviculum_ble_tx::TxHold> = Cell::new(leviculum_ble_tx::TxHold::new());
    let tx_ready: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    let link_peer: Cell<Option<[u8; 16]>> = Cell::new(None);
    let refused: Cell<bool> = Cell::new(false);
    let down: Cell<Option<u8>> = Cell::new(None);
    // The ATT MTU as last seen on a live event (the Exchange MTU Request
    // lands after the link is up; the storage is gone once it is down).
    let att_mtu_seen: Cell<u16> = Cell::new(conn.raw().att_mtu());

    let outgoing_rx = super::link_out(slot_index).receiver();
    while outgoing_rx.try_receive().is_ok() {}

    let rx_handle = server.reticulum.rx.handle;
    let tx_cccd = server.reticulum.tx.cccd_handle;

    let inbound = async {
        loop {
            match conn.next().await {
                GattConnectionEvent::Disconnected { reason } => {
                    down.set(Some(reason.into_inner()));
                    break;
                }
                GattConnectionEvent::Gatt { event } => {
                    att_mtu_seen.set(conn.raw().att_mtu());
                    match &event {
                        GattEvent::Write(w) if w.handle() == rx_handle => {
                            w.with_data(|_, data| {
                                on_rx_write(
                                    data,
                                    slot_index,
                                    conn_handle,
                                    peer,
                                    att_mtu_seen.get(),
                                    &handshake_done,
                                    &link_peer,
                                    &last_keepalive,
                                    &tx_hold,
                                    &tx_ready,
                                    &defrag,
                                    incoming_tx,
                                    &refused,
                                )
                            });
                        }
                        GattEvent::Write(w) if Some(w.handle()) == tx_cccd => {
                            // The subscription half of the drain hold
                            // (#376); read after the server processes it.
                        }
                        _ => {}
                    }
                    if refused.get() {
                        conn.raw().disconnect();
                        break;
                    }
                    match event.accept() {
                        Ok(reply) => reply.send().await,
                        Err(e) => crate::log::log_fmt(
                            "[BLE ] ",
                            format_args!("BLE: gatt reply failed: {:?}", e),
                        ),
                    }
                    let subscribed = server.reticulum.tx.should_notify(conn);
                    let mut hold = tx_hold.get();
                    hold.note_subscription(subscribed);
                    tx_hold.set(hold);
                    if hold.ready() {
                        tx_ready.signal(());
                    }
                }
                _ => {}
            }
        }
    };

    let outbound = async {
        let mut tx_gap = TxGap::new();
        loop {
            let keepalive_deadline = if tx_hold.get().ready() {
                Timer::at(last_keepalive.get() + Duration::from_millis(KEEPALIVE_INTERVAL_MS))
            } else {
                Timer::at(Instant::MAX)
            };
            match select3(outgoing_rx.receive(), keepalive_deadline, tx_ready.wait()).await {
                Either3::First(packet) => {
                    if !tx_hold.get().ready() {
                        let mut hold = tx_hold.get();
                        if hold.note_held() {
                            crate::log::log_fmt(
                                "[BLE ] ",
                                format_args!(
                                    "BLE_TX_HELD conn={} reason=not-subscribed",
                                    conn_handle
                                ),
                            );
                        }
                        tx_hold.set(hold);
                        while !tx_hold.get().ready() {
                            tx_ready.wait().await;
                        }
                    }
                    serve_tx_gap(&tx_gap, conn_handle).await;
                    let fragments =
                        ble_framing::fragment_packet(&packet, fragment_mtu(conn.raw().att_mtu()));
                    if !notify_fragments(
                        conn,
                        drain,
                        &server.reticulum.tx,
                        fragments.len(),
                        |index| &fragments[index],
                        "packet",
                        packet.len(),
                    )
                    .await
                    {
                        return;
                    }
                    tx_gap.packet_done(Instant::now().as_millis());
                }
                Either3::Second(()) => {
                    let kv = [KEEPALIVE_BYTE];
                    if !notify_fragments(
                        conn,
                        drain,
                        &server.reticulum.tx,
                        1,
                        |_| &kv,
                        "keepalive",
                        kv.len(),
                    )
                    .await
                    {
                        return;
                    }
                    last_keepalive.set(Instant::now());
                }
                Either3::Third(()) => {}
            }
        }
    };

    match select4(
        inbound,
        outbound,
        super::carrier_off(CarrierWaiter::Session(slot_index)),
        link_over(slot_index, conn_handle),
    )
    .await
    {
        Either4::Third(()) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_DROP role=peripheral slot={}", slot_index),
            );
            conn.raw().disconnect();
        }
        Either4::Fourth(end) => {
            if matches!(end, LinkEnd::Expired(_)) {
                if let Some(peer_id) = link_peer.get() {
                    note_peer_leave(&peer_id, peer, "expiry_peripheral");
                }
            }
            if matches!(end, LinkEnd::HandshakeOverdue(_)) {
                note_dial_failure(peer.value, DialFailure::NoHandshake);
            }
            log_link_end("peripheral", slot_index, conn_handle, end);
            conn.raw().disconnect();
        }
        Either4::First(_) | Either4::Second(_) => {}
    }
    log_conn_params(
        conn.raw(),
        conn_handle,
        LinkRole::Peripheral,
        LinkPhase::Close,
    );
    log_link_down(
        conn.raw(),
        conn_handle,
        LinkRole::Peripheral,
        link_peer.get(),
        down.get(),
    )
    .await;

    peer_link_down(slot_index);
    conn_link_down(slot_index);
    super::TX_DRAIN.release(conn_handle);
    att_mtu_seen.get()
}

/// One write to `rx` on a peripheral link: the handshake, a keepalive,
/// or a fragment.
#[allow(clippy::too_many_arguments)]
fn on_rx_write(
    data: &[u8],
    slot_index: usize,
    conn_handle: u16,
    peer: PeerAddr,
    att_mtu: u16,
    handshake_done: &Cell<bool>,
    link_peer: &Cell<Option<[u8; 16]>>,
    last_keepalive: &Cell<Instant>,
    tx_hold: &Cell<leviculum_ble_tx::TxHold>,
    tx_ready: &Signal<CriticalSectionRawMutex, ()>,
    defrag: &Cell<BleDefragmenter>,
    incoming_tx: &IncomingSender,
    refused: &Cell<bool>,
) {
    let peer_value = peer.value;
    if !handshake_done.get() && data.len() == 16 {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "peer id: {:02x}{:02x}{:02x}{:02x}",
                data[0], data[1], data[2], data[3]
            ),
        );
        let mut peer_id = [0u8; 16];
        peer_id.copy_from_slice(data);
        if !peer_link_up(
            slot_index,
            conn_handle,
            peer,
            peer_id,
            Origin::Incoming,
            att_mtu,
        ) {
            note_dead_end(peer_value, "dup_refused");
            refused.set(true);
            return;
        }
        link_peer.set(Some(peer_id));
        handshake_done.set(true);
        note_dial_success(peer_value);
        last_keepalive.set(Instant::now());
        let mut hold = tx_hold.get();
        hold.note_handshake();
        tx_hold.set(hold);
        if hold.ready() {
            tx_ready.signal(());
        }
    } else if !handshake_done.get() && data.len() >= FRAGMENT_HEADER_SIZE {
        BLE_PRE_HANDSHAKE_REFUSED.fetch_add(1, Ordering::Relaxed);
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_PRE_HANDSHAKE_REFUSE role=peripheral slot={} conn={} len={} action=disconnect",
                slot_index,
                conn_handle,
                data.len()
            ),
        );
        refused.set(true);
    } else if data.len() < FRAGMENT_HEADER_SIZE {
        note_heard(slot_index);
    } else {
        // Real payload: liveness AND the active-use evidence the
        // duplicate rule consults (#360); a keepalive feeds only the
        // liveness clock.
        note_data(slot_index);
        let now = Instant::now().as_millis();
        let mut d = defrag.replace(BleDefragmenter::new(super::REASSEMBLY_LIMITS));
        let result = process_logged(&mut d, data, now, slot_index);
        let frags = d.last_completed_fragments();
        defrag.set(d);
        if let DefragResult::Complete(packet) = result {
            let len = packet.len();
            if offer_incoming(incoming_tx, link_peer.get(), packet, slot_index) {
                crate::info!("BLE: RX {}B conn={} frags={}", len, conn_handle, frags);
            }
        }
    }
}

// --- Link registry (both roles) ---

static LIVE_PEERS: BlockingMutex<CriticalSectionRawMutex, RefCell<PeerRegistry<MAX_LINKS>>> =
    BlockingMutex::new(RefCell::new(PeerRegistry::new()));

pub(crate) static BLE_LINKS_DISPLACED: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
pub(crate) static BLE_LINKS_REFUSED: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
pub(crate) static BLE_PRE_HANDSHAKE_REFUSED: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

static DISPLACE_WAKES: [Signal<CriticalSectionRawMutex, u16>; MAX_LINKS] =
    [const { Signal::new() }; MAX_LINKS];

async fn displaced(slot_index: usize, conn_handle: u16) {
    let wake = &DISPLACE_WAKES[slot_index];
    loop {
        if wake.wait().await == conn_handle {
            return;
        }
    }
}

async fn link_silent(slot_index: usize) -> LinkEnd {
    loop {
        let now_ms = Instant::now().as_millis();
        match LIVE_PEERS.lock(|peers| peers.borrow().link_verdict(slot_index, now_ms)) {
            LinkVerdict::Wait(ms) => Timer::after_millis(ms).await,
            LinkVerdict::Expired(ms) => return LinkEnd::Expired(ms),
            LinkVerdict::HandshakeOverdue(ms) => return LinkEnd::HandshakeOverdue(ms),
        }
    }
}

enum LinkEnd {
    Displaced,
    Expired(u64),
    HandshakeOverdue(u64),
}

fn log_link_end(role: &str, slot_index: usize, conn_handle: u16, end: LinkEnd) {
    match end {
        LinkEnd::Expired(silence_ms) => crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_LINK_EXPIRE role={} slot={} conn={} silence_ms={}",
                role, slot_index, conn_handle, silence_ms
            ),
        ),
        LinkEnd::HandshakeOverdue(waited_ms) => crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_HANDSHAKE_TIMEOUT role={} slot={} conn={} waited_ms={}",
                role, slot_index, conn_handle, waited_ms
            ),
        ),
        LinkEnd::Displaced => {}
    }
}

async fn link_over(slot_index: usize, conn_handle: u16) -> LinkEnd {
    match select(displaced(slot_index, conn_handle), link_silent(slot_index)).await {
        Either::First(()) => LinkEnd::Displaced,
        Either::Second(end) => end,
    }
}

fn peer_link_up(
    slot_index: usize,
    conn_handle: u16,
    peer: PeerAddr,
    peer_id: [u8; 16],
    origin: Origin,
    att_mtu: u16,
) -> bool {
    let peer_value = peer.value;
    let now_ms = Instant::now().as_millis();
    let up = LIVE_PEERS.lock(|peers| {
        peers
            .borrow_mut()
            .link_up(slot_index, peer_id, origin, att_mtu, now_ms)
    });
    match up {
        LinkUp::Refused {
            old_slot,
            rule,
            old_silence_ms,
            old_data_silence_ms,
            old_usable_mtu,
            new_usable_mtu,
        } => {
            BLE_LINKS_REFUSED.fetch_add(1, Ordering::Relaxed);
            note_phase_event(PhaseEvent::LinkRefused);
            let old_conn = super::link_handle_at(old_slot).unwrap_or(u16::MAX);
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!(
                    "BLE_LINK_DUP peer={:02x}{:02x}{:02x}{:02x} addr={:012x} action=refuse rule={} origin={} old_conn={} new_conn={} old_mtu={} new_mtu={} old_silence_ms={} old_data_silence_ms={}",
                    peer_id[0], peer_id[1], peer_id[2], peer_id[3],
                    peer_value, rule.as_str(), origin_str(origin), old_conn, conn_handle,
                    old_usable_mtu, new_usable_mtu, old_silence_ms,
                    leviculum_ble_tx::DataSilence(old_data_silence_ms),
                ),
            );
            return false;
        }
        LinkUp::Displaced {
            old_slot,
            rule,
            old_silence_ms,
            old_data_silence_ms,
            old_usable_mtu,
            new_usable_mtu,
        } => {
            displace_old_link(DisplacedOld {
                old_slot,
                new_slot: slot_index,
                new_conn: conn_handle,
                peer,
                peer_id,
                origin,
                rule,
                old_silence_ms,
                old_data_silence_ms,
                old_usable_mtu,
                new_usable_mtu,
            });
            let _ = meet_link(&peer_id);
            note_phase_event(PhaseEvent::LinkDisplaced);
        }
        LinkUp::Accepted { first } => {
            if first {
                super::report_peer_event(super::PeerEvent::Up(peer_id));
            }
            note_phase_event(PhaseEvent::LinkAccepted(meet_link(&peer_id)));
        }
    }
    true
}

/// Real payload arrived on the link: the liveness clock and the payload
/// clock the duplicate rule reads (#360).
fn note_data(slot_index: usize) {
    let now_ms = Instant::now().as_millis();
    LIVE_PEERS.lock(|peers| peers.borrow_mut().note_data(slot_index, now_ms));
}

fn note_heard(slot_index: usize) {
    let now_ms = Instant::now().as_millis();
    LIVE_PEERS.lock(|peers| peers.borrow_mut().note_heard(slot_index, now_ms));
}

fn origin_str(origin: Origin) -> &'static str {
    match origin {
        Origin::Incoming => "incoming",
        Origin::Outgoing => "outgoing",
    }
}

struct DisplacedOld {
    old_slot: usize,
    new_slot: usize,
    new_conn: u16,
    peer: PeerAddr,
    peer_id: [u8; 16],
    origin: Origin,
    rule: leviculum_ble_tx::DupRule,
    old_silence_ms: u64,
    old_data_silence_ms: Option<u64>,
    old_usable_mtu: u16,
    new_usable_mtu: u16,
}

fn displace_old_link(displaced: DisplacedOld) {
    let DisplacedOld {
        old_slot,
        new_slot,
        new_conn,
        peer,
        peer_id,
        origin,
        rule,
        old_silence_ms,
        old_data_silence_ms,
        old_usable_mtu,
        new_usable_mtu,
    } = displaced;
    let peer_value = peer.value;
    let old_conn = super::link_handle_at(old_slot).unwrap_or(u16::MAX);
    let old_queue = super::link_out(old_slot);
    let new_queue = super::link_out(new_slot);
    let mut moved = 0usize;
    let mut dropped = 0usize;
    while let Ok(packet) = old_queue.try_receive() {
        if new_queue.try_send(packet).is_ok() {
            moved += 1;
        } else {
            dropped += 1;
        }
    }
    BLE_LINKS_DISPLACED.fetch_add(1, Ordering::Relaxed);
    note_peer_leave(&peer_id, peer, "replaced");
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_LINK_REPLACED peer={:02x}{:02x}{:02x}{:02x} addr={:012x} rule={} origin={} old_conn={} new_conn={} old_mtu={} new_mtu={} old_silence_ms={} old_data_silence_ms={} moved={} dropped={}",
            peer_id[0], peer_id[1], peer_id[2], peer_id[3],
            peer_value, rule.as_str(), origin_str(origin), old_conn, new_conn,
            old_usable_mtu, new_usable_mtu, old_silence_ms,
            leviculum_ble_tx::DataSilence(old_data_silence_ms), moved, dropped,
        ),
    );
    DISPLACE_WAKES[old_slot].signal(old_conn);
}

fn peer_link_down(slot_index: usize) {
    let lost = LIVE_PEERS.lock(|peers| peers.borrow_mut().link_down(slot_index));
    if let Some((identity, _origin)) = lost {
        super::report_peer_event(super::PeerEvent::Lost(identity));
    }
}

fn conn_link_up(slot_index: usize, peer: PeerAddr) {
    let now_ms = Instant::now().as_millis();
    LIVE_PEERS.lock(|peers| peers.borrow_mut().conn_up(slot_index, peer.value, now_ms));
    let neighbour = PHASE_NEIGHBOURS.lock(|seen| seen.borrow_mut().meet_conn(peer));
    note_phase_event(PhaseEvent::ConnUp(neighbour));
}

fn conn_link_down(slot_index: usize) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut().conn_down(slot_index));
    note_phase_event(PhaseEvent::ConnDown);
}

fn meet_link(identity: &[u8; 16]) -> leviculum_ble_tx::Neighbour {
    PHASE_NEIGHBOURS.lock(|seen| seen.borrow_mut().meet_link(identity))
}

pub(crate) fn live_peer_count() -> usize {
    LIVE_PEERS.lock(|peers| peers.borrow().peer_count())
}

pub(crate) fn plan_fanout(aim: leviculum_ble_tx::TxAim) -> leviculum_ble_tx::TxFanout {
    LIVE_PEERS.lock(|peers| leviculum_ble_tx::plan_fanout(&peers.borrow(), aim))
}

// --- The central half: scan, decide, initiate ---

/// Discovery-scan cadence: a 30 ms listen every 100 ms, passive.
const SCAN_INTERVAL: Duration = Duration::from_millis(100);
const SCAN_WINDOW: Duration = Duration::from_millis(30);
/// Bound on one connect attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One advertising report as the scanner's sink hands it to the central
/// task: the address, and the advertising data.
#[derive(Clone, Copy)]
struct Report {
    addr: Address,
    data: [u8; 31],
    len: u8,
}

/// The scanner's report sink: TrouBLE delivers advertising reports on
/// the runner's event handler, so this is the one `EventHandler` the
/// runner runs with ([`super::run_host`]), and it drops everything that
/// is not a connectable PDU carrying the Reticulum service.
pub(crate) struct Reports {
    queue: Channel<CriticalSectionRawMutex, Report, 8>,
}

pub(crate) static REPORTS: Reports = Reports {
    queue: Channel::new(),
};

impl Reports {
    /// Queue one connectable PDU carrying the Reticulum service.
    fn offer(&self, addr: Address, data: &[u8]) {
        if data.len() > 31 {
            return;
        }
        if !parse_peer_advertisement(data, &RETICULUM_SVC_UUID_LE).offers_service {
            return;
        }
        let mut buf = [0u8; 31];
        buf[..data.len()].copy_from_slice(data);
        let _ = self.queue.try_send(Report {
            addr,
            data: buf,
            len: data.len() as u8,
        });
    }
}

impl EventHandler for Reports {
    /// The vendored host names the connection
    /// (`vendor/trouble-host/LEVICULUM-PATCH.md`), so its drain slot
    /// alone wakes.
    fn on_packets_completed(&self, handle: bt_hci::param::ConnHandle, count: usize) {
        if count != 0 && !super::TX_DRAIN.drained(handle.raw()) {
            super::notify::BLE_TX_DRAIN_UNROUTED.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn on_adv_reports(&self, reports: bt_hci::param::LeAdvReportsIter<'_>) {
        for report in reports.flatten() {
            if !matches!(report.event_kind, bt_hci::param::LeAdvEventKind::AdvInd) {
                continue;
            }
            self.offer(Address::new(report.addr_kind, report.addr), report.data);
        }
    }

    /// The same filter on an extended report's event bits. Never reached
    /// through this board's own legacy scan (see `find_peer_to_initiate`);
    /// kept so a controller firmware that grows extended scanning needs
    /// no change here.
    fn on_ext_adv_reports(&self, reports: bt_hci::param::LeExtAdvReportsIter<'_>) {
        for report in reports.flatten() {
            let kind = report.event_kind;
            if !kind.connectable() || kind.scan_response() || kind.directed() {
                continue;
            }
            self.offer(Address::new(report.addr_kind, report.addr), report.data);
        }
    }
}

static SCAN_PHASE: BlockingMutex<CriticalSectionRawMutex, Cell<ScanPhase>> =
    BlockingMutex::new(Cell::new(ScanPhase::new()));
static PHASE_NEIGHBOURS: BlockingMutex<
    CriticalSectionRawMutex,
    RefCell<PhaseNeighbours<PHASE_NEIGHBOUR_SLOTS>>,
> = BlockingMutex::new(RefCell::new(PhaseNeighbours::new()));
const _: () = assert!(PHASE_NEIGHBOUR_SLOTS == MAX_LINKS + DEAD_END_SLOTS);
const _: () = assert!(DEAD_END_SLOTS == 2 * MAX_LINKS);

fn note_phase_event(event: PhaseEvent) {
    let now_ms = Instant::now().as_millis();
    SCAN_PHASE.lock(|clock| {
        let mut phase = clock.get();
        phase.note(event, now_ms);
        clock.set(phase);
    });
}

fn scan_mode_now() -> ScanMode {
    let now_ms = Instant::now().as_millis();
    let poll = SCAN_PHASE.lock(|clock| {
        let mut phase = clock.get();
        let poll = phase.poll(now_ms);
        clock.set(phase);
        poll
    });
    if let Some(elapsed_ms) = poll.announce_ms {
        PHASE_NEIGHBOURS.lock(|seen| seen.borrow_mut().clear());
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_SCAN_FALLBACK after_ms={elapsed_ms}"),
        );
    }
    poll.mode
}

static DEAD_ENDS: BlockingMutex<CriticalSectionRawMutex, RefCell<DeadEnds>> =
    BlockingMutex::new(RefCell::new(DeadEnds::new()));
static DIAL_BACKOFF: BlockingMutex<
    CriticalSectionRawMutex,
    RefCell<DialBackoff<DIAL_BACKOFF_SLOTS>>,
> = BlockingMutex::new(RefCell::new(DialBackoff::new()));
static DIAL_LEDGER: BlockingMutex<CriticalSectionRawMutex, Cell<DialLedger>> =
    BlockingMutex::new(Cell::new(DialLedger::new()));
static CHURN_LEDGER: BlockingMutex<CriticalSectionRawMutex, Cell<ChurnLedger>> =
    BlockingMutex::new(Cell::new(ChurnLedger::new()));
const LEDGER_POLICY: LedgerPolicy = LedgerPolicy::MEASURED;
const CHURN_POLICY: ChurnPolicy = ChurnPolicy::MEASURED;
static DIAL_HELD_LOGGED_FOR: BlockingMutex<CriticalSectionRawMutex, Cell<Option<u64>>> =
    BlockingMutex::new(Cell::new(None));
static CHURN_HELD_LOGGED_FOR: BlockingMutex<CriticalSectionRawMutex, Cell<Option<u64>>> =
    BlockingMutex::new(Cell::new(None));

fn note_dead_end(addr: u64, reason: &str) {
    let now_ms = Instant::now().as_millis();
    DEAD_ENDS.lock(|table| table.borrow_mut().note(addr, now_ms));
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_DIAL_DEAD_END addr={addr:012x} reason={reason} ttl_s={}",
            DEAD_END_TTL_MS / 1_000
        ),
    );
}

fn note_dial_failure(addr: u64, failure: DialFailure) {
    let now_ms = Instant::now().as_millis();
    let booked = DIAL_BACKOFF.lock(|b| b.borrow_mut().note_failure(addr, failure, now_ms));
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_DIAL_BACKOFF addr={addr:012x} failure={} strikes={} backoff_ms={} until_ms={}",
            failure.as_str(),
            booked.strikes,
            booked.until_ms.saturating_sub(now_ms),
            booked.until_ms,
        ),
    );
}

fn note_dial_success(addr: u64) {
    DIAL_BACKOFF.lock(|b| b.borrow_mut().note_success(addr));
}

fn note_dial_outcome(peer_id: &[u8; 16], session_ms: Option<u64>, reason: &str) {
    let now_ms = Instant::now().as_millis();
    let noted = DIAL_LEDGER.lock(|cell| {
        let mut ledger = cell.get();
        let noted = ledger.note(LEDGER_POLICY, peer_id, session_ms, now_ms);
        cell.set(ledger);
        noted
    });
    if noted.armed {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DIAL_LEDGER armed identity={:02x}{:02x}{:02x}{:02x} run={} reason={} session_ms={} pause_ms={}",
                peer_id[0], peer_id[1], peer_id[2], peer_id[3], noted.run, reason,
                SessionLen(session_ms), LEDGER_POLICY.pause_ms,
            ),
        );
    }
}

fn note_peer_leave(peer_id: &[u8; 16], peer: PeerAddr, reason: &str) {
    let now_ms = Instant::now().as_millis();
    let noted = CHURN_LEDGER.lock(|cell| {
        let mut ledger = cell.get();
        let noted = ledger.note_departure(CHURN_POLICY, peer, peer_id, now_ms);
        cell.set(ledger);
        noted
    });
    if let Some(noted) = noted.filter(|noted| noted.armed) {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_CHURN_LEDGER armed identity={:02x}{:02x}{:02x}{:02x} run={} reason={} window_ms={} cooldown_ms={}",
                peer_id[0], peer_id[1], peer_id[2], peer_id[3], noted.run, reason,
                CHURN_POLICY.window_ms, CHURN_POLICY.cooldown_ms,
            ),
        );
    }
}

fn log_dial_held(until_ms: u64, now_ms: u64) {
    let fresh = DIAL_HELD_LOGGED_FOR.lock(|cell| {
        if cell.get() == Some(until_ms) {
            return false;
        }
        cell.set(Some(until_ms));
        true
    });
    if fresh {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DIAL_LEDGER held run={} until_ms={} now_ms={}",
                LEDGER_POLICY.wasted_run, until_ms, now_ms
            ),
        );
    }
}

fn log_churn_held(until_ms: u64, now_ms: u64) {
    let fresh = CHURN_HELD_LOGGED_FOR.lock(|cell| {
        if cell.get() == Some(until_ms) {
            return false;
        }
        cell.set(Some(until_ms));
        true
    });
    if fresh {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_CHURN_LEDGER held run={} until_ms={} now_ms={}",
                CHURN_POLICY.leave_run, until_ms, now_ms
            ),
        );
    }
}

struct SessionLen(Option<u64>);

impl core::fmt::Display for SessionLen {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms}"),
            None => f.write_str("refused"),
        }
    }
}

struct IdentityHint(Option<[u8; IDENTITY_HINT_LEN]>);

impl core::fmt::Display for IdentityHint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(hint) => write!(
                f,
                "{:02x}{:02x}{:02x}{:02x}",
                hint[0], hint[1], hint[2], hint[3]
            ),
            None => f.write_str("unknown"),
        }
    }
}

struct FreeSlots(Option<u8>);

impl core::fmt::Display for FreeSlots {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(free) => write!(f, "{free}"),
            None => f.write_str("unknown"),
        }
    }
}

/// Scan until one advertisement wins an initiate decision, keep
/// collecting for one window, then return the best candidate.
async fn find_peer_to_initiate<T: HciTransport>(
    stack: &'static BleStack<T>,
    own_addr_value: u64,
) -> Result<Option<(Address, ConnectDecision, usize)>, HostError<T>> {
    let mut central = stack.central();
    let mut scanner = Scanner::new(&mut central);
    let config = ScanConfig {
        active: false,
        interval: SCAN_INTERVAL,
        window: SCAN_WINDOW,
        ..Default::default()
    };
    // Drain reports queued while nobody was scanning: they are stale.
    while REPORTS.queue.try_receive().is_ok() {}
    // A legacy scan, on purpose: the CYW43439's Bluetooth firmware answers
    // `LE Set Extended Scan Parameters` with `Unknown HCI Command` (bench,
    // 2026-09-12), and the error surfaces in the host runner rather than
    // in the scan call, taking the whole host down until it restarts. A
    // legacy scan does not report a peer that advertises with extended
    // PDUs only (a BlueZ host on a Bluetooth 5 adapter does); such a peer
    // sees this board's legacy advertisement and dials it instead.
    let session = scanner.scan(&config).await?;
    let mut window: CandidateTable<Address, WINDOW_CANDIDATES> = CandidateTable::new();
    let mut last_logged: Option<(u64, ConnectDecision)> = None;
    let mut consider = |report: Report| -> bool {
        let data = &report.data[..usize::from(report.len)];
        let parsed = parse_peer_advertisement(data, &RETICULUM_SVC_UUID_LE);
        if !parsed.offers_service {
            return false;
        }
        let adv = AdvReport {
            addr: peer_addr(&report.addr),
            caps: parsed.caps,
            identity_hint: parsed.identity_hint,
        };
        let peer_value = adv.addr.value;
        let mode = scan_mode_now();
        let now_ms = Instant::now().as_millis();
        let held = DIAL_BACKOFF.lock(|b| b.borrow_mut().sighted(peer_value, now_ms));
        let dial = DIAL_LEDGER.lock(Cell::get);
        let churn = CHURN_LEDGER.lock(Cell::get);
        let admission = LIVE_PEERS.lock(|peers| {
            DEAD_ENDS.lock(|dead| {
                admit(
                    Local {
                        caps: LOCAL_CAPS,
                        addr: own_addr_value,
                    },
                    &adv,
                    mode,
                    &peers.borrow(),
                    &dead.borrow(),
                    Ledgers {
                        dial: &dial,
                        churn: &churn,
                    },
                    now_ms,
                )
            })
        });
        let decision = admission.decision;
        let free_slots = parsed.free_slots();
        if last_logged != Some((peer_value, decision)) {
            last_logged = Some((peer_value, decision));
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!(
                    "BLE_SCAN_DECISION addr={:012x} caps_record={} caps={:#04x} free_slots={} hint={} rule={} pref={} initiate={}",
                    peer_value,
                    u8::from(parsed.caps.is_some()),
                    parsed.caps.unwrap_or(0),
                    FreeSlots(free_slots),
                    IdentityHint(parsed.identity_hint),
                    decision.as_str(),
                    dial_preference(decision, free_slots).as_str(),
                    u8::from(decision.initiate()),
                ),
            );
        }
        match admission.outcome {
            Ok(()) if held.is_some() => {
                if let Some(hold) =
                    DIAL_BACKOFF.lock(|b| b.borrow_mut().announce(peer_value, now_ms))
                {
                    crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!(
                            "BLE_SCAN_DECISION addr={:012x} hint={} rule=backoff until_ms={} in_ms={} strikes={} initiate=0",
                            peer_value, IdentityHint(parsed.identity_hint), hold.until_ms,
                            hold.until_ms.saturating_sub(now_ms), hold.strikes,
                        ),
                    );
                }
                return false;
            }
            // The address that refused our last dial sits one advertising
            // interval out (815), as on the nRF.
            Ok(()) if SCAN_PHASE.lock(|clock| clock.get().skips(peer_value, now_ms)) => {
                return false;
            }
            Ok(()) => {}
            Err(Exclusion::LedgerHeld { until_ms }) => {
                log_dial_held(until_ms, now_ms);
                return false;
            }
            Err(Exclusion::ChurnHeld { until_ms }) => {
                log_churn_held(until_ms, now_ms);
                return false;
            }
            Err(
                Exclusion::NotInitiate
                | Exclusion::DeadEnd
                | Exclusion::AddrLinked
                | Exclusion::HintLinked,
            ) => return false,
        }
        window.offer(
            peer_value,
            Some(adv.addr.addr_type),
            decision,
            free_slots,
            report.addr,
        )
    };
    // Phase 1: until the first eligible candidate opens the window.
    loop {
        let report = REPORTS.queue.receive().await;
        if consider(report) {
            break;
        }
    }
    // Phase 2: collect for the window bound.
    let deadline = Instant::now() + Duration::from_millis(SCAN_WINDOW_COLLECT_MS);
    while let Either::First(report) = select(REPORTS.queue.receive(), Timer::at(deadline)).await {
        consider(report);
    }
    session.stop().await;
    let seen = window.seen();
    Ok(window
        .into_best()
        .map(|(_, decision, peer)| (peer, decision, seen)))
}

/// One central link, returning a classified failure for shared dial backoff.
async fn central_link<T: HciTransport>(
    stack: &'static BleStack<T>,
    server: &'static ReticulumServer<'static>,
    own_identity: &[u8; 16],
    peer: Address,
    decision: ConnectDecision,
) -> Option<DialFailure> {
    let typed_peer = peer_addr(&peer);
    let peer_value = typed_peer.value;
    let accept = [peer];
    let config = ConnectConfig {
        scan_config: ScanConfig {
            filter_accept_list: &accept,
            active: false,
            interval: SCAN_INTERVAL,
            window: SCAN_WINDOW,
            timeout: CONNECT_TIMEOUT,
            ..Default::default()
        },
        connect_params: RequestedConnParams::default(),
    };
    let mut central = stack.central();
    let attempt = embassy_time::with_timeout(
        CONNECT_TIMEOUT + Duration::from_secs(1),
        central.connect(&config),
    );
    let conn = match select(attempt, super::carrier_off(CarrierWaiter::Central)).await {
        Either::First(Ok(Ok(conn))) => conn,
        Either::First(result) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!(
                    "BLE_CENTRAL_FAIL addr={peer_value:012x} stage=connect err={result:?}"
                ),
            );
            if connect_failure_condemns(decision) {
                note_dead_end(peer_value, "fallback_connect");
            }
            return Some(DialFailure::Transport);
        }
        Either::Second(()) => return None,
    };
    let conn_handle = conn.handle().raw();
    log_conn_params(&conn, conn_handle, LinkRole::Central, LinkPhase::Open);
    let setup_deadline =
        Instant::now() + Duration::from_millis(leviculum_ble_tx::HANDSHAKE_TIMEOUT_MS);
    let Some((slot_index, drain)) = super::TX_DRAIN.claim_indexed(conn_handle) else {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DRAIN_TABLE_FULL conn={} slots={}",
                conn_handle, MAX_LINKS
            ),
        );
        let refused = drain_table_refusal(LinkRole::Central);
        if refused.disconnects {
            conn.disconnect();
        }
        return refused.books;
    };
    // Empty the previous tenant's queue before a displacement can move
    // packets into this slot. The drain claim is also its sole owner.
    let outgoing_rx = super::link_out(slot_index).receiver();
    while outgoing_rx.try_receive().is_ok() {}
    conn_link_up(slot_index, typed_peer);
    let session_from: Cell<Option<Instant>> = Cell::new(None);
    let link_peer: Cell<Option<[u8; 16]>> = Cell::new(None);
    let down: Cell<Option<u8>> = Cell::new(None);
    // Whether the peer delivered one frame, keepalives included (815).
    let heard = Cell::new(false);
    // Answers the peer's ATT requests; unanswered, BlueZ drops the link after 30 s.
    let gatt = match conn.clone().with_attribute_server(&server.server) {
        Ok(gatt) => Some(gatt),
        Err(e) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE: attribute server binding failed: {:?}", e),
            );
            None
        }
    };
    let establish = async {
        let client = match embassy_time::with_deadline(
            setup_deadline,
            GattClient::<super::Controller<T>, DefaultPacketPool, 4>::new(stack, &conn),
        )
        .await
        {
            Ok(Ok(client)) => client,
            Ok(Err(err)) => {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_CENTRAL_FAIL addr={peer_value:012x} stage=client err={err:?}"
                    ),
                );
                return Some(DialFailure::Transport);
            }
            Err(_) => {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_CENTRAL_FAIL addr={peer_value:012x} stage=client err=Timeout"
                    ),
                );
                return Some(DialFailure::Transport);
            }
        };
        let discover = async {
            let services = client
                .services_by_uuid(&SERVICE_UUID)
                .await
                .map_err(|err| (DialStage::Discover, err))?;
            let service = services.first().cloned().ok_or((
                DialStage::Discover,
                HostError::<T>::from(trouble_host::Error::NotFound),
            ))?;
            let rx = client
                .characteristic_by_uuid::<[u8]>(&service, &RX_UUID)
                .await
                .map_err(|err| (DialStage::Discover, err))?;
            let tx = client
                .characteristic_by_uuid::<[u8]>(&service, &TX_UUID)
                .await
                .map_err(|err| (DialStage::Discover, err))?;
            let identity = client
                .characteristic_by_uuid::<[u8]>(&service, &IDENTITY_UUID)
                .await
                .map_err(|err| (DialStage::Discover, err))?;
            // A seventeenth byte distinguishes a truncated identity from
            // a valid sixteen-byte value instead of admitting its prefix.
            let mut bytes = [0u8; 17];
            let n = client
                .read_characteristic(&identity, &mut bytes)
                .await
                .map_err(|err| (DialStage::IdentityRead, err))?;
            if n != 16 {
                return Err((
                    DialStage::IdentityRead,
                    HostError::<T>::from(trouble_host::Error::InvalidValue),
                ));
            }
            let mut id = [0u8; 16];
            id.copy_from_slice(&bytes[..16]);
            Ok::<_, (DialStage, HostError<T>)>((rx, tx, id))
        };
        let (rx, tx, peer_id) = match select(
            embassy_time::with_deadline(setup_deadline, discover),
            client.task(),
        )
        .await
        {
            Either::First(Ok(Ok(found))) => found,
            Either::First(Ok(Err((stage, err)))) => {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_CENTRAL_FAIL addr={peer_value:012x} stage={stage:?} err={err:?}"
                    ),
                );
                return Some(stage_failure(stage, &err));
            }
            Either::First(Err(_)) => return Some(DialFailure::Transport),
            Either::Second(_) => return Some(DialFailure::Transport),
        };
        if judge_read_identity(&peer_id, own_identity) == ReadIdentity::Own {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_LINK_SELF addr={peer_value:012x} action=disconnect"),
            );
            note_dead_end(peer_value, "self_link");
            return None;
        }
        if !peer_link_up(
            slot_index,
            conn_handle,
            typed_peer,
            peer_id,
            Origin::Outgoing,
            conn.att_mtu(),
        ) {
            note_dead_end(peer_value, "dup_refused");
            note_dial_outcome(&peer_id, None, "dup_refused_outgoing");
            return None;
        }
        link_peer.set(Some(peer_id));
        session_from.set(Some(Instant::now()));
        run_central_session(
            &conn,
            &client,
            drain,
            &rx,
            &tx,
            own_identity,
            peer_id,
            slot_index,
            conn_handle,
            typed_peer,
            setup_deadline,
            &heard,
        )
        .await
    };
    let failure = match select3(
        async {
            match select(establish, serve_peer_requests(gatt.as_ref(), &down)).await {
                Either::First(failure) => failure,
                Either::Second(never) => match never {},
            }
        },
        super::carrier_off(CarrierWaiter::Session(slot_index)),
        link_over(slot_index, conn_handle),
    )
    .await
    {
        Either3::First(failure) => failure,
        Either3::Second(()) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_DROP role=central slot={}", slot_index),
            );
            None
        }
        Either3::Third(end) => {
            if matches!(end, LinkEnd::Expired(_)) {
                if let Some(peer_id) = link_peer.get() {
                    note_peer_leave(&peer_id, typed_peer, "expiry_central");
                }
            }
            let overdue = matches!(end, LinkEnd::HandshakeOverdue(_));
            log_link_end("central", slot_index, conn_handle, end);
            overdue.then_some(DialFailure::NoHandshake)
        }
    };
    conn.disconnect();
    log_conn_params(&conn, conn_handle, LinkRole::Central, LinkPhase::Close);
    log_link_down(
        &conn,
        conn_handle,
        LinkRole::Central,
        link_peer.get(),
        down.get(),
    )
    .await;
    let mut lived_ms = None;
    if let (Some(peer_id), Some(from)) = (link_peer.get(), session_from.get()) {
        let session_ms = from.elapsed().as_millis();
        lived_ms = Some(session_ms);
        note_dial_outcome(&peer_id, Some(session_ms), "session_end");
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_CENTRAL_DOWN peer={:02x}{:02x}{:02x}{:02x} slot={} session_ms={}",
                peer_id[0], peer_id[1], peer_id[2], peer_id[3], slot_index, session_ms,
            ),
        );
    }
    peer_link_down(slot_index);
    conn_link_down(slot_index);
    // A dial the peer refused buys one advertising interval, not a whole
    // strict-phase bound (815, lnsd's 810).
    if let Some(lived_ms) = lived_ms {
        if refused_dial(Origin::Outgoing, heard.get(), lived_ms) {
            note_dial_refused(peer_value, lived_ms);
        }
    }
    super::TX_DRAIN.release(conn_handle);
    failure
}

/// Answers every ATT request the peer sends on a central link.
async fn serve_peer_requests(
    gatt: Option<&GattConnection<'static, 'static, DefaultPacketPool>>,
    down: &Cell<Option<u8>>,
) -> core::convert::Infallible {
    let Some(gatt) = gatt else {
        return core::future::pending().await;
    };
    loop {
        match gatt.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                down.set(Some(reason.into_inner()));
                core::future::pending::<()>().await
            }
            GattConnectionEvent::Gatt { event } => match event.accept() {
                Ok(reply) => reply.send().await,
                Err(e) => {
                    crate::log::log_fmt("[BLE ] ", format_args!("BLE: gatt reply failed: {:?}", e))
                }
            },
            _ => {}
        }
    }
}

/// Our dial at `peer_value` ended in the peer's refusal ([`refused_dial`]):
/// the strict phase lasts one advertising interval from now and the address
/// sits it out ([`ScanPhase::note_refused`]).
fn note_dial_refused(peer_value: u64, lived_ms: u64) {
    let now_ms = Instant::now().as_millis();
    SCAN_PHASE.lock(|clock| {
        let mut phase = clock.get();
        phase.note_refused(peer_value, now_ms);
        clock.set(phase);
    });
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "{}",
            DialRefusedLine {
                addr: peer_value,
                lived_ms,
            }
        ),
    );
}

fn stage_failure<E>(stage: DialStage, error: &BleHostError<E>) -> DialFailure {
    let error = match error {
        BleHostError::BleHost(
            trouble_host::Error::NotFound
            | trouble_host::Error::InvalidValue
            | trouble_host::Error::UnexpectedDataLength { .. }
            | trouble_host::Error::Att(_),
        ) => StageError::PeerAnswered,
        _ => StageError::Link,
    };
    dial_stage_failure(stage, error)
}

type Client<'a, T> = GattClient<'a, super::Controller<T>, DefaultPacketPool, 4>;

/// The host's error type for the board's controller.
type HostError<T> = BleHostError<<T as embedded_io::ErrorType>::Error>;

/// Subscribe, handshake, then pump beside the client's response task.
/// The outer session select owns carrier, displacement and expiry teardown.
#[allow(clippy::too_many_arguments)]
async fn run_central_session<T: HciTransport>(
    conn: &Connection<'static, DefaultPacketPool>,
    client: &Client<'_, T>,
    drain: &DrainSlot,
    rx: &Characteristic<[u8]>,
    tx: &Characteristic<[u8]>,
    own_identity: &[u8; 16],
    peer_id: [u8; 16],
    slot_index: usize,
    conn_handle: u16,
    peer: PeerAddr,
    setup_deadline: Instant,
    heard: &Cell<bool>,
) -> Option<DialFailure> {
    let peer_value = peer.value;
    let incoming_tx = super::BLE_INCOMING.sender();
    let defrag: Cell<BleDefragmenter> = Cell::new(BleDefragmenter::new(super::REASSEMBLY_LIMITS));
    let last_keepalive: Cell<Instant> = Cell::new(Instant::now());
    let outgoing_rx = super::link_out(slot_index).receiver();
    let handshaked: Cell<bool> = Cell::new(false);
    let session = async {
        // Discovery/subscription and the confirmed identity write are
        // setup transactions, not best-effort fragment enqueues.
        let mut listener =
            match embassy_time::with_deadline(setup_deadline, client.subscribe(tx, false)).await {
                Ok(Ok(listener)) => listener,
                Ok(Err(err)) => {
                    crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!(
                            "BLE_CENTRAL_FAIL addr={peer_value:012x} stage=subscribe err={err:?}"
                        ),
                    );
                    return Some(stage_failure(DialStage::Subscribe, &err));
                }
                Err(_) => return Some(DialFailure::NoHandshake),
            };
        match embassy_time::with_deadline(
            setup_deadline,
            client.write_characteristic(rx, own_identity),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_CENTRAL_FAIL addr={peer_value:012x} stage=handshake err={err:?}"
                    ),
                );
                return Some(stage_failure(DialStage::Handshake, &err));
            }
            Err(_) => return Some(DialFailure::NoHandshake),
        }
        handshaked.set(true);
        note_dial_success(peer_value);
        note_heard(slot_index);
        last_keepalive.set(Instant::now());
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_CENTRAL_UP peer={:02x}{:02x}{:02x}{:02x} slot={} att_mtu={}",
                peer_id[0],
                peer_id[1],
                peer_id[2],
                peer_id[3],
                slot_index,
                conn.att_mtu(),
            ),
        );
        let inbound = async {
            loop {
                let notification = listener.next().await;
                let data = notification.as_ref();
                heard.set(true);
                if data.len() < FRAGMENT_HEADER_SIZE {
                    note_heard(slot_index);
                    continue;
                }
                note_data(slot_index);
                let now = Instant::now().as_millis();
                let mut d = defrag.replace(BleDefragmenter::new(super::REASSEMBLY_LIMITS));
                let result = process_logged(&mut d, data, now, slot_index);
                let frags = d.last_completed_fragments();
                defrag.set(d);
                if let DefragResult::Complete(packet) = result {
                    let len = packet.len();
                    if offer_incoming(&incoming_tx, Some(peer_id), packet, slot_index) {
                        crate::info!("BLE: RX {}B conn={} frags={}", len, conn_handle, frags);
                    }
                }
            }
        };
        let outbound = async {
            let mut tx_gap = TxGap::new();
            loop {
                let keepalive_deadline =
                    Timer::at(last_keepalive.get() + Duration::from_millis(KEEPALIVE_INTERVAL_MS));
                match select(outgoing_rx.receive(), keepalive_deadline).await {
                    Either::First(packet) => {
                        serve_tx_gap(&tx_gap, conn_handle).await;
                        let fragments =
                            ble_framing::fragment_packet(&packet, fragment_mtu(conn.att_mtu()));
                        if !write_fragments(
                            conn,
                            client,
                            drain,
                            rx,
                            fragments.len(),
                            |index| &fragments[index],
                            "packet",
                            packet.len(),
                        )
                        .await
                        {
                            return;
                        }
                        tx_gap.packet_done(Instant::now().as_millis());
                    }
                    Either::Second(()) => {
                        let keepalive = [KEEPALIVE_BYTE];
                        if !write_fragments(
                            conn,
                            client,
                            drain,
                            rx,
                            1,
                            |_| &keepalive,
                            "keepalive",
                            keepalive.len(),
                        )
                        .await
                        {
                            return;
                        }
                        last_keepalive.set(Instant::now());
                    }
                }
            }
        };
        let _ = select(inbound, outbound).await;
        None
    };
    match select(session, client.task()).await {
        Either::First(failure) => failure,
        Either::Second(_) if handshaked.get() => None,
        Either::Second(_) => Some(DialFailure::Transport),
    }
}

/// The scanner/initiator: one central link at a time; the body of the
/// firmware's central task.
pub async fn central<T: HciTransport>(
    stack: &'static BleStack<T>,
    server: &'static ReticulumServer<'static>,
    identity_hash: [u8; 16],
) {
    let own = super::own_address(&identity_hash);
    let own_addr_value = addr_value(&own.addr.into_inner());
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!("BLE_CENTRAL_ADDR addr={own_addr_value:012x} caps={LOCAL_CAPS:#04x}"),
    );
    loop {
        if !crate::media::ble_active() {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=central state=off"),
            );
            super::carrier_on(CarrierWaiter::Central).await;
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=central state=on"),
            );
        }
        let pass = match select(
            find_peer_to_initiate(stack, own_addr_value),
            super::carrier_off(CarrierWaiter::Central),
        )
        .await
        {
            Either::First(Ok(Some((peer, decision, seen)))) => {
                note_phase_event(if decision == ConnectDecision::InitiateFallback {
                    PhaseEvent::FallbackTarget
                } else {
                    PhaseEvent::StrictTarget
                });
                let peer_value = addr_value(&peer.addr.into_inner());
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_SCAN_WINDOW seen={} chosen={:012x} rule={}",
                        seen,
                        peer_value,
                        decision.as_str(),
                    ),
                );
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!("BLE_CENTRAL_CONNECT addr={:012x}", peer_value),
                );
                if let Some(failure) =
                    central_link(stack, server, &identity_hash, peer, decision).await
                {
                    note_dial_failure(peer_value, failure);
                }
                ScanPass::Dialled
            }
            Either::First(Ok(None)) => ScanPass::Empty,
            Either::First(Err(err)) => {
                crate::log::log_fmt("[BLE ] ", format_args!("BLE_SCAN_FAIL err={:?}", err));
                ScanPass::Failed
            }
            Either::Second(()) => ScanPass::CarrierOff,
        };
        if let Some(pause_ms) = scan_pass_pause_ms(pass) {
            Timer::after_millis(pause_ms).await;
        }
    }
}
