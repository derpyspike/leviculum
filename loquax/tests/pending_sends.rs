//! Order 1986 finding 3b (audit of 1881, Refs #232): the waiting send
//! against the real home server. A client's `/send` kept in
//! `home-pending/` waits out a mailbox (`loquax::home::daemon`, serving
//! with `server.rs` on a real store) that is away for several retry
//! waits, no error reaches the bubble meanwhile, and when the mailbox is
//! back the message arrives exactly once - the pull's handover and the
//! still-running send runner ask for the same submission, and the store's
//! `outbox_by_client` folds both asks into one message.

use std::time::Duration;

use loquax::home::runner::{self, HomeClient};
use loquax::home::{client, pending};
use loquax::outbox::{Command, Outbox, OutboxEvent, OutboxGone};
use loquax::ui::UiEvent;

#[path = "support/reach.rs"]
mod reach;

use reach::{entry, logs, wait_until, Mailbox, Phone, Relay};

/// A tap that takes every command and has no events: the runners' settle
/// waits are zero here, and nothing reads the engine.
struct NullTap;

impl Outbox for NullTap {
    fn submit(&self, _: Command) -> Result<(), OutboxGone> {
        Ok(())
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_waits_out_the_real_mailbox_and_arrives_exactly_once() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = Mailbox::start(vec![entry(relay.peer)]).await;
    let phone = Phone::start(vec![entry(relay.ap)], &mailbox, true).await;
    let node = phone.attached.node();
    let home = mailbox.home();
    let public_key = mailbox.public_key;
    wait_until("the phone's path to the mailbox", 15, || {
        node.has_path(&home)
    })
    .await;
    client::join(
        &node,
        &public_key,
        &phone.device,
        &mailbox.token,
        "phone",
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("join: {error}"));
    client::save_client(phone.dir.path(), &public_key).expect("client file");

    // The mailbox goes away before the send is queued.
    let servers = vec![entry(relay.peer)];
    let dir = mailbox.stop_keeping().await;

    let budget = Duration::from_secs(5);
    let home_dir = phone.dir.path().to_path_buf();
    let home_client = HomeClient {
        public_key,
        device: phone.device.clone(),
        node: node.clone(),
    };
    let submission = runner::kept_submission(
        &home_dir,
        [0x5a; 16],
        "kept while the mailbox was away".to_string(),
        Vec::new(),
    );
    let client_id = submission.client_id;
    let send = {
        let (client, home_dir) = (home_client.clone(), home_dir.clone());
        tokio::spawn(async move {
            let mut tap = NullTap;
            runner::send_home(
                &mut tap,
                &client,
                &home_dir,
                submission,
                budget,
                Duration::ZERO,
                &mut |_| {},
            )
            .await
        })
    };

    // Several retry waits pass with the mailbox away: the runner is still
    // asking - no end, so no `SendDone(Err)` reached any bubble - and the
    // kept file is still there.
    tokio::time::sleep(Duration::from_secs(50)).await;
    assert!(!send.is_finished(), "the send waits instead of ending");
    let kept = pending::Pending::at(&home_dir);
    assert_eq!(kept.waiting().expect("the listing").len(), 1);

    // The mailbox comes back on its kept state; the next pull hands the
    // kept send over while the send runner still holds it, and refuses
    // nothing.
    let mailbox = Mailbox::start_in(dir, servers, false).await;
    wait_until("the path to the returned mailbox", 30, || {
        node.has_path(&home)
    })
    .await;
    let mut tap = NullTap;
    let mut refused = Vec::new();
    let end = runner::pull_watching(&mut tap, &home_client, &home_dir, budget, &mut |event| {
        if let UiEvent::PendingRefused { error, .. } = event {
            refused.push(error);
        }
    })
    .await;
    assert!(end.result.is_ok(), "the pull: {:?}", end.result);
    assert!(refused.is_empty(), "nothing was refused: {refused:?}");

    // The runner's own next ask finds the message in the store by its
    // client id and ends well: once, not twice.
    let event = tokio::time::timeout(Duration::from_secs(240), send)
        .await
        .expect("the send ends once the mailbox is back")
        .expect("the send task");
    assert!(matches!(event, UiEvent::SendDone(Ok(()))), "{event:?}");
    assert!(kept.waiting().expect("the listing").is_empty());

    drop(node);
    phone.attached.stop().await;
    let dir = mailbox.stop_keeping().await;
    relay.stop().await;

    // The mailbox's own store holds exactly one message for the client id.
    let identity =
        loquax::identity::load_or_create(&dir.path().join("identity")).expect("identity");
    let address = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(dir.path(), address).expect("the store");
    store
        .outbox_by_client(&client_id)
        .expect("the lookup")
        .expect("the one message");
}
