//! IPv6 link-local frames for a node without an IP stack (1479).
//!
//! The ThinkNode M9 speaks AutoInterface ([`crate::auto_wire`]) over its
//! WiFi station, whose driver hands out and takes raw Ethernet II frames.
//! AutoInterface needs four things from IPv6 and nothing more, so the
//! board carries those four instead of a general stack:
//!
//! - a link-local address, the modified EUI-64 of the station's MAC
//!   ([`link_local_from_mac`]);
//! - UDP datagrams to and from link-local and multicast addresses, with
//!   the checksum IPv6 makes mandatory ([`udp_frame`], [`parse`]);
//! - the neighbour advertisement a daemon's kernel asks for before its
//!   first unicast datagram to the board ([`Inbound::NeighbourSolicit`],
//!   [`neighbour_advert`]); the board itself never solicits, it learns a
//!   peer's MAC from the frame that carried the peer's discovery token;
//! - an unsolicited MLDv2 report ([`mld_report`]) for the discovery group
//!   and the solicited-node group, so an access point that snoops
//!   multicast forwards both to the board.
//!
//! No extension headers are parsed: a datagram that carries one is not
//! taken ([`Inbound::Other`]). AutoInterface's senders put none on UDP.
//!
//! Every function is pure and host-tested; the frames are checked
//! against hand-assembled reference bytes in the tests below.

use core::net::Ipv6Addr;

/// Ethernet II header length.
pub const ETH_HEADER: usize = 14;
/// IPv6 fixed header length.
pub const IPV6_HEADER: usize = 40;
/// UDP header length.
pub const UDP_HEADER: usize = 8;
/// Everything in front of a UDP payload.
pub const UDP_OVERHEAD: usize = ETH_HEADER + IPV6_HEADER + UDP_HEADER;
/// The IPv6 ethertype.
pub const ETHERTYPE_IPV6: u16 = 0x86dd;

const NEXT_UDP: u8 = 17;
const NEXT_ICMPV6: u8 = 58;
const NEXT_HOP_BY_HOP: u8 = 0;
const ICMP_NS: u8 = 135;
const ICMP_NA: u8 = 136;
const ICMP_MLD2_REPORT: u8 = 143;
/// The hop limit RFC 4861 requires on neighbour discovery.
const ND_HOP_LIMIT: u8 = 255;
/// The length of a neighbour advertisement with its target link-layer
/// address option.
const NA_LEN: usize = 32;
/// MLDv2 record type CHANGE_TO_EXCLUDE_MODE: join with no source filter.
const MLD_CHANGE_TO_EXCLUDE: u8 = 4;
/// The all-MLDv2-capable-routers group, where reports go.
pub const MLD2_ROUTERS: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0x16);

/// The modified-EUI-64 link-local address of `mac` (RFC 4291 app. A).
pub fn link_local_from_mac(mac: [u8; 6]) -> Ipv6Addr {
    let mut b = [0u8; 16];
    b[0] = 0xfe;
    b[1] = 0x80;
    b[8] = mac[0] ^ 0x02;
    b[9] = mac[1];
    b[10] = mac[2];
    b[11] = 0xff;
    b[12] = 0xfe;
    b[13] = mac[3];
    b[14] = mac[4];
    b[15] = mac[5];
    Ipv6Addr::from(b)
}

/// The Ethernet destination of an IPv6 multicast address (RFC 2464 §7).
pub fn multicast_mac(group: &Ipv6Addr) -> [u8; 6] {
    let o = group.octets();
    [0x33, 0x33, o[12], o[13], o[14], o[15]]
}

/// The solicited-node multicast group of `addr` (RFC 4291 §2.7.1).
pub fn solicited_node(addr: &Ipv6Addr) -> Ipv6Addr {
    let o = addr.octets();
    let mut b = [0u8; 16];
    b[0] = 0xff;
    b[1] = 0x02;
    b[11] = 0x01;
    b[12] = 0xff;
    b[13] = o[13];
    b[14] = o[14];
    b[15] = o[15];
    Ipv6Addr::from(b)
}

/// The internet checksum over the IPv6 pseudo-header and `body`.
fn checksum(src: &Ipv6Addr, dst: &Ipv6Addr, next: u8, body: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut add = |bytes: &[u8]| {
        let mut chunks = bytes.chunks_exact(2);
        for pair in &mut chunks {
            sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
        }
        if let [last] = chunks.remainder() {
            sum += u32::from(u16::from_be_bytes([*last, 0]));
        }
    };
    add(&src.octets());
    add(&dst.octets());
    // Upper-layer length as 32 bits, then three zero bytes and next.
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    add(&len.to_be_bytes());
    add(&[0, 0, 0, next]);
    add(body);
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The Ethernet and IPv6 headers into `out[..54]`.
#[allow(clippy::too_many_arguments)]
fn headers(
    out: &mut [u8],
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src: &Ipv6Addr,
    dst: &Ipv6Addr,
    next: u8,
    hop_limit: u8,
    payload_len: u16,
) {
    out[0..6].copy_from_slice(&dst_mac);
    out[6..12].copy_from_slice(&src_mac);
    out[12..14].copy_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
    let ip = &mut out[ETH_HEADER..ETH_HEADER + IPV6_HEADER];
    ip[0] = 0x60;
    ip[1] = 0;
    ip[2] = 0;
    ip[3] = 0;
    ip[4..6].copy_from_slice(&payload_len.to_be_bytes());
    ip[6] = next;
    ip[7] = hop_limit;
    ip[8..24].copy_from_slice(&src.octets());
    ip[24..40].copy_from_slice(&dst.octets());
}

/// One UDP datagram as an Ethernet frame into `out`; the frame's length,
/// or `None` when `out` is too short or the payload does not fit a
/// datagram.
#[allow(clippy::too_many_arguments)]
pub fn udp_frame(
    out: &mut [u8],
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src: &Ipv6Addr,
    dst: &Ipv6Addr,
    src_port: u16,
    dst_port: u16,
    hop_limit: u8,
    payload: &[u8],
) -> Option<usize> {
    let udp_len = u16::try_from(UDP_HEADER + payload.len()).ok()?;
    let total = UDP_OVERHEAD + payload.len();
    let out = out.get_mut(..total)?;
    headers(
        out, src_mac, dst_mac, src, dst, NEXT_UDP, hop_limit, udp_len,
    );
    let udp = &mut out[ETH_HEADER + IPV6_HEADER..];
    udp[0..2].copy_from_slice(&src_port.to_be_bytes());
    udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    udp[4..6].copy_from_slice(&udp_len.to_be_bytes());
    udp[6..8].copy_from_slice(&[0, 0]);
    udp[UDP_HEADER..].copy_from_slice(payload);
    let mut sum = checksum(src, dst, NEXT_UDP, udp);
    // RFC 768/8200: a computed zero goes on the wire as all ones.
    if sum == 0 {
        sum = 0xffff;
    }
    udp[6..8].copy_from_slice(&sum.to_be_bytes());
    Some(total)
}

/// A frame the board has a use for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inbound<'a> {
    /// A UDP datagram whose checksum holds.
    Udp {
        src_mac: [u8; 6],
        src: Ipv6Addr,
        dst: Ipv6Addr,
        src_port: u16,
        dst_port: u16,
        payload: &'a [u8],
    },
    /// A neighbour solicitation from a node with an address, asking for
    /// `target`'s link-layer address.
    NeighbourSolicit {
        src_mac: [u8; 6],
        src: Ipv6Addr,
        target: Ipv6Addr,
    },
    /// Anything else: not IPv6, a bad checksum, an extension header,
    /// another ICMPv6 message, a solicitation for duplicate detection.
    Other,
}

/// What `frame` carries, for the four things the board takes.
pub fn parse(frame: &[u8]) -> Inbound<'_> {
    parse_inner(frame).unwrap_or(Inbound::Other)
}

fn parse_inner(frame: &[u8]) -> Option<Inbound<'_>> {
    if frame.get(12..14)? != ETHERTYPE_IPV6.to_be_bytes() {
        return None;
    }
    let mut src_mac = [0u8; 6];
    src_mac.copy_from_slice(&frame[6..12]);
    let ip = frame.get(ETH_HEADER..ETH_HEADER + IPV6_HEADER)?;
    if ip[0] >> 4 != 6 {
        return None;
    }
    let payload_len = usize::from(u16::from_be_bytes([ip[4], ip[5]]));
    let next = ip[6];
    let hop_limit = ip[7];
    let src = Ipv6Addr::from(<[u8; 16]>::try_from(&ip[8..24]).ok()?);
    let dst = Ipv6Addr::from(<[u8; 16]>::try_from(&ip[24..40]).ok()?);
    // Ethernet may pad a short frame; the IPv6 length says where it ends.
    let body = frame.get(ETH_HEADER + IPV6_HEADER..ETH_HEADER + IPV6_HEADER + payload_len)?;
    match next {
        NEXT_UDP => {
            if body.len() < UDP_HEADER {
                return None;
            }
            let udp_len = usize::from(u16::from_be_bytes([body[4], body[5]]));
            if udp_len != body.len() || body[6..8] == [0, 0] {
                return None;
            }
            if checksum(&src, &dst, NEXT_UDP, body) != 0 {
                return None;
            }
            Some(Inbound::Udp {
                src_mac,
                src,
                dst,
                src_port: u16::from_be_bytes([body[0], body[1]]),
                dst_port: u16::from_be_bytes([body[2], body[3]]),
                payload: &body[UDP_HEADER..],
            })
        }
        NEXT_ICMPV6 => {
            // RFC 4861 §7.1.1: hop limit 255, code 0, at least 24 bytes.
            if body.len() < 24 || body[0] != ICMP_NS || body[1] != 0 || hop_limit != ND_HOP_LIMIT {
                return None;
            }
            if checksum(&src, &dst, NEXT_ICMPV6, body) != 0 {
                return None;
            }
            if src.is_unspecified() {
                // Duplicate address detection: nobody to answer to.
                return None;
            }
            let target = Ipv6Addr::from(<[u8; 16]>::try_from(&body[8..24]).ok()?);
            Some(Inbound::NeighbourSolicit {
                src_mac,
                src,
                target,
            })
        }
        _ => None,
    }
}

/// The solicited answer to a neighbour solicitation: `own` (at
/// `own_mac`) tells `to` (at `to_mac`) where it is. Returns the frame's
/// length, `None` when `out` is too short.
pub fn neighbour_advert(
    out: &mut [u8],
    own_mac: [u8; 6],
    own: &Ipv6Addr,
    to_mac: [u8; 6],
    to: &Ipv6Addr,
) -> Option<usize> {
    let total = ETH_HEADER + IPV6_HEADER + NA_LEN;
    let out = out.get_mut(..total)?;
    headers(
        out,
        own_mac,
        to_mac,
        own,
        to,
        NEXT_ICMPV6,
        ND_HOP_LIMIT,
        NA_LEN as u16,
    );
    let icmp = &mut out[ETH_HEADER + IPV6_HEADER..];
    icmp[0] = ICMP_NA;
    icmp[1] = 0;
    icmp[2..4].copy_from_slice(&[0, 0]);
    // Solicited and override; not a router.
    icmp[4..8].copy_from_slice(&[0x60, 0, 0, 0]);
    icmp[8..24].copy_from_slice(&own.octets());
    // Option 2, target link-layer address, one 8-byte unit.
    icmp[24] = 2;
    icmp[25] = 1;
    icmp[26..32].copy_from_slice(&own_mac);
    let sum = checksum(own, to, NEXT_ICMPV6, icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(total)
}

/// An unsolicited MLDv2 report from `own` joining every group in
/// `groups`: a hop-by-hop header with the router alert, then one
/// CHANGE_TO_EXCLUDE record per group. Returns the frame's length,
/// `None` when `out` is too short.
pub fn mld_report(
    out: &mut [u8],
    own_mac: [u8; 6],
    own: &Ipv6Addr,
    groups: &[Ipv6Addr],
) -> Option<usize> {
    const HBH: usize = 8;
    let icmp_len = 8 + 20 * groups.len();
    let payload_len = u16::try_from(HBH + icmp_len).ok()?;
    let total = ETH_HEADER + IPV6_HEADER + HBH + icmp_len;
    let out = out.get_mut(..total)?;
    headers(
        out,
        own_mac,
        multicast_mac(&MLD2_ROUTERS),
        own,
        &MLD2_ROUTERS,
        NEXT_HOP_BY_HOP,
        1,
        payload_len,
    );
    let ext = &mut out[ETH_HEADER + IPV6_HEADER..];
    // Next header ICMPv6, length 0 (8 bytes), router alert (MLD), PadN 0.
    ext[..HBH].copy_from_slice(&[NEXT_ICMPV6, 0, 5, 2, 0, 0, 1, 0]);
    let icmp = &mut ext[HBH..];
    icmp[0] = ICMP_MLD2_REPORT;
    icmp[1] = 0;
    icmp[2..6].copy_from_slice(&[0, 0, 0, 0]);
    let records = u16::try_from(groups.len()).ok()?;
    icmp[6..8].copy_from_slice(&records.to_be_bytes());
    for (i, group) in groups.iter().enumerate() {
        let rec = &mut icmp[8 + 20 * i..8 + 20 * (i + 1)];
        rec[0] = MLD_CHANGE_TO_EXCLUDE;
        rec[1] = 0;
        rec[2..4].copy_from_slice(&[0, 0]);
        rec[4..20].copy_from_slice(&group.octets());
    }
    let sum = checksum(own, &MLD2_ROUTERS, NEXT_ICMPV6, icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56];
    const PEER_MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

    fn ll(s: &str) -> Ipv6Addr {
        s.parse().unwrap()
    }

    #[test]
    fn the_link_local_address_is_the_modified_eui64() {
        assert_eq!(
            link_local_from_mac(MAC),
            ll("fe80::260a:c4ff:fe12:3456"),
            "the universal/local bit flips, ff:fe goes in the middle"
        );
    }

    #[test]
    fn multicast_and_solicited_node_addresses_follow_the_rfcs() {
        let group = ll("ff12:0:d70b:fb1c:16e4:5e39:485e:31e1");
        assert_eq!(multicast_mac(&group), [0x33, 0x33, 0x48, 0x5e, 0x31, 0xe1]);
        assert_eq!(
            solicited_node(&ll("fe80::260a:c4ff:fe12:3456")),
            ll("ff02::1:ff12:3456")
        );
        assert_eq!(multicast_mac(&MLD2_ROUTERS), [0x33, 0x33, 0, 0, 0, 0x16]);
    }

    /// A UDP datagram as Linux puts it on the wire (captured shape:
    /// fe80::1 port 1234 to fe80::2 port 42671, payload "hi"), checksum
    /// worked by hand: the frame the board builds is byte-equal.
    #[test]
    fn a_udp_frame_is_byte_equal_to_the_reference() {
        let src = ll("fe80::1");
        let dst = ll("fe80::2");
        let mut out = [0u8; 128];
        let n = udp_frame(&mut out, MAC, PEER_MAC, &src, &dst, 1234, 42671, 64, b"hi").unwrap();
        // Pseudo-header sum: fe80+0001 + fe80+0002 + 000a + 0011
        // UDP: 04d2 + a6af + 000a + 0000 + 6869
        let mut want = alloc::vec::Vec::new();
        want.extend_from_slice(&PEER_MAC);
        want.extend_from_slice(&MAC);
        want.extend_from_slice(&[0x86, 0xdd, 0x60, 0, 0, 0, 0, 10, 17, 64]);
        want.extend_from_slice(&src.octets());
        want.extend_from_slice(&dst.octets());
        want.extend_from_slice(&[0x04, 0xd2, 0xa6, 0xaf, 0, 10]);
        let sum: u32 = 0xfe80 + 1 + 0xfe80 + 2 + 10 + 17 + 0x04d2 + 0xa6af + 10 + 0x6869;
        let folded = (sum & 0xffff) + (sum >> 16);
        let check = !(folded as u16);
        want.extend_from_slice(&check.to_be_bytes());
        want.extend_from_slice(b"hi");
        assert_eq!(&out[..n], &want[..]);
    }

    #[test]
    fn a_udp_frame_round_trips_and_a_flipped_bit_is_refused() {
        let src = link_local_from_mac(PEER_MAC);
        let dst = ll("ff12:0:d70b:fb1c:16e4:5e39:485e:31e1");
        let mut out = [0u8; 256];
        let payload = [7u8; 42];
        let n = udp_frame(
            &mut out,
            PEER_MAC,
            multicast_mac(&dst),
            &src,
            &dst,
            40000,
            29716,
            1,
            &payload,
        )
        .unwrap();
        assert_eq!(
            parse(&out[..n]),
            Inbound::Udp {
                src_mac: PEER_MAC,
                src,
                dst,
                src_port: 40000,
                dst_port: 29716,
                payload: &payload,
            }
        );
        // Ethernet padding after the datagram is not part of it.
        let mut padded = alloc::vec::Vec::from(&out[..n]);
        padded.extend_from_slice(&[0; 6]);
        assert!(matches!(parse(&padded), Inbound::Udp { payload: p, .. } if p == payload));
        let mut bad = out;
        bad[n - 1] ^= 1;
        assert_eq!(parse(&bad[..n]), Inbound::Other);
        // A zero UDP checksum is invalid over IPv6.
        let mut zero = out;
        zero[ETH_HEADER + IPV6_HEADER + 6] = 0;
        zero[ETH_HEADER + IPV6_HEADER + 7] = 0;
        assert_eq!(parse(&zero[..n]), Inbound::Other);
        // Truncated frames and IPv4 are not taken.
        assert_eq!(parse(&out[..n - 1]), Inbound::Other);
        let mut v4 = out;
        v4[12..14].copy_from_slice(&[0x08, 0x00]);
        assert_eq!(parse(&v4[..n]), Inbound::Other);
    }

    /// A solicitation as Linux sends it before its first datagram to the
    /// board, and the advertisement the board answers with: its
    /// checksum verifies and it names the board's MAC.
    #[test]
    fn a_solicitation_is_answered_with_the_boards_mac() {
        let own = link_local_from_mac(MAC);
        let asker = link_local_from_mac(PEER_MAC);
        let group = solicited_node(&own);
        let mut ns = [0u8; 86];
        let total = ETH_HEADER + IPV6_HEADER + 32;
        headers(
            &mut ns,
            PEER_MAC,
            multicast_mac(&group),
            &asker,
            &group,
            NEXT_ICMPV6,
            255,
            32,
        );
        {
            let icmp = &mut ns[ETH_HEADER + IPV6_HEADER..total];
            icmp[0] = ICMP_NS;
            icmp[8..24].copy_from_slice(&own.octets());
            icmp[24] = 1;
            icmp[25] = 1;
            icmp[26..32].copy_from_slice(&PEER_MAC);
            let sum = checksum(&asker, &group, NEXT_ICMPV6, icmp);
            icmp[2..4].copy_from_slice(&sum.to_be_bytes());
        }
        assert_eq!(
            parse(&ns[..total]),
            Inbound::NeighbourSolicit {
                src_mac: PEER_MAC,
                src: asker,
                target: own,
            }
        );
        // A hop limit below 255 means it crossed a router: refused.
        let mut routed = ns;
        routed[ETH_HEADER + 7] = 254;
        assert_eq!(parse(&routed[..total]), Inbound::Other);

        let mut na = [0u8; 128];
        let n = neighbour_advert(&mut na, MAC, &own, PEER_MAC, &asker).unwrap();
        assert_eq!(n, ETH_HEADER + IPV6_HEADER + NA_LEN);
        assert_eq!(&na[0..6], &PEER_MAC);
        assert_eq!(&na[6..12], &MAC);
        assert_eq!(na[ETH_HEADER + 7], 255, "ND hop limit");
        let icmp = &na[ETH_HEADER + IPV6_HEADER..n];
        assert_eq!(icmp[0], ICMP_NA);
        assert_eq!(icmp[4], 0x60, "solicited and override");
        assert_eq!(&icmp[8..24], &own.octets());
        assert_eq!(&icmp[24..26], &[2, 1]);
        assert_eq!(&icmp[26..32], &MAC);
        assert_eq!(checksum(&own, &asker, NEXT_ICMPV6, icmp), 0);
    }

    #[test]
    fn duplicate_address_detection_is_not_answered() {
        let own = link_local_from_mac(MAC);
        let group = solicited_node(&own);
        let mut ns = [0u8; 78];
        headers(
            &mut ns,
            PEER_MAC,
            multicast_mac(&group),
            &Ipv6Addr::UNSPECIFIED,
            &group,
            NEXT_ICMPV6,
            255,
            24,
        );
        let icmp = &mut ns[ETH_HEADER + IPV6_HEADER..];
        icmp[0] = ICMP_NS;
        icmp[8..24].copy_from_slice(&own.octets());
        let sum = checksum(&Ipv6Addr::UNSPECIFIED, &group, NEXT_ICMPV6, icmp);
        icmp[2..4].copy_from_slice(&sum.to_be_bytes());
        assert_eq!(parse(&ns), Inbound::Other);
    }

    #[test]
    fn the_mld_report_joins_each_group_with_the_router_alert() {
        let own = link_local_from_mac(MAC);
        let groups = [
            ll("ff12:0:d70b:fb1c:16e4:5e39:485e:31e1"),
            solicited_node(&own),
        ];
        let mut out = [0u8; 256];
        let n = mld_report(&mut out, MAC, &own, &groups).unwrap();
        assert_eq!(n, ETH_HEADER + IPV6_HEADER + 8 + 8 + 40);
        assert_eq!(&out[0..6], &[0x33, 0x33, 0, 0, 0, 0x16]);
        let ip = &out[ETH_HEADER..ETH_HEADER + IPV6_HEADER];
        assert_eq!(ip[6], NEXT_HOP_BY_HOP);
        assert_eq!(ip[7], 1, "link-local hop limit");
        assert_eq!(u16::from_be_bytes([ip[4], ip[5]]), 56);
        let ext = &out[ETH_HEADER + IPV6_HEADER..n];
        assert_eq!(&ext[..8], &[58, 0, 5, 2, 0, 0, 1, 0]);
        let icmp = &ext[8..];
        assert_eq!(icmp[0], 143);
        assert_eq!(u16::from_be_bytes([icmp[6], icmp[7]]), 2);
        assert_eq!(icmp[8], MLD_CHANGE_TO_EXCLUDE);
        assert_eq!(&icmp[12..28], &groups[0].octets());
        assert_eq!(&icmp[32..48], &groups[1].octets());
        assert_eq!(checksum(&own, &MLD2_ROUTERS, NEXT_ICMPV6, icmp), 0);
    }

    #[test]
    fn a_short_buffer_is_refused_not_overrun() {
        let a = ll("fe80::1");
        let mut out = [0u8; UDP_OVERHEAD + 1];
        assert!(udp_frame(&mut out, MAC, MAC, &a, &a, 1, 2, 1, b"xx").is_none());
        let mut short_na = [0u8; ETH_HEADER + IPV6_HEADER + NA_LEN - 1];
        assert!(neighbour_advert(&mut short_na, MAC, &a, MAC, &a).is_none());
        assert!(mld_report(&mut out, MAC, &a, &[a, a]).is_none());
    }
}
