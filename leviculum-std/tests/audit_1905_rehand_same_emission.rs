//! Audit of order 1905, second auditor (order 1909): what restarts an
//! owner's unproven-expiry count. Handing in the same emission again does
//! not (it is no first-heard install); a newer emission of the client's own
//! destination does (it is one, and the client can only make it if it holds
//! the destination's key).
//!
//! `rehanding_the_same_emission_does_not_restart_the_count` is written from
//! the audit's description; its verbatim text was not reachable from the
//! coder host. The positive twin is this pass's own.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::traits::Storage;
use leviculum_core::transport::LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
use support::{announce, announce_emission, destination, expire_unproven, transport, LOCAL};

#[test]
fn rehanding_the_same_emission_does_not_restart_the_count() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    let own = announce(&dest, 0);
    t.process_incoming(LOCAL, &own).unwrap();
    t.drain_actions();
    assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

    expire_unproven(&mut t, &clock, [0x80; TRUNCATED_HASHBYTES], hash);
    assert!(!t.path_is_unresponsive(&hash));

    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(LOCAL, &own);
    t.drain_actions();

    // One expiry is already counted: the bound is reached LIMIT - 1 later.
    for i in 1..LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT {
        expire_unproven(&mut t, &clock, [0x80 + i; TRUNCATED_HASHBYTES], hash);
        assert_eq!(
            t.path_is_unresponsive(&hash),
            i + 1 == LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT,
            "expiry {} of {}: the re-handed emission restarted nothing",
            i + 1,
            LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT
        );
    }
}

#[test]
fn a_newer_emission_from_the_owner_restarts_the_count() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();

    expire_unproven(&mut t, &clock, [0x90; TRUNCATED_HASHBYTES], hash);
    assert!(!t.path_is_unresponsive(&hash));

    // A newer emission: fresh random bytes, a later emission time.
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    t.process_incoming(LOCAL, &announce_emission(&dest, 0, 0x78, 9100))
        .unwrap();
    t.drain_actions();
    assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

    // The count starts again: LIMIT - 1 dead requests keep the path, the
    // LIMIT-th marks it.
    for i in 0..LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT {
        expire_unproven(&mut t, &clock, [0x91 + i; TRUNCATED_HASHBYTES], hash);
        assert_eq!(
            t.path_is_unresponsive(&hash),
            i + 1 == LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT,
            "expiry {} after the newer emission",
            i + 1
        );
    }
}
