//! Audit of 1922, second auditor: a local process that replays, at hops 0,
//! every fresh emission the network brings of a destination it does not
//! own. Each emission was heard first from the network, so none earns a
//! hold (`Transport::mark_path_unresponsive`): the replayer may not keep the
//! path from the unresponsive mark for longer than one hold in all.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use support::{
    announce_emission, destination, expire_unproven, path_of, transport, LOCAL, NETWORK,
};

/// `leviculum_core::transport::LOCAL_EMISSION_HOLD_MS`, written out.
const HOLD_MS: u64 = 60 * 60 * 1000;

#[test]
fn a_local_replayer_of_every_fresh_network_emission_is_held_at_most_one_hold() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    const ROUNDS: u8 = 9;
    const ROUND_MS: u64 = 20 * 60 * 1000;
    let mut held_rounds = 0u8;
    for round in 0..ROUNDS {
        // The destination announces anew; the network brings it first.
        let emitted_s = 9000 + u64::from(round) * (ROUND_MS / 1000);
        let nonce = 0xA0 + round;
        t.storage_mut().clear_packet_hashes();
        clock.advance(20_000);
        let _ = t.process_incoming(NETWORK, &announce_emission(&dest, 1, nonce, emitted_s));
        t.drain_actions();
        assert_eq!(
            path_of(&t, &hash).0,
            NETWORK,
            "round {round}: the fresh emission installs from the network"
        );

        // The local process replays it at hops 0 and takes the path.
        t.storage_mut().clear_packet_hashes();
        clock.advance(20_000);
        let _ = t.process_incoming(LOCAL, &announce_emission(&dest, 0, nonce, emitted_s));
        t.drain_actions();
        assert_eq!(path_of(&t, &hash), (LOCAL, 0), "round {round}: the replay");

        // A network request toward it dies.
        expire_unproven(&mut t, &clock, [0xB0 + round; TRUNCATED_HASHBYTES], hash);
        if !t.path_is_unresponsive(&hash) {
            held_rounds += 1;
        }
        clock.advance(ROUND_MS - 40_000);
    }
    let held_ms = u64::from(held_rounds) * ROUND_MS;
    assert!(
        held_ms <= HOLD_MS,
        "{held_rounds} of {ROUNDS} rounds ({held_ms_min} min) held the replayed path from the mark",
        held_ms_min = held_ms / 60_000
    );
}
