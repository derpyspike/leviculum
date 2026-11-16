//! Audit of order 1905 (order 1909): a replayer that took a foreign path
//! with a captured hops-0 announce lost it at the bound. Handing in the
//! same capture again must not make it the owner again: the path already
//! remembers that emission, so the re-install is not a first-heard one, and
//! the path stays markable.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::traits::Storage;
use leviculum_core::transport::LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
use support::{announce, destination, expire_unproven, transport, LOCAL};

#[test]
fn a_replayer_that_lost_the_bound_cannot_regain_ownership_with_the_same_capture() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();
    let capture = announce(&dest, 0);

    t.process_incoming(LOCAL, &capture).unwrap();
    t.drain_actions();
    assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

    for i in 0..LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT {
        expire_unproven(&mut t, &clock, [0x70 + i; TRUNCATED_HASHBYTES], hash);
    }
    assert!(t.path_is_unresponsive(&hash));
    assert_eq!(t.storage().local_path_owner(&hash), None);

    // The same capture again, past the announce rate limit.
    t.storage_mut().clear_packet_hashes();
    clock.advance(20_000);
    let _ = t.process_incoming(LOCAL, &capture);
    t.drain_actions();
    assert_eq!(
        t.storage().local_path_owner(&hash),
        None,
        "the same capture must not restore ownership"
    );
    assert!(
        t.mark_path_unresponsive(&hash),
        "the path stays markable after the re-replay"
    );
}
