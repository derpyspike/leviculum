//! Audit of 1950: the self-repair beat's catalogued events keep the event
//! log's single-token contract.
//!
//! `PATH_SELF_REPAIR_SKIP` once carried the RPC error's free text in its
//! `reason` field (`RPC failed: authentication failed`), so every skip
//! raised an `EVENT_FIELD_VIOLATION … value_problem=whitespace` beside it
//! and grep/awk over the log split the record. The field is a fixed cause
//! code now, and the free text goes to a plain log line of its own.
//!
//! Topology: an in-process daemon and a shared-instance client whose
//! `rpc_key` the daemon was never keyed with, beating every 500 ms.

use std::time::{Duration, Instant};

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;
use rand_core::OsRng;

const BEAT_MS: u64 = 500;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audit_1950_skip_event_fields_are_single_tokens() {
    let evlog = leviculum_std::test_support::event_log::init_event_log();

    let instance = format!("audit1950_tokens_{}", std::process::id());
    let daemon_dir = tempfile::tempdir().expect("daemon dir");
    let mut daemon = ReticulumNodeBuilder::new()
        .identity(Identity::generate(&mut OsRng))
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(daemon_dir.path().join("storage"))
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");

    let wrong_dir = tempfile::tempdir().expect("wrong key dir");
    std::fs::write(
        wrong_dir.path().join("config"),
        "[reticulum]\n  rpc_key = 00112233445566778899aabbccddeeff\n",
    )
    .expect("write config");
    let client_dir = tempfile::tempdir().expect("client storage");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(instance.clone())
        .shared_instance_config_dir(wrong_dir.path().to_path_buf())
        .path_self_repair_beat_ms(BEAT_MS)
        .storage_path(client_dir.path().to_path_buf())
        .build()
        .await
        .expect("build client");
    client.start().await.expect("start client");

    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "audit1950",
        &["tokens"],
    )
    .expect("destination");
    let dest_hash = *dest.hash();
    client.register_destination(dest);
    client
        .announce_destination(&dest_hash, None)
        .await
        .expect("announce");
    assert!(
        wait_until(Duration::from_secs(10), || daemon.has_path(&dest_hash)).await,
        "baseline: the daemon must learn the destination"
    );

    // Positive control: the skip was logged at all.
    let dest_field = format!("dest={dest_hash}");
    assert!(
        wait_until(Duration::from_secs(10), || evlog.dump().iter().any(|l| l
            .starts_with("PATH_SELF_REPAIR_SKIP ")
            && l.contains(&dest_field)))
        .await,
        "the wrong key must produce a PATH_SELF_REPAIR_SKIP"
    );
    tokio::time::sleep(Duration::from_millis(2 * BEAT_MS)).await;

    let violations: Vec<String> = evlog
        .dump()
        .into_iter()
        .filter(|l| l.starts_with("EVENT_FIELD_VIOLATION event=PATH_SELF_REPAIR"))
        .collect();
    assert!(
        violations.is_empty(),
        "every field of the beat's events is a single token: {violations:?}"
    );

    client.stop().await.expect("stop client");
    daemon.stop().await.expect("stop daemon");
}
