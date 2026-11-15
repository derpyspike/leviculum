//! `run_columba` against a scripted H4 controller (1715).
//!
//! The whole backend runs as the M9 runs it: the vendored TrouBLE host
//! over an HCI transport, the three peripheral tasks, the central half,
//! the shared fan-out, all joined in one future polled with a no-op
//! waker, once a turn. Only the controller is a script: it answers every
//! HCI command the host issues, and plays the air side of each link as
//! raw H4 bytes — LE Connection Complete, ATT writes in ACL frames,
//! Disconnection Complete — so the GATT server, the handshake, the
//! fragmenter and the session registry run their real code paths.
//!
//! One test, not five: the backend's state is process-wide statics
//! (the GATT server cell, the drain table, the session registry), and
//! `run_columba` never returns, so the phases share one running stack
//! the way a field board's links share one boot.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::vec::Vec as StdVec;

use core::convert::Infallible;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use bt_hci::transport::{PacketKind, PacketToController, PacketToHost, Transport, WithIndicator};
use bt_hci::ReadHciError;
use leviculum_ble_link::BleLinkEvent;
use leviculum_ble_tx::TxAim;
use leviculum_core::framing::ble as framing;

use super::{build_stack, columba, run_columba, Resources, StackCell};

/// What the controller has seen from the host, and what it queues back.
#[derive(Default)]
struct Air {
    /// H4 frames for the host, indicator byte first.
    to_host: VecDeque<StdVec<u8>>,
    /// Every command opcode the host issued, in order.
    opcodes: StdVec<u16>,
    /// ATT PDUs the host sent, per connection handle, in order.
    att_out: StdVec<(u16, StdVec<u8>)>,
    /// Legacy advertising is on (the controller stops it on a connect).
    advertising: bool,
    /// Handles the host asked to disconnect.
    disconnects: StdVec<u16>,
    /// Links on which WE are the central: the script plays a Columba
    /// GATT server there ([`Script::serve_att`]) with this identity.
    gatt_peers: StdVec<(u16, [u8; 16])>,
}

/// The scripted controller: a `bt_hci` transport over [`Air`].
struct Script {
    air: Mutex<Air>,
}

impl Script {
    fn air(&self) -> std::sync::MutexGuard<'_, Air> {
        self.air.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn push(&self, frame: StdVec<u8>) {
        self.air().to_host.push_back(frame);
    }

    /// The controller's answer to one command.
    fn on_command(&self, opcode: u16, params: &[u8]) {
        let mut air = self.air();
        air.opcodes.push(opcode);
        let complete = |ret: &[u8]| {
            let mut f = vec![0x04, 0x0e, (3 + ret.len()) as u8, 1];
            f.extend_from_slice(&opcode.to_le_bytes());
            f.extend_from_slice(ret);
            f
        };
        let status = || {
            let mut f = vec![0x04, 0x0f, 4, 0x00, 1];
            f.extend_from_slice(&opcode.to_le_bytes());
            f
        };
        let handle = || {
            [
                params.first().copied().unwrap_or(0),
                params.get(1).copied().unwrap_or(0),
            ]
        };
        let frame = match opcode {
            // HostNumberOfCompletedPackets: no answer, by the spec.
            0x0c35 => return,
            // LeReadFilterAcceptListSize.
            0x200f => complete(&[0, 8]),
            // LeReadBufferSize: 251-byte ACL, four buffers.
            0x2002 => complete(&[0, 251, 0, 4]),
            // ReadBdAddr: none (the host keeps its static random one).
            0x1009 => complete(&[0, 0, 0, 0, 0, 0, 0]),
            // LeSetAdvEnable.
            0x200a => {
                air.advertising = params.first() == Some(&1);
                complete(&[0])
            }
            // Disconnect: status now, the completion follows.
            0x0406 => {
                let h = handle();
                air.disconnects.push(u16::from_le_bytes(h));
                air.to_host.push_back(status());
                vec![0x04, 0x05, 4, 0, h[0], h[1], 0x16]
            }
            // The asynchronous LE procedures this backend may start
            // (create connection, connection update, remote features,
            // PHY): Command Status, nothing after.
            0x200d | 0x2013 | 0x2016 | 0x2032 | 0x2043 => status(),
            // LeSetDataLength, LeRemoteConnParamReqReply/NegReply: handle.
            0x2020..=0x2022 => {
                let h = handle();
                complete(&[0, h[0], h[1]])
            }
            // LeReadPhy: 1M both ways.
            0x2030 => {
                let h = handle();
                complete(&[0, h[0], h[1], 1, 1])
            }
            // ReadRssi.
            0x1405 => {
                let h = handle();
                complete(&[0, h[0], h[1], 0xc4])
            }
            // Every other command this host issues returns only a status.
            _ => complete(&[0]),
        };
        air.to_host.push_back(frame);
    }

    /// One ACL frame from the host: an ATT PDU on a link. The buffer
    /// comes back at once (Number Of Completed Packets).
    fn on_acl(&self, bytes: &[u8]) {
        let handle = u16::from_le_bytes([bytes[0], bytes[1]]) & 0x0fff;
        let mut air = self.air();
        let l2cap = &bytes[4..];
        let cid = u16::from_le_bytes([l2cap[2], l2cap[3]]);
        if cid == 0x0004 {
            let pdu = l2cap[4..].to_vec();
            if let Some(&(_, id)) = air.gatt_peers.iter().find(|(h, _)| *h == handle) {
                if let Some(answer) = serve_att(&pdu, &id) {
                    let frame = att_frame(handle, &answer);
                    air.to_host.push_back(frame);
                }
            }
            air.att_out.push((handle, pdu));
        }
        let h = handle.to_le_bytes();
        air.to_host
            .push_back(vec![0x04, 0x13, 5, 1, h[0], h[1], 1, 0]);
    }

    /// A central connects to our advertisement (the peer's random address).
    fn connect(&self, handle: u16, addr: [u8; 6]) {
        let h = handle.to_le_bytes();
        let mut f = vec![0x04, 0x3e, 19, 0x01, 0x00, h[0], h[1], 0x01, 0x01];
        f.extend_from_slice(&addr);
        // 30 ms interval, no latency, 5 s supervision timeout.
        f.extend_from_slice(&[24, 0, 0, 0, 0xf4, 0x01, 0]);
        let mut air = self.air();
        air.advertising = false;
        air.to_host.push_back(f);
    }

    /// The link drops from the air (supervision timeout, 0x08).
    fn drop_link(&self, handle: u16) {
        let h = handle.to_le_bytes();
        self.push(vec![0x04, 0x05, 4, 0, h[0], h[1], 0x08]);
    }

    /// One ATT PDU from the peer on `handle`.
    fn att_in(&self, handle: u16, pdu: &[u8]) {
        self.push(att_frame(handle, pdu));
    }

    /// Our central's dial completes: the link is up with US as central,
    /// and the peer behind it serves the Columba GATT table.
    fn central_connected(&self, handle: u16, addr: [u8; 6], identity: [u8; 16]) {
        let h = handle.to_le_bytes();
        let mut f = vec![0x04, 0x3e, 19, 0x01, 0x00, h[0], h[1], 0x00, 0x01];
        f.extend_from_slice(&addr);
        f.extend_from_slice(&[24, 0, 0, 0, 0xf4, 0x01, 0]);
        let mut air = self.air();
        air.gatt_peers.push((handle, identity));
        air.to_host.push_back(f);
    }

    /// One legacy advertising report: a connectable Columba peer.
    fn advertise_peer(&self, addr: [u8; 6], identity: [u8; 16]) {
        let svc = 0x37145b00_442d_4a94_917f_8f42c5da28e3_u128.to_le_bytes();
        let record = leviculum_ble_tx::manufacturer_data_with_hint(
            leviculum_ble_tx::with_free_slots(0, 3),
            &leviculum_ble_tx::identity_hint(&identity),
        );
        let mut data = vec![2, 0x01, 0x06, 17, 0x07];
        data.extend_from_slice(&svc);
        data.push(record.len() as u8 + 1);
        data.push(0xff);
        data.extend_from_slice(&record);
        let mut f = vec![0x04, 0x3e, 0, 0x02, 1, 0x00, 0x01];
        f.extend_from_slice(&addr);
        f.push(data.len() as u8);
        f.extend_from_slice(&data);
        f.push(0xc4);
        f[2] = (f.len() - 3) as u8;
        self.push(f);
    }

    /// ATT Write Request (answered by a Write Response).
    fn write_req(&self, handle: u16, attr: u16, value: &[u8]) {
        let a = attr.to_le_bytes();
        let mut pdu = vec![0x12, a[0], a[1]];
        pdu.extend_from_slice(value);
        self.att_in(handle, &pdu);
    }

    /// ATT Write Command (no response), how Columba carries fragments.
    fn write_cmd(&self, handle: u16, attr: u16, value: &[u8]) {
        let a = attr.to_le_bytes();
        let mut pdu = vec![0x52, a[0], a[1]];
        pdu.extend_from_slice(value);
        self.att_in(handle, &pdu);
    }

    /// The notifications the host sent on `handle` for attribute `attr`.
    fn notifications(&self, handle: u16, attr: u16) -> StdVec<StdVec<u8>> {
        let a = attr.to_le_bytes();
        self.air()
            .att_out
            .iter()
            .filter(|(h, pdu)| *h == handle && pdu.len() >= 3 && pdu[0] == 0x1b && pdu[1..3] == a)
            .map(|(_, pdu)| pdu[3..].to_vec())
            .collect()
    }
}

/// One ATT PDU as an H4 ACL frame on `handle`.
fn att_frame(handle: u16, pdu: &[u8]) -> StdVec<u8> {
    let h = handle.to_le_bytes();
    let l2len = (pdu.len() as u16).to_le_bytes();
    let acllen = (pdu.len() as u16 + 4).to_le_bytes();
    let mut f = vec![
        0x02,
        h[0],
        h[1] | 0x20,
        acllen[0],
        acllen[1],
        l2len[0],
        l2len[1],
        4,
        0,
    ];
    f.extend_from_slice(pdu);
    f
}

/// The peer's Columba GATT table, as a phone or board lays it out:
/// 1 service, 2/3 `rx`, 4/5 `tx` with its CCCD at 6, 7/8 `identity`.
const PEER_TABLE: [(u16, u8, u16, u128); 3] = [
    (2, 0x0c, 3, 0x37145b00_442d_4a94_917f_8f42c5da28e5),
    (4, 0x12, 5, 0x37145b00_442d_4a94_917f_8f42c5da28e4),
    (7, 0x02, 8, 0x37145b00_442d_4a94_917f_8f42c5da28e6),
];

/// The scripted peer's answer to one ATT request from our GATT client
/// (ATT MTU 23 throughout: one 21-byte characteristic entry per reply).
fn serve_att(pdu: &[u8], identity: &[u8; 16]) -> Option<StdVec<u8>> {
    let u16_at = |i: usize| u16::from_le_bytes([pdu[i], pdu[i + 1]]);
    let not_found = |op: u8, at: u16| {
        let a = at.to_le_bytes();
        Some(vec![0x01, op, a[0], a[1], 0x0a])
    };
    match pdu[0] {
        // Exchange MTU: stay at 23.
        0x02 => Some(vec![0x03, 23, 0]),
        // Find By Type Value: the primary service, handles 1..=8.
        0x06 => {
            let start = u16_at(1);
            if start <= 1 {
                Some(vec![0x07, 1, 0, 8, 0])
            } else {
                not_found(0x06, start)
            }
        }
        // Read By Type (characteristic declarations from `start`).
        0x08 => {
            let start = u16_at(1);
            let end = u16_at(3);
            match PEER_TABLE
                .iter()
                .find(|(decl, ..)| *decl >= start && *decl <= end)
            {
                Some(&(decl, props, value, uuid)) => {
                    let mut r = vec![0x09, 21];
                    r.extend_from_slice(&decl.to_le_bytes());
                    r.push(props);
                    r.extend_from_slice(&value.to_le_bytes());
                    r.extend_from_slice(&uuid.to_le_bytes());
                    Some(r)
                }
                None => not_found(0x08, start),
            }
        }
        // Find Information: the one descriptor, `tx`'s CCCD at 6.
        0x04 => {
            let start = u16_at(1);
            let end = u16_at(3);
            if start <= 6 && end >= 6 {
                Some(vec![0x05, 0x01, 6, 0, 0x02, 0x29])
            } else {
                not_found(0x04, start)
            }
        }
        // Read: the identity value.
        0x0a if u16_at(1) == 8 => {
            let mut r = vec![0x0b];
            r.extend_from_slice(identity);
            Some(r)
        }
        0x0a => not_found(0x0a, u16_at(1)),
        // Write Request: accepted.
        0x12 => Some(vec![0x13]),
        _ => None,
    }
}

/// A byte sink for one serialised packet.
struct Sink(StdVec<u8>);

impl embedded_io::ErrorType for Sink {
    type Error = Infallible;
}

impl embedded_io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Infallible> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<(), Infallible> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ScriptedController(&'static Script);

impl embedded_io::ErrorType for ScriptedController {
    type Error = ReadHciError<Infallible>;
}

impl Transport for ScriptedController {
    fn read<'a, P: PacketToHost<'a>>(
        &self,
        rx: &'a mut [u8],
    ) -> impl Future<Output = Result<P, Self::Error>> {
        let script = self.0;
        let mut rx = Some(rx);
        core::future::poll_fn(move |_| {
            let Some(frame) = script.air().to_host.pop_front() else {
                // The test polls again on its next turn, as the M9's
                // core-0 loop does; no waker is needed.
                return Poll::Pending;
            };
            let kind = match frame[0] {
                2 => PacketKind::AclData,
                4 => PacketKind::Event,
                other => panic!("the script only plays ACL and events, not {other}"),
            };
            let mut body: &[u8] = &frame[1..];
            let buf = rx.take().expect("one frame per read");
            Poll::Ready(P::read_hci(kind, &mut body, buf))
        })
    }

    fn write<P: PacketToController>(
        &self,
        tx: &P,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        let mut sink = Sink(StdVec::new());
        let _ = WithIndicator::new(tx).write_hci(&mut sink);
        let bytes = sink.0;
        match bytes[0] {
            1 => {
                let opcode = u16::from_le_bytes([bytes[1], bytes[2]]);
                self.0.on_command(opcode, &bytes[4..]);
            }
            2 => self.0.on_acl(&bytes[1..]),
            other => panic!("unexpected packet kind {other} from the host"),
        }
        core::future::ready(Ok(()))
    }
}

/// The running backend and the test's view of the node side.
struct Bench {
    script: &'static Script,
    run: Pin<Box<dyn Future<Output = ()>>>,
    chans: leviculum_ble_link::BleChannels,
    rx_attr: u16,
    tx_attr: u16,
    cccd_attr: u16,
    events: StdVec<BleLinkEvent>,
    log: String,
}

impl Bench {
    /// Poll the backend once and collect what it handed the node.
    fn turn(&mut self) {
        let mut cx = Context::from_waker(Waker::noop());
        let _ = self.run.as_mut().poll(&mut cx);
        while let Ok(e) = self.chans.peer_event_rx.try_receive() {
            self.events.push(e);
        }
        while let Ok(e) = self.chans.incoming_rx.try_receive() {
            if let BleLinkEvent::Inbound { packet, .. } = &e {
                leviculum_ble_link::incoming_held_sub(packet.capacity());
            }
            self.events.push(e);
        }
        let mut chunk = [0u8; 512];
        loop {
            let n = crate::log::LOG_RING.read(&mut chunk);
            if n == 0 {
                break;
            }
            self.log.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    /// Turn until `done` holds, or fail with what the backend said.
    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done(self) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}\nopcodes: {:04x?}\nlog:\n{}",
                self.script.air().opcodes,
                self.log
            );
            self.turn();
        }
    }

    /// Turn for `ms` milliseconds of wall time.
    fn settle(&mut self, ms: u64) {
        let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < end {
            self.turn();
        }
    }

    /// Wait for the advertisement, then let `peer` connect on `handle`
    /// and present `identity`, subscribed to notifications.
    fn link_up(&mut self, handle: u16, addr: [u8; 6], identity: [u8; 16]) {
        self.until("advertising", |b| b.script.air().advertising);
        self.script.connect(handle, addr);
        self.settle(30);
        self.script.write_req(handle, self.cccd_attr, &[1, 0]);
        self.script.write_req(handle, self.rx_attr, &identity);
    }

    fn peer_ups(&self, id: [u8; 16]) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, BleLinkEvent::PeerUp { identity, .. } if *identity == id))
            .count()
    }

    fn peer_losts(&self, id: [u8; 16]) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, BleLinkEvent::PeerLost { identity, .. } if *identity == id))
            .count()
    }

    fn inbound(&self) -> StdVec<(Option<[u8; 16]>, StdVec<u8>)> {
        self.events
            .iter()
            .filter_map(|e| match e {
                BleLinkEvent::Inbound { link, packet } => Some((*link, packet.clone())),
                _ => None,
            })
            .collect()
    }

    /// Columba fragments of `packet` at ATT MTU 23, peer to us.
    fn send_packet(&self, handle: u16, packet: &[u8]) {
        for fragment in framing::fragment_packet(packet, framing::MIN_MTU) {
            self.script.write_cmd(handle, self.rx_attr, &fragment);
        }
    }
}

/// Closes the runtime log gate again when the test ends, pass or fail:
/// the log module's own test needs it closed, and runs in this binary.
struct GateRestore;

impl Drop for GateRestore {
    fn drop(&mut self) {
        crate::log::RUNTIME_DRAIN_OPEN.store(false, core::sync::atomic::Ordering::Relaxed);
    }
}

/// The shared crate's lines into this crate's ring, as the M9 installs
/// it (`leviculum-esp/src/ble.rs`'s `ble_link_log`).
fn ble_link_log(
    prefix: &str,
    args: core::fmt::Arguments<'_>,
    level: leviculum_ble_link::log::Level,
) {
    match level {
        leviculum_ble_link::log::Level::Runtime => crate::log::log_fmt(prefix, args),
        leviculum_ble_link::log::Level::Critical => crate::log::log_fmt_critical(prefix, args),
    }
}

fn bench() -> Bench {
    let script: &'static Script = Box::leak(Box::new(Script {
        air: Mutex::new(Air::default()),
    }));
    let identity = [0x4d; 16];
    crate::log::open_runtime_drain();
    leviculum_ble_link::log::set_sink(ble_link_log);
    leviculum_ble_link::set_active(true);
    let (server, hint) = columba::start(identity).expect("the GATT table takes the service");
    static CELL: StackCell<ScriptedController> = StackCell::new();
    let resources: &'static mut Resources = Box::leak(Box::new(Resources::new()));
    let stack = build_stack(&CELL, resources, ScriptedController(script), &identity);
    let (rx_attr, tx_attr, cccd_attr) = columba::attribute_handles(server);
    let chans = leviculum_ble_link::channels();
    while chans.incoming_rx.try_receive().is_ok() {}
    while chans.peer_event_rx.try_receive().is_ok() {}
    Bench {
        script,
        run: Box::pin(async move {
            run_columba(stack, server, hint, identity).await;
        }),
        chans,
        rx_attr,
        tx_attr,
        cccd_attr: cccd_attr.expect("a notify characteristic has a CCCD"),
        events: StdVec::new(),
        log: String::new(),
    }
}

/// Every link this board serves must stay inside ATT MTU 23 when no
/// exchange happened, survive a peer that leaves mid-fragment, keep two
/// peers apart, hold one link per identity, and come back from a
/// reconnect storm with no slot leaked.
#[test]
fn run_columba_against_a_scripted_h4_controller() {
    let _ring = crate::log::test_ring::lock();
    let _gate = GateRestore;
    let mut b = bench();
    let (pa, pb, pc) = ([0xa1; 16], [0xb2; 16], [0xc3; 16]);
    let (addr_a, addr_b, addr_c) = (
        [1, 1, 1, 1, 1, 0xc1],
        [2, 2, 2, 2, 2, 0xc2],
        [3, 3, 3, 3, 3, 0xc3],
    );

    // Two peers, both at the default ATT MTU of 23.
    b.link_up(0x0040, addr_a, pa);
    b.until("peer A up", |b| b.peer_ups(pa) == 1);
    b.link_up(0x0041, addr_b, pb);
    b.until("peer B up", |b| b.peer_ups(pb) == 1);
    assert_eq!(super::links_claimed(), 2);

    // A flood reaches both links, fragmented for MTU 23: no value over
    // 20 bytes, and the fragments reassemble to the packet on each link.
    let packet: StdVec<u8> = (0..40u8).collect();
    b.chans
        .outgoing_tx
        .try_send((TxAim::Flood, packet.clone()))
        .expect("the outbound channel has room");
    let tx_attr = b.tx_attr;
    let reassembles = |b: &Bench, handle: u16| {
        let mut d = framing::BleDefragmenter::new(super::REASSEMBLY_LIMITS);
        b.script
            .notifications(handle, tx_attr)
            .iter()
            .filter(|v| v.len() >= framing::FRAGMENT_HEADER_SIZE)
            .any(|v| matches!(d.process(v, 0), framing::DefragResult::Complete(p) if p == packet))
    };
    b.until("the flood on both links", |b| {
        reassembles(b, 0x0040) && reassembles(b, 0x0041)
    });
    for handle in [0x0040, 0x0041] {
        for value in b.script.notifications(handle, b.tx_attr) {
            assert!(value.len() <= framing::MIN_MTU - framing::ATT_HEADER_SIZE);
        }
    }

    // Inbound from each peer lands attributed to that peer's link.
    let from_a: StdVec<u8> = (100..150u8).collect();
    let from_b: StdVec<u8> = (150..170u8).collect();
    b.send_packet(0x0040, &from_a);
    b.send_packet(0x0041, &from_b);
    b.until("both inbound packets", |b| b.inbound().len() == 2);
    let inbound = b.inbound();
    assert!(inbound.contains(&(Some(pa), from_a.clone())));
    assert!(inbound.contains(&(Some(pb), from_b.clone())));

    // B leaves mid-fragment: the first fragment of a long packet, then
    // the link drops. The half packet never reaches the node, the peer
    // is lost exactly once, and its slot and held bytes come back.
    let long: StdVec<u8> = (0..60u8).collect();
    let fragments = framing::fragment_packet(&long, framing::MIN_MTU);
    assert!(fragments.len() > 2);
    b.script.write_cmd(0x0041, b.rx_attr, &fragments[0]);
    b.settle(20);
    b.script.drop_link(0x0041);
    b.until("peer B lost", |b| b.peer_losts(pb) == 1);
    b.until("B's slot freed", |_| super::links_claimed() == 1);
    b.until("B's link-down line", |b| b.log.contains("BLE_LINK_DOWN"));
    assert_eq!(b.inbound().len(), 2, "no half packet reaches the node");
    assert_eq!(leviculum_ble_link::defrag_held_bytes(), 0);

    // A second link presenting A's identity (a phone reconnecting from
    // a new address): the board keeps one link per identity — the
    // duplicate rule tears exactly one of the two down — and the node
    // hears no second PeerUp and no PeerLost for A.
    b.link_up(0x0042, [9, 9, 9, 9, 9, 0xc9], pa);
    b.until("one link for A", |b| {
        super::links_claimed() == 1 && b.script.air().disconnects.len() == 1
    });
    b.settle(100);
    assert_eq!(b.peer_ups(pa), 1, "one PeerUp per identity");
    assert_eq!(b.peer_losts(pa), 0, "A never went away");
    assert_eq!(columba::live_peer_count(), 1);
    assert!(
        b.log.contains("BLE_LINK_REPLACED peer=a1a1a1a1")
            || b.log.contains("BLE_LINK_DUP peer=a1a1a1a1")
    );
    let torn = b.script.air().disconnects[0];
    assert!(torn == 0x0040 || torn == 0x0042);
    let a_handle = if torn == 0x0040 { 0x0042 } else { 0x0040 };

    // A reconnect storm from C: twenty connect, handshake, drop rounds.
    for round in 0..20u16 {
        let handle = 0x0050 + round;
        b.link_up(handle, addr_c, pc);
        b.until("C up", |b| b.peer_ups(pc) == usize::from(round) + 1);
        b.script.drop_link(handle);
        b.until("C lost", |b| b.peer_losts(pc) == usize::from(round) + 1);
    }
    b.until("no slot leaked by the storm", |_| {
        super::links_claimed() == 1
    });
    assert_eq!(columba::live_peer_count(), 1);
    assert_eq!(leviculum_ble_link::defrag_held_bytes(), 0);
    // And the board still serves: it advertises for the next peer, and
    // A still gets its packets.
    b.until("advertising after the storm", |b| {
        b.script.air().advertising
    });
    let after: StdVec<u8> = (7..30u8).collect();
    let before = b.script.notifications(a_handle, b.tx_attr).len();
    b.chans
        .outgoing_tx
        .try_send((TxAim::Flood, after))
        .expect("the outbound channel has room");
    b.until("A served after the storm", |b| {
        b.script.notifications(a_handle, b.tx_attr).len() > before
    });

    // A duplicate across roles: D advertises, our central dials it, and
    // while that dial is in flight D dials our advertisement and
    // completes its handshake there. When our central link then reads
    // D's identity, the board still holds one link for D, and the node
    // still hears one PeerUp and no PeerLost.
    let pd = [0xd4; 16];
    let addr_d = [0x10, 0, 0, 0, 0, 0xf0];
    let dials = |b: &Bench| {
        b.script
            .air()
            .opcodes
            .iter()
            .filter(|o| **o == 0x200d)
            .count()
    };
    let dials_before = dials(&b);
    b.until("our central dials D", |b| {
        if dials(b) == dials_before {
            b.script.advertise_peer(addr_d, pd);
            false
        } else {
            true
        }
    });
    b.link_up(0x0070, addr_d, pd);
    b.until("D up over our peripheral", |b| b.peer_ups(pd) == 1);
    // Every wait below is satisfiable only by D's central link: the
    // log is read from where it stood before 0x0071 connected (A's
    // duplicate line is older), the line must name D, and the torn
    // link must be one of D's two handles.
    let links_with_d = super::links_claimed();
    let log_mark = b.log.len();
    let disconnects_before = b.script.air().disconnects.len();
    b.script.central_connected(0x0071, addr_d, pd);
    b.until("D's duplicate decided after the central link", |b| {
        let fresh = &b.log[log_mark..];
        fresh.contains("BLE_LINK_DUP peer=d4d4d4d4")
            || fresh.contains("BLE_LINK_REPLACED peer=d4d4d4d4")
    });
    let torn_of_d = |b: &Bench| {
        b.script.air().disconnects[disconnects_before..]
            .iter()
            .filter(|h| **h == 0x0070 || **h == 0x0071)
            .count()
    };
    b.until("exactly one of D's links torn down", |b| torn_of_d(b) == 1);
    b.until("one link for D", |_| super::links_claimed() == links_with_d);
    b.settle(100);
    assert_eq!(torn_of_d(&b), 1, "one of D's two links, not both");
    assert_eq!(super::links_claimed(), links_with_d);
    assert_eq!(b.peer_ups(pd), 1, "one PeerUp for D across both roles");
    assert_eq!(b.peer_losts(pd), 0, "D never went away");
    assert_eq!(columba::live_peer_count(), 2);
}
