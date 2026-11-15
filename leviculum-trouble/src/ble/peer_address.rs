//! Translate HCI's address kinds without treating public high bits as privacy.

use leviculum_ble_tx::{addr_value, AddrType, PeerAddr};
use trouble_host::prelude::Address;

pub(crate) fn peer_addr(addr: &Address) -> PeerAddr {
    let value = addr_value(&addr.addr.into_inner());
    // HCI's random kind alone needs its high bits decoded. Resolved
    // identity kinds describe fixed identities, not the current RPA.
    let addr_type = match addr.kind.into_inner() {
        0 | 2 => AddrType::Public,
        1 => match value >> 46 {
            0b11 => AddrType::RandomStatic,
            0b01 => AddrType::RandomPrivateResolvable,
            _ => AddrType::RandomPrivateNonResolvable,
        },
        3 => AddrType::RandomStatic,
        0xFF => AddrType::Anonymous,
        other => AddrType::Other(other),
    };
    PeerAddr { value, addr_type }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trouble_host::prelude::{AddrKind, BdAddr};

    #[test]
    fn public_high_bits_do_not_turn_a_fixed_peer_into_an_rpa() {
        let bytes = [1, 2, 3, 4, 5, 0x47];
        let public = peer_addr(&Address::new(AddrKind::PUBLIC, BdAddr::new(bytes)));
        let random = peer_addr(&Address::new(AddrKind::RANDOM, BdAddr::new(bytes)));
        assert_eq!(public.value, random.value);
        assert_eq!(public.addr_type, AddrType::Public);
        assert!(!public.rotating());
        assert_eq!(random.addr_type, AddrType::RandomPrivateResolvable);
        assert!(random.rotating());
    }

    #[test]
    fn random_static_and_nonresolvable_addresses_keep_their_actual_class() {
        for (high, expected) in [
            (0xC7, AddrType::RandomStatic),
            (0x07, AddrType::RandomPrivateNonResolvable),
        ] {
            let peer = peer_addr(&Address::new(
                AddrKind::RANDOM,
                BdAddr::new([1, 2, 3, 4, 5, high]),
            ));
            assert_eq!(peer.addr_type, expected);
        }
    }
}
