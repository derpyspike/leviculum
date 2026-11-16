//! Audit of order 1905, second auditor, round 1 (order 1909): a local
//! client that owns a path answers only link requests it started itself
//! (or a sibling local-client connection started), while every request
//! from the network dies. Before order 1909 each such self-proven probe
//! reset the unproven-expiry count, so the client held the path without
//! limit. A proof resets the count only for a link started outside the
//! local clients; the network's dead requests must reach the bound.
//!
//! Written from the audit's description; the verbatim text of the round-1
//! verdict was not reachable from the coder host.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::traits::Storage;
use leviculum_core::transport::LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
use support::{
    announce, destination, expire_unproven, prove_over_client_from, transport, LOCAL, SIBLING,
};

#[test]
fn a_client_proving_only_its_own_probes_loses_the_path_to_dead_network_requests() {
    for prober in [LOCAL, SIBLING] {
        let (mut t, clock) = transport();
        t.set_local_client(SIBLING, true);
        let dest = destination();
        let hash = dest.hash().into_bytes();

        t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
        t.drain_actions();
        assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

        let limit = LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
        for i in 0..limit {
            // A probe started by a local client, proven over the owner.
            prove_over_client_from(
                &mut t,
                &clock,
                [0x60 + 2 * i; TRUNCATED_HASHBYTES],
                &dest,
                prober,
            );
            // A request from the network dies unproven.
            expire_unproven(&mut t, &clock, [0x61 + 2 * i; TRUNCATED_HASHBYTES], hash);
            assert_eq!(
                t.path_is_unresponsive(&hash),
                i + 1 == limit,
                "prober {prober}, network expiry {} of {limit}: self-proven probes \
                 must not restart the count",
                i + 1
            );
        }
        assert_eq!(t.storage().local_path_owner(&hash), None);
    }
}
