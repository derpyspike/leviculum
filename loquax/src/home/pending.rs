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
use std::sync::Mutex;
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
/// How many doubted answers (`Refused(Failed)`, `Malformed`) a send
/// takes before it ends with that error (order 1986 finding 3a;
/// `Malformed` joined by the
/// audit of 1986): a store failure or a mangled answer is usually
/// passing (a busy database, a lost frame), so it is asked again like an
/// unreachable home, but a home that keeps answering so must not be
/// asked forever with the bubble sending. Five asks are about four
/// minutes under [`retry_wait`]. Only doubted answers are bounded; an
/// unreachable home (`NoPath`, `NoLink`, `Transfer`, `Timeout`) is
/// waited out without bound.
pub const STORE_FAILED_ASKS: u32 = 5;

/// How old a temporary file must be before [`Pending::waiting`] sweeps
/// it as an orphan: a keep's write takes milliseconds, so ten minutes
/// is a kill between the write and the rename, never a write still in
/// progress.
const ORPHAN_AGE: Duration = Duration::from_secs(600);

/// Whether a failed ask is worth asking again: the home was out of reach
/// or the answer was lost, its store failed this time, or its answer did
/// not decode (one mangled answer is no verdict on the message, audit of
/// 1986). A refusal that names the client (not admitted, not identified)
/// or a home that does not know `/send` will say the same next time.
pub fn worth_retrying(error: &HomeError) -> bool {
    matches!(
        error,
        HomeError::NoPath
            | HomeError::NoLink(_)
            | HomeError::Transfer(_)
            | HomeError::Timeout
            | HomeError::Refused(Status::Failed)
            | HomeError::Malformed
    )
}

/// The retryable answers [`STORE_FAILED_ASKS`] bounds: the home DID
/// answer, but with a store failure or an answer that did not decode.
/// Unlike an unreachable home, one that keeps answering so is broken
/// rather than away, and waiting it out without bound would hold the
/// bubble in "sending" forever.
fn doubted(error: &HomeError) -> bool {
    matches!(
        error,
        HomeError::Refused(Status::Failed) | HomeError::Malformed
    )
}

/// The sends this process holds queued or running, by their directory
/// and client id: held when a frontend queues one
/// ([`crate::home::runner::kept_submission`]), released in
/// [`Pending::taken`] once the home has taken or refused it. A pull's
/// handover ([`hand_leftovers`]) leaves these to their own runner
/// (audit of 1986): handing one over as a leftover answered it a second
/// time behind its runner's back, and the pulled archive then gave it a
/// second bubble while its own still said sending. Files from an
/// earlier run of the process are in no registry and are handed over.
static HELD: Mutex<Vec<(PathBuf, [u8; 16])>> = Mutex::new(Vec::new());

/// How often a leftover was given a doubted answer ([`doubted`]) across
/// this process's pulls, by directory and client id: at
/// [`STORE_FAILED_ASKS`] the leftover ends as refused
/// ([`hand_leftovers`]). A restart starts the count afresh — the bound
/// guards a run's pulls against a home that is broken rather than away,
/// not the file's whole life.
static DOUBTED: Mutex<Vec<(PathBuf, [u8; 16], u32)>> = Mutex::new(Vec::new());

/// A lock on `book` that outlives a panic under it: the books hold plain
/// data, so the last write before a poisoning panic is still whole.
fn book<T>(book: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    book.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One more doubted answer for `client_id` under `dir`: the count so far.
fn doubted_again(dir: &Path, client_id: &[u8; 16]) -> u32 {
    let mut doubts = book(&DOUBTED);
    match doubts
        .iter_mut()
        .find(|(d, id, _)| d == dir && id == client_id)
    {
        Some((_, _, count)) => {
            *count += 1;
            *count
        }
        None => {
            doubts.push((dir.to_path_buf(), *client_id, 1));
            1
        }
    }
}

/// The doubt over `client_id` under `dir` is settled either way: the
/// home took it, or the leftover ended as refused.
fn doubt_settled(dir: &Path, client_id: &[u8; 16]) {
    book(&DOUBTED).retain(|(d, id, _)| !(d == dir && id == client_id));
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

    /// Keep `submission` until [`Self::taken`] is called for it. Written
    /// durably and renamed into place
    /// ([`leviculum_std::atomic_write_durable`]): when this returns the
    /// file is whole and on the disk, so a send may count as queued — a
    /// crash right after cannot lose it. A process-wide counter follows
    /// the time in the name: two kept in the same millisecond still list
    /// in the order written.
    pub fn keep(&self, submission: &Submission) -> std::io::Result<()> {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        self.keep_at(millis, submission)
    }

    /// [`Self::keep`] at `millis` on the wall clock: the seam the order
    /// test fixes, so the counter's work shows without racing the clock.
    fn keep_at(&self, millis: u128, submission: &Submission) -> std::io::Result<()> {
        use core::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        std::fs::create_dir_all(&self.dir)?;
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!("{millis:020}-{seq:010}-{}.send", hex(&submission.client_id));
        leviculum_std::atomic_write_durable(&self.dir.join(name), &wire::send_request(submission))
    }

    /// Say this process holds the send `client_id` queued or running: a
    /// pull's handover leaves it to its runner ([`hand_leftovers`]).
    /// Released by [`Self::taken`].
    pub fn hold(&self, client_id: [u8; 16]) {
        let mut held = book(&HELD);
        if !held
            .iter()
            .any(|(dir, id)| *dir == self.dir && *id == client_id)
        {
            held.push((self.dir.clone(), client_id));
        }
    }

    /// Whether this process holds the send `client_id` queued or running.
    pub fn holds(&self, client_id: &[u8; 16]) -> bool {
        book(&HELD)
            .iter()
            .any(|(dir, id)| *dir == self.dir && id == client_id)
    }

    /// Forget the submission with `client_id`: the home took or refused it.
    /// Forgetting one already gone is no error, since two runners may
    /// hand the same one over.
    pub fn taken(&self, client_id: &[u8; 16]) -> std::io::Result<()> {
        book(&HELD).retain(|(dir, id)| !(*dir == self.dir && id == client_id));
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

    /// Forget the kept send read from `path`, whatever its name says
    /// (audit of 1986): the file that was actually handed over goes,
    /// and any file named by `client_id` with it, so one misnamed by
    /// hand cannot be handed over forever.
    fn took(&self, path: &Path, client_id: &[u8; 16]) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        self.taken(client_id)
    }

    /// Every kept submission, oldest first. A file that does not decode is
    /// skipped and named in the log; it is left for a person to look at.
    pub fn waiting(&self) -> std::io::Result<Vec<Submission>> {
        Ok(self
            .waiting_files()?
            .into_iter()
            .map(|(_, submission)| submission)
            .collect())
    }

    /// [`Self::waiting`], each submission with the file it was read from:
    /// what [`hand_leftovers`] removes is the file it read, not a name.
    fn waiting_files(&self) -> std::io::Result<Vec<(PathBuf, Submission)>> {
        self.waiting_with(&mut |path| std::fs::read(path))
    }

    /// [`Self::waiting_files`] with `read` for a file's bytes, the seam
    /// its test drives. A file gone between the listing and the read
    /// (order 1986 finding 3c) was taken by another runner handing the
    /// same sends over, not an error of the listing: it is passed by.
    fn waiting_with(
        &self,
        read: &mut dyn FnMut(&Path) -> std::io::Result<Vec<u8>>,
    ) -> std::io::Result<Vec<(PathBuf, Submission)>> {
        self.sweep_orphans(ORPHAN_AGE);
        let mut kept = Vec::new();
        for path in self.files()? {
            let bytes = match read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            match wire::read_send_request(&bytes) {
                Ok(submission) => kept.push((path, submission)),
                Err(_) => tracing::warn!("HOME pending send unreadable: {}", path.display()),
            }
        }
        Ok(kept)
    }

    /// Remove temporary files older than `age`: `.tmp`
    /// ([`leviculum_std::atomic_write_durable`]) and `.part` (the writer
    /// before the audit of 1986) are left only by a kill between the
    /// write and the rename. A write in progress is younger than any
    /// sensible `age`; best effort, an orphan that stays is seen again.
    fn sweep_orphans(&self, age: Duration) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "tmp" || e == "part") {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|written| written.elapsed().is_ok_and(|been| been >= age));
            if old {
                let _ = std::fs::remove_file(&path);
            }
        }
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
    let mut doubts = 0u32;
    loop {
        match hand.hand(submission).await {
            Err(error) if worth_retrying(&error) => {
                if doubted(&error) {
                    doubts += 1;
                    if doubts >= STORE_FAILED_ASKS {
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

/// Hand every leftover over once, oldest first, and stop at the first
/// the home was out of reach for: the ones after it would fare no
/// better, and the pull this runs before should not wait for each. A
/// send this process still holds queued or running ([`Pending::hold`])
/// is no leftover and is left to its own runner (audit of 1986). The
/// number the home took comes back. One the home refuses is forgotten
/// and told to `refused` (order 1986 finding 2b): the run that wrote it
/// is over, and a bubble the history restored for it does not hear the
/// refusal yet, so this is the only place the user can hear it. One
/// given a doubted answer (`Refused(Failed)`, `Malformed`) is neither
/// taken nor refused: it stays for the next pull, the ones behind it
/// go on, and at
/// [`STORE_FAILED_ASKS`] such answers across this run's pulls it ends
/// as refused after all.
pub async fn hand_leftovers(
    pending: &Pending,
    hand: &mut impl Hand,
    refused: &mut (dyn FnMut(&Submission, &HomeError) + Send),
) -> usize {
    let waiting = match pending.waiting_files() {
        Ok(waiting) => waiting,
        Err(error) => {
            tracing::warn!("HOME pending sends unreadable: {error}");
            return 0;
        }
    };
    let mut took = 0;
    for (path, submission) in waiting {
        if pending.holds(&submission.client_id) {
            continue;
        }
        match hand.hand(&submission).await {
            Ok(id) => {
                tracing::info!(
                    "HOME pending send accepted message={}",
                    crate::address::to_hex(&id)
                );
                doubt_settled(&pending.dir, &submission.client_id);
                forget_file(pending, &path, &submission.client_id);
                took += 1;
            }
            Err(error) if doubted(&error) => {
                let doubts = doubted_again(&pending.dir, &submission.client_id);
                if doubts >= STORE_FAILED_ASKS {
                    tracing::warn!("HOME pending send given up after {doubts} asks: {error}");
                    refused(&submission, &error);
                    doubt_settled(&pending.dir, &submission.client_id);
                    forget_file(pending, &path, &submission.client_id);
                } else {
                    tracing::info!("HOME pending send doubted ({doubts}): {error}");
                }
            }
            Err(error) if worth_retrying(&error) => {
                tracing::info!("HOME pending sends wait: {error}");
                break;
            }
            Err(error) => {
                tracing::warn!("HOME pending send refused: {error}");
                refused(&submission, &error);
                forget_file(pending, &path, &submission.client_id);
            }
        }
    }
    took
}

/// [`Pending::took`], with a failure in the log instead of the way.
fn forget_file(pending: &Pending, path: &Path, client_id: &[u8; 16]) {
    if let Err(error) = pending.took(path, client_id) {
        tracing::warn!("HOME pending send not forgotten: {error}");
    }
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
    fn only_a_refusal_that_names_the_client_is_final() {
        assert!(worth_retrying(&HomeError::NoPath));
        assert!(worth_retrying(&HomeError::NoLink(String::new())));
        assert!(worth_retrying(&HomeError::Transfer(String::new())));
        assert!(worth_retrying(&HomeError::Timeout));
        assert!(worth_retrying(&HomeError::Refused(Status::Failed)));
        // One mangled answer is no verdict on the message (audit of 1986).
        assert!(worth_retrying(&HomeError::Malformed));
        assert!(!worth_retrying(&HomeError::Refused(Status::NotAdmitted)));
        assert!(!worth_retrying(&HomeError::Unsupported("/send")));
        // The bounded ones among the retryable: the home answered.
        assert!(doubted(&HomeError::Refused(Status::Failed)));
        assert!(doubted(&HomeError::Malformed));
        assert!(!doubted(&HomeError::Timeout));
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
    /// file name. The clock is fixed through the seam and the client ids
    /// descend, so without the counter the names would sort by the id
    /// tail — descending — and the listing would reverse.
    #[test]
    fn sends_kept_in_the_same_millisecond_list_in_the_order_written() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        for id in (1..=9).rev() {
            pending.keep_at(77, &submission(id)).unwrap();
        }
        let kept: Vec<u8> = pending
            .waiting()
            .unwrap()
            .iter()
            .map(|s| s.client_id[0])
            .collect();
        assert_eq!(kept, (1..=9).rev().collect::<Vec<u8>>());
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
        let kept: Vec<Submission> = pending
            .waiting_with(&mut |path| match path.to_string_lossy().ends_with(&gone) {
                true => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
                false => std::fs::read(path),
            })
            .unwrap()
            .into_iter()
            .map(|(_, submission)| submission)
            .collect();
        assert_eq!(kept, vec![submission(2)]);
        let denied = pending
            .waiting_with(&mut |_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
        assert_eq!(
            denied.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    /// What a [`Script`] answers one ask with: an error, or `None`, taken.
    type Answer = Box<dyn FnMut(&[u8; 16]) -> Option<HomeError> + Send>;

    /// A home that answers each ask as the script says (`None` is taken),
    /// and keeps one message per `client_id` as `server.rs` does.
    struct Script {
        answers: Answer,
        asks: Arc<Mutex<Vec<[u8; 16]>>>,
        taken: Arc<Mutex<Vec<[u8; 16]>>>,
    }

    impl Script {
        fn new(answers: impl FnMut(&[u8; 16]) -> Option<HomeError> + Send + 'static) -> Self {
            Self {
                answers: Box::new(answers),
                asks: Arc::default(),
                taken: Arc::default(),
            }
        }
    }

    impl Hand for Script {
        async fn hand(&mut self, submission: &Submission) -> Result<[u8; 32], HomeError> {
            self.asks.lock().unwrap().push(submission.client_id);
            if let Some(error) = (self.answers)(&submission.client_id) {
                return Err(error);
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

    /// Audit of 1986: a send this process holds queued or running
    /// ([`Pending::hold`], as `kept_submission` queues it) is invisible
    /// to a pull's handover — not asked, not told — and its refusal
    /// reaches the user exactly once, through its own runner's end. A
    /// file nothing holds, an earlier run's, is still handed over.
    #[tokio::test(start_paused = true)]
    async fn a_queued_send_a_pull_cannot_see_is_refused_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        let queued = submission(0x51);
        pending.hold(queued.client_id);
        pending.keep(&queued).unwrap();
        let mut refusing = home(Duration::ZERO);
        refusing.refuse = true;
        let asks = Arc::clone(&refusing.asks);
        let mut told = 0;
        assert_eq!(
            hand_leftovers(&pending, &mut refusing, &mut |_, _| told += 1).await,
            0
        );
        assert!(
            asks.lock().unwrap().is_empty(),
            "the held send is not asked"
        );
        assert_eq!(told, 0, "and nothing is said of it");
        assert_eq!(pending.waiting().unwrap(), vec![queued.clone()]);
        // Its own runner hears the refusal: the one error the user sees.
        let end = hand_until_taken(&pending, &mut refusing, &queued, &mut |_, _| {}).await;
        assert!(matches!(end, Err(HomeError::Refused(Status::NotAdmitted))));
        assert_eq!(asks.lock().unwrap().len(), 1);
        assert!(pending.waiting().unwrap().is_empty());
        assert!(
            !pending.holds(&queued.client_id),
            "the end released the hold"
        );
        // An earlier run's leftover is held by nothing: handed over.
        pending.keep(&submission(0x52)).unwrap();
        let mut back = home(Duration::ZERO);
        assert_eq!(hand_leftovers(&pending, &mut back, &mut |_, _| {}).await, 1);
    }

    /// Audit of 1986: a leftover the home keeps answering
    /// `Refused(Failed)` for neither blocks the ones behind it nor waits
    /// forever — at [`STORE_FAILED_ASKS`] such answers across this run's
    /// pulls it ends as refused, with its error told.
    #[tokio::test(start_paused = true)]
    async fn a_failing_leftover_does_not_hold_back_the_one_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(0x41)).unwrap();
        pending.keep(&submission(0x42)).unwrap();
        let mut home =
            Script::new(|id| (id == &[0x41; 16]).then_some(HomeError::Refused(Status::Failed)));
        let taken = Arc::clone(&home.taken);
        let told = Mutex::new(Vec::new());
        let mut refused = |submission: &Submission, error: &HomeError| {
            told.lock()
                .unwrap()
                .push((submission.client_id, error.to_string()))
        };
        // Pull one: the failing one is passed over, the one behind goes.
        assert_eq!(hand_leftovers(&pending, &mut home, &mut refused).await, 1);
        assert_eq!(*taken.lock().unwrap(), vec![[0x42; 16]]);
        assert!(told.lock().unwrap().is_empty());
        assert_eq!(pending.waiting().unwrap(), vec![submission(0x41)]);
        // Pulls two to four: still waiting on the doubt.
        for _ in 2..=4 {
            assert_eq!(hand_leftovers(&pending, &mut home, &mut refused).await, 0);
            assert!(told.lock().unwrap().is_empty());
        }
        // The fifth doubted answer ends it as refused.
        assert_eq!(hand_leftovers(&pending, &mut home, &mut refused).await, 0);
        let told = told.into_inner().unwrap();
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].0, [0x41; 16]);
        assert!(pending.waiting().unwrap().is_empty());
    }

    /// Audit of 1986: a `Malformed` answer is no final refusal for a
    /// kept send. The file stays, nobody is told, and the one behind it
    /// still goes.
    #[tokio::test(start_paused = true)]
    async fn a_mangled_answer_leaves_the_file_and_tells_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(0x61)).unwrap();
        pending.keep(&submission(0x62)).unwrap();
        let mut home = Script::new(|id| (id == &[0x61; 16]).then_some(HomeError::Malformed));
        let mut told = 0;
        assert_eq!(
            hand_leftovers(&pending, &mut home, &mut |_, _| told += 1).await,
            1
        );
        assert_eq!(told, 0, "a mangled answer is told to nobody");
        assert_eq!(pending.waiting().unwrap(), vec![submission(0x61)]);
    }

    /// Audit of 1986: a home that keeps mangling its answers ends the
    /// send with that error at the same bound as a failing store, where
    /// an unreachable one is waited out without bound.
    #[tokio::test(start_paused = true)]
    async fn a_home_that_keeps_mangling_its_answers_ends_the_send_too() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        let reply = submission(0xa1);
        pending.keep(&reply).unwrap();
        let mut home = Script::new(|_| Some(HomeError::Malformed));
        let asks = Arc::clone(&home.asks);
        let end = tokio::time::timeout(
            LONGEST_WAIT * 20,
            hand_until_taken(&pending, &mut home, &reply, &mut |_, _| {}),
        )
        .await
        .expect("the send ends instead of asking forever");
        assert!(matches!(end, Err(HomeError::Malformed)), "{end:?}");
        assert_eq!(asks.lock().unwrap().len(), STORE_FAILED_ASKS as usize);
        assert!(pending.waiting().unwrap().is_empty());
    }

    /// Audit of 1986: the handover removes the file it actually read,
    /// whatever its name says — one misnamed by hand cannot be handed
    /// over forever.
    #[tokio::test(start_paused = true)]
    async fn a_file_named_for_another_id_still_goes_when_taken() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(0x71)).unwrap();
        let kept_dir = dir.path().join(DIR);
        let path = std::fs::read_dir(&kept_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let misnamed = path
            .to_string_lossy()
            .replace(&hex(&[0x71; 16]), &hex(&[0x72; 16]));
        std::fs::rename(&path, &misnamed).unwrap();
        assert_eq!(pending.waiting().unwrap(), vec![submission(0x71)]);
        let mut home = home(Duration::ZERO);
        assert_eq!(hand_leftovers(&pending, &mut home, &mut |_, _| {}).await, 1);
        assert!(
            pending.waiting().unwrap().is_empty(),
            "the file read was removed, whatever its name says"
        );
    }

    /// Audit of 1986: the kept file is whole and on disk when `keep`
    /// returns — nothing temporary beside it — so the send may count as
    /// queued. The printed line is the measured time of a 1 MB keep.
    #[test]
    fn a_keep_is_on_disk_whole_before_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        let mut big = submission(0x81);
        big.content = vec![0x42; 1_000_000];
        let started = std::time::Instant::now();
        pending.keep(&big).unwrap();
        let took = started.elapsed();
        println!("KEEP_1MB_MS {}", took.as_millis());
        let names: Vec<String> = std::fs::read_dir(dir.path().join(DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names[0].ends_with(".send"), "{names:?}");
        assert_eq!(pending.waiting().unwrap(), vec![big]);
    }

    /// Audit of 1986: a temporary file a kill orphaned is swept, and one
    /// young enough to be a write in progress is left alone.
    #[test]
    fn orphaned_temporaries_go_but_a_write_in_progress_stays() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path());
        pending.keep(&submission(0x91)).unwrap();
        let kept_dir = dir.path().join(DIR);
        std::fs::write(kept_dir.join("stale.part"), b"x").unwrap();
        std::fs::write(kept_dir.join("stale.tmp"), b"x").unwrap();
        // Young temporaries may still be written: the listing leaves them.
        assert_eq!(pending.waiting().unwrap().len(), 1);
        assert!(kept_dir.join("stale.part").exists());
        assert!(kept_dir.join("stale.tmp").exists());
        // Old ones are a kill between the write and the rename: swept.
        pending.sweep_orphans(Duration::ZERO);
        assert!(!kept_dir.join("stale.part").exists());
        assert!(!kept_dir.join("stale.tmp").exists());
        assert_eq!(pending.waiting().unwrap().len(), 1, "the kept send stays");
    }
}
