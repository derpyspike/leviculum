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

use core::cell::Cell;
use core::sync::atomic::Ordering;

use embassy_futures::select::{select, select3, select4, Either, Either3, Either4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::semaphore::{FairSemaphore, Semaphore};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use leviculum_ble_link::keepalive::{self, KeepaliveClock};
use leviculum_ble_tx::{
    addr_value, advertise_pass_pause_ms, connect_failure_condemns, dial_preference,
    dial_stage_failure, disconnect_settle_ms, drain_table_refusal, fragment_mtu, identity_hint,
    judge_read_identity, judge_supervision_timeout, manufacturer_data_with_hint,
    parse_peer_advertisement, refused_dial, scan_pass_pause_ms, with_free_slots, AdvReport,
    AdvertisePass, CandidateTable, ConnParams, ConnParamsAsk, ConnParamsLine, ConnParamsReq,
    ConnParamsReqLine, ConnectDecision, DialFailure, DialStage, DisconnectReason, DrainSlot,
    Exclusion, LinkDownLine, LinkPhase, LinkRole, Local, Origin, PeerAddr, PhaseEvent,
    ReadIdentity, ScanPass, StageError, TxGap, ADV_BYTES_USED, CAP_PERIPHERAL_ONLY, GATT_VALUE_MAX,
    IDENTITY_HINT_LEN, LEGACY_AD_CAPACITY, MANUFACTURER_DATA_HINT_LEN, PERIPH_SLOTS,
    SCAN_WINDOW_COLLECT_MS, WINDOW_CANDIDATES,
};
use leviculum_core::framing::ble::{
    self as ble_framing, BleDefragmenter, DefragResult, FRAGMENT_HEADER_SIZE, KEEPALIVE_BYTE,
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

const _: () = assert!(PERIPH_LINKS == PERIPH_SLOTS as usize);

/// The v0.3.0 capability record this node advertises right now (the
/// record stays with the flag cleared rather than being dropped; see the
/// nRF module for why).
fn capability_record(hint: [u8; IDENTITY_HINT_LEN]) -> [u8; MANUFACTURER_DATA_HINT_LEN] {
    manufacturer_data_with_hint(
        with_free_slots(LOCAL_CAPS, session::free_peripheral_slots()),
        &hint,
    )
}

/// The advertising PDU as it stands at this instant: flags, the service
/// UUID and the live capability record; built per advertising start.
///
/// The three records total `ADV_BYTES_USED` == `LEGACY_AD_CAPACITY`
/// (const-asserted at [`start`]), so the encode cannot overrun the
/// buffer by construction; were an encoder change to break that, the
/// board advertises nothing and says so once per start instead of
/// panicking mid-boot (1715, no `expect` in firmware code).
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
    .unwrap_or_else(|_| {
        crate::log::log_fmt_critical(
            "[BLE ] ",
            format_args!("BLE_ADV_ENCODE_FAIL cap={}", LEGACY_AD_CAPACITY),
        );
        0
    })
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
///
/// `None` — said on the log — when the GATT table refused the service:
/// the board runs on without BLE instead of panicking on the boot path
/// (1715, no `expect` in firmware code). The one way to reach it is an
/// attribute-table layout change in the service macro, caught by the
/// first bench boot rather than a field panic.
pub fn start(identity_hash: [u8; 16]) -> Option<(&'static ReticulumServer<'static>, AdvHint)> {
    session::set_local_identity(identity_hash);
    let hint = identity_hint(&identity_hash);
    const _: () = assert!(ADV_BYTES_USED == LEGACY_AD_CAPACITY);
    static SERVER: StaticCell<ReticulumServer<'static>> = StaticCell::new();
    static GAP_NAME: StaticCell<heapless::String<22>> = StaticCell::new();
    let gap_name = GAP_NAME.init(heapless::String::new());
    let _ = gap_name.push_str(crate::name::gap_name_now().as_str());
    let built = match ReticulumServer::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: gap_name.as_str(),
        appearance: &appearance::UNKNOWN,
    })) {
        Ok(server) => server,
        Err(e) => {
            crate::log::log_fmt_critical("[BLE ] ", format_args!("off reason=gatt err={:?}", e));
            return None;
        }
    };
    let server: &'static ReticulumServer<'static> = SERVER.init(built);
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
            session::free_peripheral_slots(),
        ),
    );
    Some((server, hint))
}

/// The Columba attributes' handles as the server laid them out: `rx`,
/// `tx` and `tx`'s CCCD, for the scripted-controller test, which plays
/// the peer's ATT writes against them.
#[cfg(test)]
pub(super) fn attribute_handles(server: &ReticulumServer<'static>) -> (u16, u16, Option<u16>) {
    (
        server.reticulum.rx.handle,
        server.reticulum.tx.handle,
        server.reticulum.tx.cccd_handle,
    )
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
    let mut peripheral = stack.peripheral();
    loop {
        if !leviculum_ble_link::carrier().is_on() {
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
            // The semaphore holds one waiter slot per peripheral task
            // and each task awaits at most once, so a refusal cannot
            // happen by construction; were the layout to change, the
            // task sits its pass out and says so instead of panicking
            // (1715, no `expect` in firmware code).
            let _adv_turn = match ADV_LOCK.acquire(1).await {
                Ok(turn) => turn,
                Err(_) => {
                    crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!("BLE_ADV_LOCK_REFUSED adv={index}"),
                    );
                    pause_advertise(AdvertisePass::Failed).await;
                    continue;
                }
            };
            if !leviculum_ble_link::carrier().is_on() {
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
                Either::Second(_) => continue,
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
                        match select(gatt_events(&gatt, server), parameters).await {
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

/// Per-connection event loop: inbound writes (handshake, keepalive,
/// fragments), outbound packets and keepalives as notifications, the
/// carrier switch and the link expiry.
async fn gatt_events(
    conn: &GattConnection<'static, 'static, DefaultPacketPool>,
    server: &'static ReticulumServer<'static>,
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
    session::conn_link_up(slot_index, peer);

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
        // One deadline for the whole loop, re-armed only when the last
        // keepalive or the hold moves, and polled before the queue: a
        // fresh `Timer::at` per pass never fired while the queue was
        // ready at every pass (1726, `leviculum_ble_link::keepalive`).
        let mut keepalive = KeepaliveClock::new();
        loop {
            let due = keepalive::due(last_keepalive.get(), tx_hold.get().ready());
            match keepalive
                .next(due, select(outgoing_rx.receive(), tx_ready.wait()))
                .await
            {
                Some(Either::First(packet)) => {
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
                    session::serve_tx_gap(&tx_gap, conn_handle).await;
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
                None => {
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
                Some(Either::Second(())) => {}
            }
        }
    };

    match select4(
        inbound,
        outbound,
        super::carrier_off(CarrierWaiter::Session(slot_index)),
        session::link_over(slot_index, conn_handle),
    )
    .await
    {
        Either4::Third(_) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_DROP role=peripheral slot={}", slot_index),
            );
            conn.raw().disconnect();
        }
        Either4::Fourth(end) => {
            if matches!(end, LinkEnd::Expired(_)) {
                if let Some(peer_id) = link_peer.get() {
                    session::note_peer_leave(&peer_id, peer, "expiry_peripheral");
                }
            }
            if matches!(end, LinkEnd::HandshakeOverdue(_)) {
                session::note_dial_failure(peer.value, DialFailure::NoHandshake);
            }
            session::log_link_end("peripheral", slot_index, conn_handle, &end);
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

    session::peer_link_down(slot_index);
    session::conn_link_down(slot_index);
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
        if !session::peer_link_up(
            slot_index,
            conn_handle,
            peer,
            peer_id,
            Origin::Incoming,
            att_mtu,
            move_queued,
        ) {
            session::note_dead_end(peer_value, "dup_refused");
            refused.set(true);
            return;
        }
        link_peer.set(Some(peer_id));
        handshake_done.set(true);
        session::note_dial_success(peer_value);
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
        session::note_heard(slot_index);
    } else {
        // Real payload: liveness AND the active-use evidence the
        // duplicate rule consults (#360); a keepalive feeds only the
        // liveness clock.
        session::note_data(slot_index);
        let now = Instant::now().as_millis();
        let mut d = defrag.replace(BleDefragmenter::new(super::REASSEMBLY_LIMITS));
        let result = session::process_logged(&mut d, data, now, slot_index);
        let frags = d.last_completed_fragments();
        defrag.set(d);
        if let DefragResult::Complete(packet) = result {
            let len = packet.len();
            if offer_incoming(link_peer.get(), packet, slot_index) {
                crate::info!("BLE: RX {}B conn={} frags={}", len, conn_handle, frags);
            }
        }
    }
}

// --- Link registry (both roles): leviculum_ble_link::session since 1715 ---

use leviculum_ble_link::session::{self, LinkEnd, PeripheralSlot};
pub(crate) use leviculum_ble_link::session::{
    live_peer_count, BLE_LINKS_DISPLACED, BLE_LINKS_REFUSED, BLE_PRE_HANDSHAKE_REFUSED,
};

/// The backend's half of a displacement (session::peer_link_up): move
/// the old slot's queued packets onto the new link's queue, answering
/// (moved, dropped) for the BLE_LINK_REPLACED line.
fn move_queued(old_slot: usize, new_slot: usize) -> (usize, usize) {
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
    (moved, dropped)
}

/// Map the core's link statement onto link slots (the fan-out's one
/// question, asked by `TroubleLink::send`).
pub(crate) fn plan_fanout(aim: leviculum_ble_tx::TxAim) -> leviculum_ble_tx::TxFanout {
    session::plan_fanout(aim)
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
        let mode = session::scan_mode_now();
        let now_ms = Instant::now().as_millis();
        let held = session::backoff_sighted(peer_value, now_ms);
        let admission = session::admit_scan(
            Local {
                caps: LOCAL_CAPS,
                addr: own_addr_value,
            },
            &adv,
            mode,
            now_ms,
        );
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
                    session::FreeSlots(free_slots),
                    session::IdentityHint(parsed.identity_hint),
                    decision.as_str(),
                    dial_preference(decision, free_slots).as_str(),
                    u8::from(decision.initiate()),
                ),
            );
        }
        match admission.outcome {
            Ok(()) if held => {
                if let Some(hold) = session::backoff_announce(peer_value, now_ms) {
                    crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!(
                            "BLE_SCAN_DECISION addr={:012x} hint={} rule=backoff until_ms={} in_ms={} strikes={} initiate=0",
                            peer_value, session::IdentityHint(parsed.identity_hint), hold.until_ms,
                            hold.until_ms.saturating_sub(now_ms), hold.strikes,
                        ),
                    );
                }
                return false;
            }
            // The address that refused our last dial sits one advertising
            // interval out (815), as on the nRF.
            Ok(()) if session::phase_skips(peer_value, now_ms) => {
                return false;
            }
            Ok(()) => {}
            Err(Exclusion::LedgerHeld { until_ms }) => {
                session::log_dial_held(until_ms, now_ms);
                return false;
            }
            Err(Exclusion::ChurnHeld { until_ms }) => {
                session::log_churn_held(until_ms, now_ms);
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
    // Phase 2: collect for the window bound. ONE timer for the whole
    // window: an embassy `Timer` answers its first poll `Pending` however
    // late it is, so a fresh `Timer::at(deadline)` per report never fired
    // while a peer reported at least once per poll turn — the window
    // stayed open and the central never dialled (1715, the
    // scripted-controller test: a 3 s window still open at 10 s).
    let deadline = Instant::now() + Duration::from_millis(SCAN_WINDOW_COLLECT_MS);
    let mut window_end = core::pin::pin!(Timer::at(deadline));
    while let Either::First(report) = select(REPORTS.queue.receive(), window_end.as_mut()).await {
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
        Either::Second(_) => return None,
        Either::First(result) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!(
                    "BLE_CENTRAL_FAIL addr={peer_value:012x} stage=connect err={result:?}"
                ),
            );
            if connect_failure_condemns(decision) {
                session::note_dead_end(peer_value, "fallback_connect");
            }
            return Some(DialFailure::Transport);
        }
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
    session::conn_link_up(slot_index, typed_peer);
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
            session::note_dead_end(peer_value, "self_link");
            return None;
        }
        if !session::peer_link_up(
            slot_index,
            conn_handle,
            typed_peer,
            peer_id,
            Origin::Outgoing,
            conn.att_mtu(),
            move_queued,
        ) {
            session::note_dead_end(peer_value, "dup_refused");
            session::note_dial_outcome(&peer_id, None, "dup_refused_outgoing");
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
        session::link_over(slot_index, conn_handle),
    )
    .await
    {
        Either3::First(failure) => failure,
        Either3::Second(_) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_DROP role=central slot={}", slot_index),
            );
            None
        }
        Either3::Third(end) => {
            if matches!(end, LinkEnd::Expired(_)) {
                if let Some(peer_id) = link_peer.get() {
                    session::note_peer_leave(&peer_id, typed_peer, "expiry_central");
                }
            }
            let overdue = matches!(end, LinkEnd::HandshakeOverdue(_));
            session::log_link_end("central", slot_index, conn_handle, &end);
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
        session::note_dial_outcome(&peer_id, Some(session_ms), "session_end");
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_CENTRAL_DOWN peer={:02x}{:02x}{:02x}{:02x} slot={} session_ms={}",
                peer_id[0], peer_id[1], peer_id[2], peer_id[3], slot_index, session_ms,
            ),
        );
    }
    session::peer_link_down(slot_index);
    session::conn_link_down(slot_index);
    // A dial the peer refused buys one advertising interval, not a whole
    // strict-phase bound (815, lnsd's 810).
    if let Some(lived_ms) = lived_ms {
        if refused_dial(Origin::Outgoing, heard.get(), lived_ms) {
            session::note_dial_refused(peer_value, lived_ms);
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
        session::note_dial_success(peer_value);
        session::note_heard(slot_index);
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
                    session::note_heard(slot_index);
                    continue;
                }
                session::note_data(slot_index);
                let now = Instant::now().as_millis();
                let mut d = defrag.replace(BleDefragmenter::new(super::REASSEMBLY_LIMITS));
                let result = session::process_logged(&mut d, data, now, slot_index);
                let frags = d.last_completed_fragments();
                defrag.set(d);
                if let DefragResult::Complete(packet) = result {
                    let len = packet.len();
                    if offer_incoming(Some(peer_id), packet, slot_index) {
                        crate::info!("BLE: RX {}B conn={} frags={}", len, conn_handle, frags);
                    }
                }
            }
        };
        let outbound = async {
            let mut tx_gap = TxGap::new();
            // As on the peripheral pump: one deadline, polled first.
            let mut keepalive = KeepaliveClock::new();
            loop {
                let due = keepalive::due(last_keepalive.get(), true);
                match keepalive.next(due, outgoing_rx.receive()).await {
                    Some(packet) => {
                        session::serve_tx_gap(&tx_gap, conn_handle).await;
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
                    None => {
                        let kv = [KEEPALIVE_BYTE];
                        if !write_fragments(
                            conn,
                            client,
                            drain,
                            rx,
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
        if !leviculum_ble_link::carrier().is_on() {
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
                session::note_phase_event(if decision == ConnectDecision::InitiateFallback {
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
                    session::note_dial_failure(peer_value, failure);
                }
                ScanPass::Dialled
            }
            Either::First(Ok(None)) => ScanPass::Empty,
            Either::First(Err(err)) => {
                crate::log::log_fmt("[BLE ] ", format_args!("BLE_SCAN_FAIL err={:?}", err));
                ScanPass::Failed
            }
            Either::Second(_) => ScanPass::CarrierOff,
        };
        if let Some(pause_ms) = scan_pass_pause_ms(pass) {
            Timer::after_millis(pause_ms).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positive control for the `expect` 1715 removed: the three
    /// advertisement records fill exactly the legacy budget, so the
    /// logged-degradation path (0 bytes) stays unreachable while the
    /// encoder and the record sizes agree.
    #[test]
    fn the_advertisement_encodes_its_full_budget() {
        let _ring = crate::log::test_ring::lock();
        let mut buf = [0u8; LEGACY_AD_CAPACITY];
        assert_eq!(
            advertisement_now(&mut buf, [0xab; IDENTITY_HINT_LEN]),
            ADV_BYTES_USED
        );
    }

    /// Audit 1715: the seam implementation's fan-out. A flood with no
    /// claimed slot queues nothing; a flood over two claimed (not yet
    /// handshaked) slots reaches both and no other; a packet routed to a
    /// peer no link carries reaches no slot and counts one route miss.
    #[test]
    fn audit_trouble_link_flood_and_route_miss() {
        use leviculum_ble_link::BleLink;
        use leviculum_ble_tx::TxAim;
        let _ring = crate::log::test_ring::lock();
        let max = crate::ble::MAX_LINKS;
        let empty_all = || {
            for i in 0..max {
                while crate::ble::link_out(i).try_receive().is_ok() {}
            }
        };
        empty_all();
        release_all();
        let mut link = crate::ble::TroubleLink;

        link.send(TxAim::Flood, vec![0xA5u8; 8]);
        for i in 0..max {
            assert!(
                crate::ble::link_out(i).try_receive().is_err(),
                "slot {i} got a flood with no link claimed"
            );
        }

        let (a, _) = crate::ble::TX_DRAIN.claim_indexed(0x0101).unwrap();
        let (b, _) = crate::ble::TX_DRAIN.claim_indexed(0x0202).unwrap();
        link.send(TxAim::Flood, vec![1u8, 2, 3]);
        for i in 0..max {
            let got = crate::ble::link_out(i).try_receive().ok();
            if i == a || i == b {
                assert_eq!(got.as_deref(), Some(&[1u8, 2, 3][..]), "claimed slot {i}");
            } else {
                assert!(got.is_none(), "unclaimed slot {i} got the flood");
            }
        }

        let before = crate::ble::BLE_TX_ROUTE_MISSES.load(core::sync::atomic::Ordering::Relaxed);
        link.send(TxAim::Peer([0x77; 16]), vec![9u8; 4]);
        for i in 0..max {
            assert!(
                crate::ble::link_out(i).try_receive().is_err(),
                "a routed packet for an unknown peer reached slot {i}"
            );
        }
        assert_eq!(
            crate::ble::BLE_TX_ROUTE_MISSES.load(core::sync::atomic::Ordering::Relaxed),
            before + 1
        );

        crate::ble::TX_DRAIN.release(0x0101);
        crate::ble::TX_DRAIN.release(0x0202);
        empty_all();
    }

    /// Free every claimed drain slot: the scripted-controller test ends
    /// with its last links still claimed (a live connection holds its
    /// slot for life, and the bench tears no one down), so a test that
    /// asserts on the whole claim table must start it empty, or a
    /// leftover link takes its flood and turns the count wrong. Seen as
    /// `slot 1 got a flood with no link claimed` and `links=3` in a full
    /// workspace run, order 1940; one injected claim reproduces both.
    fn release_all() {
        for i in 0..crate::ble::MAX_LINKS {
            if let Some(handle) = crate::ble::TX_DRAIN.handle_at(i) {
                crate::ble::TX_DRAIN.release(handle);
            }
        }
    }

    /// Everything the ring holds now, as text (empties it).
    fn ring_text() -> String {
        let mut out = String::new();
        let mut chunk = [0u8; 256];
        loop {
            let n = crate::log::LOG_RING.read(&mut chunk);
            if n == 0 {
                return out;
            }
            out.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    }

    /// Closes the runtime log gate again when a test that opened it
    /// ends, pass or fail: the log module's own test needs it closed.
    struct GateRestore;

    impl Drop for GateRestore {
        fn drop(&mut self) {
            crate::log::RUNTIME_DRAIN_OPEN.store(false, Ordering::Relaxed);
        }
    }

    /// Audit 1715: the #422 ingress exclusion on this backend. With two
    /// claimed links, one of them carrying a handshaked peer, a
    /// `FloodExcept` naming that peer reaches the other link only, and a
    /// packet routed to that peer reaches its link only.
    #[test]
    fn trouble_link_flood_except_skips_the_ingress_peer_and_routes_to_it() {
        use leviculum_ble_link::BleLink;
        use leviculum_ble_tx::{AddrType, TxAim};
        let _ring = crate::log::test_ring::lock();
        let max = crate::ble::MAX_LINKS;
        let empty_all = || {
            for i in 0..max {
                while crate::ble::link_out(i).try_receive().is_ok() {}
            }
        };
        empty_all();
        release_all();
        crate::log::open_runtime_drain();
        let _gate = GateRestore;
        let _ = ring_text();
        let ingress = [0x5e; 16];
        let (a, _) = crate::ble::TX_DRAIN.claim_indexed(0x0303).unwrap();
        let (b, _) = crate::ble::TX_DRAIN.claim_indexed(0x0404).unwrap();
        let addr = PeerAddr {
            value: 0x0000_c0ff_ee00_0001,
            addr_type: AddrType::RandomStatic,
        };
        session::conn_link_up(a, addr);
        assert!(session::peer_link_up(
            a,
            0x0303,
            addr,
            ingress,
            Origin::Incoming,
            23,
            |_, _| (0, 0),
        ));
        let mut link = crate::ble::TroubleLink;

        link.send(TxAim::FloodExcept(ingress), vec![4u8, 5, 6]);
        assert!(
            crate::ble::link_out(a).try_receive().is_err(),
            "the ingress peer's own link got its flood back"
        );
        assert_eq!(
            crate::ble::link_out(b).try_receive().ok().as_deref(),
            Some(&[4u8, 5, 6][..]),
            "the other claimed link missed the flood"
        );
        let log = ring_text();
        assert!(
            log.contains("BLE_TX_FLOOD links=1 len=3 excl_link=5e5e5e5e"),
            "the flood line names one link and the excluded peer: {log}"
        );

        link.send(TxAim::Peer(ingress), vec![7u8; 3]);
        assert_eq!(
            crate::ble::link_out(a).try_receive().ok().as_deref(),
            Some(&[7u8; 3][..]),
            "the routed packet missed its peer's link"
        );
        assert!(
            crate::ble::link_out(b).try_receive().is_err(),
            "a routed packet reached a link of another peer"
        );
        let log = ring_text();
        assert!(
            log.contains(&format!(
                "BLE_TX_ROUTE peer=5e5e5e5e conn=771 slot={a} len=3"
            )),
            "the route line names the peer, its link and the length: {log}"
        );

        session::peer_link_down(a);
        session::conn_link_down(a);
        crate::ble::TX_DRAIN.release(0x0303);
        crate::ble::TX_DRAIN.release(0x0404);
        let chans = leviculum_ble_link::channels();
        while chans.peer_event_rx.try_receive().is_ok() {}
        empty_all();
    }

    /// Audit 1715: the seam's name push reaches the name the next
    /// advertising start carries.
    #[test]
    fn trouble_link_set_advertised_name_changes_the_gap_name() {
        use leviculum_ble_link::BleLink;
        use leviculum_core::node_name::NodeName;
        let _ring = crate::log::test_ring::lock();
        crate::name::note_boot_identity([0x12; 16]);
        let mut link = crate::ble::TroubleLink;
        let name = NodeName::parse(b"Nessa").unwrap();
        link.set_advertised_name(name);
        assert_eq!(crate::name::gap_name_now().as_str(), "Nessa");
        crate::name::set_configured(None);
        assert_eq!(crate::name::gap_name_now().as_str(), "LN-12121212");
    }

    /// A displacement hands the old link's queued packets to the new
    /// link in order, as far as the new queue has room, and counts what
    /// it could not place; the old queue ends empty.
    #[test]
    fn move_queued_carries_the_old_links_queue_over_and_counts_the_overflow() {
        let _ring = crate::log::test_ring::lock();
        let (old, new) = (2usize, 3usize);
        for slot in [old, new] {
            while super::super::link_out(slot).try_receive().is_ok() {}
        }
        for n in 0..3u8 {
            super::super::link_out(old).try_send(vec![n]).unwrap();
        }
        for _ in 0..super::super::QUEUE_DEPTH - 1 {
            super::super::link_out(new).try_send(vec![0xEE]).unwrap();
        }
        assert_eq!(move_queued(old, new), (1, 2));
        assert!(super::super::link_out(old).try_receive().is_err());
        let mut tail = None;
        while let Ok(p) = super::super::link_out(new).try_receive() {
            tail = Some(p);
        }
        assert_eq!(
            tail,
            Some(vec![0u8]),
            "the oldest queued packet moved first"
        );
    }

    /// The node's own address is the identity's first six bytes, little
    /// endian, with the two top bits of the last set: random static,
    /// and the same on every boot.
    #[test]
    fn the_own_address_is_random_static_from_the_identity() {
        let mut identity = [0u8; 16];
        identity[..6].copy_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        let addr = crate::ble::own_address(&identity);
        assert_eq!(addr, Address::random([0x01, 0x02, 0x03, 0x04, 0x05, 0xC6]));
    }
}
