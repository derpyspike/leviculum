//! Order 1898 (audit of order 1881): a connected local client keeps an
//! unresponsive mark off a path only when the path is its own, so a client
//! on the IPC socket cannot pin the path of a destination it does not host.
//!
//! Drives the real `leviculum_core` transport through its public API, the
//! way lnsd does: interface 0 is the local client, interface 1 the network.

#[path = "support/path_ownership.rs"]
mod support;

use support::{announce, destination, path_of, transport, LOCAL, NETWORK};

#[test]
fn a_replayed_network_destination_heals_back_to_the_network() {
    let (mut t, time) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();

    // 1. The network copy installs the path on the network interface.
    let network = announce(&dest, 1);
    t.process_incoming(NETWORK, &network).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (NETWORK, 2));

    // 2. A local client replays the same emission at hops 0 and takes it.
    time.advance(12_000);
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    // 3. The mark is not refused: the client does not own the path.
    assert!(t.mark_path_unresponsive(&hash));

    // 4. The network copy arrives again.
    t.storage_mut().clear_packet_hashes();
    time.advance(20_000);
    t.process_incoming(NETWORK, &network).unwrap();
    t.drain_actions();

    // 5. The path is back on the network interface.
    assert_eq!(path_of(&t, &hash), (NETWORK, 2));
}

#[test]
fn a_clients_own_first_announce_stays_local_until_it_disconnects() {
    let (mut t, _time) = transport();
    let dest = destination();
    let hash = dest.hash().into_bytes();

    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));
    assert!(
        !t.mark_path_unresponsive(&hash),
        "the client's own path is kept while it is connected"
    );
    assert!(!t.path_is_unresponsive(&hash));

    t.set_local_client(LOCAL, false);
    assert!(
        t.mark_path_unresponsive(&hash),
        "after the disconnect the path is markable"
    );
    assert!(t.path_is_unresponsive(&hash));
}
