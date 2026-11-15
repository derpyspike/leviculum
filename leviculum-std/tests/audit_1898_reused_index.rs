//! Audit of order 1898 (order 1905): a client that disconnects takes its
//! ownership with it. A new client that the driver registers under the
//! same interface index inherits nothing, so its path is markable. Catches
//! the cargo-mutants survivor that replaces
//! `MemoryStorage::clear_local_path_owners_of` with `()`.
//!
//! Written from the audit's description; the verbatim text of
//! round1-verdict.md was not reachable from the coder host.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::traits::Storage;
use support::{announce, destination, transport, LOCAL};

#[test]
fn a_new_client_under_a_reused_index_does_not_inherit_ownership() {
    let (mut t, _clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();

    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

    t.set_local_client(LOCAL, false);
    t.set_local_client(LOCAL, true);
    assert!(t.storage().has_path(&hash));
    assert!(
        t.mark_path_unresponsive(&hash),
        "the new client under the reused index owns nothing"
    );
}
