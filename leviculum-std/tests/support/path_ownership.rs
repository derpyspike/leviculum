//! The harness the local-client path-ownership tests share (orders 1898 and
//! 1905): the real `leviculum_core` transport driven through its public API,
//! the way lnsd drives it. Interface 0 is a local client on the IPC socket,
//! interface 1 the network, interface 2 a sibling client where a test
//! registers one.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use leviculum_core::constants::{
    ED25519_KEY_SIZE, PATH_REQUEST_MIN_INTERVAL_MS, RANDOM_HASHBYTES, TRUNCATED_HASHBYTES,
    X25519_KEY_SIZE,
};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::storage_types::LinkEntry;
use leviculum_core::traits::Storage;
use leviculum_core::transport::{Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};

use rand_core::OsRng;

pub const LOCAL: usize = 0;
pub const NETWORK: usize = 1;
/// A second local-client connection; a test that uses it registers it with
/// `set_local_client(SIBLING, true)`.
pub const SIBLING: usize = 2;

/// A settable clock. A `Mutex<u64>`, not an `AtomicU64`: the 32-bit
/// Android triple has no 64-bit atomics (`just no-atomic64-gate`).
#[derive(Clone)]
pub struct TestClock(pub Arc<Mutex<u64>>);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}

impl TestClock {
    pub fn advance(&self, ms: u64) {
        *self.0.lock().unwrap() += ms;
    }
}

pub type T = Transport<TestClock, MemoryStorage>;

pub fn transport() -> (T, TestClock) {
    let clock = TestClock(Arc::new(Mutex::new(1_000_000)));
    let config = TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    };
    let mut t = Transport::new(
        config,
        clock.clone(),
        MemoryStorage::with_defaults(),
        Identity::generate(&mut OsRng),
    );
    t.set_local_client(LOCAL, true);
    (t, clock)
}

pub fn destination() -> Destination {
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
pub fn announce(dest: &Destination, hops: u8) -> Vec<u8> {
    announce_emission(dest, hops, 0x77, 9000)
}

/// A distinct emission of `dest` at `hops`: `nonce` fills the five random
/// bytes of the random hash and `emitted_s` its five-byte emission time, so
/// a later `emitted_s` is a newer emission than [`announce`]'s (9000 s).
pub fn announce_emission(dest: &Destination, hops: u8, nonce: u8, emitted_s: u64) -> Vec<u8> {
    let id = dest.identity().unwrap();
    let mut random_hash = [nonce; RANDOM_HASHBYTES];
    random_hash[5..].copy_from_slice(&emitted_s.to_be_bytes()[3..]);
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

pub fn path_of(t: &T, dest: &[u8; TRUNCATED_HASHBYTES]) -> (usize, u8) {
    let p = t.storage().get_path(dest).expect("a path");
    (p.interface_index, p.hops)
}

/// A link request from a one-hop initiator on the network, forwarded to
/// the client, expires unproven. The clock first moves past the
/// path-request throttle, so the expiry reaches the unresponsive mark and is
/// counted; an expiry inside `PATH_REQUEST_MIN_INTERVAL_MS` of the last path
/// request never reaches it.
pub fn expire_unproven(
    t: &mut T,
    clock: &TestClock,
    link_id: [u8; TRUNCATED_HASHBYTES],
    dest: [u8; TRUNCATED_HASHBYTES],
) {
    clock.advance(PATH_REQUEST_MIN_INTERVAL_MS);
    let now = clock.now_ms();
    t.storage_mut().set_link_entry(
        link_id,
        LinkEntry {
            timestamp_ms: now,
            next_hop_interface_index: LOCAL,
            remaining_hops: 0,
            received_interface_index: NETWORK,
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

/// A link request toward `dest`, forwarded to the client, is answered by a
/// link proof signed with the destination's key that comes back over the
/// client's interface. The client may host the destination or relay the
/// request to it; the transport cannot tell the two apart.
pub fn prove_over_client(
    t: &mut T,
    clock: &TestClock,
    link_id: [u8; TRUNCATED_HASHBYTES],
    dest: &Destination,
) {
    prove_over_client_from(t, clock, link_id, dest, NETWORK);
}

/// As [`prove_over_client`], for a link request that arrived over
/// `initiator`: `NETWORK` for a request from the mesh, `LOCAL` or another
/// local-client interface for a probe a local client started itself.
pub fn prove_over_client_from(
    t: &mut T,
    clock: &TestClock,
    link_id: [u8; TRUNCATED_HASHBYTES],
    dest: &Destination,
    initiator: usize,
) {
    let identity = dest.identity().unwrap();
    let pub_bytes = identity.public_key_bytes();
    let mut ed25519 = [0u8; ED25519_KEY_SIZE];
    ed25519.copy_from_slice(&pub_bytes[X25519_KEY_SIZE..]);
    let x25519 = [0x11u8; X25519_KEY_SIZE];
    let mut signed = Vec::new();
    signed.extend_from_slice(&link_id);
    signed.extend_from_slice(&x25519);
    signed.extend_from_slice(&ed25519);
    let signature = identity.sign(&signed).unwrap();
    let now = clock.now_ms();
    t.storage_mut().set_link_entry(
        link_id,
        LinkEntry {
            timestamp_ms: now,
            next_hop_interface_index: LOCAL,
            // The owner's own probe rides one interface both ways, where
            // only the path's frozen count (hops 0 from a client) passes the
            // same-interface echo rule.
            remaining_hops: if initiator == LOCAL { 0 } else { 1 },
            received_interface_index: initiator,
            hops: 1,
            validated: false,
            proof_timeout_ms: now + 30_000,
            destination_hash: dest.hash().into_bytes(),
            peer_signing_key: Some(ed25519),
            received_peer: None,
            next_hop_peer: None,
        },
    );
    let mut proof = Vec::with_capacity(96);
    proof.extend_from_slice(&signature);
    proof.extend_from_slice(&x25519);
    let pkt = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Link,
            packet_type: PacketType::Proof,
        },
        hops: 0,
        transport_id: None,
        destination_hash: link_id,
        context: PacketContext::Lrproof,
        data: PacketData::Owned(proof),
    };
    let mut buf = [0u8; 500];
    let len = pkt.pack(&mut buf).unwrap();
    let _ = t.process_incoming(LOCAL, &buf[..len]);
    t.drain_actions();
    assert!(
        t.storage()
            .get_link_entry(&link_id)
            .is_some_and(|e| e.validated),
        "the proof over the client must validate the link"
    );
}
