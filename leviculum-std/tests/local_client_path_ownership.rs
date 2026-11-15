//! Order 1898 (audit of order 1881): a connected local client keeps an
//! unresponsive mark off a path only when the path is its own, so a client
//! on the IPC socket cannot pin the path of a destination it does not host.
//!
//! Drives the real `leviculum_core` transport through its public API, the
//! way lnsd does: interface 0 is the local client, interface 1 the network.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::constants::{RANDOM_HASHBYTES, TRUNCATED_HASHBYTES};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::traits::Storage;
use leviculum_core::transport::{Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};

use rand_core::OsRng;

const LOCAL: usize = 0;
const NETWORK: usize = 1;

#[derive(Clone)]
struct TestClock(Arc<AtomicU64>);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn transport() -> (Transport<TestClock, MemoryStorage>, Arc<AtomicU64>) {
    let time = Arc::new(AtomicU64::new(1_000_000));
    let config = TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    };
    let mut t = Transport::new(
        config,
        TestClock(time.clone()),
        MemoryStorage::with_defaults(),
        Identity::generate(&mut OsRng),
    );
    t.set_local_client(LOCAL, true);
    (t, time)
}

fn destination() -> Destination {
    Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "ownership",
        &["path"],
    )
    .unwrap()
}

/// One signed announce emission of `dest` on the wire at `hops`.
fn announce(dest: &Destination, hops: u8) -> Vec<u8> {
    let id = dest.identity().unwrap();
    let mut random_hash = [0x77u8; RANDOM_HASHBYTES];
    random_hash[5..].copy_from_slice(&[0, 0, 0, 0x23, 0x28]);
    let app_data = b"own";
    let mut signed = Vec::new();
    signed.extend_from_slice(dest.hash().as_bytes());
    signed.extend_from_slice(&id.public_key_bytes());
    signed.extend_from_slice(dest.name_hash());
    signed.extend_from_slice(&random_hash);
    signed.extend_from_slice(app_data);
    let signature = id.sign(&signed).unwrap();
    let mut payload = Vec::new();
    payload.extend_from_slice(&id.public_key_bytes());
    payload.extend_from_slice(dest.name_hash());
    payload.extend_from_slice(&random_hash);
    payload.extend_from_slice(&signature);
    payload.extend_from_slice(app_data);
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Announce,
        },
        hops,
        transport_id: None,
        destination_hash: dest.hash().into_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };
    let mut buf = [0u8; 500];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

fn path_of(
    t: &Transport<TestClock, MemoryStorage>,
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> (usize, u8) {
    let p = t.storage().get_path(dest).expect("a path");
    (p.interface_index, p.hops)
}

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
    time.fetch_add(12_000, Ordering::SeqCst);
    t.process_incoming(LOCAL, &announce(&dest, 0)).unwrap();
    t.drain_actions();
    assert_eq!(path_of(&t, &hash), (LOCAL, 0));

    // 3. The mark is not refused: the client does not own the path.
    assert!(t.mark_path_unresponsive(&hash));

    // 4. The network copy arrives again.
    t.storage_mut().clear_packet_hashes();
    time.fetch_add(20_000, Ordering::SeqCst);
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
