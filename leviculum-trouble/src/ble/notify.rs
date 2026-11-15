//! Bounded TrouBLE notification and Write Command adapters for the shared pump.
//!
//! An enqueue accepts a fragment into TrouBLE's outbound channel, not into
//! a newly returned controller credit. Only a matching completed-packets
//! event releases this connection's drain wait. Dropping an unaccepted
//! enqueue drops its owned PDU and returns its packet-pool allocation.

use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_futures::select::{select, Either};
use embassy_time::{with_timeout, Duration, Timer};
use leviculum_ble_tx::{
    abort_disconnects, pump_fragments, AbortReason, Action, DrainSlot, Event, FragmentPort,
    LinkRole, NotifyOutcome, PacketTx, TxPktLine, DRAIN_WAIT_MS,
};
use trouble_host::prelude::*;

/// Packets whose every fragment was accepted by TrouBLE's outbound channel.
pub static BLE_TX_PACKETS: AtomicU32 = AtomicU32::new(0);
/// Packets abandoned before all fragments were accepted.
pub static BLE_TX_DROPPED: AtomicU32 = AtomicU32::new(0);
/// Bounded waits for the connection's actual completed-packets event.
pub static BLE_TX_DRAIN_WAITS: AtomicU32 = AtomicU32::new(0);
/// Completions whose handle no longer owns a drain slot.
pub static BLE_TX_DRAIN_UNROUTED: AtomicU32 = AtomicU32::new(0);

struct NotificationPort<'r, 's, 'c, 'f, P: PacketPool, T: AsGatt + ?Sized, F> {
    conn: &'r GattConnection<'s, 'c, P>,
    tx: &'r Characteristic<T>,
    drain: &'r DrainSlot,
    fragment: F,
    fragments: PhantomData<&'f [u8]>,
}

impl<'f, P, T, F> FragmentPort for NotificationPort<'_, '_, '_, 'f, P, T, F>
where
    P: PacketPool,
    T: AsGatt + ?Sized,
    F: Fn(usize) -> &'f [u8],
{
    async fn notify(&mut self, index: usize) -> NotifyOutcome {
        if !self.conn.raw().is_connected() {
            return NotifyOutcome::Disconnected;
        }
        // A cleared subscription is not an accepted notification. The
        // session's subscription hold will suspend the following packet.
        if !self.tx.should_notify(self.conn) {
            return NotifyOutcome::QueueFull;
        }
        let fragment = (self.fragment)(index);
        // TODO: upstream TrouBLE has no synchronous `try_notify_raw` yet
        // (embassy-rs/trouble#717); restore it when merged, so a full queue
        // answers at once and the drain wait paces the re-offer:
        // #[cfg(not(feature = "trouble-vendored"))]
        // let accepted = Some(self.tx.try_notify_raw(self.conn, fragment, false));
        // Until then every host uses the awaiting enqueue; its expired deadline is a full queue.
        let accepted = with_timeout(
            Duration::from_millis(DRAIN_WAIT_MS),
            self.tx.notify_raw(self.conn, fragment, false),
        )
        .await
        .ok();
        match accepted {
            Some(Ok(())) => NotifyOutcome::Sent,
            Some(Err(Error::Disconnected)) => NotifyOutcome::Disconnected,
            // `Busy`: encryption with a bonded peer is still starting.
            Some(Err(Error::OutOfMemory | Error::Busy)) | None => NotifyOutcome::QueueFull,
            Some(Err(error)) => {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_TX_ENQUEUE_FAIL role=peripheral conn={} err={:?}",
                        self.conn.raw().handle().raw(),
                        error
                    ),
                );
                NotifyOutcome::Failed(1)
            }
        }
    }

    async fn wait_drain(&mut self) -> Event {
        wait_drain(self.drain).await
    }
}

struct WriteCommandPort<'r, 's, 'c, 'f, C: Controller, P: PacketPool, const SERVICES: usize, F> {
    conn: &'r Connection<'s, P>,
    client: &'r GattClient<'c, C, P, SERVICES>,
    rx: &'r Characteristic<[u8]>,
    drain: &'r DrainSlot,
    fragment: F,
    fragments: PhantomData<&'f [u8]>,
}

impl<'f, C, P, const SERVICES: usize, F> FragmentPort
    for WriteCommandPort<'_, '_, '_, 'f, C, P, SERVICES, F>
where
    C: Controller,
    P: PacketPool,
    F: Fn(usize) -> &'f [u8],
{
    async fn notify(&mut self, index: usize) -> NotifyOutcome {
        if !self.conn.is_connected() {
            return NotifyOutcome::Disconnected;
        }
        match with_timeout(
            Duration::from_millis(DRAIN_WAIT_MS),
            self.client
                .write_characteristic_without_response(self.rx, (self.fragment)(index)),
        )
        .await
        {
            Ok(Ok(())) => NotifyOutcome::Sent,
            Ok(Err(BleHostError::BleHost(Error::Disconnected))) => NotifyOutcome::Disconnected,
            Ok(Err(BleHostError::BleHost(Error::OutOfMemory))) | Err(_) => NotifyOutcome::QueueFull,
            Ok(Err(error)) => {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_TX_ENQUEUE_FAIL role=central conn={} err={:?}",
                        self.conn.handle().raw(),
                        error
                    ),
                );
                NotifyOutcome::Failed(1)
            }
        }
    }

    async fn wait_drain(&mut self) -> Event {
        wait_drain(self.drain).await
    }
}

async fn wait_drain(drain: &DrainSlot) -> Event {
    BLE_TX_DRAIN_WAITS.fetch_add(1, Ordering::Relaxed);
    match select(drain.wait(), Timer::after_millis(DRAIN_WAIT_MS)).await {
        Either::First(()) => Event::Drained,
        Either::Second(()) => Event::WaitTimedOut,
    }
}

/// Send a peripheral packet (or keepalive); `false` ends this session.
/// Fragments stay borrowed until the enqueue is accepted or canceled.
pub async fn notify_fragments<'f, P, T, F>(
    conn: &GattConnection<'_, '_, P>,
    drain: &DrainSlot,
    tx: &Characteristic<T>,
    fragment_count: usize,
    fragment: F,
    kind: &str,
    packet_len: usize,
) -> bool
where
    P: PacketPool,
    T: AsGatt + ?Sized,
    F: Fn(usize) -> &'f [u8],
{
    drain.reset();
    let mut port = NotificationPort {
        conn,
        tx,
        drain,
        fragment,
        fragments: PhantomData,
    };
    let (state, terminal) = pump_fragments(&mut port, fragment_count).await;
    if finish_packet(
        conn.raw().handle().raw(),
        LinkRole::Peripheral,
        state,
        terminal,
        fragment_count,
        kind,
        packet_len,
    ) {
        conn.raw().disconnect();
    }
    conn.raw().is_connected()
}

/// Send a central packet or keepalive through the same bounded pump.
#[allow(clippy::too_many_arguments)]
pub async fn write_fragments<'f, C, P, const SERVICES: usize, F>(
    conn: &Connection<'_, P>,
    client: &GattClient<'_, C, P, SERVICES>,
    drain: &DrainSlot,
    rx: &Characteristic<[u8]>,
    fragment_count: usize,
    fragment: F,
    kind: &str,
    packet_len: usize,
) -> bool
where
    C: Controller,
    P: PacketPool,
    F: Fn(usize) -> &'f [u8],
{
    drain.reset();
    let mut port = WriteCommandPort {
        conn,
        client,
        rx,
        drain,
        fragment,
        fragments: PhantomData,
    };
    let (state, terminal) = pump_fragments(&mut port, fragment_count).await;
    if finish_packet(
        conn.handle().raw(),
        LinkRole::Central,
        state,
        terminal,
        fragment_count,
        kind,
        packet_len,
    ) {
        conn.disconnect();
    }
    conn.is_connected()
}

/// Report the shared terminal verdict, returning whether it requires a reset.
fn finish_packet(
    handle: u16,
    role: LinkRole,
    state: PacketTx,
    terminal: Action,
    frags: usize,
    kind: &str,
    packet_len: usize,
) -> bool {
    log_tx_pkt(handle, packet_len, frags, state.fragments_sent());
    match terminal {
        Action::Done => {
            BLE_TX_PACKETS.fetch_add(1, Ordering::Relaxed);
            false
        }
        Action::Abort { index, reason } => {
            let dropped = BLE_TX_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!(
                    "BLE_TX_DROP kind={} len={} frag={} of={} sent={} reason={} code={} conn={} waits={} dropped={}",
                    kind, packet_len, index, frags, state.fragments_sent(), reason.as_str(),
                    reason.code(), handle, state.drain_waits(), dropped,
                ),
            );
            let disconnects = abort_disconnects(
                role,
                state.torn(),
                matches!(reason, AbortReason::Disconnected),
            );
            if disconnects {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_TX_RESYNC action=disconnect frag={} of={} sent={}",
                        index,
                        frags,
                        state.fragments_sent()
                    ),
                );
            }
            disconnects
        }
        _ => unreachable!("the shared pump returns only a terminal action"),
    }
}

fn log_tx_pkt(conn: u16, packet_len: usize, frags: usize, sent: usize) {
    if frags > 1 {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "{}",
                TxPktLine {
                    conn,
                    len: packet_len,
                    frags,
                    sent
                }
            ),
        );
    }
}
