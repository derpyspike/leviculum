//! Audit of order 1898, round-1 attack 1 (order 1905): a local process
//! hands in a captured announce of a destination it does not host, after an
//! lnsd restart (or a path expiry, or an eviction), before the network's
//! copy of that emission arrives. It installs first, so it becomes the
//! owner. The hold must end after `LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT`
//! consecutive unproven link requests, after which the network's copy takes
//! the path back.
//!
//! Written from the audit's description; the verbatim text of
//! round1-verdict.md was not reachable from the coder host.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::traits::Storage;
use leviculum_core::transport::LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
use support::{announce, destination, expire_unproven, path_of, transport, LOCAL, NETWORK};

#[test]
fn a_first_installed_replay_does_not_pin_a_foreign_path() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();

    // A fresh daemon: the replay is the first copy it remembers.
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));
    assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

    // The network's copy of the same emission is refused at more hops.
    clock.advance(12_000);
    let network = announce(&dest, 1);
    let _ = t.process_incoming(NETWORK, &network);
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    // Every link request toward the replaying client dies unproven.
    for i in 0..LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT {
        expire_unproven(&mut t, &clock, [0x40 + i; TRUNCATED_HASHBYTES], hash);
    }
    assert!(
        t.path_is_unresponsive(&hash),
        "after the bound the replayed path is marked like any other"
    );

    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    t.process_incoming(NETWORK, &network).unwrap();
    t.drain_actions();
    assert_eq!(
        path_of(&t, &hash),
        (NETWORK, 2),
        "the network's copy takes the path back"
    );
}
