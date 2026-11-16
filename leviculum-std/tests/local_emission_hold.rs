//! A local client's hops-0 path is kept from the unresponsive mark only
//! while the emission it installed is young (reviewer decision 2026-11-16,
//! after orders 1881 to 1914), and only for an emission whose first receipt
//! at this daemon was that client's hops-0 copy: at most
//! `LOCAL_EMISSION_HOLD_MS` after that first install, and never past the
//! installing client's disconnect. A local hops-0 copy of an emission the
//! daemon already held (from the network, or from a client before) earns
//! nothing. Otherwise Python's rule applies: an expired link request marks
//! the path, and a relayed copy of the same emission takes it
//! (`RNS/Transport.py:746-753`, `:1819-1824`).
//!
//! The field case (miauhaus, 2026-11-15) and every attack of orders 1898 to
//! 1914 run here through the public API. A replay of an emission the
//! network brought first is marked at the first dead request; a capture
//! that reaches a fresh daemon before the network does is that emission's
//! first receipt and is held at most the hold, whatever the replayer
//! answers or lets die. These tests do not bound the residuals the protocol
//! note names (`docs/src/protocol-notes/announce-dedup-and-path-replacement.md`):
//! a replayer racing the network with each fresh emission, an lnsd
//! restart, a full record table, re-replay flapping after the hold, and a
//! reused interface index whose teardown the driver did not report.

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

/// For a replay of an emission this daemon heard from the network first:
/// `attack` runs once, then one network request dies, and that first dead
/// request must mark the path. Such a replay earns no hold, because only an
/// emission whose first receipt here was the local client's hops-0 copy
/// does (`Transport::mark_path_unresponsive`).
fn first_dead_request_marks(
    t: &mut T,
    clock: &TestClock,
    hash: [u8; TRUNCATED_HASHBYTES],
    attack: impl FnOnce(&mut T, &TestClock),
) {
    attack(t, clock);
    expire_unproven(t, clock, [0xD0; TRUNCATED_HASHBYTES], hash);
    assert!(
        t.path_is_unresponsive(&hash),
        "a replay of a network-first emission is held for no time at all"
    );
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
/// emission it already heard at hops 0. The network heard it first, so the
/// replay earns no hold: the first dead request marks the path (audit of
/// 1922; the name is kept from the hold it was once bounded by).
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

    first_dead_request_marks(&mut t, &clock, hash, |_, _| {});
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
/// destination, whose signed proofs come back over the client. The network
/// heard the emission first, so the first dead request marks the path.
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

    first_dead_request_marks(&mut t, &clock, hash, |t, clock| {
        prove_over_client(t, clock, [0x50; TRUNCATED_HASHBYTES], &dest);
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
/// The replay earns no hold, so the first dead request marks the path.
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

    first_dead_request_marks(&mut t, &clock, hash, |t, clock| {
        prove_over_client_from(t, clock, [0x80; TRUNCATED_HASHBYTES], &dest, OWN_TCP);
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

/// Audit of 1922, first auditor (verbatim): a replayer that passes on every
/// new emission the network brings, each at hops 0, must not hold the path
/// past the hold.
#[test]
fn audit_1922_rereplaying_each_new_network_emission_is_held_at_most_the_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    let first_capture_at = clock.now_ms();
    let mut checked_after_hold = 0u32;
    for round in 0..4u8 {
        let emitted_s = 9000 + u64::from(round) * 3000;
        let nonce = 0x90 + round;
        t.storage_mut().clear_packet_hashes();
        clock.advance(20_000);
        let _ = t.process_incoming(NETWORK, &announce_emission(&dest, 1, nonce, emitted_s));
        t.drain_actions();
        assert_eq!(
            path_of(&t, &hash).0,
            NETWORK,
            "round {round}: the network's newer emission installs"
        );
        t.storage_mut().clear_packet_hashes();
        clock.advance(20_000);
        let _ = t.process_incoming(LOCAL, &announce_emission(&dest, 0, nonce, emitted_s));
        t.drain_actions();
        expire_unproven(&mut t, &clock, [0xC0 + round; TRUNCATED_HASHBYTES], hash);
        if clock.now_ms() - first_capture_at >= HOLD_MS {
            assert!(t.path_is_unresponsive(&hash),
                "round {round}: {} ms after the first capture, a replayer passing on the destination's new emissions must not hold the path past the hold",
                clock.now_ms() - first_capture_at);
            checked_after_hold += 1;
        }
        clock.advance(50 * 60 * 1000);
    }
    assert!(checked_after_hold >= 2, "the loop must run past the hold");
}

/// Audit of 1922: a hold does not survive a disconnect of the client that
/// installed the emission. The client goes and comes back on the same
/// interface index inside the hold, and announces nothing: its path is
/// markable at once.
#[test]
fn audit_1922_a_hold_does_not_survive_the_installing_clients_disconnect() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    clock.advance(HOLD_MS / 4);
    t.set_local_client(LOCAL, false);
    clock.advance(1_000);
    t.set_local_client(LOCAL, true);
    assert_eq!(path_of(&t, &hash), (LOCAL, 0), "the path stays on the box");
    assert!(
        t.mark_path_unresponsive(&hash),
        "the hold ended with the installing client's connection"
    );
}

/// A client that installed its own emission and then replays a newer
/// emission the network brought first does not carry its older hold over
/// to the replay: the record names the older emission, the path holds the
/// newer one, and the path is markable at once.
#[test]
fn a_replay_of_a_newer_network_emission_does_not_inherit_the_clients_older_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();

    let newer = 9600;
    relayed_copy_arrives(&mut t, &clock, &announce_emission(&dest, 1, 0x7A, newer));
    assert_eq!(path_of(&t, &hash), (NETWORK, 2), "a newer emission wins");
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(LOCAL, &announce_emission(&dest, 0, 0x7A, newer));
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0), "the replay takes the path");
    assert_eq!(
        t.storage().local_emission_installed(&hash).map(|r| r.0),
        Some(9000),
        "the record still names the client's own, older emission"
    );
    assert!(
        t.mark_path_unresponsive(&hash),
        "the replayed newer emission holds nothing"
    );
}
