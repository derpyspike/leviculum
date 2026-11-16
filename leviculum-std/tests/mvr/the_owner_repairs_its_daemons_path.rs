//! mvr: the owner repairs the daemon's path to its own destination.
//!
//! ## The failure
//!
//! miauhaus, 2026-11-15: lnsd's hops-0 path to its own local client (the
//! Loquax mailbox on the same host) was marked unresponsive after a
//! phone's forwarded link request expired, and a relayed copy of the
//! mailbox's own emission took the path at 7 hops via a backbone. For two
//! hours every link request for the mailbox left the box. Orders 1881 to
//! 1931 tried to prevent the capture inside the transport and every rule
//! was bypassed; the reviewer decision of 2026-11-16 reverts the transport
//! to Python's rule and moves the repair to the one party that can prove
//! ownership — the client holding the destination's key. The client beats
//! on the daemon over the `rnpath` RPC calls and, when the path to its own
//! destination is not its own attachment, announces afresh: a newer
//! emission wins by the ordinary rule on every stack
//! (`leviculum-std/src/driver/mod.rs#run_path_self_repair`).
//!
//! ## Topology
//!
//! ```text
//!   client node (connect_to_shared_instance, 500 ms repair beat)
//!        │  abstract unix socket \0rns/<name>, RPC \0rns/<name>/rpc
//!        v
//!   daemon node (share_instance, transport on)
//! ```
//!
//! The displacement is staged over the daemon's real RPC: `drop_path`, the
//! same `Transport::remove_path` surface `lnpath -d` and `rnpath -d` reach
//! (`leviculum-std/src/rpc/handlers.rs#drop_path`), which is one of the
//! removal paths the audit of 1931 named. No hardware, no Docker, no
//! Python; two in-process nodes over the real IPC socket, seconds.
//!
//! **Acceptance**: red without the repair beat (the dropped path stays
//! gone until the application's own six-hourly announce), green with it:
//! the daemon has the path back, direct on the client's attachment,
//! within a few beats and without the test or the application announcing.

use std::time::{Duration, Instant};

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;
use rand_core::OsRng;

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

/// The daemon's `next_hop` answer for `dest`: the destination's own hex on
/// a direct path, another hash via a relay, `None` without a path.
async fn next_hop(instance: &str, authkey: &[u8; 32], dest: &DestinationHash) -> Option<String> {
    let v = leviculum_std::rpc_query_hash_param(
        instance,
        authkey,
        "next_hop",
        "destination_hash",
        dest.as_bytes(),
    )
    .await
    .expect("next_hop RPC");
    v.as_str().map(str::to_owned)
}

/// Poll `cond` every 100 ms until it holds or the deadline passes.
async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cond()
}

/// THE pin: the path to the client's destination disappears from the
/// daemon, and the client puts it back by itself within a few beats.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_dropped_path_is_back_within_the_beat() {
    let identity = Identity::generate(&mut OsRng);
    let authkey = authkey_of(&identity);
    let instance = format!("mvr_selfrepair_{}", std::process::id());

    // The daemon's config dir, laid out as a client tool finds it: the
    // transport identity under `storage/`, where `shared_instance_config_dir`
    // derives the RPC key from (daemon_rpc.rs `resolve_authkey`).
    let daemon_dir = tempfile::tempdir().expect("daemon dir");
    let storage = daemon_dir.path().join("storage");
    std::fs::create_dir_all(&storage).expect("storage dir");
    std::fs::write(
        storage.join("transport_identity"),
        identity.private_key_bytes().expect("private key bytes"),
    )
    .expect("write transport identity");

    let mut daemon = ReticulumNodeBuilder::new()
        .identity(identity)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(storage.clone())
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");

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

    // The mailbox stand-in announces once, as the application does.
    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvrselfrepair",
        &["mailbox"],
    )
    .expect("destination");
    let dest_hash = *dest.hash();
    client.register_destination(dest);
    client
        .announce_destination(&dest_hash, Some(b"mailbox"))
        .await
        .expect("announce");

    // Positive control: the daemon learned the path, and it is direct —
    // `next_hop` echoes the destination itself (Transport.py:1741).
    assert!(
        wait_until(Duration::from_secs(10), || daemon.has_path(&dest_hash)).await,
        "baseline: the daemon must learn the client's destination"
    );
    let dest_hex: String = dest_hash
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        next_hop(&instance, &authkey, &dest_hash).await.as_deref(),
        Some(dest_hex.as_str()),
        "baseline: the fresh path is direct"
    );

    // Give the beat a sight of the healthy path (its baseline), then take
    // the path away over the daemon's real RPC drop surface.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(
        leviculum_std::rpc_drop_path(&instance, &authkey, dest_hash.as_bytes())
            .await
            .expect("drop_path RPC"),
        "the drop must remove a live path"
    );
    assert!(
        !daemon.has_path(&dest_hash),
        "positive control: the drop left the daemon without the path"
    );

    // Nothing touches the client from here on. The repair beat (500 ms)
    // must notice the missing path and announce afresh; ten seconds is
    // many beats and far under the application's own announce interval.
    assert!(
        wait_until(Duration::from_secs(10), || daemon.has_path(&dest_hash)).await,
        "the owner must notice the lost path and announce it back by itself"
    );
    assert_eq!(
        next_hop(&instance, &authkey, &dest_hash).await.as_deref(),
        Some(dest_hex.as_str()),
        "the repaired path is the client's own attachment again"
    );

    client.stop().await.expect("stop client");
    daemon.stop().await.expect("stop daemon");
}
