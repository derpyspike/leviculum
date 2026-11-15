//! Audit of order 1898, round-1 attack 2 (order 1905): a local client that
//! does not host a destination takes its path with a hops-0 replay and
//! relays one link request to the real destination, whose signed proof
//! comes back over the client's interface. A proof shows that the
//! destination answered, not that the client hosts it. Once the client stops
//! relaying, the hold must end after `LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT`
//! consecutive unproven link requests.
//!
//! Written from the audit's description; the verbatim text of
//! round1-verdict.md was not reachable from the coder host.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::traits::Storage;
use leviculum_core::transport::LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
use support::{
    announce, destination, expire_unproven, path_of, prove_over_client, transport, LOCAL, NETWORK,
};

#[test]
fn a_relayed_proof_does_not_pin_a_foreign_path() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();

    let network = announce(&dest, 1);
    t.process_incoming(NETWORK, &network).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (NETWORK, 2));

    clock.advance(12_000);
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    prove_over_client(&mut t, &clock, [0x50; TRUNCATED_HASHBYTES], &dest);
    assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

    for i in 0..LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT {
        expire_unproven(&mut t, &clock, [0x51 + i; TRUNCATED_HASHBYTES], hash);
    }
    assert!(
        t.path_is_unresponsive(&hash),
        "after the bound a proof-gained hold ends too"
    );

    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    t.process_incoming(NETWORK, &network).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (NETWORK, 2));
}
