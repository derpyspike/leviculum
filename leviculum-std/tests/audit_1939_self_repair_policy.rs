//! Audit of 1939: the owner's path self-repair beat announces only what the
//! application would announce itself.
//!
//! The beat (`leviculum-std/src/driver/mod.rs#run_path_self_repair`) repairs
//! the daemon's path to a destination this client announced before. Two
//! doors the audit named:
//!
//! - a destination whose announces an installed `AnnounceControl` policy
//!   suppresses (membership privacy: routable, never gossiped) must never be
//!   announced by the beat either, even when the application announced it
//!   once before the policy went in;
//! - a destination the application registered but never announced must
//!   never be announced by the beat.
//!
//! Topology: an in-process daemon (`share_instance`, transport on) and an
//! in-process client over the abstract IPC socket with a 500 ms beat. The
//! path is taken away over the daemon's real RPC `drop_path`, the surface
//! `rnpath -d` reaches. The client must then leave it gone.

use std::time::{Duration, Instant};

use leviculum_core::identity::Identity;
use leviculum_core::{AnnounceControl, Destination, DestinationHash, DestinationType, Direction};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use rand_core::OsRng;

/// The beat the client runs at: a few beats fit in the observation window.
const BEAT_MS: u64 = 500;

/// How long the test watches the daemon for a repair that must not come:
/// ten beats.
const WATCH: Duration = Duration::from_millis(10 * BEAT_MS);

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

/// Suppresses exactly one destination's announces.
struct SuppressOne(DestinationHash);

impl AnnounceControl for SuppressOne {
    fn should_suppress_announce(&self, h: &DestinationHash) -> bool {
        *h == self.0
    }
}

/// A daemon and a beating client over the abstract IPC socket.
struct Pair {
    daemon: ReticulumNode,
    client: ReticulumNode,
    instance: String,
    authkey: [u8; 32],
    _daemon_dir: tempfile::TempDir,
    _client_dir: tempfile::TempDir,
}

async fn pair(tag: &str) -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let authkey = authkey_of(&identity);
    let instance = format!("audit1939_{tag}_{}", std::process::id());

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
        .storage_path(storage)
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");

    let client_dir = tempfile::tempdir().expect("client storage");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(instance.clone())
        .shared_instance_config_dir(daemon_dir.path().to_path_buf())
        .path_self_repair_beat_ms(BEAT_MS)
        .storage_path(client_dir.path().to_path_buf())
        .build()
        .await
        .expect("build client");
    client.start().await.expect("start client");

    Pair {
        daemon,
        client,
        instance,
        authkey,
        _daemon_dir: daemon_dir,
        _client_dir: client_dir,
    }
}

fn destination(aspect: &str) -> Destination {
    Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "audit1939",
        &[aspect],
    )
    .expect("destination")
}

/// The application announced the destination once, then installed a policy
/// that suppresses it. The daemon loses the path; the beat must leave it
/// lost, because announcing it would leak exactly what the policy hides.
///
/// In-test positive control (audit of 1950): an unsuppressed sibling on the
/// same client loses its path in the same drop and IS repaired, so the
/// watch window cannot pass because the beat never ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audit_1939_a_suppressed_destination_is_never_repaired_into_the_mesh() {
    let mut p = pair("suppressed").await;

    let dest = destination("suppressed");
    let dest_hash = *dest.hash();
    p.client.register_destination(dest);
    p.client
        .announce_destination(&dest_hash, Some(b"once"))
        .await
        .expect("announce");
    let loud = destination("loud");
    let loud_hash = *loud.hash();
    p.client.register_destination(loud);
    p.client
        .announce_destination(&loud_hash, None)
        .await
        .expect("announce sibling");

    // Positive control: both announces reached the daemon.
    assert!(
        wait_until(Duration::from_secs(10), || p.daemon.has_path(&dest_hash)
            && p.daemon.has_path(&loud_hash))
        .await,
        "baseline: the daemon must learn both destinations from their announces"
    );

    // The policy goes in for the first one only; a couple of beats pass,
    // and both paths go away over the real RPC surface.
    p.client
        .set_announce_control(Some(Box::new(SuppressOne(dest_hash))));
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    for h in [&dest_hash, &loud_hash] {
        assert!(
            leviculum_std::rpc_drop_path(&p.instance, &p.authkey, h.as_bytes())
                .await
                .expect("drop_path RPC"),
            "the drop must remove a live path"
        );
    }

    // The beat runs: the unsuppressed sibling is back.
    assert!(
        wait_until(Duration::from_secs(10), || p.daemon.has_path(&loud_hash)).await,
        "positive control: the beat repairs the unsuppressed sibling"
    );
    // Three more seconds (six beats) after the sibling's repair.
    assert!(
        !wait_until(Duration::from_secs(3), || p.daemon.has_path(&dest_hash)).await,
        "a destination the installed AnnounceControl suppresses must never be \
         re-announced by the self-repair beat"
    );

    p.client.stop().await.expect("stop client");
    p.daemon.stop().await.expect("stop daemon");
}

/// The application registered a destination and never announced it. The
/// beat must never put it into the mesh: announcing is the application's
/// decision.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audit_1939_a_never_announced_destination_is_not_repaired() {
    let mut p = pair("silent").await;

    // Positive control for the watch window: an announced sibling on the
    // same client IS repaired after a drop, so the beat is running.
    let loud = destination("loud");
    let loud_hash = *loud.hash();
    p.client.register_destination(loud);
    p.client
        .announce_destination(&loud_hash, None)
        .await
        .expect("announce");
    assert!(
        wait_until(Duration::from_secs(10), || p.daemon.has_path(&loud_hash)).await,
        "baseline: the daemon must learn the announced sibling"
    );

    let silent = destination("silent");
    let silent_hash = *silent.hash();
    p.client.register_destination(silent);

    tokio::time::sleep(Duration::from_millis(3 * BEAT_MS)).await;
    assert!(
        leviculum_std::rpc_drop_path(&p.instance, &p.authkey, loud_hash.as_bytes())
            .await
            .expect("drop_path RPC"),
        "the drop must remove the sibling's live path"
    );

    assert!(
        wait_until(Duration::from_secs(10), || p.daemon.has_path(&loud_hash)).await,
        "positive control: the beat repairs the announced sibling"
    );
    assert!(
        !wait_until(WATCH, || p.daemon.has_path(&silent_hash)).await,
        "a destination the application never announced must never be announced \
         by the self-repair beat"
    );

    p.client.stop().await.expect("stop client");
    p.daemon.stop().await.expect("stop daemon");
}

/// A beat that cannot ask its daemon is visible at the default log level:
/// a client whose `rpc_key` is wrong gets `PATH_SELF_REPAIR_SKIP` at warn
/// once for the cause, not once per beat.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audit_1939_a_beat_with_a_wrong_rpc_key_warns_once() {
    let evlog = leviculum_std::test_support::event_log::init_event_log();
    let warns = leviculum_std::test_support::warn_capture::register_warn_capture();

    let identity = Identity::generate(&mut OsRng);
    let instance = format!("audit1939_wrongkey_{}", std::process::id());
    let daemon_dir = tempfile::tempdir().expect("daemon dir");
    let mut daemon = ReticulumNodeBuilder::new()
        .identity(identity)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(daemon_dir.path().join("storage"))
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");

    // The client reads its key from a config directory that names a key
    // the daemon was never keyed with.
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

    let dest = destination("wrongkey");
    let dest_hash = *dest.hash();
    client.register_destination(dest);
    client
        .announce_destination(&dest_hash, None)
        .await
        .expect("announce");
    // Positive control: the data path works, only the RPC key is wrong.
    assert!(
        wait_until(Duration::from_secs(10), || daemon.has_path(&dest_hash)).await,
        "baseline: the daemon must learn the destination"
    );

    // Eight beats at least.
    tokio::time::sleep(Duration::from_millis(8 * BEAT_MS)).await;

    let dest_field = format!("dest={dest_hash}");
    let skips: Vec<String> = evlog
        .dump()
        .into_iter()
        .filter(|l| l.starts_with("PATH_SELF_REPAIR_SKIP ") && l.contains(&dest_field))
        .collect();
    assert_eq!(
        skips.len(),
        1,
        "one PATH_SELF_REPAIR_SKIP for the cause, not one per beat: {skips:?}"
    );
    assert!(
        skips[0].contains("reason=auth_failed"),
        "the line names the cause as a code: {}",
        skips[0]
    );
    // The event log's single-token contract holds for the beat's events.
    let violations: Vec<String> = evlog
        .dump()
        .into_iter()
        .filter(|l| l.starts_with("EVENT_FIELD_VIOLATION event=PATH_SELF_REPAIR"))
        .collect();
    assert!(
        violations.is_empty(),
        "the beat's events keep every field a single token: {violations:?}"
    );
    let warned = warns.snapshot();
    assert!(
        warned
            .lines()
            .any(|l| l.ends_with("event=PATH_SELF_REPAIR_SKIP")),
        "the skip is a WARN record, visible at the default level: {warned}"
    );

    client.stop().await.expect("stop client");
    daemon.stop().await.expect("stop daemon");
}

/// Audit of 1950: one destination suppressed, its sibling not; only the
/// sibling's path is dropped, and the beat repairs it while the policy is
/// installed for the other one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audit_1950_the_beat_repairs_the_unsuppressed_sibling() {
    let mut p = pair("sibling").await;

    let quiet = destination("quiet");
    let quiet_hash = *quiet.hash();
    p.client.register_destination(quiet);
    p.client
        .announce_destination(&quiet_hash, None)
        .await
        .expect("announce");
    let loud = destination("loud");
    let loud_hash = *loud.hash();
    p.client.register_destination(loud);
    p.client
        .announce_destination(&loud_hash, None)
        .await
        .expect("announce sibling");
    assert!(
        wait_until(Duration::from_secs(10), || p.daemon.has_path(&quiet_hash)
            && p.daemon.has_path(&loud_hash))
        .await,
        "baseline: the daemon must learn both destinations"
    );

    p.client
        .set_announce_control(Some(Box::new(SuppressOne(quiet_hash))));
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(
        leviculum_std::rpc_drop_path(&p.instance, &p.authkey, loud_hash.as_bytes())
            .await
            .expect("drop_path RPC"),
        "the drop must remove the sibling's live path"
    );
    assert!(
        wait_until(Duration::from_secs(10), || p.daemon.has_path(&loud_hash)).await,
        "the beat must repair the unsuppressed sibling while the other is suppressed"
    );

    p.client.stop().await.expect("stop client");
    p.daemon.stop().await.expect("stop daemon");
}
