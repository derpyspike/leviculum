//! Audit of 1922, second auditor, positive control: the genuine owner's
//! emission reaches this daemon first over the local client, and the
//! network's echo of the same emission comes back afterwards. The hold
//! belongs to the owner and survives the echo: inside the hold a dead
//! request does not mark the path, and the echo does not take it.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use support::{announce, destination, expire_unproven, path_of, transport, LOCAL, NETWORK};

#[test]
fn a_genuine_owners_emission_heard_first_locally_keeps_its_hold_through_the_echo() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    // The network echoes the owner's emission back, one hop out.
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(NETWORK, &announce(&dest, 1));
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0), "the echo takes nothing");

    // The owner hands the same emission in again.
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(LOCAL, &announce(&dest, 0));
    t.drain_actions();

    for i in 0..3u8 {
        expire_unproven(&mut t, &clock, [0x40 + i; TRUNCATED_HASHBYTES], hash);
        assert!(
            !t.path_is_unresponsive(&hash),
            "dead request {i}: the owner's young emission keeps its hold"
        );
    }
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(NETWORK, &announce(&dest, 1));
    t.drain_actions();
    assert_eq!(
        path_of(&t, &hash),
        (LOCAL, 0),
        "the path stays on the box inside the hold"
    );
}
