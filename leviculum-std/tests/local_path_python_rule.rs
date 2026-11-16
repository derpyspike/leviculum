//! A local client's path follows Python's rule, with no exemption
//! (reviewer decision 2026-11-16, ending orders 1881 to 1931): an expired
//! link request from a one-hop initiator marks the path unresponsive
//! whatever interface it sits on (`RNS/Transport.py:746-753`), and a
//! relayed copy of the same emission then takes it (`:1819-1824`). The
//! transport holds no ownership record, no emission-install table and no
//! hold timer; `Transport::mark_path_unresponsive` marks every path it is
//! given.
//!
//! What this buys back: a local process that replays its neighbour's
//! announces at hops 0 gains nothing beyond what Python grants it — the
//! replay installs by the same-emission fewer-hops clause, and the first
//! dead request takes it away again. The field case the exemptions were
//! built for (miauhaus, 2026-11-15: a mailbox's hops-0 path left the box
//! for two hours) is now repaired by its owner instead: the client watches
//! where its own path points and re-announces when it is wrong
//! (`leviculum-std/src/path_self_repair.rs`), which only the holder of the
//! destination's key can do.
//!
//! Of the attack tests of orders 1898 to 1931 this file keeps the one
//! statement that still says something: a replayer holds nothing. The
//! rest bounded the hold's timing (held at most the hold, a disconnect
//! ends it, a reconnect restarts nothing, a fresh emission starts no new
//! hour) and died with the hold itself.

#[path = "support/path_ownership.rs"]
mod support;

use support::{announce, destination, expire_unproven, path_of, transport, LOCAL, NETWORK};

/// Python's rule on the field case: the dead one-hop request marks the
/// local client's hops-0 path, and the next relayed copy of the same
/// emission takes it. This is the cost the reviewer decision accepts in
/// the transport; the owner's self-repair announces it back.
#[test]
fn a_dead_request_marks_a_local_clients_path_and_the_relay_takes_it() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(
        path_of(&t, &hash),
        (LOCAL, 0),
        "the client's announce installs at hops 0"
    );

    expire_unproven(&mut t, &clock, [0x5A; 16], hash);
    assert!(
        t.path_is_unresponsive(&hash),
        "Python's rule: the first dead one-hop request marks the path, local client or not"
    );

    // The relayed copy of the same emission arrives two hops out and takes
    // the unresponsive path (unresponsive_same_emission).
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    t.process_incoming(NETWORK, &announce(&dest, 2)).unwrap();
    t.drain_actions();
    assert_eq!(
        path_of(&t, &hash),
        (NETWORK, 3),
        "the relayed same-emission copy takes the marked path"
    );
}

/// A local replayer gains nothing beyond Python's behaviour: its hops-0
/// copy of a network-first emission installs by the same-emission
/// fewer-hops clause, and the first dead request takes it away again. No
/// record, no hold, no round survives in the transport.
#[test]
fn a_replaying_local_client_holds_nothing() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(NETWORK, &announce(&dest, 1)).unwrap();
    t.drain_actions();
    assert_eq!(
        path_of(&t, &hash),
        (NETWORK, 2),
        "the network brings the emission first"
    );

    // The replay: a local process hands the same emission back in at hops 0.
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(
        path_of(&t, &hash),
        (LOCAL, 0),
        "the replay installs by the same-emission fewer-hops clause, as in Python"
    );

    expire_unproven(&mut t, &clock, [0x5B; 16], hash);
    assert!(
        t.path_is_unresponsive(&hash),
        "the first dead request marks the replayer's path: it held nothing"
    );

    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    t.process_incoming(NETWORK, &announce(&dest, 2)).unwrap();
    t.drain_actions();
    assert_eq!(
        path_of(&t, &hash),
        (NETWORK, 3),
        "the relayed copy takes the path back off the replayer"
    );
}
