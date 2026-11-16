//! mvr: the owner takes its path back from a relayed capture, and a link
//! from the mesh reaches it again (audit of 1939, reviewer test 3(a)).
//!
//! ## The failure
//!
//! The miauhaus field case of 2026-11-15: the daemon's path to its own
//! local client's destination was taken by a relayed copy and pointed off
//! the box, so every link request for the mailbox left the host. The
//! sibling mvr (`the_owner_repairs_its_daemons_path.rs`) stages a vanished
//! path; this one stages the capture itself: a relayed announce of a NEWER
//! emission of the client's destination, signed with the destination's
//! key (as a second holder of the key would make it), arrives at the
//! daemon over TCP and wins by the ordinary newer-emission rule
//! (`Transport.py:1809`). The daemon's `next_hop` now names the relay.
//!
//! ## Topology
//!
//! ```text
//!   client (connect_to_shared_instance, 500 ms beat, holds the dest)
//!        │  abstract unix socket
//!        v
//!   daemon (share_instance, transport, TCP server) <── raw TCP: forged relay
//!        ^
//!        │  TCP
//!   second node (plain, opens the link)
//! ```
//!
//! **Acceptance**: red with the beat spawn disabled (the relay keeps the
//! path), green with it: `next_hop == destination` again within a few
//! beats, and a link request from the second node is proven by the client.

use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use leviculum_core::identity::Identity;
use leviculum_core::packet::{HeaderType, TransportType};
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;
use rand_core::OsRng;

/// The forged relay's transport id: what `next_hop` names while the
/// capture holds.
const RELAY_ID: [u8; 16] = [0xA7; 16];

/// RPC authkey the daemon derives from its transport identity: SHA256 over
/// the 64 private key bytes (Reticulum.py:355-356).
fn authkey_of(identity: &Identity) -> [u8; 32] {
    use sha2::Digest;
    let mut key = [0u8; 32];
    key.copy_from_slice(&sha2::Sha256::digest(
        identity.private_key_bytes().expect("private key bytes"),
    ));
    key
}

/// The daemon's `next_hop` answer for `dest`.
async fn next_hop(instance: &str, authkey: &[u8; 32], dest: &DestinationHash) -> Option<String> {
    leviculum_std::rpc_query_hash_param(
        instance,
        authkey,
        "next_hop",
        "destination_hash",
        dest.as_bytes(),
    )
    .await
    .expect("next_hop RPC")
    .as_str()
    .map(str::to_owned)
}

/// Poll the daemon's `next_hop` until it equals `want` or the time is up.
async fn next_hop_becomes(
    instance: &str,
    authkey: &[u8; 32],
    dest: &DestinationHash,
    want: &str,
    limit: Duration,
) -> bool {
    let end = Instant::now() + limit;
    loop {
        if next_hop(instance, authkey, dest).await.as_deref() == Some(want) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn dest_for(identity: Identity) -> Destination {
    Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrselfrepair",
        &["relayed"],
    )
    .expect("destination")
}

/// THE pin: a relayed capture of a newer emission is taken back by the
/// owner's beat, and the mesh reaches the owner again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_takes_back_a_relayed_capture() {
    let daemon_identity = Identity::generate(&mut OsRng);
    let authkey = authkey_of(&daemon_identity);
    let instance = format!("mvr_relayedcap_{}", std::process::id());

    let daemon_dir = tempfile::tempdir().expect("daemon dir");
    let storage = daemon_dir.path().join("storage");
    std::fs::create_dir_all(&storage).expect("storage dir");
    std::fs::write(
        storage.join("transport_identity"),
        daemon_identity
            .private_key_bytes()
            .expect("private key bytes"),
    )
    .expect("write transport identity");
    let mut daemon = ReticulumNodeBuilder::new()
        .identity(daemon_identity)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .add_tcp_server(crate::ports::LOOPBACK_ANY_PORT)
        .storage_path(storage)
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");
    let daemon_tcp = crate::ports::bound_tcp_addr(&daemon.tcp_listen_addrs());

    let client_storage = tempfile::tempdir().expect("client storage");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(instance.clone())
        .shared_instance_config_dir(daemon_dir.path().to_path_buf())
        .path_self_repair_beat_ms(500)
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("build client");
    client.start().await.expect("start client");

    // The second node: a plain node on the daemon's TCP server.
    let second_storage = tempfile::tempdir().expect("second storage");
    let mut second = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(daemon_tcp)
        .storage_path(second_storage.path().to_path_buf())
        .build()
        .await
        .expect("build second node");
    second.start().await.expect("start second node");

    // The destination's key: the client holds it, and the test keeps a
    // copy to sign the capture with.
    let dest_identity = Identity::generate(&mut OsRng);
    let key_bytes = dest_identity.private_key_bytes().expect("dest key bytes");
    let signing_key: [u8; 32] = dest_identity.public_key_bytes()[32..64]
        .try_into()
        .expect("signing key");
    let dest = dest_for(dest_identity);
    let dest_hash = *dest.hash();
    let dest_hex = hex::encode(dest_hash.as_bytes());
    client.register_destination(dest);
    tokio::time::sleep(Duration::from_millis(500)).await;
    client
        .announce_destination(&dest_hash, Some(b"owner"))
        .await
        .expect("announce");

    // Positive control: the daemon's path is direct on the client.
    assert!(
        next_hop_becomes(
            &instance,
            &authkey,
            &dest_hash,
            &dest_hex,
            Duration::from_secs(10)
        )
        .await,
        "baseline: the daemon must learn the client's destination as a direct path"
    );
    // The beat learns its baseline, and the clock moves past the owner's
    // emission second, so the forged emission below is strictly newer than
    // the owner's and every later repair strictly newer than the forgery.
    tokio::time::sleep(Duration::from_millis(3_000)).await;

    // The capture: a newer emission, signed with the held key, relayed by
    // RELAY_ID at one hop, framed onto a raw TCP connection to the daemon.
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let mut forger = dest_for(Identity::from_private_key_bytes(&key_bytes).expect("key"));
    let mut packet = forger
        .announce(Some(b"relayed"), &mut OsRng, 0, now_secs - 1)
        .expect("forged announce");
    packet.flags.header_type = HeaderType::Type2;
    packet.flags.transport_type = TransportType::Transport;
    packet.transport_id = Some(RELAY_ID);
    packet.hops = 1;
    let mut raw = [0u8; leviculum_core::constants::MTU];
    let len = packet.pack(&mut raw).expect("pack");
    let mut framed = Vec::new();
    leviculum_core::framing::hdlc::frame(&raw[..len], &mut framed);
    let mut relay = std::net::TcpStream::connect(daemon_tcp).expect("raw relay connect");
    tokio::time::sleep(Duration::from_millis(300)).await;
    relay.write_all(&framed).expect("inject the relayed copy");

    // The capture took: next_hop names the relay. Polled every 100 ms so
    // the first sight is caught before the beat (500 ms) undoes it.
    let relay_hex = hex::encode(RELAY_ID);
    assert!(
        next_hop_becomes(
            &instance,
            &authkey,
            &dest_hash,
            &relay_hex,
            Duration::from_secs(5)
        )
        .await,
        "positive control: the newer relayed emission must take the daemon's path"
    );

    // Nothing touches the client. The beat must take the path back.
    assert!(
        next_hop_becomes(
            &instance,
            &authkey,
            &dest_hash,
            &dest_hex,
            Duration::from_secs(5)
        )
        .await,
        "the owner must take its path back from the relayed capture within a few beats"
    );

    // And the mesh reaches the owner: a link from the second node, routed
    // by the daemon, is proven by the client.
    assert!(
        {
            let end = Instant::now() + Duration::from_secs(10);
            while !second.has_path(&dest_hash) && Instant::now() < end {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            second.has_path(&dest_hash)
        },
        "the second node must learn a path to the destination"
    );
    let (_link, established) = second
        .connect_awaited(&dest_hash, &signing_key)
        .await
        .expect("link request");
    let proven = tokio::time::timeout(Duration::from_secs(10), established).await;
    assert!(
        matches!(proven, Ok(Ok(_))),
        "a link request from the second node must reach the client and be proven: {proven:?}"
    );

    drop(relay);
    second.stop().await.expect("stop second node");
    client.stop().await.expect("stop client");
    daemon.stop().await.expect("stop daemon");
}
