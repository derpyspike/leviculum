//! AutoInterface over raw Ethernet frames, for a board without an IP
//! stack (1479).
//!
//! [`AutoLan`] is the whole AutoInterface of a single-NIC board that
//! sees its network as Ethernet II frames (the ThinkNode M9's WiFi
//! station): the peer table and the discovery cadence of
//! [`crate::auto_wire::Peers`], the frames of [`crate::lan_frame`], and
//! what neither has on its own:
//!
//! - which MAC a peer sits at, learned from the frame that carried its
//!   discovery token, so the board never has to solicit a neighbour;
//! - the neighbour advertisement a daemon's kernel asks for before its
//!   first datagram to the board;
//! - the MLDv2 report that keeps a snooping access point forwarding the
//!   discovery group and the board's solicited-node group;
//! - the reverse peering Python sends: the token unicast to each peer's
//!   unicast discovery port, so a peer whose multicast does not reach the
//!   board still keeps the board in its table;
//! - the data dedup ring Python keeps.
//!
//! The wire facts are Python's, by way of `auto_wire`: the group address
//! for group `reticulum` at link scope, discovery port 29716, data port
//! 42671, a token every 1.6 s, a peer gone after 22 s silent. Sans-I/O:
//! every frame to send goes through the caller's `emit`, every frame
//! received comes in through [`AutoLan::receive`].

use core::net::Ipv6Addr;

use crate::auto_wire::{
    discovery_token_for, multicast_address, scope_nibble, unicast_discovery_port, Dedup, Heard,
    MulticastAddressType, Peers, DEFAULT_DATA_PORT, DEFAULT_DISCOVERY_PORT, HW_MTU, NONCE_SIZE,
    PEER_JOB_INTERVAL_MS, REVERSE_PEERING_INTERVAL_MS,
};
use crate::lan_frame::{
    link_local_from_mac, mld_report, multicast_mac, neighbour_advert, parse, solicited_node,
    udp_frame, Inbound, UDP_OVERHEAD,
};

/// The longest frame [`AutoLan`] builds: a datagram of the interface's
/// hardware MTU with its headers.
pub const FRAME_MAX: usize = UDP_OVERHEAD + HW_MTU as usize;

/// How often the MLDv2 report repeats. A snooping switch forgets a
/// listener after the multicast listener interval, 260 s by default
/// (RFC 3810 §9.4); a report each minute stays well inside it without a
/// query having to reach the board.
pub const MLD_REPORT_INTERVAL_MS: u64 = 60_000;

/// The hop limit on the discovery multicast: Python leaves the socket's
/// default, 1.
const MULTICAST_HOPS: u8 = 1;
/// The hop limit on unicast datagrams, Linux's default.
const UNICAST_HOPS: u8 = 64;
/// The source port the board sends from. Receivers look at the address,
/// never the port; Python sends from an ephemeral one.
const SOURCE_PORT: u16 = 49_152 + 1479;

/// What a received frame was, for the caller's counters and its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received<'a> {
    /// A Reticulum packet from a peer: the node's.
    Data(&'a [u8]),
    /// A discovery token that made `addr` a new peer.
    PeerAdded(Ipv6Addr),
    /// A discovery token from a known peer, or our own.
    Discovery,
    /// A discovery token that did not verify, or a full table.
    Refused,
    /// A datagram on the data port from an address that is no peer
    /// (Python drops these too), or a copy the dedup ring knew.
    Dropped,
    /// A neighbour solicitation for the board, answered.
    Solicited,
    /// Nothing for AutoInterface.
    Other,
}

/// One board's AutoInterface on one NIC. `N` bounds the peers.
pub struct AutoLan<const N: usize> {
    mac: [u8; 6],
    own: Ipv6Addr,
    group: Ipv6Addr,
    group_id: &'static [u8],
    peers: Peers<N>,
    macs: [Option<(Ipv6Addr, [u8; 6])>; N],
    dedup: Dedup,
    next_mld_ms: u64,
    next_reverse_ms: u64,
    next_expire_ms: u64,
    buf: [u8; FRAME_MAX],
}

impl<const N: usize> AutoLan<N> {
    /// The interface of a station at `mac`, in AutoInterface group
    /// `group_id`, with this boot's discovery `nonce`.
    pub fn new(mac: [u8; 6], group_id: &'static [u8], nonce: [u8; NONCE_SIZE]) -> Self {
        let own = link_local_from_mac(mac);
        Self {
            mac,
            own,
            group: multicast_address(
                group_id,
                scope_nibble("link"),
                MulticastAddressType::Temporary,
            ),
            group_id,
            peers: Peers::new(group_id, own, nonce, DEFAULT_DATA_PORT),
            macs: [None; N],
            dedup: Dedup::new(),
            next_mld_ms: 0,
            next_reverse_ms: REVERSE_PEERING_INTERVAL_MS,
            next_expire_ms: PEER_JOB_INTERVAL_MS,
            buf: [0; FRAME_MAX],
        }
    }

    /// The board's link-local address.
    pub fn own(&self) -> Ipv6Addr {
        self.own
    }

    /// The discovery group's address.
    pub fn group(&self) -> Ipv6Addr {
        self.group
    }

    /// How many peers are live.
    pub fn peer_count(&self) -> usize {
        self.peers.peers().count()
    }

    /// The link came (back) up: the MLD report and a discovery token go
    /// out on the next [`Self::poll`], whatever their cadence said.
    pub fn link_up(&mut self) {
        self.next_mld_ms = 0;
        // `Peers` sends at once when its next time has passed; a fresh
        // table at 0 does, a table from before the drop is past due.
    }

    /// The link went down: every peer is gone with it, and how many were
    /// is returned. A peer comes back with its next token.
    pub fn link_down(&mut self) -> usize {
        let gone = self.peer_count();
        // Expire everything: a time far past every peer's last token.
        self.peers.expire(u64::MAX);
        self.macs = [None; N];
        gone
    }

    /// The timers at `now_ms`: the discovery token, the MLD report, the
    /// reverse peering, the expiry of silent peers. Frames go out through
    /// `emit`; returns how many peers expired.
    pub fn poll(&mut self, now_ms: u64, emit: &mut dyn FnMut(&[u8])) -> usize {
        if now_ms >= self.next_mld_ms {
            self.next_mld_ms = now_ms + MLD_REPORT_INTERVAL_MS;
            let groups = [self.group, solicited_node(&self.own)];
            if let Some(n) = mld_report(&mut self.buf, self.mac, &self.own, &groups) {
                emit(&self.buf[..n]);
            }
        }
        if let Some(token) = self.peers.discovery_due(now_ms) {
            if let Some(n) = udp_frame(
                &mut self.buf,
                self.mac,
                multicast_mac(&self.group),
                &self.own,
                &self.group,
                SOURCE_PORT,
                DEFAULT_DISCOVERY_PORT,
                MULTICAST_HOPS,
                &token,
            ) {
                emit(&self.buf[..n]);
            }
        }
        if now_ms >= self.next_reverse_ms {
            self.next_reverse_ms = now_ms + REVERSE_PEERING_INTERVAL_MS;
            // Python's reverse peering sends the bare token.
            let token = discovery_token_for(self.group_id, &self.own);
            for i in 0..N {
                let Some((addr, mac)) = self.macs[i] else {
                    continue;
                };
                if !self.peers.is_peer(&addr) {
                    continue;
                }
                if let Some(n) = udp_frame(
                    &mut self.buf,
                    self.mac,
                    mac,
                    &self.own,
                    &addr,
                    SOURCE_PORT,
                    unicast_discovery_port(DEFAULT_DISCOVERY_PORT),
                    UNICAST_HOPS,
                    &token,
                ) {
                    emit(&self.buf[..n]);
                }
            }
        }
        let mut expired = 0;
        if now_ms >= self.next_expire_ms {
            self.next_expire_ms = now_ms + PEER_JOB_INTERVAL_MS;
            expired = self.peers.expire(now_ms);
            if expired > 0 {
                for slot in &mut self.macs {
                    if slot.is_some_and(|(addr, _)| !self.peers.is_peer(&addr)) {
                        *slot = None;
                    }
                }
            }
        }
        expired
    }

    /// One frame the NIC received at `now_ms`. A solicitation for the
    /// board is answered through `emit`.
    pub fn receive<'f>(
        &mut self,
        frame: &'f [u8],
        now_ms: u64,
        emit: &mut dyn FnMut(&[u8]),
    ) -> Received<'f> {
        match parse(frame) {
            Inbound::NeighbourSolicit {
                src_mac,
                src,
                target,
            } => {
                if target != self.own {
                    return Received::Other;
                }
                if let Some(n) = neighbour_advert(&mut self.buf, self.mac, &self.own, src_mac, &src)
                {
                    emit(&self.buf[..n]);
                }
                Received::Solicited
            }
            Inbound::Udp {
                src_mac,
                src,
                dst,
                dst_port,
                payload,
                ..
            } => {
                let discovery_port = DEFAULT_DISCOVERY_PORT;
                let to_group = dst == self.group && dst_port == discovery_port;
                let to_us = dst == self.own;
                if to_group || (to_us && dst_port == unicast_discovery_port(discovery_port)) {
                    return self.heard(src_mac, src, payload, now_ms);
                }
                if to_us && dst_port == DEFAULT_DATA_PORT {
                    if !self.peers.is_peer(&src) || self.dedup.is_duplicate(payload, now_ms) {
                        return Received::Dropped;
                    }
                    return Received::Data(payload);
                }
                Received::Other
            }
            Inbound::Other => Received::Other,
        }
    }

    fn heard<'f>(
        &mut self,
        src_mac: [u8; 6],
        src: Ipv6Addr,
        payload: &'f [u8],
        now_ms: u64,
    ) -> Received<'f> {
        match self.peers.heard(src, payload, now_ms) {
            Heard::Refused | Heard::Full => Received::Refused,
            Heard::OwnEcho => Received::Discovery,
            heard @ (Heard::Added | Heard::Refreshed) => {
                self.learn_mac(src, src_mac);
                if heard == Heard::Added {
                    Received::PeerAdded(src)
                } else {
                    Received::Discovery
                }
            }
        }
    }

    fn learn_mac(&mut self, addr: Ipv6Addr, mac: [u8; 6]) {
        if let Some(slot) = self
            .macs
            .iter_mut()
            .find(|s| s.is_some_and(|(a, _)| a == addr))
        {
            *slot = Some((addr, mac));
            return;
        }
        // A free slot, or one whose peer the table no longer holds.
        let peers = &self.peers;
        if let Some(slot) = self
            .macs
            .iter_mut()
            .find(|s| s.is_none_or(|(a, _)| !peers.is_peer(&a)))
        {
            *slot = Some((addr, mac));
        }
    }

    /// One Reticulum packet to every live peer, unicast on its data port.
    /// Returns how many peers it went to; 0 when there are none or the
    /// packet is longer than the interface's MTU.
    pub fn send(&mut self, packet: &[u8], emit: &mut dyn FnMut(&[u8])) -> usize {
        if packet.len() > HW_MTU as usize {
            return 0;
        }
        let mut sent = 0;
        for i in 0..N {
            let Some((addr, mac)) = self.macs[i] else {
                continue;
            };
            let Some(peer) = self.peers.peers().find(|p| p.addr == addr).copied() else {
                continue;
            };
            if let Some(n) = udp_frame(
                &mut self.buf,
                self.mac,
                mac,
                &self.own,
                &addr,
                SOURCE_PORT,
                peer.data_port,
                UNICAST_HOPS,
                packet,
            ) {
                emit(&self.buf[..n]);
                sent += 1;
            }
        }
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_wire::{discovery_packet, DEFAULT_GROUP_ID, PEERING_TIMEOUT_MS};
    use alloc::vec::Vec;

    const BOARD_MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56];
    const HOST_MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    const NONCE: [u8; NONCE_SIZE] = [1, 2, 3, 4, 5, 6, 7, 8];

    fn board() -> AutoLan<4> {
        AutoLan::new(BOARD_MAC, DEFAULT_GROUP_ID, NONCE)
    }

    fn host() -> Ipv6Addr {
        link_local_from_mac(HOST_MAC)
    }

    /// The discovery datagram a daemon at `HOST_MAC` multicasts.
    fn host_discovery(token_for: &Ipv6Addr) -> Vec<u8> {
        let lan = board();
        let token = discovery_token_for(DEFAULT_GROUP_ID, token_for);
        let pkt = discovery_packet(&token, &[9; NONCE_SIZE], DEFAULT_DATA_PORT);
        let mut out = [0u8; FRAME_MAX];
        let n = udp_frame(
            &mut out,
            HOST_MAC,
            multicast_mac(&lan.group()),
            &host(),
            &lan.group(),
            51000,
            DEFAULT_DISCOVERY_PORT,
            1,
            &pkt,
        )
        .unwrap();
        out[..n].to_vec()
    }

    fn host_data(port: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = [0u8; FRAME_MAX];
        let n = udp_frame(
            &mut out,
            HOST_MAC,
            BOARD_MAC,
            &host(),
            &link_local_from_mac(BOARD_MAC),
            51001,
            port,
            64,
            payload,
        )
        .unwrap();
        out[..n].to_vec()
    }

    fn sink() -> (Vec<Vec<u8>>, impl FnMut(&[u8])) {
        (Vec::new(), |_: &[u8]| {})
    }

    #[test]
    fn the_first_poll_reports_mld_and_multicasts_the_pythons_token() {
        let mut lan = board();
        let mut sent: Vec<Vec<u8>> = Vec::new();
        lan.poll(0, &mut |f| sent.push(f.to_vec()));
        assert_eq!(sent.len(), 2, "the MLD report and the discovery token");
        assert!(sent[0].starts_with(&[0x33, 0x33, 0, 0, 0, 0x16]));
        match parse(&sent[1]) {
            Inbound::Udp {
                src,
                dst,
                dst_port,
                payload,
                ..
            } => {
                assert_eq!(src, lan.own());
                assert_eq!(
                    dst,
                    "ff12:0:d70b:fb1c:16e4:5e39:485e:31e1"
                        .parse::<Ipv6Addr>()
                        .unwrap()
                );
                assert_eq!(dst_port, 29716);
                assert_eq!(
                    &payload[..32],
                    &discovery_token_for(b"reticulum", &lan.own())
                );
            }
            other => panic!("not a datagram: {other:?}"),
        }
        assert_eq!(sent[1][..6], [0x33, 0x33, 0x48, 0x5e, 0x31, 0xe1]);
        // Nothing more until the cadence says so.
        sent.clear();
        lan.poll(1_599, &mut |f| sent.push(f.to_vec()));
        assert!(sent.is_empty());
        lan.poll(1_600, &mut |f| sent.push(f.to_vec()));
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn a_daemon_on_the_lan_becomes_a_peer_and_its_data_reaches_the_node() {
        let mut lan = board();
        let (_, mut emit) = sink();
        assert_eq!(
            lan.receive(&host_discovery(&host()), 10, &mut emit),
            Received::PeerAdded(host())
        );
        assert_eq!(lan.peer_count(), 1);
        let frame = host_data(DEFAULT_DATA_PORT, b"a reticulum packet");
        assert_eq!(
            lan.receive(&frame, 20, &mut emit),
            Received::Data(b"a reticulum packet")
        );
        // The same packet again inside the dedup window is a copy.
        assert_eq!(lan.receive(&frame, 30, &mut emit), Received::Dropped);
    }

    #[test]
    fn data_from_no_peer_is_dropped_as_python_drops_it() {
        let mut lan = board();
        let (_, mut emit) = sink();
        let frame = host_data(DEFAULT_DATA_PORT, b"x");
        assert_eq!(lan.receive(&frame, 0, &mut emit), Received::Dropped);
    }

    #[test]
    fn a_token_for_another_address_is_refused() {
        let mut lan = board();
        let (_, mut emit) = sink();
        let forged = host_discovery(&"fe80::dead".parse().unwrap());
        assert_eq!(lan.receive(&forged, 0, &mut emit), Received::Refused);
        assert_eq!(lan.peer_count(), 0);
    }

    #[test]
    fn a_packet_goes_to_each_peer_at_the_mac_its_token_came_from() {
        let mut lan = board();
        let (_, mut emit) = sink();
        let mut sent: Vec<Vec<u8>> = Vec::new();
        assert_eq!(lan.send(b"nobody", &mut |f| sent.push(f.to_vec())), 0);
        lan.receive(&host_discovery(&host()), 0, &mut emit);
        assert_eq!(lan.send(b"hello", &mut |f| sent.push(f.to_vec())), 1);
        assert_eq!(&sent[0][..6], &HOST_MAC);
        match parse(&sent[0]) {
            Inbound::Udp {
                src,
                dst,
                dst_port,
                payload,
                ..
            } => {
                assert_eq!(src, lan.own());
                assert_eq!(dst, host());
                assert_eq!(dst_port, DEFAULT_DATA_PORT);
                assert_eq!(payload, b"hello");
            }
            other => panic!("not a datagram: {other:?}"),
        }
        let too_long = [0u8; HW_MTU as usize + 1];
        assert_eq!(lan.send(&too_long, &mut |f| sent.push(f.to_vec())), 0);
    }

    #[test]
    fn a_kernels_solicitation_for_the_board_is_answered() {
        let mut lan = board();
        let own = lan.own();
        let group = solicited_node(&own);
        // Build the NS the way the lan_frame test does, through the NA
        // builder's mirror: an NS is checked by the parser only.
        let mut ns = [0u8; 86];
        ns[0..6].copy_from_slice(&multicast_mac(&group));
        ns[6..12].copy_from_slice(&HOST_MAC);
        ns[12..14].copy_from_slice(&[0x86, 0xdd]);
        ns[14] = 0x60;
        ns[18..20].copy_from_slice(&32u16.to_be_bytes());
        ns[20] = 58;
        ns[21] = 255;
        ns[22..38].copy_from_slice(&host().octets());
        ns[38..54].copy_from_slice(&group.octets());
        ns[54] = 135;
        ns[62..78].copy_from_slice(&own.octets());
        ns[78] = 1;
        ns[79] = 1;
        ns[80..86].copy_from_slice(&HOST_MAC);
        let sum = icmp_sum(&host(), &group, &ns[54..86]);
        ns[56..58].copy_from_slice(&sum.to_be_bytes());
        let mut sent: Vec<Vec<u8>> = Vec::new();
        assert_eq!(
            lan.receive(&ns, 0, &mut |f| sent.push(f.to_vec())),
            Received::Solicited
        );
        assert_eq!(sent.len(), 1);
        assert_eq!(&sent[0][..6], &HOST_MAC);
        assert_eq!(sent[0][54], 136);
        // A solicitation for someone else is not ours to answer.
        let mut other = ns;
        other[77] ^= 1;
        let sum = icmp_sum(&host(), &group, &{
            let mut b = [0u8; 32];
            b.copy_from_slice(&other[54..86]);
            b[2] = 0;
            b[3] = 0;
            b
        });
        other[56..58].copy_from_slice(&sum.to_be_bytes());
        sent.clear();
        assert_eq!(
            lan.receive(&other, 0, &mut |f| sent.push(f.to_vec())),
            Received::Other
        );
        assert!(sent.is_empty());
    }

    fn icmp_sum(src: &Ipv6Addr, dst: &Ipv6Addr, body: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        let mut add = |b: &[u8]| {
            for pair in b.chunks(2) {
                let hi = u32::from(pair[0]) << 8;
                let lo = pair.get(1).copied().map_or(0, u32::from);
                sum += hi | lo;
            }
        };
        add(&src.octets());
        add(&dst.octets());
        add(&(body.len() as u32).to_be_bytes());
        add(&[0, 0, 0, 58]);
        let mut zeroed = [0u8; 32];
        zeroed.copy_from_slice(body);
        zeroed[2] = 0;
        zeroed[3] = 0;
        add(&zeroed);
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !(sum as u16)
    }

    #[test]
    fn a_silent_peer_expires_and_takes_no_more_packets() {
        let mut lan = board();
        let (_, mut emit) = sink();
        lan.receive(&host_discovery(&host()), 0, &mut emit);
        let mut sent = 0usize;
        let expired = lan.poll(PEERING_TIMEOUT_MS + PEER_JOB_INTERVAL_MS, &mut |_| {
            sent += 1
        });
        assert_eq!(expired, 1);
        assert_eq!(lan.peer_count(), 0);
        assert_eq!(lan.send(b"late", &mut |_| {}), 0);
        let frame = host_data(DEFAULT_DATA_PORT, b"late");
        assert_eq!(lan.receive(&frame, 30_000, &mut emit), Received::Dropped);
    }

    #[test]
    fn the_link_going_down_drops_every_peer_and_up_reports_at_once() {
        let mut lan = board();
        let (_, mut emit) = sink();
        lan.poll(0, &mut |_| {});
        lan.receive(&host_discovery(&host()), 5, &mut emit);
        assert_eq!(lan.link_down(), 1);
        assert_eq!(lan.peer_count(), 0);
        assert_eq!(lan.send(b"x", &mut |_| {}), 0);
        lan.link_up();
        let mut sent: Vec<Vec<u8>> = Vec::new();
        lan.poll(100, &mut |f| sent.push(f.to_vec()));
        assert!(
            sent.iter()
                .any(|f| f.starts_with(&[0x33, 0x33, 0, 0, 0, 0x16])),
            "the MLD report goes out again when the link is back"
        );
    }

    #[test]
    fn reverse_peering_sends_the_token_to_each_peers_unicast_port() {
        let mut lan = board();
        let (_, mut emit) = sink();
        lan.receive(&host_discovery(&host()), 0, &mut emit);
        let mut sent: Vec<Vec<u8>> = Vec::new();
        lan.poll(REVERSE_PEERING_INTERVAL_MS, &mut |f| sent.push(f.to_vec()));
        let reverse: Vec<_> = sent
            .iter()
            .filter_map(|f| match parse(f) {
                Inbound::Udp {
                    dst,
                    dst_port,
                    payload,
                    ..
                } if dst == host() => Some((dst_port, payload.to_vec())),
                _ => None,
            })
            .collect();
        assert_eq!(reverse.len(), 1);
        assert_eq!(reverse[0].0, 29717);
        assert_eq!(
            reverse[0].1,
            discovery_token_for(DEFAULT_GROUP_ID, &lan.own()).to_vec()
        );
        // A peer's reverse token to the board refreshes it.
        let token = discovery_token_for(DEFAULT_GROUP_ID, &host());
        let frame = host_data(29717, &token);
        assert_eq!(lan.receive(&frame, 1, &mut emit), Received::Discovery);
    }
}
