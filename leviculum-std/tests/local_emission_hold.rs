//! A local client's hops-0 path is kept from the unresponsive mark only
//! while the emission it installed is young (reviewer decision 2026-11-16,
//! after orders 1881 to 1914): at most `LOCAL_EMISSION_HOLD_MS` after this
//! daemon first installed that emission from a local-client interface.
//! After that, or once the client is gone, Python's rule applies: an expired
//! link request marks the path, and a relayed copy of the same emission
//! takes it (`RNS/Transport.py:746-753`, `:1819-1824`).
//!
//! The field case (miauhaus, 2026-11-15) and every attack of orders 1898 to
//! 1914 run here through the public API. The attacks no longer fail on who
//! proved what: a replaying process cannot make a new emission without the
//! destination's private key, so whatever it does, the path it captured is
//! held for at most the hold.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::storage_types::LinkEntry;
use leviculum_core::traits::Storage;
use leviculum_core::Clock;
use support::{
    announce, announce_emission, destination, expire_unproven, path_of, prove_over_client,
    prove_over_client_from, transport, TestClock, LOCAL, NETWORK, SIBLING, T,
};

/// `leviculum_core::transport::LOCAL_EMISSION_HOLD_MS`, written out so this
/// file states the bound it tests; the core unit test
/// `local_emission_hold_is_an_hour` pins the constant to the same value.
const HOLD_MS: u64 = 60 * 60 * 1000;

/// A connection into lnsd that is not a local-client interface: a TCP, LAN
/// or backbone link the client process also holds (orders 1914, 1914b).
const OWN_TCP: usize = 3;

/// The network's relayed copy of `dest`'s emission arrives one hop out
/// (hops 2 once received), past the announce rate limit.
fn relayed_copy_arrives(t: &mut T, clock: &TestClock, copy: &[u8]) {
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(NETWORK, copy);
    t.drain_actions();
}

/// Run `round` until the clock has passed `HOLD_MS` since `installed_at`.
/// Each round lets one network request die. While the emission is young the
/// path must stay unmarked; the first dead request after the hold must mark
/// it. Returns the number of rounds that ran inside the hold.
fn held_at_most_the_hold(
    t: &mut T,
    clock: &TestClock,
    hash: [u8; TRUNCATED_HASHBYTES],
    installed_at: u64,
    mut round: impl FnMut(&mut T, &TestClock, u8),
) -> u8 {
    let mut i: u8 = 0;
    loop {
        round(t, clock, i);
        expire_unproven(t, clock, [0xE0 ^ i; TRUNCATED_HASHBYTES], hash);
        let age = clock.now_ms() - installed_at;
        if age < HOLD_MS {
            assert!(
                !t.path_is_unresponsive(&hash),
                "round {i}, emission {age} ms old: kept inside the hold"
            );
        } else {
            assert!(
                t.path_is_unresponsive(&hash),
                "round {i}, emission {age} ms old: the first dead request after the hold marks the path"
            );
            return i;
        }
        // Ten minutes a round, so the hold ends after a handful of rounds.
        clock.advance(10 * 60 * 1000);
        i = i.wrapping_add(1);
    }
}

/// (a) The field case: a hops-0 path of a local client, a dead one-hop
/// request, a relayed copy of the same emission. The path stays on the box
/// while the emission is young.
#[test]
fn the_field_case_keeps_the_path_while_the_emission_is_young() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    for i in 0..3u8 {
        expire_unproven(&mut t, &clock, [0x10 + i; TRUNCATED_HASHBYTES], hash);
    }
    assert!(!t.path_is_unresponsive(&hash));
    relayed_copy_arrives(&mut t, &clock, &announce(&dest, 1));
    assert_eq!(
        path_of(&t, &hash),
        (LOCAL, 0),
        "a young emission's path stays on the box"
    );
}

/// (b) The same after the hold: Python's repair happens.
#[test]
fn after_the_hold_a_dead_request_marks_the_path_and_the_relayed_copy_takes_it() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();

    clock.advance(HOLD_MS);
    expire_unproven(&mut t, &clock, [0x20; TRUNCATED_HASHBYTES], hash);
    assert!(
        t.path_is_unresponsive(&hash),
        "an old emission's path is marked like any other"
    );
    relayed_copy_arrives(&mut t, &clock, &announce(&dest, 1));
    assert_eq!(path_of(&t, &hash), (NETWORK, 2), "Python's repair");
}

/// The genuine owner renews its exemption with every real announce: a newer
/// emission starts a new hold.
#[test]
fn a_newer_emission_from_the_client_starts_a_new_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();

    clock.advance(HOLD_MS / 2);
    t.storage_mut().clear_packet_hashes();
    t.process_incoming(LOCAL, &announce_emission(&dest, 0, 0x78, 9100))
        .unwrap();
    t.drain_actions();
    let renewed_at = clock.now_ms();

    // Past the first emission's hold, inside the second's.
    clock.advance(HOLD_MS / 2 + 1);
    expire_unproven(&mut t, &clock, [0x30; TRUNCATED_HASHBYTES], hash);
    assert!(
        !t.path_is_unresponsive(&hash),
        "the newer emission's hold covers it"
    );
    clock.advance(HOLD_MS - (clock.now_ms() - renewed_at));
    expire_unproven(&mut t, &clock, [0x31; TRUNCATED_HASHBYTES], hash);
    assert!(t.path_is_unresponsive(&hash), "and ends with it");
}

/// A disconnected client's path is markable at once, young or not.
#[test]
fn a_disconnected_clients_path_is_markable_at_once() {
    let (mut t, _clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    t.set_local_client(LOCAL, false);
    assert!(t.storage().has_path(&hash), "the disconnect keeps the path");
    assert!(t.mark_path_unresponsive(&hash));
}

/// (d) A newer emission from the network beats a replayed older one the
/// client holds locally, inside the hold.
#[test]
fn a_newer_network_emission_beats_a_replayed_older_one_held_locally() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    relayed_copy_arrives(&mut t, &clock, &announce_emission(&dest, 1, 0x79, 9200));
    assert_eq!(path_of(&t, &hash), (NETWORK, 2), "a newer emission wins");
}

// (c) Every attack of orders 1898 to 1914 holds a foreign path at most the
// hold. Each one plays its attack in every round, while network requests
// die, and must lose the path at the first dead request after the hold.

/// Order 1898 (audit of 1881): the client replays a network destination's
/// emission it already heard at hops 0.
#[test]
fn attack_1898_a_replay_of_a_network_destination_is_held_at_most_the_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    let network = announce(&dest, 1);
    t.process_incoming(NETWORK, &network).unwrap();
    t.drain_actions();
    clock.advance(12_000);
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));
    let at = clock.now_ms();

    held_at_most_the_hold(&mut t, &clock, hash, at, |_, _, _| {});
    relayed_copy_arrives(&mut t, &clock, &network);
    assert_eq!(
        path_of(&t, &hash),
        (NETWORK, 2),
        "the network takes it back"
    );
}

/// Order 1905, attack 1: the replay is the first copy a fresh daemon hears.
#[test]
fn attack_1905_a_first_installed_replay_is_held_at_most_the_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    let at = clock.now_ms();

    held_at_most_the_hold(&mut t, &clock, hash, at, |_, _, _| {});
    relayed_copy_arrives(&mut t, &clock, &announce(&dest, 1));
    assert_eq!(path_of(&t, &hash), (NETWORK, 2));
}

/// Order 1905, attack 2: the client relays link requests to the real
/// destination, whose signed proofs come back over the client.
#[test]
fn attack_1905_relayed_proofs_hold_at_most_the_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(NETWORK, &announce(&dest, 1)).unwrap();
    t.drain_actions();
    clock.advance(12_000);
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    let at = clock.now_ms();

    held_at_most_the_hold(&mut t, &clock, hash, at, |t, clock, i| {
        prove_over_client(t, clock, [0x50 ^ i; TRUNCATED_HASHBYTES], &dest);
    });
}

/// Order 1909: the client proves only probes it started itself, over its
/// own interface and over a sibling local-client connection.
#[test]
fn attack_1909_self_proven_probes_hold_at_most_the_hold() {
    let (mut t, clock) = transport();
    t.set_local_client(SIBLING, true);
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    let at = clock.now_ms();

    held_at_most_the_hold(&mut t, &clock, hash, at, |t, clock, i| {
        prove_over_client_from(t, clock, [0x60 ^ i; TRUNCATED_HASHBYTES], &dest, LOCAL);
        prove_over_client_from(t, clock, [0x68 ^ i; TRUNCATED_HASHBYTES], &dest, SIBLING);
    });
}

/// Orders 1914 and 1914b: the client also holds a TCP connection into lnsd.
/// Each round it lets one of its own requests on that connection die and
/// proves another probe it started there.
#[test]
fn attack_1914_own_dead_probes_and_proven_probes_on_its_own_connection_hold_at_most_the_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    let at = clock.now_ms();

    held_at_most_the_hold(&mut t, &clock, hash, at, |t, clock, i| {
        expire_own(t, clock, [0x70 ^ i; TRUNCATED_HASHBYTES], hash, OWN_TCP);
        prove_over_client_from(t, clock, [0x78 ^ i; TRUNCATED_HASHBYTES], &dest, OWN_TCP);
    });
}

/// Order 1914, second condition: the network heard the destination first, the
/// client replays it and proves probes that entered over its own connection.
#[test]
fn attack_1914_a_replayed_network_destination_proven_over_its_own_connection_is_held_at_most_the_hold(
) {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(NETWORK, &announce(&dest, 1)).unwrap();
    t.drain_actions();
    clock.advance(12_000);
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    let at = clock.now_ms();

    held_at_most_the_hold(&mut t, &clock, hash, at, |t, clock, i| {
        prove_over_client_from(t, clock, [0x80 ^ i; TRUNCATED_HASHBYTES], &dest, OWN_TCP);
    });
}

/// Order 1905, second auditor: handing in the same emission again restarts
/// nothing, and neither does a re-replay of the capture after the client
/// lost the path.
#[test]
fn attack_1905_rehanding_or_rereplaying_the_same_emission_restarts_nothing() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    let capture = announce(&dest, 0);
    t.process_incoming(LOCAL, &capture).unwrap();
    t.drain_actions();
    let at = clock.now_ms();

    held_at_most_the_hold(&mut t, &clock, hash, at, |t, clock, _| {
        t.storage_mut().clear_packet_hashes();
        clock.advance(20_000);
        let _ = t.process_incoming(LOCAL, &capture);
        t.drain_actions();
    });
    relayed_copy_arrives(&mut t, &clock, &announce(&dest, 1));
    assert_eq!(path_of(&t, &hash), (NETWORK, 2));

    // The same capture again: it may take the path back at hops 0 (a
    // fewer-hops copy of the emission, Codeberg #231), but it brings no new
    // hold, so the path is markable at once.
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(LOCAL, &capture);
    t.drain_actions();
    assert!(
        t.mark_path_unresponsive(&hash),
        "a re-replayed capture holds nothing"
    );
}

/// Order 1905 (reused index): a client that disconnects and reconnects
/// under the same interface index and hands the same emission in again gets
/// no new hold.
#[test]
fn attack_1905_a_reconnect_under_a_reused_index_brings_no_new_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    let capture = announce(&dest, 0);
    t.process_incoming(LOCAL, &capture).unwrap();
    t.drain_actions();

    clock.advance(HOLD_MS);
    t.set_local_client(LOCAL, false);
    t.set_local_client(LOCAL, true);
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(LOCAL, &capture);
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));
    assert!(
        t.mark_path_unresponsive(&hash),
        "the reconnected client's old emission holds nothing"
    );
}

/// A request the client itself started over `initiator` (a non-local
/// connection) toward its own path, which it lets die.
fn expire_own(
    t: &mut T,
    clock: &TestClock,
    link_id: [u8; TRUNCATED_HASHBYTES],
    dest: [u8; TRUNCATED_HASHBYTES],
    initiator: usize,
) {
    let now = clock.now_ms();
    t.storage_mut().set_link_entry(
        link_id,
        LinkEntry {
            timestamp_ms: now,
            next_hop_interface_index: LOCAL,
            remaining_hops: 0,
            received_interface_index: initiator,
            hops: 1,
            validated: false,
            proof_timeout_ms: now + 1000,
            destination_hash: dest,
            peer_signing_key: None,
            received_peer: None,
            next_hop_peer: None,
        },
    );
    clock.advance(1001);
    t.poll();
    t.drain_actions();
    assert!(!t.storage().has_link_entry(&link_id));
}
