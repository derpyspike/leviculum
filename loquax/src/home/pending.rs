//! A device's sends the home node has not taken yet (order 1881).
//!
//! A `/send` to the home used to be asked three times and then given up
//! with an error bubble. On miauhaus on 2026-11-15 the home was out of
//! reach for two hours (its daemon routed the home's own destination into
//! the mesh), and a reply Lew wrote on his phone in that time failed and
//! was gone. Now every submission is kept in a file under the device's
//! home directory before it is first asked, and stays there until the
//! home has taken it or refused it for good: the send runner asks again
//! while the home is out of reach, and one left over from an earlier run
//! (the app was closed, the phone restarted) is handed over before the
//! next pull. The home keeps one message per `client_id`
//! (`server.rs`, `outbox_by_client`), so asking again, from two runners at
//! once even, delivers it exactly once.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::ask::HomeError;
use super::wire::{self, Status, Submission};

/// The directory under the device's home directory that holds the files.
pub const DIR: &str = "home-pending";

/// The first wait before a send the home did not take is asked again.
pub const FIRST_WAIT: Duration = Duration::from_secs(15);
/// The longest wait between two asks: two minutes, so a home that comes
/// back has the message within about two minutes plus one ask.
pub const LONGEST_WAIT: Duration = Duration::from_secs(120);
/// How many `Refused(Failed)` answers - the home's store failed - a send
/// takes before it ends with that error (order 1986 finding 3a): a store
/// failure is usually passing (a busy database, a full disk being
/// cleared), so it is asked again like an unreachable home, but a home
/// that keeps failing must not be asked forever with the bubble sending.
/// Five asks are about four minutes under [`retry_wait`]. Only this
/// refusal is bounded; an unreachable home (`NoPath`, `NoLink`,
/// `Transfer`, `Timeout`) is waited out without bound.
pub const STORE_FAILED_ASKS: u32 = 5;

/// Whether a failed ask is worth asking again: the home was out of reach
/// or the answer was lost, or its store failed this time. Any other
/// refusal (not admitted, not identified), an answer that does not decode
/// or a home that does not know `/send` will say the same next time.
pub fn worth_retrying(error: &HomeError) -> bool {
    matches!(
        error,
        HomeError::NoPath
            | HomeError::NoLink(_)
            | HomeError::Transfer(_)
            | HomeError::Timeout
            | HomeError::Refused(Status::Failed)
    )
}

/// The wait before ask number `failed + 1`, after `failed` failed ones:
/// [`FIRST_WAIT`], doubled each time, never more than [`LONGEST_WAIT`].
pub fn retry_wait(failed: u32) -> Duration {
    let doubled = FIRST_WAIT.saturating_mul(1u32 << failed.saturating_sub(1).min(16));
    doubled.min(LONGEST_WAIT)
}

/// The kept submissions of one device, one file each, named by the time
/// they were kept and their `client_id`, so they go in the order written.
#[derive(Debug, Clone)]
pub struct Pending {
    dir: PathBuf,
}

impl Pending {
    /// The kept sends under the device's home directory `home`.
    pub fn at(home: &Path) -> Self {
        Self {
            dir: home.join(DIR),
        }
    }

    /// Keep `submission` until [`Self::taken`] is called for it. Written to
    /// a temporary file and renamed, so a crash leaves it whole or absent.
    /// A process-wide counter follows the time in the name: two kept in
    /// the same millisecond still list in the order written.
    pub fn keep(&self, submission: &Submission) -> std::io::Result<()> {
        use core::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        std::fs::create_dir_all(&self.dir)?;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!("{millis:020}-{seq:010}-{}.send", hex(&submission.client_id));
        let temporary = self.dir.join(format!("{name}.part"));
        std::fs::write(&temporary, wire::send_request(submission))?;
        std::fs::rename(&temporary, self.dir.join(name))
    }

    /// Forget the submission with `client_id`: the home took or refused it.
    /// Forgetting one already gone is no error, since two runners may
    /// hand the same one over.
    pub fn taken(&self, client_id: &[u8; 16]) -> std::io::Result<()> {
        let suffix = format!("-{}.send", hex(client_id));
        for path in self.files()? {
            if path.to_string_lossy().ends_with(&suffix) {
                match std::fs::remove_file(&path) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(error)
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Every kept submission, oldest first. A file that does not decode is
    /// skipped and named in the log; it is left for a person to look at.
    pub fn waiting(&self) -> std::io::Result<Vec<Submission>> {
        self.waiting_with(&mut |path| std::fs::read(path))
    }

    /// [`Self::waiting`] with `read` for a file's bytes, the seam its test
    /// drives. A file gone between the listing and the read (order 1986
    /// finding 3c) was taken by another runner handing the same sends
    /// over, not an error of the listing: it is passed by.
    fn waiting_with(
        &self,
        read: &mut dyn FnMut(&Path) -> std::io::Result<Vec<u8>>,
    ) -> std::io::Result<Vec<Submission>> {
        let mut kept = Vec::new();
        for path in self.files()? {
            let bytes = match read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            match wire::read_send_request(&bytes) {
                Ok(submission) => kept.push(submission),
                Err(_) => tracing::warn!("HOME pending send unreadable: {}", path.display()),
            }
        }
        Ok(kept)
    }

    fn files(&self) -> std::io::Result<Vec<PathBuf>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "send") {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }
}

/// One ask of the home to take a submission: the network in the runner,
/// a script in the tests.
pub trait Hand {
    /// Ask the home once to take `submission`; the message id it gave.
    fn hand(
        &mut self,
        submission: &Submission,
    ) -> impl Future<Output = Result<[u8; 32], HomeError>> + Send;
}

/// Hand `submission`, kept in `pending`, over until the home takes it or
/// refuses it for good, waiting [`retry_wait`] between asks the home was
/// out of reach for. `waiting` hears each such failure. The file is
/// forgotten on either end; the error of a refusal comes back.
pub async fn hand_until_taken(
    pending: &Pending,
    hand: &mut impl Hand,
    submission: &Submission,
    waiting: &mut (dyn FnMut(&HomeError, Duration) + Send),
) -> Result<[u8; 32], HomeError> {
    let mut failed = 0u32;
    let mut store_failed = 0u32;
    loop {
        match hand.hand(submission).await {
            Err(error) if worth_retrying(&error) => {
                if matches!(error, HomeError::Refused(Status::Failed)) {
                    store_failed += 1;
                    if store_failed >= STORE_FAILED_ASKS {
                        forget(pending, &submission.client_id);
                        return Err(error);
                    }
                }
                failed = failed.saturating_add(1);
                let wait = retry_wait(failed);
                waiting(&error, wait);
                tokio::time::sleep(wait).await;
            }
            end => {
                forget(pending, &submission.client_id);
                return end;
            }
        }
    }
}

/// Hand every kept submission over once, oldest first, and stop at the
/// first the home was out of reach for: the ones after it would fare no
/// better, and the pull this runs before should not wait for each. The
/// number the home took comes back. One the home refuses is forgotten and
/// told to `refused` (order 1986 finding 2b): the run that wrote it is
/// over, its bubble gone, so this is the only place the user can hear it.
pub async fn hand_leftovers(
    pending: &Pending,
    hand: &mut impl Hand,
    refused: &mut (dyn FnMut(&Submission, &HomeError) + Send),
) -> usize {
    let waiting = match pending.waiting() {
        Ok(waiting) => waiting,
        Err(error) => {
            tracing::warn!("HOME pending sends unreadable: {error}");
            return 0;
        }
    };
    let mut took = 0;
    for submission in waiting {
        match hand.hand(&submission).await {
            Ok(id) => {
                tracing::info!(
                    "HOME pending send accepted message={}",
                    crate::address::to_hex(&id)
                );
                forget(pending, &submission.client_id);
                took += 1;
            }
            Err(error) if worth_retrying(&error) => {
                tracing::info!("HOME pending sends wait: {error}");
                break;
            }
            Err(error) => {
                tracing::warn!("HOME pending send refused: {error}");
                refused(&submission, &error);
                forget(pending, &submission.client_id);
            }
        }
    }
    took
}

fn forget(pending: &Pending, client_id: &[u8; 16]) {
    if let Err(error) = pending.taken(client_id) {
        tracing::warn!("HOME pending send not forgotten: {error}");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn submission(id: u8) -> Submission {
        Submission {
            client_id: [id; 16],
            destination: [0xd0; 16],
            title: Vec::new(),
            content: format!("reply {id}").into_bytes(),
            fields: vec![(1, vec![2, 3])],
        }
    }

    /// A home that is out of reach until `back_at` on tokio's clock and
    /// keeps one message per `client_id` from then on, as `server.rs` does.
    struct Home {
        back_at: tokio::time::Instant,
        asks: Arc<Mutex<Vec<[u8; 16]>>>,
        taken: Arc<Mutex<Vec<[u8; 16]>>>,
        refuse: bool,
        /// Answer `Refused(Failed)` once back, as a home whose store
        /// fails on every ask does.
        fail: bool,
    }

    impl Hand for Home {
        async fn hand(&mut self, submission: &Submission) -> Result<[u8; 32], HomeError> {
            self.asks.lock().unwrap().push(submission.client_id);
            if tokio::time::Instant::now() < self.back_at {
                return Err(HomeError::NoLink("link closed (Timeout)".into()));
            }
            if self.refuse {
                return Err(HomeError::Refused(Status::NotAdmitted));
            }
            if self.fail {
                return Err(HomeError::Refused(Status::Failed));
            }
            let mut taken = self.taken.lock().unwrap();
            if !taken.contains(&submission.client_id) {
                taken.push(submission.client_id);
            }
            let mut id = [0; 32];
            id[..16].copy_from_slice(&submission.client_id);
            Ok(id)
        }
    }

    fn home(back_in: Duration) -> Home {
        Home {
            back_at: tokio::time::Instant::now() + back_in,
            asks: Arc::default(),
            taken: Arc::default(),
            refuse: false,
            fail: false,
        }
    }

    #[test]
    fn the_wait_doubles_from_fifteen_seconds_to_two_minutes() {
        let waits: Vec<u64> = (1..=6).map(|n| retry_wait(n).as_secs()).collect();
        assert_eq!(waits, [15, 30, 60, 120, 120, 120]);
        assert_eq!(retry_wait(u32::MAX), LONGEST_WAIT);
    }

    #[test]
    fn only_an_unreachable_home_is_asked_again() {
        assert!(worth_retrying(&HomeError::NoPath));
        assert!(worth_retrying(&HomeError::NoLink(String::new())));
        assert!(worth_retrying(&HomeError::Transfer(String::new())));
        assert!(worth_retrying(&HomeError::Timeout));
        assert!(worth_retrying(&HomeError::Refused(Status::Failed)));
        assert!(!worth_retrying(&HomeError::Refused(Status::NotAdmitted)));
        assert!(!worth_retrying(&HomeError::Malformed));
        assert!(!worth_retrying(&HomeError::Unsupported("/send")));
    }

    #[test]
    fn kept_sends_survive_a_new_reader_in_the_order_written_and_go_when_taken() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        assert!(pending.waiting().unwrap().is_empty());
        pending.keep(&submission(1)).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        pending.keep(&submission(2)).unwrap();

        let again = Pending::at(dir.path());
        assert_eq!(again.waiting().unwrap(), vec![submission(1), submission(2)]);
        again.taken(&[1; 16]).unwrap();
        again.taken(&[1; 16]).unwrap();
        assert_eq!(pending.waiting().unwrap(), vec![submission(2)]);
    }

    /// The field case: the home is out of reach for 25 minutes, then back.
    /// The send waits instead of failing, arrives once, and its file goes.
    #[tokio::test(start_paused = true)]
    async fn a_send_waits_out_an_unreachable_home_and_arrives_once() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        let reply = submission(7);
        pending.keep(&reply).unwrap();
        let mut home = home(Duration::from_secs(25 * 60));
        let started = tokio::time::Instant::now();
        let mut waits = 0;
        let taken = hand_until_taken(&pending, &mut home, &reply, &mut |_, _| waits += 1).await;

        let id = taken.expect("the home takes it once it is back");
        assert_eq!(&id[..16], &[7; 16]);
        assert_eq!(*home.taken.lock().unwrap(), vec![[7; 16]]);
        assert!(waits >= 12, "asked again while out of reach: {waits}");
        let took = started.elapsed();
        assert!(
            took >= Duration::from_secs(25 * 60)
                && took <= Duration::from_secs(25 * 60) + LONGEST_WAIT,
            "taken within one longest wait of the home's return: {took:?}"
        );
        assert!(pending.waiting().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_ends_the_send_with_its_error_and_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        let reply = submission(8);
        pending.keep(&reply).unwrap();
        let mut home = home(Duration::ZERO);
        home.refuse = true;
        let end = hand_until_taken(&pending, &mut home, &reply, &mut |_, _| {}).await;
        assert!(matches!(end, Err(HomeError::Refused(_))));
        assert_eq!(home.asks.lock().unwrap().len(), 1);
        assert!(pending.waiting().unwrap().is_empty());
    }

    /// A send left over from a run that ended while the home was away is
    /// handed over before the next pull, once, even when the runner of the
    /// next run asks for the same one too.
    #[tokio::test(start_paused = true)]
    async fn a_leftover_goes_before_the_next_pull_and_arrives_once() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(1)).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        pending.keep(&submission(2)).unwrap();

        let mut away = home(Duration::from_secs(60));
        assert_eq!(hand_leftovers(&pending, &mut away, &mut |_, _| {}).await, 0);
        assert_eq!(
            *away.asks.lock().unwrap(),
            vec![[1; 16]],
            "the first unreachable ask stops the round"
        );
        assert_eq!(pending.waiting().unwrap().len(), 2);

        let mut back = home(Duration::ZERO);
        let taken = Arc::clone(&back.taken);
        assert_eq!(hand_leftovers(&pending, &mut back, &mut |_, _| {}).await, 2);
        assert!(pending.waiting().unwrap().is_empty());
        // A runner that still held the second one asks for it again.
        let mut late = Home {
            taken: Arc::clone(&taken),
            ..home(Duration::ZERO)
        };
        hand_until_taken(&pending, &mut late, &submission(2), &mut |_, _| {})
            .await
            .unwrap();
        assert_eq!(*taken.lock().unwrap(), vec![[1; 16], [2; 16]]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_leftover_the_home_refuses_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(3)).unwrap();
        let mut home = home(Duration::ZERO);
        home.refuse = true;
        assert_eq!(hand_leftovers(&pending, &mut home, &mut |_, _| {}).await, 0);
        assert!(pending.waiting().unwrap().is_empty());
    }

    /// Finding 3a of order 1986 (audit of 1881): a home that answers
    /// `Refused(Failed)` - its store fails - is asked again, but not
    /// forever: after [`STORE_FAILED_ASKS`] such answers the send ends
    /// with that error, where an unreachable home (the 25-minute test
    /// above) is waited out without bound.
    #[tokio::test(start_paused = true)]
    async fn a_home_that_keeps_failing_its_store_ends_the_send_with_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        let reply = submission(6);
        pending.keep(&reply).unwrap();
        let mut failing = home(Duration::ZERO);
        failing.fail = true;
        let asks = Arc::clone(&failing.asks);
        let end = tokio::time::timeout(
            LONGEST_WAIT * 20,
            hand_until_taken(&pending, &mut failing, &reply, &mut |_, _| {}),
        )
        .await
        .expect("the send ends instead of asking forever");
        assert!(
            matches!(end, Err(HomeError::Refused(Status::Failed))),
            "{end:?}"
        );
        // The bound itself: five asks, about four minutes of retry_wait.
        assert_eq!(asks.lock().unwrap().len(), STORE_FAILED_ASKS as usize);
        assert!(pending.waiting().unwrap().is_empty());
    }

    /// Finding 2b of order 1986 (audit of 1881): a leftover the home
    /// finally refuses is told to the caller with its refusal, so the
    /// user hears of a message that is now lost for good.
    #[tokio::test(start_paused = true)]
    async fn a_refused_leftover_is_told_with_its_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(4)).unwrap();
        let mut refusing = home(Duration::ZERO);
        refusing.refuse = true;
        let mut told = Vec::new();
        let count = hand_leftovers(&pending, &mut refusing, &mut |refused, error| {
            told.push((refused.clone(), error.to_string()))
        })
        .await;
        assert_eq!(count, 0);
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].0, submission(4));
        assert_eq!(
            told[0].1,
            HomeError::Refused(Status::NotAdmitted).to_string()
        );
        // An unreachable home is no refusal: nothing is told.
        pending.keep(&submission(5)).unwrap();
        let mut gone = home(Duration::from_secs(60));
        let mut spoken = 0;
        hand_leftovers(&pending, &mut gone, &mut |_, _| spoken += 1).await;
        assert_eq!(spoken, 0);
    }

    /// Finding 2a's order half (order 1986): sends kept within the same
    /// millisecond still list in the order written, by the counter in the
    /// file name. Without it the tail of the name, the random client id,
    /// decided.
    #[test]
    fn sends_kept_in_the_same_millisecond_list_in_the_order_written() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        for id in 1..=9 {
            pending.keep(&submission(id)).unwrap();
        }
        let kept: Vec<u8> = pending
            .waiting()
            .unwrap()
            .iter()
            .map(|s| s.client_id[0])
            .collect();
        assert_eq!(kept, (1..=9).collect::<Vec<u8>>());
    }

    /// Finding 3c of order 1986 (audit of 1881): a file gone between the
    /// listing and the read was taken by another runner handing the same
    /// sends over - the listing passes it by instead of failing whole.
    /// Any other read error still fails it.
    #[test]
    fn a_file_taken_between_listing_and_read_is_passed_by() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(1)).unwrap();
        pending.keep(&submission(2)).unwrap();
        let gone = format!("-{}.send", hex(&[1; 16]));
        let kept = pending
            .waiting_with(&mut |path| match path.to_string_lossy().ends_with(&gone) {
                true => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
                false => std::fs::read(path),
            })
            .unwrap();
        assert_eq!(kept, vec![submission(2)]);
        let denied = pending.waiting_with(&mut |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert_eq!(
            denied.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
