//! Drop-in half of the owner's path self-repair (reviewer decision
//! 2026-11-16): the SAME client code that repairs its path against `lnsd`
//! (`leviculum-std/tests/mvr/the_owner_repairs_its_daemons_path.rs`) does
//! it against a Python `rnsd`, over the same shared-instance socket and
//! the same `rnpath` RPC calls (`next_hop`, `next_hop_if_name`,
//! Reticulum.py:1222-1223; `drop_path`, the arm `rnpath -d` reaches).
//!
//! Topology: our builder client attaches to the vendored Python `rnsd`'s
//! abstract unix socket (`\0rns/<instance>`, LocalInterface.py:386),
//! announces a destination, and the test takes the daemon's path away over
//! the RPC drop surface. The client's repair beat must notice against the
//! Python daemon exactly as against ours and announce the path back,
//! without the test or the application announcing again.
//!
//! Skips (passes) where `python3` + the vendored RNS package is absent,
//! like every module of this suite.

use std::time::{Duration, Instant};

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;

use crate::harness::find_available_ports;
use crate::reverse_rpc_interop_tests::{python_rns_available, spawn_rnsd};

#[tokio::test]
async fn our_client_repairs_its_path_on_a_python_rnsd() {
    if !python_rns_available() {
        eprintln!("skipping path self-repair interop: python3 + vendored RNS unavailable");
        return;
    }

    // port-handoff: the Python rnsd reads it from its config file.
    let (ports, _alloc) = find_available_ports::<2>().await.expect("allocate port");
    let rnsd = spawn_rnsd(ports[0]);

    // Readiness: the RPC socket answering is the "daemon up" condition.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if leviculum_std::rpc_query(&rnsd.instance_name, &rnsd.authkey, "interface_stats")
            .await
            .is_ok()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "python rnsd RPC not ready within 30 s (log: {})",
            rnsd.config_dir.join("daemon.log").display()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The same client code as against lnsd: attached to rnsd's shared
    // instance, keyed from rnsd's config dir, repair beat shortened.
    let client_storage = tempfile::tempdir().expect("client storage");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(rnsd.instance_name.clone())
        .shared_instance_config_dir(rnsd.config_dir.clone())
        .path_self_repair_beat_ms(500)
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("build client");
    client.start().await.expect("start client");

    let dest = Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        Direction::In,
        DestinationType::Single,
        "selfrepairinterop",
        &["mailbox"],
    )
    .expect("destination");
    let dest_hash = *dest.hash();
    let dest_hex: String = dest_hash
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    client.register_destination(dest);
    client
        .announce_destination(&dest_hash, Some(b"mailbox"))
        .await
        .expect("announce");

    let next_hop = || async {
        leviculum_std::rpc_query_hash_param(
            &rnsd.instance_name,
            &rnsd.authkey,
            "next_hop",
            "destination_hash",
            dest_hash.as_bytes(),
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
    };

    // Positive control: rnsd learned the path, direct on the client
    // (`next_hop` echoes the destination itself, Transport.py:1741).
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if next_hop().await.as_deref() == Some(dest_hex.as_str()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "baseline: rnsd must learn the client's destination as a direct path"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // A beat's sight of the healthy path, then the displacement over the
    // real drop surface (Reticulum.py's drop arm, what `rnpath -d` sends).
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(
        leviculum_std::rpc_drop_path(&rnsd.instance_name, &rnsd.authkey, dest_hash.as_bytes())
            .await
            .expect("drop_path RPC"),
        "the drop must remove a live path from rnsd"
    );

    // Python's drop arm only expires the entry (`Transport.expire_path`,
    // Transport.py:2711-2718, timestamp 0 and a forced cull); the row
    // leaves the table on the next jobs pass. Wait for it to be GONE
    // before expecting the repair: a check right after the drop would
    // still see the old row and prove nothing.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if next_hop().await.as_deref() != Some(dest_hex.as_str()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the expired path must leave rnsd's table with the next cull"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Nothing touches the client from here on: the repair beat must put
    // the path back into the PYTHON daemon by announcing afresh — only
    // the key holder can; the application announced once at start and
    // never again.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if next_hop().await.as_deref() == Some(dest_hex.as_str()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the owner must repair its path on a Python rnsd by itself"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    client.stop().await.expect("stop client");
}
