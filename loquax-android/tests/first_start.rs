//! The first start "on this phone" (1798, render walk 2026-11-13 finding
//! 2): a fresh install that chooses its own phone must reach the chat
//! list, and one whose local mailbox cannot start must say so at once, in
//! words about this phone — not wait the join's whole budget and then ask
//! "Is the computer on?", the text for a mailbox on a computer.

use std::time::{Duration, Instant};

use loquax_android::schema::{Query, Start};
use loquax_android::session::Session;

/// How long the happy first start may take on a host: session start
/// (mailbox daemon, leaf, loopback join) plus the first list.
const REACH_THE_LIST: Duration = Duration::from_secs(30);

/// How long the unhappy first start may take: the local daemon's end is
/// known the moment it ends, so the join budget (60 s) must not pass.
const FAIL_FAST: Duration = Duration::from_secs(20);

fn start_here(dir: &std::path::Path) -> Result<Session, loquax_android::session::StartError> {
    Session::start(&Start {
        dir: dir.display().to_string(),
        here: true,
        lang: Some("en".to_string()),
        ..Start::default()
    })
}

/// The happy path of order 1798 step 4: a fresh profile, "on this phone",
/// and the chat list answers within [`REACH_THE_LIST`].
#[test]
fn a_fresh_first_start_on_this_phone_reaches_the_chat_list() {
    let dir = tempfile::tempdir().expect("app dir");
    // No dialling out: the mailbox's node runs offline, as every host test
    // of the phone's mailbox does.
    std::fs::write(dir.path().join("config"), "rendezvous_dial = off\n").expect("config");
    std::fs::create_dir_all(dir.path().join(loquax_android::mailbox::DIR)).expect("mailbox dir");
    std::fs::write(
        dir.path()
            .join(loquax_android::mailbox::DIR)
            .join("servers"),
        "127.0.0.1:1\n",
    )
    .expect("servers");
    let begin = Instant::now();
    let session = start_here(dir.path()).unwrap_or_else(|error| panic!("{error:?}"));
    session
        .query(Query::Conversations { search: None })
        .expect("the chat list");
    let took = begin.elapsed();
    session.stop();
    assert!(
        took < REACH_THE_LIST,
        "the first start took {took:?}, the bar is {REACH_THE_LIST:?}"
    );
}

/// The unhappy path of order 1798 step 4: the local mailbox cannot start
/// (here: its delivery lock is already held, the first failure the
/// Android runtime showed). The start must end within [`FAIL_FAST`]
/// — not sit out the join's 60 s budget against a daemon that is gone —
/// and its words must name this phone's mailbox and carry the daemon's
/// own reason, never the mailbox-on-a-computer question.
#[test]
fn a_mailbox_that_cannot_start_here_says_so_at_once_and_in_its_own_words() {
    let dir = tempfile::tempdir().expect("app dir");
    std::fs::write(dir.path().join("config"), "rendezvous_dial = off\n").expect("config");
    let mailbox_dir = dir.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox_dir).expect("mailbox dir");
    std::fs::write(mailbox_dir.join("servers"), "127.0.0.1:1\n").expect("servers");
    let _held = loquax::home::lock::DeliveryLock::acquire(&mailbox_dir, "this test")
        .expect("the test holds the delivery lock");
    let begin = Instant::now();
    let error = match start_here(dir.path()) {
        Err(error) => error,
        Ok(session) => {
            session.stop();
            panic!("the first start succeeded under a held delivery lock");
        }
    };
    let waited = begin.elapsed();
    assert!(
        waited < FAIL_FAST,
        "the failure took {waited:?} to surface, the bar is {FAIL_FAST:?}"
    );
    assert!(
        !error.text.to_lowercase().contains("computer"),
        "the words are about a computer: {:?}",
        error.text
    );
    assert!(
        error.text.to_lowercase().contains("phone"),
        "the words do not name this phone: {:?}",
        error.text
    );
    let details = error.details.unwrap_or_default();
    assert!(
        details.contains("already runs"),
        "the daemon's own reason is missing from the details: {details:?}"
    );
}
