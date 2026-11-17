//! mvr for order 1656 (Refs #232, #24): an announce made around the time a
//! new interface comes up reaches that interface exactly once, and soon.
//!
//! Field shape: `loquax-android/tests/facade.rs`, measured by order 1643 on
//! lane 4. Spawning one idle task at the phone mailbox's start turned
//! "Python never learned the phone's mailbox" from 0 of 8 runs into 7 of
//! 10. The mailbox announces its home destination and then its delivery
//! destination at start (`loquax/src/home/daemon.rs`), the next delivery
//! announce six minutes later, and its link to the Python peer is a
//! rendezvous dial (`loquax/src/home/rendezvous.rs`, `spawn_interface` of a
//! `TCPClientInterface`) that the event loop registers at its own time.
//!
//! The hypothesis the order came with, a window that neither the broadcast
//! nor `handle_interface_up`'s re-announce covers, is refuted by the first
//! test below: from 100 ms before the announce to 1 s after it, every offset
//! reaches the peer at once. What the facade's own event log showed instead
//! (red run on 3a0ede467) is the opposite of a gap, a duplicate: the
//! mailbox's home announce left on the fresh connection twice, 187 µs apart
//! and with different packet hashes, followed by the paced delivery
//! announce. A Python peer holds the third announce of a burst on an
//! interface younger than two hours (`RNS/Transport.py:1695-1706`, see
//! `a_third_announce_on_a_fresh_interface_waits_at_python.rs`), and nothing
//! releases it before the next announce.
//!
//! The mechanism is in Branch 5 of `run_event_loop`
//! (`leviculum-std/src/driver/mod.rs`). Order 1320 drains the action channel
//! before a new interface registers, then registers it and calls
//! `handle_interface_up`, which re-announces every destination with a cached
//! announce. An announce committed by another thread after the drain and
//! before `handle_interface_up` is cached when the interface-up looks AND
//! queued as a broadcast that the loop dispatches after the registration:
//! two copies on the new interface. The second test races exactly that and
//! counts the copies on the wire; on HEAD 58 of 120 rounds carried two.
//! The fix holds one gate across every commit-and-enqueue and across Branch
//! 5's drain-register-interface-up (`ActionTx` in the driver).
//!
//! The Python half, the delivery destination held, is
//! `leviculum-std/tests/rnsd_interop/mailbox_announce_new_interface_tests.rs`.

use std::time::Duration;

use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_std::config::InterfaceConfig;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use tokio::time::Instant;

/// How long the peer may take after the client is online.
const FIRST_SECOND: Duration = Duration::from_secs(1);
/// How long an arm waits for the client to come online at all.
const CONNECT_BOUND: Duration = Duration::from_secs(5);

fn destination() -> Destination {
    Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr",
        &["1656", "delivery"],
    )
    .expect("create destination")
}

async fn listening_peer(storage: &std::path::Path) -> ReticulumNode {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the peer's port");
    let mut peer = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server_listener(listener)
        .storage_path(storage.to_path_buf())
        .build()
        .await
        .expect("build peer");
    peer.start().await.expect("start peer");
    peer
}

fn tcp_client_to(addr: std::net::SocketAddr) -> InterfaceConfig {
    InterfaceConfig {
        name: "mvr1656 client".to_string(),
        interface_type: "TCPClientInterface".to_string(),
        enabled: true,
        target_host: Some(addr.ip().to_string()),
        target_port: Some(addr.port()),
        ..Default::default()
    }
}

fn online(node: &ReticulumNode, id: leviculum_core::transport::InterfaceId) -> bool {
    node.interface_stats()
        .iter()
        .any(|s| s.interface_id == id && s.online)
}

/// One arm: the client is spawned `offset_ms` after the announce (negative:
/// before it). Returns the arm's timeline, or `Err` with it when the peer
/// did not learn the destination within the client's first second.
async fn arm(offset_ms: i64) -> Result<String, String> {
    let peer_dir = tempfile::tempdir().expect("tempdir");
    let node_dir = tempfile::tempdir().expect("tempdir");
    let mut peer = listening_peer(peer_dir.path()).await;
    let addr = *peer.tcp_listen_addrs().first().expect("the peer listens");

    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .storage_path(node_dir.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    let dest = destination();
    let hash: DestinationHash = *dest.hash();
    node.register_destination(dest);

    let origin = Instant::now();
    let at = |t: Instant| t.saturating_duration_since(origin).as_millis();
    let mut timeline = String::new();
    let spawn = |node: &ReticulumNode| {
        node.spawn_interface(tcp_client_to(addr))
            .expect("spawn the client")[0]
    };

    let id;
    if offset_ms < 0 {
        id = spawn(&node);
        timeline += &format!("t={} SPAWN\n", at(Instant::now()));
        tokio::time::sleep(Duration::from_millis(offset_ms.unsigned_abs())).await;
        node.announce_destination(&hash, None)
            .await
            .expect("announce");
        timeline += &format!("t={} ANNOUNCE\n", at(Instant::now()));
    } else {
        node.announce_destination(&hash, None)
            .await
            .expect("announce");
        timeline += &format!("t={} ANNOUNCE\n", at(Instant::now()));
        tokio::time::sleep(Duration::from_millis(offset_ms.unsigned_abs())).await;
        id = spawn(&node);
        timeline += &format!("t={} SPAWN\n", at(Instant::now()));
    }

    let bound = Instant::now() + CONNECT_BOUND;
    while !online(&node, id) && Instant::now() < bound {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let up = Instant::now();
    if !online(&node, id) {
        return Err(format!("{timeline}client never came online"));
    }
    timeline += &format!("t={} ONLINE\n", at(up));

    let deadline = up + FIRST_SECOND;
    let mut learned = None;
    while Instant::now() < deadline + Duration::from_secs(4) {
        if peer.has_path(&hash) {
            learned = Some(Instant::now());
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    node.stop().await.ok();
    peer.stop().await.ok();
    match learned {
        Some(t) => {
            timeline += &format!("t={} PEER_HAS_PATH\n", at(t));
            if t <= deadline {
                Ok(timeline)
            } else {
                Err(format!("{timeline}learned past the first second"))
            }
        }
        None => Err(format!("{timeline}peer never learned it in 5 s")),
    }
}

/// Every offset from 100 ms before the announce to 1 s after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_interface_hears_an_announce_made_before_it() {
    let mut red = Vec::new();
    for offset_ms in [-100i64, -10, 0, 10, 100, 1000] {
        match arm(offset_ms).await {
            Ok(timeline) => eprintln!("offset {offset_ms} ms GREEN\n{timeline}"),
            Err(timeline) => {
                eprintln!("offset {offset_ms} ms RED\n{timeline}");
                red.push(offset_ms);
            }
        }
    }
    assert!(
        red.is_empty(),
        "the peer missed the announce for client offsets {red:?} ms (timelines above)"
    );
}

/// What one round's raw reader saw on the wire, every time an offset in
/// microseconds from the round's epoch (taken just before the client
/// spawns), so a red round names its whole timeline in one clock.
struct WireTrace {
    /// When the client's dial was accepted.
    accept_us: u128,
    /// Every announce of the destination, in arrival order.
    arrivals_us: Vec<u128>,
    /// Why reading stopped: `window` (the strict window elapsed with a
    /// copy in), `grace` (the long grace elapsed with none), `closed`
    /// (EOF), or the read error. Before order 1977 an error ended the
    /// count silently and a red could not tell "nothing arrived" from
    /// "the reader stopped listening".
    ended: String,
}

/// Read the one accepted connection for announces of `hash`.
///
/// A copy inside `window` of the accept is on time. When the window ends
/// empty, the reader keeps listening up to `grace`, so a red can tell a
/// copy that was late (the 672/689 class, a busy host scheduling the
/// event loop behind the reader's clock) from one that never came (a
/// real loss). After a copy is in, reading still spans the full strict
/// window, where the historical duplicate (187 µs apart) would show.
fn announces_read(
    listener: std::net::TcpListener,
    hash: DestinationHash,
    window: Duration,
    grace: Duration,
    epoch: std::time::Instant,
) -> WireTrace {
    use leviculum_core::framing::hdlc::{DeframeResult, Deframer};
    use leviculum_core::packet::{Packet, PacketType};
    use std::io::Read;
    listener.set_nonblocking(false).expect("blocking listener");
    let (mut stream, _) = listener.accept().expect("the client dials");
    let accept = std::time::Instant::now();
    let mut trace = WireTrace {
        accept_us: accept.duration_since(epoch).as_micros(),
        arrivals_us: Vec::new(),
        ended: String::new(),
    };
    let mut deframer = Deframer::new();
    let mut buf = [0u8; 4096];
    loop {
        let deadline = if trace.arrivals_us.is_empty() {
            accept + grace
        } else {
            accept + window
        };
        let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
            trace.ended = if trace.arrivals_us.is_empty() {
                "grace".to_string()
            } else {
                "window".to_string()
            };
            break;
        };
        stream
            .set_read_timeout(Some(left.max(Duration::from_millis(1))))
            .expect("read timeout");
        let n = match stream.read(&mut buf) {
            Ok(0) => {
                trace.ended = "closed".to_string();
                break;
            }
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(e) => {
                trace.ended = format!("read error: {e}");
                break;
            }
        };
        for result in deframer.process(&buf[..n]) {
            if let DeframeResult::Frame(raw) = result {
                if let Ok(p) = Packet::unpack(&raw) {
                    if p.flags.packet_type == PacketType::Announce
                        && &p.destination_hash == hash.as_bytes()
                    {
                        trace.arrivals_us.push(accept.elapsed().as_micros());
                    }
                }
            }
        }
    }
    trace
}

/// The facade's red, as 1656 found it in the mailbox's event log: the
/// rendezvous dial's interface reaches the event loop while the home
/// daemon announces from another thread. Branch 5 of `run_event_loop`
/// drains the action channel (order 1320), then registers the interface
/// and re-announces every destination with a cached announce on it. An
/// announce committed between the drain and the registration is in the
/// cache when the interface-up looks, and its queued broadcast is
/// dispatched after the registration: the new connection carries it twice,
/// and with the delivery announce that is the three-announce burst a Python
/// peer holds the third of (`a_third_announce_on_a_fresh_interface_waits_at_python.rs`).
///
/// Each round spawns a client to a raw listener and announces once, a swept
/// spin after the spawn so that some round lands in the window. Every round
/// must put exactly one copy on the connection, inside [`WINDOW`] of the
/// accept.
///
/// A red round explains itself (order 1977, after an unexplained
/// `[(52, 0)]` on a host at load 26, order 1881's report): its line carries
/// the round's whole timeline (spawn, announce, accept, every arrival) and
/// a verdict — `doubled` for the race this mvr exists for, `late` for a
/// copy inside [`LATE_GRACE`] but past the window (the 672/689 class, the
/// host scheduling the event loop behind the reader's clock), `lost` for
/// none within the grace — followed by the node's own structured events
/// (`ANN_TX`, `ANN_PACED`) for that round. Under a parallel test run other
/// tests' events can interleave into those lines; standalone they are this
/// round's alone.
#[tokio::test(flavor = "multi_thread")]
async fn an_announce_racing_a_new_interface_reaches_it_once() {
    const ROUNDS: u32 = 120;
    /// A copy inside this of the accept is on time: what the test has
    /// asserted since 1656. 24 000 baseline rounds (order 1977: 50 runs
    /// alone and 50 under a 25 % CPU quota on each of 95ecc0730 and
    /// e2ccf9ff) all stayed inside it.
    const WINDOW: Duration = Duration::from_millis(250);
    /// How much longer the reader listens after an empty window, to tell
    /// a late copy from a lost one. Costs nothing on a clean round.
    const LATE_GRACE: Duration = Duration::from_secs(3);
    let evlog = leviculum_std::test_support::event_log::init_event_log();
    let mut anomalies = Vec::new();
    for round in 0..ROUNDS {
        let events_before = evlog.dump().len();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let node_dir = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .storage_path(node_dir.path().to_path_buf())
            .build()
            .await
            .expect("build node");
        node.start().await.expect("start node");
        let dest = destination();
        let hash: DestinationHash = *dest.hash();
        node.register_destination(dest);

        let epoch = std::time::Instant::now();
        let reader =
            std::thread::spawn(move || announces_read(listener, hash, WINDOW, LATE_GRACE, epoch));
        node.spawn_interface(tcp_client_to(addr))
            .expect("spawn the client");
        let spawned_us = epoch.elapsed().as_micros();
        // Sweep 0..=119 µs: the loop's wake-up, drain and registration
        // take tens of microseconds on the node's own thread.
        let spin = std::time::Instant::now() + Duration::from_micros(u64::from(round));
        while std::time::Instant::now() < spin {
            std::hint::spin_loop();
        }
        node.announce_destination(&hash, None)
            .await
            .expect("announce");
        let announced_us = epoch.elapsed().as_micros();
        let trace = reader.join().expect("reader");
        node.stop().await.ok();
        let on_time = trace
            .arrivals_us
            .iter()
            .filter(|&&a| a <= WINDOW.as_micros())
            .count();
        if on_time != 1 || trace.arrivals_us.len() != 1 {
            let verdict = match (on_time, trace.arrivals_us.len()) {
                (0, 0) => "lost",
                (0, _) => "late",
                _ => "doubled",
            };
            eprintln!(
                "MVR1656_ROUND round={round} spin_us={round} verdict={verdict} \
                 spawned_us={spawned_us} announced_us={announced_us} accept_us={} \
                 arrivals_from_accept_us={:?} reader_ended={}",
                trace.accept_us, trace.arrivals_us, trace.ended
            );
            let mut events = evlog.dump();
            for ev in events.split_off(events_before.min(events.len())) {
                eprintln!("  node: {ev}");
            }
            anomalies.push((round, verdict));
        }
    }
    assert!(
        anomalies.is_empty(),
        "rounds (spin µs, verdict) whose new connection did not carry the \
         announce exactly once within {WINDOW:?} of the accept: {anomalies:?} \
         (timelines and node events above; `late` means the one copy arrived \
         within {LATE_GRACE:?} of the accept, `lost` that none did)"
    );
}
