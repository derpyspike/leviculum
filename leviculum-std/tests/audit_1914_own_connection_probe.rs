//! Order 1914, the reviewer's narrowing of order 1909's residual: a local
//! client that also holds a non-local connection into lnsd (a TCP client
//! interface, a LAN peer, a backbone route back to this host) starts its
//! own link requests there and answers them over its IPC connection, while
//! every request from the network dies. Order 1909 told initiators apart by
//! interface class only, so such a probe counted as network-initiated: its
//! proof restarted the owner's unproven-expiry count, and after the bound it
//! handed the ownership back. The client held the path without limit.
//!
//! A proof now restarts the count only when its link request entered on the
//! ingress (interface and peer) of the last counted dead request, that is
//! when the client answers the neighbour whose request it let die, and a
//! proof never grants an ownership the path does not have.

#[path = "support/path_ownership.rs"]
mod support;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::traits::Storage;
use leviculum_core::transport::LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
use support::{announce, destination, expire_unproven, prove_over_client_from, transport, LOCAL};

/// The owner's own TCP connection into lnsd: an interface that is not a
/// local client's, so its requests look like the network's by class.
const OWN_TCP: usize = 3;

#[test]
fn a_client_proving_probes_from_its_own_network_connection_loses_the_path() {
    let (mut t, clock) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();

    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(t.storage().local_path_owner(&hash), Some(LOCAL));

    let limit = LOCAL_OWNER_UNPROVEN_EXPIRY_LIMIT;
    for i in 0..limit {
        // A probe the client started over its own TCP connection, proven
        // over its IPC connection.
        prove_over_client_from(
            &mut t,
            &clock,
            [0x70 + 2 * i; TRUNCATED_HASHBYTES],
            &dest,
            OWN_TCP,
        );
        // A request from the network dies unproven.
        expire_unproven(&mut t, &clock, [0x71 + 2 * i; TRUNCATED_HASHBYTES], hash);
        assert_eq!(
            t.path_is_unresponsive(&hash),
            i + 1 == limit,
            "network expiry {} of {limit}: probes from the client's own \
             connection must not restart the count",
            i + 1
        );
    }
    assert_eq!(t.storage().local_path_owner(&hash), None);

    // After the bound, one more self-started probe does not hand the
    // ownership back.
    prove_over_client_from(&mut t, &clock, [0x7F; TRUNCATED_HASHBYTES], &dest, OWN_TCP);
    assert_eq!(
        t.storage().local_path_owner(&hash),
        None,
        "a self-started probe after the bound must not restore ownership"
    );
}
