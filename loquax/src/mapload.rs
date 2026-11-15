//! The map's autoload (`docs/src/concepts/loquax-map.md`, section 7,
//! decision 3 as Lew revised it on 2026-10-28; leviculum 1089): the regions
//! the model asks for ([`MapWant`]) cut from a PMTiles archive read by HTTP
//! range requests and written into the maps directory, where every painter
//! picks them up ([`crate::picture::Maps`]).
//!
//! - One load at a time, on a thread of its own, so a key never waits for
//!   one; the others queue, a box asked twice once.
//! - The queue is ordered by what is on screen (1292,
//!   [`crate::ui::viewload`]): a box asked for by hand or for the screen
//!   first, then the boxes next to it, then those the view left, at most
//!   [`MAX_AWAY`] of them. A load runs a round of its cut at a time
//!   ([`leviculum_mapcut::load::Loading`], 1836), and a load whose area
//!   left the screen pauses at its next round while anything for the
//!   screen runs or waits, its file kept and drawn as far as it came; it
//!   goes on from there once the screen has what it wants, before any
//!   new box starts. At most [`MAX_RUNNING`] loads run, at most
//!   [`MAX_PAUSED`] wait paused; past that the worst-ranked paused load
//!   is dropped, its file removed and its box queued again.
//! - The screen's own box at its zoom (1527, [`LoadReason::Screen`]) goes
//!   before the cells under the screen and starts beside any load but
//!   another for the screen; a cell for the screen waits until it landed,
//!   so the screen's tiles draw before a byte of the cell arrives. A
//!   screen's box the view left is dropped while it waits: the cells
//!   hold its area. A screen's box a running load brings is not asked
//!   twice, and that load ranks as the screen's, so the cells wait for
//!   it too (1616).
//! - A box that failed stays queued and the loader retries it on its own
//!   clock (1811, the load walk's `fault_recovers`): a box for the screen
//!   after [`SCREEN_RETRY_FIRST_MS`] doubling to [`SCREEN_RETRY_MAX_MS`],
//!   any other after [`BACKOFF_FIRST_MS`] doubling to [`BACKOFF_MAX_MS`],
//!   so a transient fault heals with no key and no new decision, and a
//!   source that is down is not asked again on every frame. While a box
//!   for the screen waits to retry nothing else starts: the link is not
//!   spent next to a screen that lacks its data. A landing clears every
//!   wait: the source answers again. A box that found no internet is no
//!   failure of the box: it retries after [`OFFLINE_RETRY_MS`], and
//!   "offline" holds only until a load starts. An ask of a box already
//!   waiting is "queued": [`Loader::ask`] is also called on every frame
//!   (a place, the pins), and a wait a frame could end is no wait.
//! - No internet is said once, not per box, until a load succeeds again.
//! - The region is written in place under its own name, as its tiles
//!   arrive (1291): coarse to fine, a round at a time, and a painter draws
//!   every tile there, the nearest coarser one standing in for the rest
//!   ([`leviculum_map::region::Region::stand_in`]). Each round bumps
//!   [`Maps::progress`], at most every [`PROGRESS_EVERY_MS`], so a frame
//!   on screen draws again; once complete it is checked whole and lands
//!   ([`Maps::changed`], by the shell). A load that fails removes its file.
//! - A load whose name is taken, a region installed before that it
//!   replaces, writes beside it under `PART_SUFFIX` and renames over it
//!   once the new file verifies (1727): the old region draws until then,
//!   and a load that fails leaves it as it was.
//! - Autoloaded regions are a cache under a cap, named [`AUTO_PREFIX`],
//!   evicted least recently viewed first (a painter marks a region viewed
//!   by its file's modification time). A file without the prefix was placed
//!   by hand and is never evicted, nor counted.
//!
//! - A region that landed wants its height packs (1863): the blocks of
//!   the height catalogue ([`leviculum_mapcut::heights`]) under its box,
//!   each fetched by one range request and written beside the region as
//!   `auto-h<zoom>-<x>-<y>.lmh`, under a `.part` name first and renamed
//!   once it checked whole, so a painter never indexes half a pack. The
//!   packs queue behind the regions: one fetch at a time, the screen's
//!   region's first, and none starts while a region for the screen runs
//!   or waits. They count in the cache cap and go with the last
//!   autoloaded region over them ([`evict`]). The autoloaded regions on
//!   disk want theirs too, once a session from the view's first decision
//!   on ([`Loader::schedule`], the autoload on), the most recently viewed
//!   first, after every region that lands. A pack
//!   that failed is retried on the long ladder; so is a catalogue the
//!   server does not have (no such file, or not a catalogue), once a rung
//!   for every pack waiting, since the default source's catalogue may come
//!   after the first regions; until then the map draws as it did without
//!   relief. No line is shown for a pack: the relief is a finish, its
//!   absence is no fault. A block or directory past the catalogue's
//!   limits ([`BLOCK_MAX`], [`DIRECTORY_MAX`]) is never asked for.
//!
//! The source is `map_source` from the config, or the newest Protomaps
//! daily build ([`leviculum_mapcut::http::daily`]) found once a session.
//! The height catalogue is `height_source` from the config, else
//! [`CATALOGUE_NAME`] beside a configured `map_source`, else
//! [`HEIGHTS_DEFAULT`] ([`heights_url`]).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Instant, SystemTime};
use std::vec::Vec;

use leviculum_map::region::{Bounds, Region};
use leviculum_mapcut::heights::{
    local_pack, BlockEntry, Catalogue, BLOCK_MAX, CATALOGUE_NAME, DIRECTORY_MAX,
};
use leviculum_mapcut::http::{self, HttpError, HttpRange};
use leviculum_mapcut::load::{plan, Empty, LoadProgress, Loaded, Loading, TooLarge, Want};
use leviculum_mapcut::pmtiles::Archive;
use leviculum_mapcut::pmtiles::RangeRead;

use crate::picture::{is_auto, Maps, AUTO_PREFIX};
use crate::ui::map::{LoadReason, MapWant};

/// The most archive bytes one load fetches: the deepest zoom is lowered
/// until the box's tiles come under it. A dense 12 km city box was 20 MB
/// (Berlin-Mitte, 2026-10-28), a region about two fifths of that.
pub const LOAD_CAP: u64 = 64 << 20;
/// The autoloaded regions' cap when the config names none.
pub const CACHE_DEFAULT_MB: u64 = 500;
/// A failed box's first wait before the loader retries it.
pub const BACKOFF_FIRST_MS: u64 = 30_000;
/// The longest a failed box waits.
pub const BACKOFF_MAX_MS: u64 = 3_600_000;
/// A failed box for the screen retries sooner (1811): what the reader
/// looks at must fill by itself once the source answers again.
pub const SCREEN_RETRY_FIRST_MS: u64 = 3_000;
/// The longest a failed box for the screen waits.
pub const SCREEN_RETRY_MAX_MS: u64 = 30_000;
/// A box that found no internet retries after this: no fault of the box,
/// and the map must come back by itself when the internet does.
pub const OFFLINE_RETRY_MS: u64 = 10_000;
/// The most loads that run at once: one for an area the view left, which
/// runs on, and one for the screen.
pub const MAX_RUNNING: usize = 2;
/// The most queued boxes the view left that are kept, the newest.
pub const MAX_AWAY: usize = 8;
/// Loads paused at most (1836): each holds a thread and its file open.
pub const MAX_PAUSED: usize = 4;
/// The layer profile the autoload cuts under, the map tool's default:
/// profile 2 (1707), whose deepest zoom keeps the source's geometry. A
/// region cut the old way no longer covers the screen
/// ([`crate::ui::viewload`]), so an autoloaded one is asked for again
/// and replaced in place under its own name.
const PROFILE: u8 = leviculum_map::profile::CUT_2;
/// The suffix of a region written beside the one it replaces (1727): not
/// `.lmr`, so no painter opens it and no eviction counts it.
const PART_SUFFIX: &str = ".part";
/// What the region's notice names as the program that cut it.
const BY: &str = "loquax";
/// The least time between two progress bumps of a region being written
/// (1291): a frame on screen is drawn again at most four times a second.
pub const PROGRESS_EVERY_MS: u64 = 250;

/// The height catalogue when the config names neither `height_source`
/// nor `map_source` (1863): the project's own map server.
pub const HEIGHTS_DEFAULT: &str = "https://leviculum.network/maps/heights.tsv";
/// The longest catalogue a device reads: a world of z9 blocks is about
/// 80,000 lines, 8 MB.
pub const CATALOGUE_MAX: u64 = 16 << 20;

/// The height catalogue's URL: `height_source` when set, else the
/// catalogue beside the configured `map_source` (a server of its own
/// serves both from one directory), else [`HEIGHTS_DEFAULT`].
pub fn heights_url(height_source: Option<&str>, map_source: Option<&str>) -> String {
    match (height_source, map_source) {
        (Some(url), _) => url.to_string(),
        (None, Some(map)) => match map.rfind('/') {
            Some(at) => format!("{}{CATALOGUE_NAME}", &map[..=at]),
            None => HEIGHTS_DEFAULT.to_string(),
        },
        (None, None) => HEIGHTS_DEFAULT.to_string(),
    }
}

/// The URL of `name` beside the catalogue at `catalogue`.
fn beside(catalogue: &str, name: &str) -> String {
    match catalogue.rfind('/') {
        Some(at) => format!("{}{name}", &catalogue[..=at]),
        None => name.to_string(),
    }
}

/// The language of a locale setting (`de_DE.UTF-8` is `de`), `None` for
/// the C and POSIX locales and an empty one.
pub fn language_of(setting: &str) -> Option<String> {
    let lang: String = setting
        .split(['_', '.', '@', '-'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let letters = (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase());
    (letters && lang != "c").then_some(lang)
}

/// The user's language, which the autoload cuts names in beside the local
/// ones: the first of `LC_ALL`, `LC_MESSAGES` and `LANG` that is set, as
/// the C library chooses the language of its messages.
pub fn user_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .and_then(|v| language_of(&v))
}

/// Where the archive is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The newest daily build under this base URL.
    Daily(String),
    /// This archive.
    Url(String),
}

impl Source {
    /// `map_source` from the config, or the Protomaps daily builds.
    pub fn of(configured: Option<&str>) -> Self {
        match configured {
            Some(url) => Self::Url(url.to_string()),
            None => Self::Daily(http::PROTOMAPS_BUILDS.to_string()),
        }
    }
}

/// What a step of the loader was, for the shell's log and the model.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Started(MapWant),
    Done {
        want: MapWant,
        landed: Landed,
        elapsed_ms: u64,
    },
    Failed {
        want: MapWant,
        detail: String,
        /// The line to show; `None` when it was said already (no internet).
        toast: Option<String>,
        elapsed_ms: u64,
    },
    /// Not loaded: the box is queued or loading already, or backs off.
    Skipped {
        want: MapWant,
        why: &'static str,
    },
    /// A running load was paused between two rounds, went on, or was
    /// dropped to make room (1836): a line for the log.
    Held {
        want: MapWant,
        how: Held,
    },
    /// The height packs of a region that landed (1863) were fetched, or
    /// failed to be: a line for the log, never one for the screen.
    Heights {
        want: MapWant,
        landed: Result<HeightsLanded, String>,
        elapsed_ms: u64,
    },
}

/// What became of a running load the screen no longer needs (1836).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// It stopped after its round, its file kept.
    Paused,
    /// It goes on from the round it stopped at.
    Resumed,
    /// Too many waited paused: it stopped for good, its file removed and
    /// its box queued again.
    Dropped,
}

impl Held {
    /// The word for the log.
    pub fn word(self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::Resumed => "resumed",
            Self::Dropped => "dropped",
        }
    }
}

/// The height packs one fetch wrote (1863).
#[derive(Debug, PartialEq, Eq)]
pub struct HeightsLanded {
    /// The packs' file names, the ones written now.
    pub packs: Vec<String>,
    /// Pack bytes fetched for them.
    pub fetched: u64,
    /// Blocks under the region that were there already.
    pub had: usize,
    /// Files removed to stay under the cap.
    pub evicted: Vec<String>,
}

/// What a [`Step`] did, without its detail: what a frontend keeps of
/// the loader's steps for a test to read their order by (1616).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Started,
    Done,
    Failed,
    Skipped,
    /// Paused, resumed or dropped (1836).
    Held(Held),
    Heights,
}

impl Step {
    /// What the step did.
    pub fn kind(&self) -> StepKind {
        match self {
            Self::Started(_) => StepKind::Started,
            Self::Done { .. } => StepKind::Done,
            Self::Failed { .. } => StepKind::Failed,
            Self::Skipped { .. } => StepKind::Skipped,
            Self::Held { how, .. } => StepKind::Held(*how),
            Self::Heights { .. } => StepKind::Heights,
        }
    }

    /// The box the step is about.
    pub fn want(&self) -> &MapWant {
        match self {
            Self::Started(want)
            | Self::Done { want, .. }
            | Self::Failed { want, .. }
            | Self::Skipped { want, .. }
            | Self::Held { want, .. }
            | Self::Heights { want, .. } => want,
        }
    }
}

/// A region that landed.
#[derive(Debug, PartialEq, Eq)]
pub struct Landed {
    /// Its file name in the maps directory.
    pub name: String,
    pub bytes: u64,
    pub zooms: (u8, u8),
    /// Archive bytes fetched for it.
    pub fetched: u64,
    /// Autoloaded regions removed to stay under the cap.
    pub evicted: Vec<String>,
}

/// Where a box stands in the queue (1292), first first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// Asked for by hand: a place, the pins, "load this area".
    Asked,
    /// The screen itself at its zoom (1527).
    Screen,
    /// Under the screen.
    Here,
    /// Next to the screen.
    Ahead,
    /// An area the view left.
    Away,
}

impl Rank {
    /// Whether the box is for what the user looks at.
    fn on_screen(self) -> bool {
        self <= Self::Here
    }
}

struct Queued {
    want: MapWant,
    rank: Rank,
    /// The box failed and waits: not started before this time. 0 is due.
    retry_at: u64,
}

/// A failed box's wait.
struct Backoff {
    bounds: Bounds,
    failures: u32,
    until_ms: u64,
}

struct Running {
    want: MapWant,
    rank: Rank,
    started: Instant,
    done: Receiver<Result<Landed, Failure>>,
    /// How far the load came, as its rounds report it
    /// ([`LoadState::Running`]).
    progress: Arc<Mutex<LoadProgress>>,
    /// Run, pause between rounds, or stop (1836).
    gate: Arc<Gate>,
    /// What the gate was last set to.
    hold: Hold,
}

/// What a running load is to do at its next round (1836).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Hold {
    #[default]
    Run,
    Pause,
    Stop,
}

/// A running load's switch (1836): the loader sets it, the load's thread
/// looks at it before every round and waits while it says pause.
#[derive(Default)]
struct Gate {
    hold: Mutex<Hold>,
    changed: Condvar,
}

impl Gate {
    fn set(&self, hold: Hold) {
        *self.hold.lock().unwrap_or_else(PoisonError::into_inner) = hold;
        self.changed.notify_all();
    }

    /// Wait while paused; whether the load goes on (`false`: stop).
    fn pass(&self) -> bool {
        let mut hold = self.hold.lock().unwrap_or_else(PoisonError::into_inner);
        while *hold == Hold::Pause {
            hold = self
                .changed
                .wait(hold)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *hold == Hold::Run
    }
}

/// The load was told to stop between two rounds (1836).
#[derive(Debug)]
struct Stopped;

impl core::fmt::Display for Stopped {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the load was set aside")
    }
}

impl std::error::Error for Stopped {}

/// Why a load failed, as the thread hands it back.
struct Failure {
    detail: String,
    unreachable: bool,
    too_large: bool,
    /// The box itself cannot load (too large, no tiles there): retrying
    /// cannot help, the box is not requeued.
    permanent: bool,
    /// The loader stopped it ([`Held::Dropped`]): no fault of the box.
    stopped: bool,
}

/// One box's state as the loader tells it (1811, exposed for 1812's
/// loading view): beside it the box's bounds and rank. "Complete" is the
/// regions' to say, "no internet" [`Loader::offline`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// The load runs: tiles written, of the tiles planned (0 of 0 until
    /// the plan is made), and the tiles' archive bytes fetched of those
    /// planned (1836).
    Running {
        done: u32,
        planned: u32,
        bytes_done: u64,
        bytes: u64,
    },
    /// The load waits paused between two rounds, this far (1836).
    Paused {
        done: u32,
        planned: u32,
        bytes_done: u64,
        bytes: u64,
    },
    /// The box waits in the queue at this place, first is 0.
    Waiting { place: usize },
    /// The box failed and retries in this many milliseconds.
    Retrying { in_ms: u64 },
}

/// The loader the shell holds.
pub struct Loader {
    maps: Maps,
    source: Source,
    cache_bytes: u64,
    /// The daily build found, kept for the session.
    resolved: Arc<Mutex<Option<String>>>,
    queue: Vec<Queued>,
    running: Vec<Running>,
    backoff: Vec<Backoff>,
    /// The last load that ended found no internet, and none started since.
    offline: bool,
    /// No internet was said, and no load landed since.
    offline_said: bool,
    /// The height packs' side (1863), `None` without a catalogue.
    heights: Option<Heights>,
}

/// The height packs' side of the loader (1863).
struct Heights {
    /// The catalogue's URL; the packs lie beside it.
    url: String,
    /// The catalogue read, kept for the session.
    catalogue: Arc<Mutex<Option<Arc<Catalogue>>>>,
    /// Landed regions whose packs are wanted, in the order to fetch them.
    queue: Vec<HeightWant>,
    running: Option<RunningHeights>,
    /// The view scheduled ([`Loader::schedule`], which the frontends call
    /// with the autoload on): the regions on disk want their packs.
    scan_asked: bool,
    /// The regions on disk were queued.
    scanned: bool,
    /// Asks of a catalogue the server did not have, in a row: the long
    /// ladder's rung.
    catalogue_failures: u32,
    /// No height fetch starts before this time: the server had no
    /// catalogue, and every pack waits on its ladder together.
    catalogue_until: u64,
}

/// A landed region whose height packs are wanted.
struct HeightWant {
    want: MapWant,
    /// The region's file name: kept by the eviction its packs run.
    region: String,
    rank: Rank,
    /// Not started before this time; 0 is due.
    retry_at: u64,
    failures: u32,
}

struct RunningHeights {
    want: HeightWant,
    started: Instant,
    done: Receiver<Result<HeightsLanded, HeightFailure>>,
}

/// Why a height fetch failed, as its thread hands it back.
struct HeightFailure {
    detail: String,
    /// The server has no catalogue, or none this reader reads.
    absent: bool,
}

impl Loader {
    pub fn new(maps: Maps, source: Source, cache_bytes: u64) -> Self {
        Self {
            maps,
            source,
            cache_bytes,
            resolved: Arc::new(Mutex::new(None)),
            queue: Vec::new(),
            running: Vec::new(),
            backoff: Vec::new(),
            offline: false,
            offline_said: false,
            heights: None,
        }
    }

    /// Fetch the height packs of every region that lands from the
    /// catalogue at `catalogue` ([`heights_url`], 1863).
    pub fn with_heights(mut self, catalogue: String) -> Self {
        self.heights = Some(Heights {
            url: catalogue,
            catalogue: Arc::new(Mutex::new(None)),
            queue: Vec::new(),
            running: None,
            scan_asked: false,
            scanned: false,
            catalogue_failures: 0,
            catalogue_until: 0,
        });
        self
    }

    /// Whether height packs are fetched or wait to be, not counting those
    /// waiting to retry.
    fn heights_pending(&self) -> bool {
        self.heights
            .as_ref()
            .is_some_and(|h| h.running.is_some() || h.queue.iter().any(|q| q.retry_at == 0))
    }

    /// Milliseconds until the next height fetch could start by the
    /// heights' own clock, `None` when one runs or none waits.
    fn heights_due_ms(&self, now_ms: u64) -> Option<u64> {
        let h = self.heights.as_ref()?;
        // A server without a catalogue wakes no frontend: the packs wait
        // for its next rung and start at the first poll after it, which
        // any frame brings; a still screen loses nothing meanwhile, and
        // a phone is not woken every rung for a server that may never
        // have one (the phone's load walk, 1868).
        if h.running.is_some() || h.catalogue_failures > 0 {
            return None;
        }
        h.queue
            .iter()
            .map(|q| q.retry_at.saturating_sub(now_ms))
            .min()
    }

    /// Whether a load runs or waits to start. A box waiting to retry
    /// after a failure is not loading (1811): its state is "failed,
    /// retry in t" ([`LoadState::Retrying`]), and the chip and the
    /// offline word must not claim activity the loader postponed.
    pub fn busy(&self) -> bool {
        // A paused load waits for the screen's: it owes the view nothing.
        if self.running.iter().any(|r| r.hold != Hold::Pause) || self.heights_pending() {
            return true;
        }
        // Mirror [`Loader::start_next`]: an off-screen box behind an
        // on-screen box waiting to retry does not start, so it is not
        // loading either.
        let mut screen_waits = false;
        for queued in &self.queue {
            match (queued.retry_at > 0, queued.rank.on_screen()) {
                (true, on_screen) => screen_waits |= on_screen,
                (false, false) if screen_waits => {}
                (false, _) => return true,
            }
        }
        false
    }

    /// Whether a load runs or waits for what the user looks at: a box
    /// asked for by hand or under the screen (1292). The phone's chip says
    /// "Loading the map of this area" for these only.
    pub fn loading_here(&self) -> bool {
        self.running.iter().any(|r| r.rank.on_screen())
            || self
                .queue
                .iter()
                .any(|q| q.rank.on_screen() && q.retry_at == 0)
    }

    /// Milliseconds until the loader's next own move (1821, the audit of
    /// 1811; 1835, the audit of 1821): the earliest instant at which
    /// `start_next` could start a queued box — the one scan, `due_at`,
    /// judges both — 0 when one may start now, `None` when no waiting
    /// box has such an instant ahead.
    ///
    /// The loader owes the view something while [`busy`](Self::busy),
    /// [`loading_here`](Self::loading_here) or this says so — the one
    /// definition every frontend reads. The window's pump polls on its
    /// own tick anyway; a frame-driven frontend (the phone) runs the
    /// loader only inside a frame, so it asks for its next frame at this
    /// instant (the phone's `MapShown.retry_in_ms`), and a box that
    /// failed at rest retries with no touch. An off-screen box that is
    /// due but held back — `start_next` starts nothing behind an
    /// on-screen box still waiting to retry — names no wake of its own:
    /// nothing could start at it, and a phone woken then would redraw
    /// every 50 ms for nothing until the screen box's retry (the audit
    /// of 1821, finding 1). The wake named is the screen box's instead,
    /// and the held box starts, in order, once the screen is served.
    ///
    /// A height fetch waiting (1863) names its own wake, unless a region
    /// for the screen loads or waits to: the packs start after it, and
    /// the region's own end wakes the loader. Packs waiting for a
    /// catalogue the server did not have name none (1868).
    pub fn next_due_ms(&self, now_ms: u64) -> Option<u64> {
        let regions = match self.due_at(now_ms) {
            Some(_) => Some(0),
            None => self
                .queue
                .iter()
                .filter(|q| q.retry_at > now_ms)
                .map(|q| q.retry_at)
                .filter(|&at| self.due_at(at).is_some())
                .min()
                .map(|at| at - now_ms),
        };
        let heights = match self.loading_here() {
            true => None,
            false => self.heights_due_ms(now_ms),
        };
        match (regions, heights) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Whether the last load that ended found no internet and none started
    /// since: the phone's map says "Offline" while it holds (1200), and
    /// the next load asked tests it again (1292).
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// The boxes queued and running, the running first, each with its
    /// rank: what a test reads the order from.
    pub fn order(&self) -> Vec<(Bounds, Rank, bool)> {
        self.running
            .iter()
            .map(|r| (r.want.bounds, r.rank, true))
            .chain(self.queue.iter().map(|q| (q.want.bounds, q.rank, false)))
            .collect()
    }

    /// Every box the loader holds with its state (1811, for 1812): the
    /// running loads first with their progress, then the queue in order,
    /// each waiting at its place or retrying after its failure.
    pub fn report(&self, now_ms: u64) -> Vec<(Bounds, Rank, LoadState)> {
        let mut out: Vec<(Bounds, Rank, LoadState)> = self
            .running
            .iter()
            .map(|r| {
                let p = *r.progress.lock().unwrap_or_else(PoisonError::into_inner);
                let (done, planned) = (p.tiles_done as u32, p.tiles as u32);
                let (bytes_done, bytes) = (p.bytes_done, p.bytes);
                let state = match r.hold {
                    Hold::Pause => LoadState::Paused {
                        done,
                        planned,
                        bytes_done,
                        bytes,
                    },
                    _ => LoadState::Running {
                        done,
                        planned,
                        bytes_done,
                        bytes,
                    },
                };
                (r.want.bounds, r.rank, state)
            })
            .collect();
        for (place, q) in self.queue.iter().enumerate() {
            let state = match q.retry_at.saturating_sub(now_ms) {
                0 => LoadState::Waiting { place },
                in_ms => LoadState::Retrying { in_ms },
            };
            out.push((q.want.bounds, q.rank, state));
        }
        out
    }

    /// Whether `bounds` is queued or loading.
    fn has(&self, bounds: &Bounds) -> bool {
        self.running.iter().any(|r| r.want.bounds == *bounds)
            || self.queue.iter().any(|q| q.want.bounds == *bounds)
    }

    /// Why `want` cannot be queued now, if it cannot. A box waiting to
    /// retry is "queued": it stays and retries on the loader's clock.
    fn refuse(&self, want: &MapWant) -> Option<&'static str> {
        if self.has(&want.bounds) {
            return Some("queued");
        }
        self.maps.dir().is_none().then_some("no-maps-dir")
    }

    /// When a new box of `bounds` may start: now, or after the wait its
    /// failures earned it.
    fn backoff_until(&self, bounds: &Bounds, now_ms: u64) -> u64 {
        self.backoff
            .iter()
            .find(|b| b.bounds == *bounds && now_ms < b.until_ms)
            .map_or(0, |b| b.until_ms)
    }

    /// Queue `want`, asked for by hand, before every box the view asked
    /// for, and start it when nothing runs. `now_ms` is the shell's
    /// monotonic clock.
    pub fn ask(&mut self, want: MapWant, now_ms: u64) -> Vec<Step> {
        if let Some(why) = self.refuse(&want) {
            return vec![Step::Skipped { want, why }];
        }
        let at = self
            .queue
            .iter()
            .position(|q| q.rank > Rank::Asked)
            .unwrap_or(self.queue.len());
        self.queue.insert(
            at,
            Queued {
                want,
                rank: Rank::Asked,
                retry_at: 0,
            },
        );
        self.start_next(now_ms)
    }

    /// The scheduler's decision (1292, [`crate::ui::viewload`]): `here`
    /// the boxes under the screen, `ahead` those next to it, each in the
    /// order to load them. Every box queued or running takes the rank of
    /// the list it is in now, or is one the view left; the queue is
    /// ordered by rank, the lists' order within it, the boxes the view
    /// left in the order they came, at most [`MAX_AWAY`]; then a load for
    /// the screen starts when nothing for it runs. A box asked for by
    /// hand keeps its place.
    pub fn schedule(&mut self, here: Vec<MapWant>, ahead: Vec<MapWant>, now_ms: u64) -> Vec<Step> {
        // The view decides: the autoload is on, and the regions on disk
        // want their height packs as those that land (1868).
        if let Some(h) = self.heights.as_mut() {
            h.scan_asked = true;
        }
        let place = |b: &Bounds| {
            here.iter()
                .position(|w| w.bounds == *b)
                .map(|i| (Self::rank_here(&here[i]), i))
                .or_else(|| {
                    ahead
                        .iter()
                        .position(|w| w.bounds == *b)
                        .map(|i| (Rank::Ahead, i))
                })
        };
        for running in &mut self.running {
            if running.rank != Rank::Asked {
                // A load that brings the screen's box is the screen's
                // (1616): the cells wait for it as for the box itself.
                let screens = here.iter().any(|w| brings(&running.want, w));
                running.rank = match place(&running.want.bounds) {
                    _ if screens => Rank::Screen,
                    Some(p) => p.0,
                    None => Rank::Away,
                };
            }
        }
        let mut keyed: Vec<((Rank, usize), Queued)> = Vec::new();
        for (n, mut queued) in std::mem::take(&mut self.queue).into_iter().enumerate() {
            let key = match (queued.rank, place(&queued.want.bounds)) {
                (Rank::Asked, _) => (Rank::Asked, n),
                (_, Some(key)) => key,
                // A screen's box the view left: the cells hold its area.
                (_, None) if queued.want.reason == LoadReason::Screen => continue,
                (_, None) => (Rank::Away, n),
            };
            queued.rank = key.0;
            keyed.push((key, queued));
        }
        for (i, want) in here.into_iter().enumerate() {
            if self.refuse(&want).is_none()
                && !keyed.iter().any(|k| k.1.want.bounds == want.bounds)
                && !self.brought(&want)
            {
                let rank = Self::rank_here(&want);
                let retry_at = self.backoff_until(&want.bounds, now_ms);
                keyed.push((
                    (rank, i),
                    Queued {
                        want,
                        rank,
                        retry_at,
                    },
                ));
            }
        }
        for (i, want) in ahead.into_iter().enumerate() {
            if self.refuse(&want).is_none() && !keyed.iter().any(|k| k.1.want.bounds == want.bounds)
            {
                let retry_at = self.backoff_until(&want.bounds, now_ms);
                keyed.push((
                    (Rank::Ahead, i),
                    Queued {
                        want,
                        rank: Rank::Ahead,
                        retry_at,
                    },
                ));
            }
        }
        keyed.sort_by_key(|k| k.0);
        let away = keyed.iter().filter(|k| k.0 .0 == Rank::Away).count();
        let mut drop = away.saturating_sub(MAX_AWAY);
        self.queue = keyed
            .into_iter()
            .filter(|k| {
                let gone = k.0 .0 == Rank::Away && drop > 0;
                drop -= usize::from(gone);
                !gone
            })
            .map(|k| k.1)
            .collect();
        self.start_next(now_ms)
    }

    /// Whether `want`, the screen's own box (1527), is what a load
    /// running already brings ([`brings`]). Two loads of the same tiles
    /// share the link and both arrive later.
    fn brought(&self, want: &MapWant) -> bool {
        self.running.iter().any(|r| brings(&r.want, want))
    }

    /// The rank of `want` in the scheduler's `here`: the screen's own box
    /// first (1527), the cells after it.
    fn rank_here(want: &MapWant) -> Rank {
        match want.reason {
            LoadReason::Screen => Rank::Screen,
            _ => Rank::Here,
        }
    }

    /// What finished since the last call, and the next loads started.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut n = 0;
        while n < self.running.len() {
            let result = match self.running[n].done.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    n += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(Failure {
                    detail: "the load stopped".to_string(),
                    unreachable: false,
                    too_large: false,
                    permanent: false,
                    stopped: false,
                }),
            };
            let running = self.running.remove(n);
            steps.push(self.ended(running, result, now_ms));
        }
        steps.extend(self.heights_ended(now_ms));
        steps.extend(self.start_next(now_ms));
        self.start_heights(now_ms);
        steps
    }

    /// The step of the height fetch that ended, if one did.
    fn heights_ended(&mut self, now_ms: u64) -> Option<Step> {
        let h = self.heights.as_mut()?;
        let result = match h.running.as_ref()?.done.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(HeightFailure {
                detail: "the height fetch stopped".to_string(),
                absent: false,
            }),
        };
        let running = h.running.take()?;
        let elapsed_ms = u64::try_from(running.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut item = running.want;
        let want = item.want.clone();
        let landed = match result {
            Ok(landed) => {
                h.catalogue_failures = 0;
                h.catalogue_until = 0;
                Ok(landed)
            }
            Err(failure) if failure.absent => {
                // No catalogue there yet (the default source's comes after
                // the first regions): asked again on the long ladder, once
                // a rung, every pack waiting on it together.
                h.catalogue_failures += 1;
                let until = now_ms.saturating_add(ladder_ms(h.catalogue_failures));
                h.catalogue_until = until;
                for queued in &mut h.queue {
                    queued.retry_at = queued.retry_at.max(until);
                }
                item.retry_at = until;
                let at = h
                    .queue
                    .iter()
                    .position(|q| q.rank > item.rank)
                    .unwrap_or(h.queue.len());
                h.queue.insert(at, item);
                Err(failure.detail)
            }
            Err(failure) => {
                // The long ladder, as a region away from the screen: the
                // relief is a finish, it does not hurry a failing source.
                item.failures += 1;
                item.retry_at = now_ms.saturating_add(ladder_ms(item.failures));
                let at = h
                    .queue
                    .iter()
                    .position(|q| q.rank > item.rank)
                    .unwrap_or(h.queue.len());
                h.queue.insert(at, item);
                Err(failure.detail)
            }
        };
        Some(Step::Heights {
            want,
            landed,
            elapsed_ms,
        })
    }

    /// Queue the height packs of `region`, landed for `want` at `rank`:
    /// after every region of its rank or before, the same region once.
    fn want_heights(&mut self, want: &MapWant, region: &str, rank: Rank) {
        let Some(h) = self.heights.as_mut() else {
            return;
        };
        h.queue.retain(|q| q.region != region);
        let at = h
            .queue
            .iter()
            .position(|q| q.rank > rank)
            .unwrap_or(h.queue.len());
        h.queue.insert(
            at,
            HeightWant {
                want: want.clone(),
                region: region.to_string(),
                rank,
                retry_at: h.catalogue_until,
                failures: 0,
            },
        );
    }

    /// Queue the height packs of every autoloaded region on disk that is
    /// not queued yet, once a session after the view first scheduled (the
    /// autoload is on: with it off nothing is fetched by itself), the most
    /// recently viewed first, as boxes the view left: a region that landed
    /// before heights were fetched (another session, a catalogue served
    /// later) wants its packs as one that lands now. A region's packs on
    /// disk already are counted, not fetched again.
    fn want_heights_on_disk(&mut self, dir: &Path) {
        let Some(h) = self.heights.as_mut() else {
            return;
        };
        if !h.scan_asked || std::mem::replace(&mut h.scanned, true) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut on_disk: Vec<(SystemTime, String, MapWant)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let want = disk_want(&name)?;
                let viewed = e.metadata().and_then(|m| m.modified()).ok()?;
                Some((viewed, name, want))
            })
            .collect();
        on_disk.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for (_, region, want) in on_disk {
            if h.queue.iter().any(|q| q.region == region)
                || h.running.as_ref().is_some_and(|r| r.want.region == region)
            {
                continue;
            }
            h.queue.push(HeightWant {
                want,
                region,
                rank: Rank::Away,
                retry_at: h.catalogue_until,
                failures: 0,
            });
        }
    }

    /// Start the first due height fetch when none runs and no region for
    /// the screen loads or waits to: the packs come after the regions.
    fn start_heights(&mut self, now_ms: u64) {
        if self.loading_here() {
            return;
        }
        let Some(dir) = self.maps.dir().map(Path::to_path_buf) else {
            return;
        };
        self.want_heights_on_disk(&dir);
        let maps = self.maps.clone();
        let cache_bytes = self.cache_bytes;
        let Some(h) = self.heights.as_mut() else {
            return;
        };
        if h.running.is_some() {
            return;
        }
        let Some(n) = h.queue.iter().position(|q| q.retry_at <= now_ms) else {
            return;
        };
        let item = h.queue.remove(n);
        let job = HeightJob {
            dir,
            maps,
            url: h.url.clone(),
            catalogue: Arc::clone(&h.catalogue),
            bounds: item.want.bounds,
            region: item.region.clone(),
            cache_bytes,
        };
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("map-heights".to_string())
            .spawn(move || {
                let _ = tx.send(job.run());
            });
        match spawned {
            Ok(_) => {
                h.running = Some(RunningHeights {
                    want: item,
                    started: Instant::now(),
                    done: rx,
                });
            }
            // No thread now: the item waits for the next poll.
            Err(_) => h.queue.insert(n, item),
        }
    }

    /// The step of `running`, which ended with `result`.
    fn ended(&mut self, running: Running, result: Result<Landed, Failure>, now_ms: u64) -> Step {
        let elapsed_ms = u64::try_from(running.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let want = running.want;
        match result {
            Ok(landed) => {
                self.offline = false;
                self.offline_said = false;
                // The source answers: every wait is over (1811). The
                // failure counts stay (1821): a box that keeps failing
                // retries quietly on its ladder's next rung instead of
                // starting over — and toasting over — on every landing.
                for backoff in &mut self.backoff {
                    backoff.until_ms = 0;
                }
                for queued in &mut self.queue {
                    queued.retry_at = 0;
                }
                self.maps.changed();
                self.want_heights(&want, &landed.name, running.rank);
                Step::Done {
                    want,
                    landed,
                    elapsed_ms,
                }
            }
            Err(failure) if failure.stopped => {
                // Dropped to make room (1836): queued again at its rank's
                // tail, due at once, no failure booked.
                let at = self
                    .queue
                    .iter()
                    .position(|q| q.rank > running.rank)
                    .unwrap_or(self.queue.len());
                self.queue.insert(
                    at,
                    Queued {
                        want: want.clone(),
                        rank: running.rank,
                        retry_at: 0,
                    },
                );
                Step::Held {
                    want,
                    how: Held::Dropped,
                }
            }
            Err(failure) => {
                // No internet is no fault of the box: it retries soon,
                // and the state holds until a load starts.
                self.offline = failure.unreachable;
                let (retry_at, failures) = match failure.unreachable {
                    true => (now_ms.saturating_add(OFFLINE_RETRY_MS), 0),
                    false => self.back_off(want.bounds, now_ms, running.rank.on_screen()),
                };
                // The box stays, at its rank's tail, and retries by
                // itself (1811): a transient fault heals with no key. A
                // permanent failure is not retried: a box too large wants
                // zooming in, one with no tiles has nothing to fetch.
                if !failure.permanent {
                    let at = self
                        .queue
                        .iter()
                        .position(|q| q.rank > running.rank)
                        .unwrap_or(self.queue.len());
                    self.queue.insert(
                        at,
                        Queued {
                            want: want.clone(),
                            rank: running.rank,
                            retry_at,
                        },
                    );
                }
                // One line per box, not one per retry (1811): the first
                // failure speaks, the loader's own retries stay quiet.
                let toast = match (failure.unreachable, failure.too_large) {
                    (true, _) if self.offline_said => None,
                    (true, _) => {
                        self.offline_said = true;
                        Some("map: no internet; the map shows what is here".to_string())
                    }
                    (false, true) => Some(format!("map: {}; zoom in", failure.detail)),
                    (false, false) if failures > 1 => None,
                    (false, false) => Some(format!("map: {}", failure.detail)),
                };
                Step::Failed {
                    want,
                    detail: failure.detail,
                    toast,
                    elapsed_ms,
                }
            }
        }
    }

    /// Book `bounds`'s failure and say when it may try again and how
    /// often it failed: the screen's ladder for a box the reader looks
    /// at, the long one else.
    fn back_off(&mut self, bounds: Bounds, now_ms: u64, on_screen: bool) -> (u64, u32) {
        let failures = self
            .backoff
            .iter()
            .find(|b| b.bounds == bounds)
            .map_or(0, |b| b.failures)
            + 1;
        let (first, max) = match on_screen {
            true => (SCREEN_RETRY_FIRST_MS, SCREEN_RETRY_MAX_MS),
            false => (BACKOFF_FIRST_MS, BACKOFF_MAX_MS),
        };
        let wait = first.saturating_mul(1 << (failures - 1).min(16)).min(max);
        let until_ms = now_ms.saturating_add(wait);
        self.backoff.retain(|b| b.bounds != bounds);
        self.backoff.push(Backoff {
            bounds,
            failures,
            until_ms,
        });
        (until_ms, failures)
    }

    /// The queue's next box at `now_ms`, apart from what already runs:
    /// the first box due, unless it is off-screen behind an on-screen
    /// box still waiting to retry — then nothing, the link is not spent
    /// next to a screen that lacks its data (1811). The one scan
    /// [`start_next`](Self::start_next) starts by and [`next_due_ms`](Self::next_due_ms)
    /// names wakes by (1835, the audit of 1821): an instant this scan
    /// refuses is no wake, because nothing could start at it.
    fn due_at(&self, now_ms: u64) -> Option<usize> {
        let mut screen_waits = false;
        for (n, queued) in self.queue.iter().enumerate() {
            if queued.retry_at > now_ms {
                screen_waits |= queued.rank.on_screen();
                continue;
            }
            if screen_waits && !queued.rank.on_screen() {
                return None;
            }
            return Some(n);
        }
        None
    }

    /// Pause, resume and drop running loads by what the screen wants
    /// (1836): while a box for the screen runs or waits, every load for
    /// an area off the screen pauses at its next round; once nothing for
    /// the screen is left, the paused loads go on, the best ranked first,
    /// [`MAX_RUNNING`] at most, before any new box starts. Past
    /// [`MAX_PAUSED`] paused loads the worst ranked, the latest started
    /// among equals, stops for good.
    fn steer(&mut self) -> Vec<Step> {
        let screen_wants = self
            .running
            .iter()
            .any(|r| r.hold == Hold::Run && r.rank.on_screen())
            || self.queue.iter().any(|q| q.rank.on_screen());
        let mut steps = Vec::new();
        let set = |r: &mut Running, hold: Hold, steps: &mut Vec<Step>| {
            if r.hold == hold || r.hold == Hold::Stop {
                return;
            }
            let how = match hold {
                Hold::Run => Held::Resumed,
                Hold::Pause => Held::Paused,
                Hold::Stop => Held::Dropped,
            };
            r.hold = hold;
            r.gate.set(hold);
            // A drop says so when its thread hands the box back.
            if hold != Hold::Stop {
                steps.push(Step::Held {
                    want: r.want.clone(),
                    how,
                });
            }
        };
        if screen_wants {
            for r in &mut self.running {
                if !r.rank.on_screen() {
                    set(r, Hold::Pause, &mut steps);
                }
            }
        } else {
            let mut order: Vec<usize> = (0..self.running.len())
                .filter(|&n| self.running[n].hold != Hold::Stop)
                .collect();
            order.sort_by_key(|&n| (self.running[n].rank, self.running[n].started));
            for (k, n) in order.into_iter().enumerate() {
                let hold = match k < MAX_RUNNING {
                    true => Hold::Run,
                    false => Hold::Pause,
                };
                set(&mut self.running[n], hold, &mut steps);
            }
        }
        let mut paused: Vec<usize> = (0..self.running.len())
            .filter(|&n| self.running[n].hold == Hold::Pause)
            .collect();
        paused.sort_by_key(|&n| {
            (
                std::cmp::Reverse(self.running[n].rank),
                std::cmp::Reverse(self.running[n].started),
            )
        });
        for &n in paused.iter().take(paused.len().saturating_sub(MAX_PAUSED)) {
            set(&mut self.running[n], Hold::Stop, &mut steps);
        }
        steps
    }

    /// Start the queue's first boxes ([`due_at`](Self::due_at)): one
    /// when nothing runs, one for the screen beside loads for areas the
    /// view left, and the screen's own box (1527) beside any load but
    /// another for the screen itself.
    fn start_next(&mut self, now_ms: u64) -> Vec<Step> {
        let mut steps = self.steer();
        while let Some(n) = self.due_at(now_ms) {
            let active: Vec<&Running> = self
                .running
                .iter()
                .filter(|r| r.hold == Hold::Run)
                .collect();
            // Loads paused or stopping make way: a box waits only for one
            // that runs ([`steer`](Self::steer) resumes the paused before
            // a box off the screen is due).
            let beside = match self.queue[n].rank {
                Rank::Screen => !active.iter().any(|r| r.rank <= Rank::Screen),
                rank if rank.on_screen() => !active.iter().any(|r| r.rank.on_screen()),
                _ => false,
            };
            if !(active.is_empty() || active.len() < MAX_RUNNING && beside) {
                break;
            }
            let queued = self.queue.remove(n);
            steps.extend(self.start(queued));
        }
        steps
    }

    /// Start `queued` on a thread of its own.
    fn start(&mut self, queued: Queued) -> Vec<Step> {
        let Queued { want, rank, .. } = queued;
        let Some(dir) = self.maps.dir() else {
            return Vec::new();
        };
        let (tx, rx) = channel();
        let progress = Arc::new(Mutex::new(LoadProgress::default()));
        let gate = Arc::new(Gate::default());
        let job = Job {
            dir: dir.to_path_buf(),
            maps: self.maps.clone(),
            source: self.source.clone(),
            resolved: Arc::clone(&self.resolved),
            want: want.clone(),
            cache_bytes: self.cache_bytes,
            progress: Arc::clone(&progress),
            gate: Arc::clone(&gate),
        };
        let spawned = std::thread::Builder::new()
            .name("map-load".to_string())
            .spawn(move || {
                let _ = tx.send(job.run());
            });
        if let Err(error) = spawned {
            return vec![Step::Failed {
                want,
                detail: error.to_string(),
                toast: Some(format!("map: {error}")),
                elapsed_ms: 0,
            }];
        }
        self.offline = false;
        self.running.push(Running {
            want: want.clone(),
            rank,
            started: Instant::now(),
            done: rx,
            progress,
            gate,
            hold: Hold::Run,
        });
        vec![Step::Started(want)]
    }
}

impl Drop for Loader {
    /// A paused load's thread waits on its gate: told to stop, it removes
    /// its unfinished file and ends instead of waiting for ever (1836).
    fn drop(&mut self) {
        for r in &self.running {
            r.gate.set(Hold::Stop);
        }
    }
}

/// Whether a load of `load` brings `want`, the screen's own box (1527):
/// its box and zooms inside the load's.
fn brings(load: &MapWant, want: &MapWant) -> bool {
    want.reason == LoadReason::Screen
        && load.bounds.holds(&want.bounds)
        && load.zooms.0 <= want.zooms.0
        && load.zooms.1 >= want.zooms.1
}

/// One load, on its thread.
struct Job {
    dir: PathBuf,
    maps: Maps,
    source: Source,
    resolved: Arc<Mutex<Option<String>>>,
    want: MapWant,
    cache_bytes: u64,
    /// How far the load came, for [`Loader::report`].
    progress: Arc<Mutex<LoadProgress>>,
    /// Looked at before every round (1836).
    gate: Arc<Gate>,
}

impl Job {
    fn run(self) -> Result<Landed, Failure> {
        self.load().map_err(|e| {
            let too_large = e.downcast_ref::<TooLarge>().is_some();
            Failure {
                unreachable: http::unreachable(e.as_ref()),
                too_large,
                permanent: too_large || e.downcast_ref::<Empty>().is_some(),
                stopped: e.downcast_ref::<Stopped>().is_some(),
                detail: e.to_string(),
            }
        })
    }

    fn url(&self) -> leviculum_mapcut::Res<String> {
        match &self.source {
            Source::Url(url) => Ok(url.clone()),
            Source::Daily(base) => {
                let mut held = self.resolved.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(url) = held.as_ref() {
                    return Ok(url.clone());
                }
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                let url = http::daily(base, now)?.url().to_string();
                *held = Some(url.clone());
                Ok(url)
            }
        }
    }

    fn load(&self) -> leviculum_mapcut::Res<Landed> {
        let archive = Archive::from_source(HttpRange::new(&self.url()?))?;
        std::fs::create_dir_all(&self.dir)?;
        let want = Want {
            bounds: self.want.bounds,
            zooms: self.want.zooms.0..=self.want.zooms.1,
            floor: self.want.floor,
            cap: LOAD_CAP,
            profile: PROFILE,
            locale: user_language(),
            // The phone and the window draw them from z16 (1191); the
            // terminal and the M9 never read the section.
            buildings: true,
            // Every point of interest rides along as a section a reader
            // that shows none skips for free (1685).
            pois: true,
        };
        let planned = plan(&archive, &want)?;
        let zooms = (*planned.zooms.start(), *planned.zooms.end());
        let name = auto_name(&want.bounds, zooms);
        // Under its own name from the first byte (1291): a painter draws
        // what has arrived, a version 4 file says what that is. A load
        // that replaces an installed region writes beside it instead and
        // takes its place only once the new file verifies (1727), so a
        // broken link or a failed check never costs the region there.
        let path = self.dir.join(&name);
        let replaces = path.exists();
        let written = match replaces {
            true => self.dir.join(format!("{name}{PART_SUFFIX}")),
            false => path.clone(),
        };
        // As wide as the tile painters (1473): one core count for the map.
        let threads = crate::picture::cores();
        let maps = self.maps.clone();
        let mut bumped: Option<Instant> = None;
        let shared = Arc::clone(&self.progress);
        let mut progress = |p: LoadProgress| {
            *shared.lock().unwrap_or_else(PoisonError::into_inner) = p;
            let due =
                bumped.is_none_or(|at| at.elapsed().as_millis() >= u128::from(PROGRESS_EVERY_MS));
            // A file written beside the region it replaces is no painter's
            // until it lands.
            if !p.complete && due && !replaces {
                bumped = Some(Instant::now());
                maps.arrived();
            }
        };
        let write = || -> leviculum_mapcut::Res<Loaded> {
            let mut out = BufWriter::new(File::create(&written)?);
            // A round at a time (1836): before each the gate says whether
            // to go on, wait paused, or stop.
            let mut loading = Loading::begin(&archive, &want, planned, BY, threads, &mut out)?;
            progress(loading.progress());
            loop {
                if !self.gate.pass() {
                    return Err(Box::new(Stopped));
                }
                match loading.step()? {
                    Some(p) => progress(p),
                    None => break,
                }
            }
            let last = loading.progress();
            let loaded = loading.finish()?;
            progress(LoadProgress {
                complete: true,
                ..last
            });
            let file = out.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            let region = Region::open(File::open(&written)?).map_err(|e| format!("{e:?}"))?;
            region
                .verify()
                .map_err(|e| format!("the region does not verify: {e:?}"))?;
            if replaces {
                std::fs::rename(&written, &path)?;
            }
            Ok(loaded)
        };
        let loaded = match write() {
            Ok(loaded) => loaded,
            Err(error) => {
                let _ = std::fs::remove_file(&written);
                // A region written in place was drawn as it arrived and is
                // gone now; one written beside the old was never seen.
                if !replaces {
                    self.maps.changed();
                }
                return Err(error);
            }
        };
        let evicted = evict(&self.dir, self.cache_bytes, &name)?;
        Ok(Landed {
            bytes: loaded.stats.bytes,
            name,
            zooms,
            fetched: loaded.fetched,
            evicted,
        })
    }
}

/// One height fetch, on its thread (1863).
struct HeightJob {
    dir: PathBuf,
    maps: Maps,
    url: String,
    catalogue: Arc<Mutex<Option<Arc<Catalogue>>>>,
    bounds: Bounds,
    region: String,
    cache_bytes: u64,
}

impl HeightJob {
    fn run(self) -> Result<HeightsLanded, HeightFailure> {
        let catalogue = self.catalogue().map_err(|e| HeightFailure {
            absent: absent(e.as_ref()),
            detail: e.to_string(),
        })?;
        self.fetch(&catalogue).map_err(|e| HeightFailure {
            absent: false,
            detail: e.to_string(),
        })
    }

    /// The catalogue, read once a session.
    fn catalogue(&self) -> leviculum_mapcut::Res<Arc<Catalogue>> {
        let mut held = self
            .catalogue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(catalogue) = held.as_ref() {
            return Ok(Arc::clone(catalogue));
        }
        let bytes = HttpRange::new(&self.url).read_range(0, CATALOGUE_MAX)?;
        let text = String::from_utf8(bytes).map_err(|_| NoCatalogue("not UTF-8".to_string()))?;
        let catalogue = Arc::new(Catalogue::parse(&text).map_err(|e| NoCatalogue(e.to_string()))?);
        *held = Some(Arc::clone(&catalogue));
        Ok(catalogue)
    }

    /// Every block under the region's box not on disk yet, a range
    /// request each, checked and written as a pack of its own.
    fn fetch(&self, catalogue: &Catalogue) -> leviculum_mapcut::Res<HeightsLanded> {
        let mut landed = HeightsLanded {
            packs: Vec::new(),
            fetched: 0,
            had: 0,
            evicted: Vec::new(),
        };
        // A pack file's directory, read once per fetch.
        let mut directories: Vec<(usize, Vec<u8>)> = Vec::new();
        for entry in catalogue.under(&self.bounds) {
            let name = format!("{AUTO_PREFIX}{}", entry.local_name());
            let path = self.dir.join(&name);
            if path.exists() {
                landed.had += 1;
                continue;
            }
            let file = &catalogue.packs[entry.pack];
            let source = HttpRange::new(&beside(&self.url, &file.name));
            let directory = match directories.iter().find(|d| d.0 == entry.pack) {
                Some(d) => d.1.clone(),
                None => {
                    let d = exact(&source, 0, file.directory, DIRECTORY_MAX)?;
                    landed.fetched += d.len() as u64;
                    directories.push((entry.pack, d.clone()));
                    d
                }
            };
            let block = exact(&source, entry.offset, entry.len, BLOCK_MAX)?;
            landed.fetched += block.len() as u64;
            let pack = local_pack(&directory, &block, &entry)?;
            write_pack(&self.dir, &name, &pack)?;
            landed.packs.push(name);
            // Every painter indexes the packs again and shades with it.
            self.maps.changed();
        }
        if !landed.packs.is_empty() {
            landed.evicted = evict(&self.dir, self.cache_bytes, &self.region)?;
        }
        Ok(landed)
    }
}

/// A catalogue that is no catalogue this reader reads.
#[derive(Debug)]
struct NoCatalogue(String);

impl core::fmt::Display for NoCatalogue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "no height catalogue: {}", self.0)
    }
}

impl std::error::Error for NoCatalogue {}

/// The long ladder's wait after `failures` in a row: [`BACKOFF_FIRST_MS`]
/// doubling to [`BACKOFF_MAX_MS`].
fn ladder_ms(failures: u32) -> u64 {
    BACKOFF_FIRST_MS
        .saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(BACKOFF_MAX_MS)
}

/// Whether `e` says the server has no catalogue: none at the URL, or one
/// this reader does not read. Retrying this session cannot help.
fn absent(e: &(dyn std::error::Error + 'static)) -> bool {
    e.downcast_ref::<NoCatalogue>().is_some()
        || matches!(
            e.downcast_ref::<HttpError>(),
            Some(HttpError::Status(403 | 404 | 410))
        )
}

/// Exactly `len` bytes at `offset` of `source`, or an error: a server
/// that sends fewer has not the file the catalogue named. A `len` past
/// `max` is not asked for: the catalogue's reader refuses it already
/// ([`Catalogue::parse`]), and this read holds the limit whatever made
/// the entry.
fn exact(source: &HttpRange, offset: u64, len: u64, max: u64) -> leviculum_mapcut::Res<Vec<u8>> {
    if len > max {
        return Err(format!("{}: {len} bytes at {offset}, more than {max}", source.url()).into());
    }
    let bytes = source.read_range(offset, len)?;
    if bytes.len() as u64 != len {
        return Err(format!(
            "{}: {} bytes at {offset}, the catalogue says {len}",
            source.url(),
            bytes.len()
        )
        .into());
    }
    Ok(bytes)
}

/// Write `bytes` as `name` in `dir`: under a `.part` name first, synced,
/// then renamed, so no painter indexes half a pack.
fn write_pack(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{name}{PART_SUFFIX}"));
    let written = (|| {
        let mut file = File::create(&part)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&part, dir.join(name))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    written
}

/// An autoloaded region's file name: the box in degrees times 10^7 and the
/// zooms, so the same box asked again replaces its file.
pub fn auto_name(b: &Bounds, zooms: (u8, u8)) -> String {
    format!(
        "{AUTO_PREFIX}{}_{}_{}_{}-z{}-{}.lmr",
        b.west, b.south, b.east, b.north, zooms.0, zooms.1
    )
}

/// The box of an autoloaded region from its name ([`auto_name`]).
fn auto_bounds(name: &str) -> Option<Bounds> {
    let rest = name.strip_prefix(AUTO_PREFIX)?.strip_suffix(".lmr")?;
    let (corners, _) = rest.split_once("-z")?;
    let mut parts = corners.split('_').map(|p| p.parse::<i32>().ok());
    let bounds = Bounds {
        west: parts.next()??,
        south: parts.next()??,
        east: parts.next()??,
        north: parts.next()??,
    };
    parts.next().is_none().then_some(bounds)
}

/// The want of an autoloaded region on disk from its name ([`auto_name`]):
/// its box, its zooms, its centre.
fn disk_want(name: &str) -> Option<MapWant> {
    let bounds = auto_bounds(name)?;
    let (_, zooms) = name.strip_suffix(".lmr")?.rsplit_once("-z")?;
    let (low, high) = zooms.split_once('-')?;
    let zooms = (low.parse().ok()?, high.parse().ok()?);
    Some(MapWant {
        bounds,
        zooms,
        floor: zooms.1,
        reason: LoadReason::View,
        centre: leviculum_map::mercator::World {
            x: leviculum_map::mercator::x_of_lon(bounds.west / 2 + bounds.east / 2),
            y: leviculum_map::mercator::y_of_lat(bounds.south / 2 + bounds.north / 2),
        },
    })
}

/// The block of an autoloaded height pack from its name
/// ([`BlockEntry::local_name`] behind [`AUTO_PREFIX`]).
fn pack_bounds(name: &str) -> Option<Bounds> {
    let rest = name.strip_prefix(AUTO_PREFIX)?.strip_prefix('h')?;
    let rest = rest.strip_suffix(".lmh")?;
    let mut parts = rest.split('-').map(|p| p.parse::<u32>().ok());
    let zoom = u8::try_from(parts.next()??).ok()?;
    let (x, y) = (parts.next()??, parts.next()??);
    if parts.next().is_some() || zoom > 31 || u64::from(x.max(y)) >= 1 << zoom {
        return None;
    }
    let entry = BlockEntry {
        pack: 0,
        zoom,
        x,
        y,
        offset: 0,
        len: 0,
    };
    Some(entry.bounds())
}

/// Whether two boxes share ground; a common edge is none.
fn overlaps(a: &Bounds, b: &Bounds) -> bool {
    a.west < b.east && b.west < a.east && a.south < b.north && b.south < a.north
}

/// Remove autoloaded regions of `dir`, least recently viewed first, until
/// they and the autoloaded height packs come to `cap` bytes or less;
/// `keep` (the region just loaded) and every file without [`AUTO_PREFIX`]
/// stay. A height pack goes with the last autoloaded region over its
/// block (1863), whether that went now or before. The names removed.
pub fn evict(dir: &Path, cap: u64, keep: &str) -> std::io::Result<Vec<String>> {
    let mut auto: Vec<(SystemTime, String, u64)> = Vec::new();
    let mut packs: Vec<(String, u64, Option<Bounds>)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_auto(&name) {
            continue;
        }
        // Gone since the listing: another writer evicted it.
        let meta = match entry.metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            meta => meta?,
        };
        if name.ends_with(".lmr") {
            let viewed = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            auto.push((viewed, name, meta.len()));
        } else if name.ends_with(".lmh") {
            let bounds = pack_bounds(&name);
            packs.push((name, meta.len(), bounds));
        }
    }
    let mut total: u64 =
        auto.iter().map(|a| a.2).sum::<u64>() + packs.iter().map(|p| p.1).sum::<u64>();
    auto.sort();
    let mut evicted = Vec::new();
    // The packs no region left covers: removed at once, before a region
    // is weighed against the cap.
    let mut orphans = |auto: &[(SystemTime, String, u64)],
                       total: &mut u64,
                       evicted: &mut Vec<String>|
     -> std::io::Result<()> {
        let regions: Vec<Bounds> = auto.iter().filter_map(|a| auto_bounds(&a.1)).collect();
        let mut n = 0;
        while n < packs.len() {
            let covered = packs[n]
                .2
                .is_some_and(|b| regions.iter().any(|r| overlaps(r, &b)));
            if covered {
                n += 1;
                continue;
            }
            let (name, len, _) = packs.remove(n);
            remove_evicted(&dir.join(&name))?;
            *total = total.saturating_sub(len);
            evicted.push(name);
        }
        Ok(())
    };
    orphans(&auto, &mut total, &mut evicted)?;
    let mut n = 0;
    while n < auto.len() && total > cap {
        if auto[n].1 == keep {
            n += 1;
            continue;
        }
        let (_, name, len) = auto.remove(n);
        remove_evicted(&dir.join(&name))?;
        total = total.saturating_sub(len);
        evicted.push(name);
        orphans(&auto, &mut total, &mut evicted)?;
    }
    Ok(evicted)
}

/// Remove the evicted file at `path`. A file already gone is removed: the
/// region jobs and the height fetch evict from listings of their own (1863),
/// and the one that comes second finds the file the other took.
fn remove_evicted(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    tests::another_writer_removes(path);
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::map::LoadReason;

    /// The directories in which another writer of the maps directory
    /// removes each file an eviction listed, just before the eviction
    /// removes it: the race of a region's eviction and a height fetch's
    /// (audit 1863). Keyed by directory, so tests beside it run as ever.
    static RACED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(super) fn another_writer_removes(path: &Path) {
        let raced = RACED.lock().unwrap_or_else(PoisonError::into_inner);
        if path.parent().is_some_and(|d| raced.iter().any(|r| r == d)) {
            let _ = std::fs::remove_file(path);
        }
    }
    use leviculum_map::mercator::World;
    use leviculum_mapcut::serve::Serve;
    use std::time::Duration;

    const FIXTURE: &[u8] = include_bytes!("../../leviculum-mapcut/fixtures/oltrarno.pmtiles");

    fn want(west: i32) -> MapWant {
        MapWant {
            bounds: Bounds {
                west,
                south: 437_405_000,
                east: west + 210_000,
                north: 437_545_000,
            },
            zooms: (14, 15),
            floor: 14,
            reason: LoadReason::View,
            centre: World { x: 0, y: 0 },
        }
    }

    /// The language a region is cut in comes from the locale settings:
    /// the language part, lower case, and none for C or POSIX.
    #[test]
    fn the_cut_language_is_the_locales() {
        for (setting, want) in [
            ("de_DE.UTF-8", Some("de")),
            ("en_GB", Some("en")),
            ("pt-BR", Some("pt")),
            ("fil_PH.UTF-8", Some("fil")),
            ("sr_RS@latin", Some("sr")),
            ("C", None),
            ("C.UTF-8", None),
            ("POSIX", None),
            ("", None),
        ] {
            assert_eq!(language_of(setting).as_deref(), want, "{setting:?}");
        }
    }

    /// Poll until a step that is not `Started` comes, at most ten seconds.
    fn settle(loader: &mut Loader, now_ms: u64) -> Vec<Step> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let steps = loader.poll(now_ms);
            if steps.iter().any(|s| !matches!(s, Step::Started(_))) {
                return steps;
            }
            assert!(Instant::now() < deadline, "no load ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_load_lands_in_place_and_bumps_the_epoch() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let w = want(112_615_000);
        assert_eq!(loader.ask(w.clone(), 0), [Step::Started(w.clone())]);
        assert_eq!(
            loader.ask(w.clone(), 0),
            [Step::Skipped {
                want: w.clone(),
                why: "queued"
            }]
        );
        let steps = settle(&mut loader, 0);
        let [Step::Done { landed, .. }] = steps.as_slice() else {
            panic!("{steps:?}");
        };
        assert_eq!(landed.name, auto_name(&w.bounds, (14, 15)));
        assert_eq!(maps.epoch(), 1);
        // The z14 round told the painters before the region was whole
        // (1291).
        assert!(maps.progress() >= 1);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            std::slice::from_ref(&landed.name),
            "no part file left"
        );
        assert_eq!(maps.headers()[0].bounds, w.bounds);
    }

    /// Cut the fixture's box of `w` under `profile` into `dir` under the
    /// name the autoload gives it: a region installed before.
    fn installed(serve: &Serve, dir: &Path, w: &MapWant, profile: u8) -> PathBuf {
        let archive =
            Archive::from_source(HttpRange::new(&serve.url("o.pmtiles"))).expect("archive");
        let opts = leviculum_mapcut::cut::CutOptions {
            bounds: w.bounds,
            zooms: w.zooms.0..=w.zooms.1,
            profile,
            attribution: "test".to_string(),
            notice: "test".to_string(),
            threads: 1,
            locale: None,
            buildings: false,
            pois: false,
            round: leviculum_mapcut::cut::ROUND,
        };
        let path = dir.join(auto_name(&w.bounds, w.zooms));
        let file = File::create(&path).expect("create");
        leviculum_mapcut::cut::cut(&archive, &opts, BufWriter::new(file)).expect("cut");
        path
    }

    /// The profile of the region file at `path`.
    fn profile_of(path: &Path) -> u8 {
        Region::open(File::open(path).expect("open"))
            .expect("region")
            .header()
            .profile
    }

    /// A load that replaces an installed region writes beside it and
    /// takes its place only once the new file verifies (1727, the audit
    /// of 1707): the link breaking mid-body leaves the old region byte
    /// for byte, and a later load that succeeds replaces it, no part
    /// file left.
    #[test]
    fn a_load_that_breaks_leaves_the_region_it_was_to_replace() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let w = want(112_615_000);
        // What a whole load sends, measured on a directory of its own.
        let measure = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(
            Maps::new(Some(measure.path().to_path_buf())),
            Source::Url(serve.url("o.pmtiles")),
            1 << 30,
        );
        loader.ask(w.clone(), 0);
        assert!(matches!(
            settle(&mut loader, 0).as_slice(),
            [Step::Done { .. }]
        ));
        let whole = serve.sent();

        let dir = tempfile::tempdir().expect("tmp");
        let path = installed(&serve, dir.path(), &w, leviculum_map::profile::CUT_1);
        let old = std::fs::read(&path).expect("read");
        assert_eq!(profile_of(&path), leviculum_map::profile::CUT_1);
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        serve.drop_at(Some(serve.sent() + whole / 2));
        loader.ask(w.clone(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { .. }]),
            "{steps:?}"
        );
        assert_eq!(
            std::fs::read(&path).expect("still there"),
            old,
            "byte for byte"
        );
        let names = |d: &Path| -> Vec<String> {
            let mut n: Vec<String> = std::fs::read_dir(d)
                .expect("dir")
                .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                .collect();
            n.sort();
            n
        };
        let name = auto_name(&w.bounds, w.zooms);
        assert_eq!(
            names(dir.path()),
            std::slice::from_ref(&name),
            "no part file left"
        );

        serve.drop_at(None);
        loader.ask(w.clone(), BACKOFF_FIRST_MS);
        let steps = settle(&mut loader, BACKOFF_FIRST_MS);
        let [Step::Done { landed, .. }] = steps.as_slice() else {
            panic!("{steps:?}");
        };
        assert_eq!(landed.name, name);
        assert_eq!(names(dir.path()), [name], "replaced in place");
        assert_eq!(profile_of(&path), leviculum_map::profile::CUT_2);
        assert_ne!(std::fs::read(&path).expect("read"), old);
    }

    /// An installed autoloaded region of profile 1 is replaced, not
    /// joined by another (1727, the audit of 1707): a view over it asks
    /// for its own box and zooms, so the load lands under its name. The
    /// region's box is not the tile grid's, as a screen's box or a hand
    /// asked box is not: the cells and the screen's box of the view would
    /// land beside it under other names and leave the old one on disk
    /// for ever.
    #[test]
    fn a_view_over_an_old_profile_region_replaces_it() {
        use crate::ui::viewload::wants;
        use leviculum_map::mercator::project;
        use leviculum_map::viewport::Viewport;

        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let w = want(112_615_000);
        let path = installed(&serve, dir.path(), &w, leviculum_map::profile::CUT_1);
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        // A z16 view inside the box.
        let b = w.bounds;
        let centre = project(leviculum_map::mercator::LatLon {
            lat_e7: (b.south / 2) + (b.north / 2),
            lon_e7: (b.west / 2) + (b.east / 2),
        });
        let view = Viewport::new(centre, 16, 480, 320);
        let mut now = 0;
        for _ in 0..8 {
            let asked = wants(&view, &maps.headers(), false, None);
            if asked.here.is_empty() {
                break;
            }
            loader.schedule(asked.here, Vec::new(), now);
            while loader.busy() {
                settle(&mut loader, now);
                now += 1;
            }
        }
        assert!(wants(&view, &maps.headers(), false, None).here.is_empty());
        let (regions, problems) = maps.load();
        assert!(problems.is_empty(), "{problems:?}");
        let covering: Vec<(&str, u8)> = regions
            .iter()
            .filter(|r| r.region.header().bounds.holds(&b))
            .map(|r| (r.name.as_str(), r.region.header().profile))
            .collect();
        let name = auto_name(&b, w.zooms);
        assert_eq!(
            covering,
            [(name.as_str(), leviculum_map::profile::CUT_2)],
            "exactly one region covers the box, of profile 2"
        );
        assert!(path.exists());
    }

    #[test]
    fn a_failing_box_toasts_its_first_failure_and_retries_quietly() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let w = want(112_615_000);
        loader.ask(w.clone(), 0);
        let steps = settle(&mut loader, 0);
        let [Step::Failed {
            toast: Some(toast), ..
        }] = steps.as_slice()
        else {
            panic!("{steps:?}");
        };
        assert!(toast.contains("503"), "{toast}");
        // The box stays queued, not loading; inside its wait nothing
        // goes out, on a key or a frame.
        assert_eq!(loader.order().len(), 1);
        assert!(!loader.busy(), "waiting to retry is not loading");
        let asked = serve.requests();
        for now in [1, 1_000, SCREEN_RETRY_FIRST_MS - 1] {
            assert!(loader.poll(now).is_empty(), "not due at {now}");
        }
        assert_eq!(serve.requests(), asked, "no retry storm");
        // When due the loader retries by itself; the retry's failure
        // says nothing again, and the wait doubles.
        assert!(matches!(
            loader.poll(SCREEN_RETRY_FIRST_MS).as_slice(),
            [Step::Started(_)]
        ));
        let steps = settle(&mut loader, SCREEN_RETRY_FIRST_MS);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { toast: None, .. }]),
            "quiet retry: {steps:?}"
        );
        assert!(loader.poll(3 * SCREEN_RETRY_FIRST_MS - 1).is_empty());
        // An ask inside the wait is "queued", not a retry: asks come on
        // every frame too, and a wait a frame could end is no wait.
        let steps = loader.ask(w.clone(), 3 * SCREEN_RETRY_FIRST_MS - 1);
        assert!(
            matches!(steps.as_slice(), [Step::Skipped { why: "queued", .. }]),
            "{steps:?}"
        );
        // The doubled wait ends: the loader retries by itself.
        assert!(matches!(
            loader.poll(3 * SCREEN_RETRY_FIRST_MS).as_slice(),
            [Step::Started(_)]
        ));
        settle(&mut loader, 3 * SCREEN_RETRY_FIRST_MS);
        assert_eq!(maps.epoch(), 0, "nothing landed");
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 0);
    }

    #[test]
    fn no_internet_is_said_once() {
        let serve = Serve::start(Vec::new()).expect("serve");
        let url = serve.url("o.pmtiles");
        drop(serve);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(url), 1 << 30);
        let mut toasts = Vec::new();
        for west in [112_615_000, 112_000_000, 111_000_000] {
            loader.ask(want(west), 0);
            for step in settle(&mut loader, 0) {
                if let Step::Failed { toast, .. } = step {
                    toasts.push(toast);
                }
            }
        }
        assert_eq!(toasts.len(), 3);
        assert!(toasts[0]
            .as_deref()
            .is_some_and(|t| t.contains("no internet")));
        assert_eq!(toasts[1..], [None, None]);
    }

    /// A source that takes the connection and never answers: a load
    /// started stays running for the test.
    fn stalling() -> (std::net::TcpListener, Source) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("its address").port();
        (
            listener,
            Source::Url(format!("http://127.0.0.1:{port}/o.pmtiles")),
        )
    }

    /// The `n`th of a row of boxes.
    fn nth(n: i32) -> MapWant {
        want(112_615_000 + n * 1000)
    }

    /// The `n`th box as the screen's own load (1527).
    fn screen(n: i32) -> MapWant {
        MapWant {
            reason: LoadReason::Screen,
            ..nth(n)
        }
    }

    /// The queue as the numbers of [`nth`]'s boxes.
    fn order(loader: &Loader) -> Vec<(i32, Rank, bool)> {
        loader
            .order()
            .into_iter()
            .map(|(b, rank, running)| ((b.west - 112_615_000) / 1000, rank, running))
            .collect()
    }

    /// Two boxes queued for the screen, the view moves: the box under the
    /// new screen goes first, the one still on it next, the one the view
    /// left last (1292). A box asked for by hand keeps its place.
    #[test]
    fn the_box_on_screen_now_goes_first() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (p, a, b, c) = (nth(1), nth(2), nth(3), nth(4));
        assert!(matches!(loader.ask(p, 0).as_slice(), [Step::Started(_)]));
        assert!(loader
            .schedule(vec![a.clone(), b.clone()], Vec::new(), 0)
            .is_empty());
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (2, Rank::Here, false),
                (3, Rank::Here, false)
            ]
        );
        loader.schedule(vec![c, b], Vec::new(), 250);
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (4, Rank::Here, false),
                (3, Rank::Here, false),
                (2, Rank::Away, false)
            ]
        );
        assert!(loader.loading_here());
        // The screen's own box (1527) goes before the cells under it; the
        // view moves, the old screen box is dropped, not kept as left.
        let (s, t) = (screen(6), screen(7));
        loader.schedule(vec![s.clone(), nth(4), nth(3)], Vec::new(), 500);
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (6, Rank::Screen, false),
                (4, Rank::Here, false),
                (3, Rank::Here, false),
                (2, Rank::Away, false)
            ]
        );
        loader.schedule(vec![t, nth(3)], Vec::new(), 750);
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (7, Rank::Screen, false),
                (3, Rank::Here, false),
                (4, Rank::Away, false),
                (2, Rank::Away, false)
            ],
            "the screen box the view left is gone: {s:?}"
        );
    }

    /// The screen's own box loads first and alone (1527): the cell under
    /// the screen waits until it landed, so no byte of the cell comes
    /// before the screen's tiles; a screen's box starts beside a cell for
    /// the screen that runs already, never beside another screen box.
    #[test]
    fn the_screens_own_box_loads_before_the_cell_around_it() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (s, a, n) = (screen(1), nth(2), nth(5));
        let steps = loader.schedule(vec![s.clone(), a.clone()], vec![n.clone()], 0);
        assert_eq!(steps, [Step::Started(s.clone())], "the screen alone");
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Screen, true),
                (2, Rank::Here, false),
                (5, Rank::Ahead, false)
            ]
        );
        assert!(loader.loading_here());
        // A cell for the screen runs (the screen's box landed meanwhile,
        // here: was never asked); a new screen box starts beside it.
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        assert_eq!(
            loader.schedule(vec![a.clone()], Vec::new(), 0),
            [Step::Started(a.clone())]
        );
        let t = screen(3);
        assert_eq!(
            loader.schedule(vec![t.clone(), a.clone()], Vec::new(), 250),
            [Step::Started(t.clone())],
            "beside the cell"
        );
        // A second screen box does not wait for the first, which the view
        // left (1836): that one pauses at its next round, the new one
        // starts beside the cell at once.
        let u = screen(4);
        assert_eq!(
            loader.schedule(vec![u.clone(), a.clone()], Vec::new(), 500),
            [
                Step::Held {
                    want: t.clone(),
                    how: Held::Paused
                },
                Step::Started(u.clone())
            ]
        );
        assert_eq!(
            order(&loader),
            [
                (2, Rank::Here, true),
                (3, Rank::Away, true),
                (4, Rank::Screen, true)
            ]
        );
    }

    /// A screen's box a running load brings is not loaded twice (1527),
    /// and that load is then the screen's: the cell under the screen waits
    /// until it landed, as for the screen's own box (1616). The window's
    /// first-tile walk caught the cell starting beside it: a drag's last
    /// screen box held the box of the view it stopped on, ranked as left,
    /// and the cell took half the link from the screen's tiles.
    #[test]
    fn a_running_load_that_brings_the_screens_box_holds_the_cell() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (s, a) = (screen(1), nth(30));
        assert_eq!(
            loader.schedule(vec![s.clone(), a.clone()], Vec::new(), 0),
            [Step::Started(s.clone())]
        );
        // The view moved on inside the box loading: its screen's box is
        // that load's.
        let inner = MapWant {
            bounds: Bounds {
                west: s.bounds.west + 1000,
                east: s.bounds.east - 1000,
                ..s.bounds
            },
            ..s.clone()
        };
        let steps = loader.schedule(vec![inner, a.clone()], Vec::new(), 250);
        assert!(steps.is_empty(), "the cell waits: {steps:?}");
        assert_eq!(
            order(&loader),
            [(1, Rank::Screen, true), (30, Rank::Here, false)]
        );
        assert!(loader.loading_here());
    }

    /// A load whose area the view left runs on, as the cut cannot pause;
    /// the box under the new screen starts beside it, and the chip's
    /// "here" names it, not the old one (1292).
    #[test]
    fn a_load_the_view_left_does_not_hold_up_the_screen() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (a, b, n) = (nth(2), nth(3), nth(5));
        let steps = loader.schedule(vec![a.clone()], vec![n.clone()], 0);
        assert_eq!(steps, [Step::Started(a.clone())]);
        assert!(loader.loading_here());
        // The view moved on: a is left, b is on screen.
        let steps = loader.schedule(vec![b.clone()], Vec::new(), 250);
        let paused = Step::Held {
            want: a.clone(),
            how: Held::Paused,
        };
        assert_eq!(
            steps,
            [paused, Step::Started(b.clone())],
            "the old load pauses (1836)"
        );
        assert_eq!(
            order(&loader),
            [
                (2, Rank::Away, true),
                (3, Rank::Here, true),
                (5, Rank::Away, false)
            ]
        );
        // Away only: no chip for the screen; the paused load goes on.
        let steps = loader.schedule(Vec::new(), Vec::new(), 500);
        assert_eq!(
            steps,
            [Step::Held {
                want: a.clone(),
                how: Held::Resumed
            }]
        );
        assert!(loader.busy() && !loader.loading_here());
        // Never more than the queue's cap of left boxes.
        let left: Vec<MapWant> = (10..30).map(nth).collect();
        loader.schedule(Vec::new(), left, 750);
        loader.schedule(Vec::new(), Vec::new(), 1000);
        let queued = order(&loader).iter().filter(|o| !o.2).count();
        assert_eq!(queued, MAX_AWAY);
        // The screen's own box (1527) goes before every queued box and
        // waits for no load for an area left (1836): both pause at their
        // next round and it starts at once.
        let s = screen(40);
        assert_eq!(
            loader.schedule(vec![s.clone()], Vec::new(), 1250),
            [
                Step::Held {
                    want: a.clone(),
                    how: Held::Paused
                },
                Step::Held {
                    want: b.clone(),
                    how: Held::Paused
                },
                Step::Started(s.clone())
            ]
        );
        assert_eq!(order(&loader)[2], (40, Rank::Screen, true));
        assert!(loader.loading_here());
    }

    /// Poll until `done` holds of every step seen, at most twenty seconds;
    /// every step seen.
    fn poll_until(loader: &mut Loader, now_ms: u64, done: impl Fn(&[Step]) -> bool) -> Vec<Step> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut all = Vec::new();
        loop {
            all.extend(loader.poll(now_ms));
            if done(&all) {
                return all;
            }
            assert!(Instant::now() < deadline, "not reached: {all:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn landed(steps: &[Step], w: &MapWant) -> bool {
        steps
            .iter()
            .any(|s| matches!(s, Step::Done { want, .. } if want.bounds == w.bounds))
    }

    fn state_of(loader: &Loader, w: &MapWant) -> Option<LoadState> {
        loader
            .report(0)
            .into_iter()
            .find(|r| r.0 == w.bounds)
            .map(|r| r.2)
    }

    /// A load whose area the view left pauses between two rounds, its
    /// file kept in the maps directory as far as it came, while the
    /// screen's box loads alone; once that landed it goes on from where
    /// it was and lands too (1836). Before, it ran on beside the screen's
    /// and took half the link: at 200 kB/s the screen waited 43.6 s
    /// behind two such loads (the load walk's wheel-rested-slow).
    #[test]
    fn a_load_the_view_left_pauses_until_the_screen_has_its_box() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(
            Maps::new(Some(dir.path().to_path_buf())),
            Source::Url(serve.url("o.pmtiles")),
            1 << 30,
        );
        // Both loads wait at the link until the view has moved.
        serve.hold_at(0);
        let (a, b) = (nth(2), nth(3));
        assert_eq!(
            loader.schedule(vec![a.clone()], Vec::new(), 0),
            [Step::Started(a.clone())]
        );
        assert_eq!(
            loader.schedule(vec![b.clone()], Vec::new(), 10),
            [
                Step::Held {
                    want: a.clone(),
                    how: Held::Paused
                },
                Step::Started(b.clone())
            ]
        );
        serve.release();
        // a planned, wrote its header and slots, and waits at its gate.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(LoadState::Paused { planned, bytes, .. }) = state_of(&loader, &a) {
                if planned > 0 && bytes > 0 {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "{:?}", state_of(&loader, &a));
            std::thread::sleep(Duration::from_millis(5));
        }
        let Some(LoadState::Paused {
            done, bytes_done, ..
        }) = state_of(&loader, &a)
        else {
            panic!("a runs: {:?}", state_of(&loader, &a));
        };
        assert_eq!((done, bytes_done), (0, 0), "paused before its first round");
        let file = std::fs::read_dir(dir.path())
            .expect("dir")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.file_name().and_then(|n| n.to_str()).and_then(auto_bounds) == Some(a.bounds)
            })
            .expect("the paused load's file is kept");
        let region = Region::open(File::open(&file).expect("open")).expect("a paused file reads");
        assert!(!region.is_complete());

        let steps = poll_until(&mut loader, 20, |s| landed(s, &b));
        assert!(
            !landed(&steps, &a),
            "a paused load does not land: {steps:?}"
        );
        assert!(
            steps.contains(&Step::Held {
                want: a.clone(),
                how: Held::Resumed
            }),
            "it goes on once the screen's box landed: {steps:?}"
        );
        poll_until(&mut loader, 30, |s| landed(s, &a));
        let region = Region::open(File::open(&file).expect("open")).expect("read");
        region.verify().expect("the resumed load's region verifies");
    }

    /// Once nothing for the screen runs or waits, the paused loads go on,
    /// the best ranked and among equals the oldest first, as many as may
    /// run, before any queued box starts (1836): what was loading when the
    /// user moved on is finished first, not started over.
    #[test]
    fn paused_loads_go_on_before_a_new_box_starts() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (a, b, c, n) = (nth(1), nth(2), nth(3), nth(9));
        loader.schedule(vec![a.clone()], Vec::new(), 0);
        loader.schedule(vec![b.clone()], Vec::new(), 10);
        loader.schedule(vec![c.clone()], Vec::new(), 20);
        let holds = |l: &Loader| -> Vec<(i32, Hold)> {
            l.running
                .iter()
                .map(|r| ((r.want.bounds.west - 112_615_000) / 1000, r.hold))
                .collect()
        };
        assert_eq!(
            holds(&loader),
            [(1, Hold::Pause), (2, Hold::Pause), (3, Hold::Run)]
        );
        // The view rests on an area with its regions: only a box next to
        // it is wanted, and it waits for the paused loads.
        let steps = loader.schedule(Vec::new(), vec![n.clone()], 30);
        assert_eq!(
            steps,
            [
                Step::Held {
                    want: a.clone(),
                    how: Held::Resumed
                },
                Step::Held {
                    want: b.clone(),
                    how: Held::Resumed
                },
                Step::Held {
                    want: c.clone(),
                    how: Held::Paused
                },
            ]
        );
        assert_eq!(
            holds(&loader),
            [(1, Hold::Run), (2, Hold::Run), (3, Hold::Pause)]
        );
        assert!(!order(&loader).contains(&(9, Rank::Ahead, true)));
        assert!(loader.busy(), "two loads run");
        assert!(matches!(
            state_of(&loader, &c),
            Some(LoadState::Paused { .. })
        ));
    }

    /// A paused load holds a thread and its file: past [`MAX_PAUSED`] the
    /// worst ranked, the latest started among equals, stops for good, its
    /// file removed and its box queued again, and it lands later like
    /// every other (1836): nothing wanted is lost.
    #[test]
    fn past_the_paused_cap_the_worst_is_dropped_and_loads_again() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(
            Maps::new(Some(dir.path().to_path_buf())),
            Source::Url(serve.url("o.pmtiles")),
            1 << 30,
        );
        serve.hold_at(0);
        let boxes: Vec<MapWant> = (0..=MAX_PAUSED as i32 + 1).map(nth).collect();
        for (k, w) in boxes.iter().enumerate() {
            loader.schedule(vec![w.clone()], Vec::new(), k as u64 * 10);
        }
        let dropped = &boxes[MAX_PAUSED];
        let holds: Vec<Hold> = loader.running.iter().map(|r| r.hold).collect();
        let mut want_holds = vec![Hold::Pause; MAX_PAUSED];
        want_holds.push(Hold::Stop);
        want_holds.push(Hold::Run);
        assert_eq!(holds, want_holds);
        serve.release();
        let steps = poll_until(&mut loader, 100, |s| {
            s.contains(&Step::Held {
                want: dropped.clone(),
                how: Held::Dropped,
            })
        });
        assert!(!landed(&steps, dropped));
        assert!(
            !std::fs::read_dir(dir.path())
                .expect("dir")
                .filter_map(|e| e.ok())
                .any(|e| auto_bounds(&e.file_name().to_string_lossy()) == Some(dropped.bounds)),
            "the dropped load's file is gone"
        );
        assert!(order(&loader).contains(&(MAX_PAUSED as i32, Rank::Away, false)));
        let early: Vec<Bounds> = steps
            .iter()
            .filter(|s| matches!(s, Step::Done { .. }))
            .map(|s| s.want().bounds)
            .collect();
        // Nothing for the screen is left once the screen's box landed:
        // every box lands, the dropped one too.
        loader.schedule(Vec::new(), Vec::new(), 200);
        poll_until(&mut loader, 300, |s| {
            boxes
                .iter()
                .all(|w| early.contains(&w.bounds) || landed(s, w))
        });
    }

    /// "Offline" is a state, not a latch (1292): a load that found no
    /// internet sets it, the next view's ask tries the same box again at
    /// once, with no wait, and while that load runs the state is unknown.
    #[test]
    fn offline_is_retried_by_the_loader_itself() {
        let serve = Serve::start(Vec::new()).expect("serve");
        let url = serve.url("o.pmtiles");
        drop(serve);
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(
            Maps::new(Some(dir.path().to_path_buf())),
            Source::Url(url),
            1 << 30,
        );
        let a = nth(2);
        loader.schedule(vec![a.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { toast: Some(_), .. }]),
            "{steps:?}"
        );
        // The box stays and waits, not loading: offline is a state, not
        // a latch, and the map comes back by itself when the internet
        // does (1811), while the map says "Offline", not "loading".
        assert!(loader.offline() && !loader.busy());
        assert_eq!(loader.order().len(), 1, "the box stays queued");
        // The same view's frames start nothing new inside the wait.
        assert!(loader.schedule(vec![a.clone()], Vec::new(), 1).is_empty());
        assert!(loader.poll(OFFLINE_RETRY_MS - 1).is_empty());
        let steps = loader.poll(OFFLINE_RETRY_MS);
        assert_eq!(steps, [Step::Started(a.clone())], "retried by itself");
        assert!(!loader.offline(), "unknown while it runs");
        let steps = settle(&mut loader, OFFLINE_RETRY_MS);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { toast: None, .. }]),
            "said once: {steps:?}"
        );
        assert!(loader.offline());
    }

    #[test]
    fn eviction_takes_the_least_recently_viewed_and_never_a_hand_placed_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let put = |name: &str, len: usize, age_s: u64| {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![0u8; len]).expect("write");
            let when = SystemTime::now() - Duration::from_secs(age_s);
            File::open(&path)
                .expect("open")
                .set_modified(when)
                .expect("mtime");
        };
        put("berlin.lmr", 5000, 9000);
        put("auto-old.lmr", 1000, 3000);
        put("auto-mid.lmr", 1000, 2000);
        put("auto-new.lmr", 1000, 10);
        put("auto-oldest-but-just-loaded.lmr", 1000, 5000);
        let evicted = evict(dir.path(), 2000, "auto-oldest-but-just-loaded.lmr").expect("evict");
        assert_eq!(evicted, ["auto-old.lmr", "auto-mid.lmr"]);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "auto-new.lmr",
                "auto-oldest-but-just-loaded.lmr",
                "berlin.lmr"
            ]
        );
        // A cap below the hand-placed file alone takes every other
        // autoloaded region and leaves the hand-placed one.
        let evicted = evict(dir.path(), 0, "none").expect("evict");
        assert_eq!(evicted.len(), 2);
        assert!(dir.path().join("berlin.lmr").exists());
    }

    /// A transient fault heals by itself (1811, the load walk's
    /// `fault_recovers`): a box for the screen that failed stays queued
    /// and the loader retries it on its own clock
    /// ([`SCREEN_RETRY_FIRST_MS`]), with no new decision and no key, so
    /// the view fills once the source answers again.
    #[test]
    fn a_failed_screen_box_retries_by_itself_and_fills() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let s = screen(1);
        loader.schedule(vec![s.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { .. }]),
            "{steps:?}"
        );
        assert_eq!(
            loader.order(),
            [(s.bounds, Rank::Screen, false)],
            "the failed box stays queued"
        );
        assert!(!loader.busy(), "waiting to retry is not loading");
        // The source answers again; the loader alone retries when due.
        serve.fail(false);
        assert!(
            loader.poll(SCREEN_RETRY_FIRST_MS - 1).is_empty(),
            "not due yet"
        );
        let steps = loader.poll(SCREEN_RETRY_FIRST_MS);
        assert!(matches!(steps.as_slice(), [Step::Started(_)]), "{steps:?}");
        let steps = settle(&mut loader, SCREEN_RETRY_FIRST_MS);
        assert!(matches!(steps.as_slice(), [Step::Done { .. }]), "{steps:?}");
        assert_eq!(maps.epoch(), 1);
    }

    /// A landing says the source answers: every backoff is cleared and a
    /// queued box waiting to retry is due at once (1811).
    #[test]
    fn a_landing_clears_every_backoff() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let s = screen(1);
        loader.schedule(vec![s.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { .. }]),
            "{steps:?}"
        );
        // The user asks another box by hand; the source answers now.
        serve.fail(false);
        let a = nth(30);
        assert!(matches!(
            loader.ask(a.clone(), 10).as_slice(),
            [Step::Started(_)]
        ));
        let steps = settle(&mut loader, 10);
        // The hand-asked box lands; the failed screen box, cleared of its
        // wait by the landing, starts in the same poll.
        assert!(
            steps
                .iter()
                .any(|st| matches!(st, Step::Done { want, .. } if want.bounds == a.bounds)),
            "{steps:?}"
        );
        assert!(
            steps
                .iter()
                .any(|st| matches!(st, Step::Started(w) if w.bounds == s.bounds)),
            "cleared and started: {steps:?}"
        );
    }

    /// A box that keeps failing keeps its ladder when a neighbour lands
    /// (1821, the audit of 1811): the landing makes its wait due at once
    /// (the source answers), but its failure count stays, so the next
    /// failure is quiet and books the ladder's next rung instead of
    /// starting over at [`SCREEN_RETRY_FIRST_MS`] — and toasting over —
    /// on every landing.
    #[test]
    fn a_box_that_keeps_failing_keeps_its_ladder_when_a_neighbour_lands() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let s = screen(1);
        loader.schedule(vec![s.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { toast: Some(_), .. }]),
            "the first failure speaks: {steps:?}"
        );
        // A neighbour lands while the failed box waits.
        serve.fail(false);
        let a = nth(30);
        assert!(matches!(
            loader.ask(a.clone(), 10).as_slice(),
            [Step::Started(_)]
        ));
        // Wait for the neighbour's region to be whole on disk, so the
        // serve can fail again before the landing is polled: its result
        // is sent right after the file verifies.
        let path = dir.path().join(auto_name(&a.bounds, a.zooms));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let whole = File::open(&path)
                .ok()
                .and_then(|f| Region::open(f).ok())
                .is_some_and(|r| r.verify().is_ok());
            if whole {
                break;
            }
            assert!(Instant::now() < deadline, "the neighbour never landed");
            std::thread::sleep(Duration::from_millis(5));
        }
        serve.fail(true);
        let steps = settle(&mut loader, 10);
        assert!(
            steps
                .iter()
                .any(|st| matches!(st, Step::Done { want, .. } if want.bounds == a.bounds)),
            "{steps:?}"
        );
        assert!(
            steps
                .iter()
                .any(|st| matches!(st, Step::Started(w) if w.bounds == s.bounds)),
            "the landing made the wait due: {steps:?}"
        );
        // The retried box fails again: quietly, and on the ladder's next
        // rung, not back at the first.
        let steps = settle(&mut loader, 10);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { toast: None, .. }]),
            "the second failure is quiet: {steps:?}"
        );
        let report = loader.report(10);
        let LoadState::Retrying { in_ms } = report[0].2 else {
            panic!("{:?}", report[0].2);
        };
        assert!(
            in_ms > SCREEN_RETRY_FIRST_MS,
            "the ladder continued: {in_ms} ms"
        );
    }

    /// While a box for the screen waits to retry, nothing else starts
    /// (1811, the walk's `screen_first` under a fault): the link is not
    /// spent next to a screen that lacks its data.
    #[test]
    fn nothing_starts_behind_a_screen_box_waiting_to_retry() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let (s, n) = (screen(1), nth(5));
        loader.schedule(vec![s.clone()], vec![n.clone()], 0);
        // Both fail: the screen box first, the ahead cell after it.
        settle(&mut loader, 0);
        while loader.order().iter().any(|(_, _, running)| *running) {
            settle(&mut loader, 0);
        }
        serve.fail(false);
        // Before the screen box is due, the ahead cell must not start.
        let steps = loader.poll(1_000);
        assert!(
            !steps
                .iter()
                .any(|st| matches!(st, Step::Started(w) if w.bounds == n.bounds)),
            "the ahead cell waited: {steps:?}"
        );
        // When the screen box is due it goes first.
        let steps = loader.poll(SCREEN_RETRY_FIRST_MS);
        assert!(
            matches!(steps.as_slice(), [Step::Started(w)] if w.bounds == s.bounds),
            "{steps:?}"
        );
    }

    /// A frame-driven frontend runs the loader only inside a frame, and
    /// draws a frame only when the last one told it to ask again (1821,
    /// the audit of 1811): while a box waits to retry — when neither
    /// [`Loader::busy`] nor [`Loader::loading_here`] claims activity —
    /// [`Loader::next_due_ms`] names the instant of the loader's next
    /// own move, so a screen box that failed at rest is retried with no
    /// touch and fills once the source answers.
    #[test]
    fn audit_a_box_waiting_to_retry_keeps_a_frame_driven_platform_asking() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let s = screen(1);
        loader.schedule(vec![s.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { .. }]),
            "{steps:?}"
        );
        // The source answers again; nobody touches the map. The platform
        // asks for frames only the way the phone does: at once while a
        // load runs, at the named instant while a box waits, never else.
        serve.fail(false);
        let mut now: u64 = 100;
        let deadline = Instant::now() + Duration::from_secs(10);
        while maps.epoch() == 0 {
            assert!(Instant::now() < deadline, "the region never landed");
            if loader.busy() || loader.loading_here() {
                loader.poll(now);
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let Some(in_ms) = loader.next_due_ms(now) else {
                panic!("nothing waits and nothing runs: the platform would stop asking");
            };
            now += in_ms;
            let steps = loader.poll(now);
            assert!(
                steps
                    .iter()
                    .any(|st| matches!(st, Step::Started(w) if w.bounds == s.bounds)),
                "due at the named instant: {steps:?}"
            );
        }
        assert_eq!(maps.epoch(), 1, "the screen box landed hands off");
    }

    /// A box the source holds no tiles for fails for good (1811, the
    /// phone's first-frame walk: a z10 screen box past the archive): it
    /// is not requeued and not retried, as retrying cannot help, so its
    /// failures never spend the link beside a box that can load.
    #[test]
    fn a_box_with_no_tiles_is_not_retried() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        // A box far from the fixture: the plan finds nothing there.
        let far = MapWant {
            reason: LoadReason::Screen,
            ..want(900_000_000)
        };
        loader.schedule(vec![far.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        let [Step::Failed { detail, .. }] = steps.as_slice() else {
            panic!("{steps:?}");
        };
        assert!(detail.contains("no tiles"), "{detail}");
        assert!(loader.order().is_empty(), "not requeued");
        assert!(loader.poll(60_000).is_empty(), "not retried");
    }

    /// What the loader tells the map about each box (1811, for 1812):
    /// running with its tiles done of planned, waiting at its place,
    /// failed with its retry in milliseconds.
    #[test]
    fn the_loader_reports_each_boxes_state() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let s = screen(1);
        loader.schedule(vec![s.clone()], Vec::new(), 0);
        settle(&mut loader, 0);
        let report = loader.report(1_000);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].0, s.bounds);
        let LoadState::Retrying { in_ms } = report[0].2 else {
            panic!("{:?}", report[0].2);
        };
        assert!(in_ms > 0 && in_ms <= SCREEN_RETRY_FIRST_MS, "{in_ms}");
        // A stalling source: the load runs, the box behind it waits.
        let (_held, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        loader.schedule(vec![screen(1)], vec![nth(5)], 0);
        let report = loader.report(0);
        assert_eq!(report.len(), 2);
        assert!(
            matches!(report[0].2, LoadState::Running { .. }),
            "{report:?}"
        );
        assert!(
            matches!(report[1].2, LoadState::Waiting { place: 0 }),
            "{report:?}"
        );
    }

    /// Audit 1821: the phone wakes at `next_due_ms` (MapView.java:1252-1264,
    /// at least 50 ms). When the earliest due box is an off-screen one that
    /// cannot start because an on-screen box ahead of it still waits
    /// (start_next's screen_waits), a wake of 0 makes the phone redraw
    /// every 50 ms until the screen box's retry. Safe outcome: at the
    /// named wake something starts, or the named wake is later.
    #[test]
    fn audit_a_named_wake_always_lets_something_start() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let (s, a) = (screen(1), nth(5));
        // The neighbour loads first and fails: the off-screen ladder.
        loader.schedule(Vec::new(), vec![a.clone()], 0);
        let steps = settle(&mut loader, 0);
        assert!(
            steps.iter().any(|st| matches!(st, Step::Failed { .. })),
            "{steps:?}"
        );
        let ahead_due = loader.next_due_ms(0).expect("the neighbour waits");
        // The view moves onto the screen box; it fails until its wait
        // outlasts the neighbour's.
        let mut now: u64 = 1_000;
        loader.schedule(vec![s.clone()], vec![a.clone()], now);
        loop {
            let steps = settle(&mut loader, now);
            assert!(
                steps
                    .iter()
                    .any(|st| matches!(st, Step::Failed { want, .. } if want.bounds == s.bounds)),
                "{steps:?}"
            );
            let state = loader
                .report(now)
                .into_iter()
                .find(|(b, _, _)| *b == s.bounds)
                .map(|(_, _, st)| st);
            let Some(LoadState::Retrying { in_ms }) = state else {
                panic!("{state:?}");
            };
            if now + in_ms > ahead_due + 1_000 {
                break;
            }
            now += in_ms;
            let started = loader.poll(now);
            assert!(
                started
                    .iter()
                    .any(|st| matches!(st, Step::Started(w) if w.bounds == s.bounds)),
                "{started:?}"
            );
        }
        // The neighbour's wait is over, the screen box's is not.
        let t = ahead_due + 500;
        assert!(
            !loader.busy() && !loader.loading_here(),
            "{:?}",
            loader.report(t)
        );
        let wake = loader.next_due_ms(t).expect("boxes wait");
        let report = loader.report(t);
        let steps = loader.poll(t + wake);
        assert!(
            !steps.is_empty() || loader.busy(),
            "the loader named a wake in {wake} ms, yet nothing can start then \
             (an on-screen box still waits): the phone redraws every 50 ms \
             until it does: {report:?}"
        );
    }
    /// An off-screen box queued behind a screen box that waits to retry
    /// does not start, so the loader is not busy with it (`busy` mirrors
    /// `start_next`; the coverage of 1850 found the branch unrun).
    #[test]
    fn a_box_held_behind_a_waiting_screen_box_is_not_loading() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let (s, b) = (screen(1), nth(7));
        loader.schedule(vec![s.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            steps.iter().any(|st| matches!(st, Step::Failed { .. })),
            "{steps:?}"
        );
        let steps = loader.schedule(vec![s], vec![b.clone()], 10);
        assert!(
            !steps.iter().any(|st| matches!(st, Step::Started(_))),
            "nothing starts beside a screen box that waits: {steps:?}"
        );
        let report = loader.report(10);
        assert!(
            report.iter().any(|(bounds, _, _)| *bounds == b.bounds),
            "the off-screen box is queued: {report:?}"
        );
        assert!(!loader.busy(), "a held box is not loading: {report:?}");
        // The wake is the screen box's retry, counted from now.
        let waits = report
            .iter()
            .find_map(|(_, _, st)| match st {
                LoadState::Retrying { in_ms } => Some(*in_ms),
                _ => None,
            })
            .expect("the screen box retries");
        assert_eq!(loader.next_due_ms(10), Some(waits), "{report:?}");
    }

    /// A box due now that the running load holds back names a wake of 0:
    /// the loader can start it the moment the running load ends.
    #[test]
    fn a_due_box_behind_a_running_load_names_a_wake_of_now() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30);
        // Two boxes off the screen: the first starts, the second may not
        // start beside it, and a load stays running until `poll` reads it.
        let steps = loader.schedule(Vec::new(), vec![nth(3), nth(5)], 0);
        assert_eq!(
            steps
                .iter()
                .filter(|st| matches!(st, Step::Started(_)))
                .count(),
            1,
            "{steps:?}"
        );
        assert_eq!(loader.next_due_ms(0), Some(0));
    }

    // 1863: the height packs come with the regions.

    const CHEBBI: &[u8] = include_bytes!("../../leviculum-map/fixtures/erg-chebbi.lmh");

    /// The block of `w`'s centre at the Erg Chebbi fixture's block zoom,
    /// and a one-block pack file there: the fixture's block with its
    /// coordinates moved under Florence, where the region fixture is (the
    /// heights are data, a tile's checksum covers its heights alone).
    fn pack_under(w: &MapWant) -> ((u32, u32), Vec<u8>) {
        use leviculum_map::elevation::{split, write_directory, Directory, DirectoryInfo};
        use leviculum_map::mercator::{x_of_lon, y_of_lat};
        let (dir, ranges) = split(CHEBBI).expect("split");
        let zoom = dir.block_zoom();
        let shift = 32 - u32::from(zoom);
        let x = x_of_lon((w.bounds.west + w.bounds.east) / 2) >> shift;
        let y = y_of_lat((w.bounds.south + w.bounds.north) / 2) >> shift;
        let mut block = CHEBBI[ranges[1].clone()].to_vec();
        block[12..16].copy_from_slice(&x.to_le_bytes());
        block[16..20].copy_from_slice(&y.to_le_bytes());
        let (d, _) = Directory::parse_prefix(CHEBBI).expect("directory");
        let mut pack = write_directory(
            &DirectoryInfo {
                tile_zoom: d.tile_zoom(),
                block_zoom: zoom,
                bounds: [w.bounds.west, w.bounds.south, w.bounds.east, w.bounds.north],
                attribution: d.attribution(),
                notice: d.notice(),
            },
            &[(x, y)],
        )
        .expect("write");
        pack.extend_from_slice(&block);
        ((x, y), pack)
    }

    /// A server of the region fixture, a height pack under `w` and its
    /// catalogue; the pack's local name.
    fn heights_server(w: &MapWant) -> (Serve, String) {
        let (xy, pack) = pack_under(w);
        let mut catalogue = Catalogue::default();
        catalogue.add_pack("tuscany.lmh", &pack).expect("catalogue");
        let serve = Serve::start(vec![
            ("o.pmtiles".into(), FIXTURE.to_vec()),
            ("tuscany.lmh".into(), pack),
            (CATALOGUE_NAME.into(), catalogue.write().into_bytes()),
        ])
        .expect("serve");
        (serve, format!("{AUTO_PREFIX}h11-{}-{}.lmh", xy.0, xy.1))
    }

    /// Poll until a heights step comes, at most ten seconds; the steps
    /// before it too.
    fn settle_heights(loader: &mut Loader, now_ms: u64) -> Vec<Step> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        loop {
            let steps = loader.poll(now_ms);
            let done = steps.iter().any(|s| s.kind() == StepKind::Heights);
            all.extend(steps);
            if done {
                return all;
            }
            assert!(Instant::now() < deadline, "no height fetch ended: {all:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_landed_region_brings_its_height_pack_after_it() {
        let w = want(112_615_000);
        let (serve, pack) = heights_server(&w);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30)
            .with_heights(serve.url(CATALOGUE_NAME));
        loader.ask(w.clone(), 0);
        let steps = settle_heights(&mut loader, 0);
        let kinds: Vec<StepKind> = steps.iter().map(Step::kind).collect();
        assert_eq!(kinds, [StepKind::Done, StepKind::Heights], "{steps:?}");
        let Step::Heights {
            want: got,
            landed: Ok(landed),
            ..
        } = &steps[1]
        else {
            panic!("{steps:?}");
        };
        assert_eq!(got, &w);
        assert_eq!(landed.packs, std::slice::from_ref(&pack));
        assert_eq!(landed.had, 0);
        assert!(landed.fetched > 0);
        assert_eq!(
            names(dir.path()),
            [auto_name(&w.bounds, (14, 15)), pack.clone()]
        );
        // Every painter sees it: the relief has a pack to shade from.
        let relief = crate::relief::ReliefStore::default();
        relief.refresh(maps.dir());
        assert!(relief.any());
        assert!(!loader.busy());
        // The same region again: the pack is there, nothing fetched.
        std::fs::remove_file(dir.path().join(auto_name(&w.bounds, (14, 15)))).expect("rm");
        loader.ask(w.clone(), 0);
        let steps = settle_heights(&mut loader, 0);
        let Some(Step::Heights {
            landed: Ok(again), ..
        }) = steps.last()
        else {
            panic!("{steps:?}");
        };
        assert_eq!((again.packs.len(), again.had, again.fetched), (0, 1, 0));
    }

    #[test]
    fn a_region_on_disk_before_the_loader_starts_brings_its_pack() {
        let w = want(112_615_000);
        let (serve, pack) = heights_server(&w);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut first = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        first.ask(w.clone(), 0);
        let steps = settle(&mut first, 0);
        assert!(matches!(steps.as_slice(), [Step::Done { .. }]), "{steps:?}");
        drop(first);
        let region = auto_name(&w.bounds, (14, 15));
        assert_eq!(names(dir.path()), std::slice::from_ref(&region));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30)
            .with_heights(serve.url(CATALOGUE_NAME));
        // With the autoload off the view never schedules: nothing is
        // fetched by itself.
        let requests = serve.requests();
        for _ in 0..20 {
            assert!(loader.poll(0).is_empty());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            serve.requests(),
            requests,
            "nothing fetched without the view"
        );
        // The view over the region: nothing uncovered, no region asked.
        assert!(loader.schedule(Vec::new(), Vec::new(), 0).is_empty());
        let steps = settle_heights(&mut loader, 0);
        let Some(Step::Heights {
            want: got,
            landed: Ok(landed),
            ..
        }) = steps.last()
        else {
            panic!("{steps:?}");
        };
        assert_eq!(got.bounds, w.bounds);
        assert_eq!(landed.packs, std::slice::from_ref(&pack));
        assert_eq!(names(dir.path()), [region, pack]);
        // Once a session: nothing more is fetched.
        let requests = serve.requests();
        for _ in 0..20 {
            assert!(loader.poll(0).is_empty());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(serve.requests(), requests);
        assert!(!loader.busy());
    }

    /// Audit 1863: a server without a catalogue (404) is asked again on
    /// the long ladder, not given up for the session: the catalogue at
    /// the default source comes after the first regions. One ask a rung,
    /// however many regions wait on it.
    #[test]
    fn no_catalogue_is_asked_again_on_the_long_ladder_once_a_rung() {
        let w = want(112_615_000);
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30)
            .with_heights(serve.url(CATALOGUE_NAME));
        loader.ask(w.clone(), 0);
        let steps = settle_heights(&mut loader, 0);
        assert!(
            matches!(steps.last(), Some(Step::Heights { landed: Err(_), .. })),
            "{steps:?}"
        );
        assert!(
            !loader.busy(),
            "a catalogue waiting to be asked is not loading"
        );
        assert_eq!(
            loader.next_due_ms(0),
            None,
            "a missing catalogue wakes nothing"
        );
        // A second region lands meanwhile: its packs wait on the same rung.
        loader.ask(want(112_825_000), 0);
        let steps = settle(&mut loader, 0);
        assert!(matches!(steps.as_slice(), [Step::Done { .. }]), "{steps:?}");
        let requests = serve.requests();
        for _ in 0..20 {
            assert!(loader.poll(BACKOFF_FIRST_MS - 1).is_empty());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(serve.requests(), requests, "nothing asked before the rung");
        let steps = settle_heights(&mut loader, BACKOFF_FIRST_MS);
        assert!(
            matches!(steps.last(), Some(Step::Heights { landed: Err(_), .. })),
            "{steps:?}"
        );
        assert_eq!(serve.requests(), requests + 1, "one ask on the rung");
        // The next rung is twice as far: nothing asked before it.
        for _ in 0..20 {
            assert!(loader.poll(3 * BACKOFF_FIRST_MS - 1).is_empty());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            serve.requests(),
            requests + 1,
            "nothing before the next rung"
        );
        let steps = settle_heights(&mut loader, 3 * BACKOFF_FIRST_MS);
        assert!(
            matches!(steps.last(), Some(Step::Heights { landed: Err(_), .. })),
            "{steps:?}"
        );
        assert_eq!(serve.requests(), requests + 2, "one ask on the second rung");
        assert_eq!(
            names(dir.path())
                .iter()
                .filter(|n| n.ends_with(".lmh"))
                .count(),
            0
        );
    }

    #[test]
    fn a_failed_height_fetch_retries_on_the_long_ladder() {
        let w = want(112_615_000);
        let (serve, pack) = heights_server(&w);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1 << 30)
            .with_heights(serve.url(CATALOGUE_NAME));
        loader.ask(w.clone(), 0);
        let steps = settle(&mut loader, 0);
        assert!(matches!(steps.as_slice(), [Step::Done { .. }]), "{steps:?}");
        // The source breaks before the packs start.
        serve.fail(true);
        let steps = settle_heights(&mut loader, 1_000);
        assert!(
            matches!(steps.last(), Some(Step::Heights { landed: Err(_), .. })),
            "{steps:?}"
        );
        assert!(!loader.busy(), "a pack waiting to retry is not loading");
        assert_eq!(loader.next_due_ms(1_000), Some(BACKOFF_FIRST_MS));
        assert!(loader.poll(1_000 + BACKOFF_FIRST_MS - 1).is_empty());
        serve.fail(false);
        let steps = settle_heights(&mut loader, 1_000 + BACKOFF_FIRST_MS);
        let Some(Step::Heights {
            landed: Ok(landed), ..
        }) = steps.last()
        else {
            panic!("{steps:?}");
        };
        assert_eq!(landed.packs, [pack]);
    }

    #[test]
    fn the_packs_wait_while_a_region_for_the_screen_loads() {
        let w = want(112_615_000);
        let (serve, _) = heights_server(&w);
        let (_listener, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, source, 1 << 30).with_heights(serve.url(CATALOGUE_NAME));
        // A landed region's packs are wanted; a box asked by hand runs
        // against a source that never answers.
        loader.want_heights(&w, &auto_name(&w.bounds, (14, 15)), Rank::Here);
        loader.ask(nth(1), 0);
        for _ in 0..20 {
            loader.poll(0);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(loader.loading_here());
        let h = loader.heights.as_ref().expect("heights");
        assert!(
            h.running.is_none(),
            "a pack started beside the screen's region"
        );
        assert_eq!(h.queue.len(), 1);
        assert_eq!(serve.requests(), 0);
        assert!(loader.busy());
    }

    #[test]
    fn a_pack_goes_with_the_last_region_over_it_and_counts_in_the_cap() {
        let dir = tempfile::tempdir().expect("tmp");
        let w = want(112_615_000);
        let ((x, y), pack) = pack_under(&w);
        let block = leviculum_mapcut::heights::block_bounds(11, x, y);
        let pack_name = format!("{AUTO_PREFIX}h11-{x}-{y}.lmh");
        // Two regions over the block, one far away, and a pack elsewhere
        // that no region covers any more.
        let inside = Bounds {
            west: block.west + 10_000,
            south: block.south + 10_000,
            east: block.west + 20_000,
            north: block.south + 20_000,
        };
        let a = auto_name(&inside, (14, 15));
        let b = auto_name(&w.bounds, (14, 15));
        let far = auto_name(
            &Bounds {
                west: 130_000_000,
                south: 520_000_000,
                east: 131_000_000,
                north: 521_000_000,
            },
            (14, 15),
        );
        let stray = format!("{AUTO_PREFIX}h11-0-0.lmh");
        let hand = "dunes.lmh";
        let write = |name: &str, len: usize, age: u64| {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![0u8; len]).expect("write");
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + age);
            File::options()
                .write(true)
                .open(&path)
                .expect("open")
                .set_modified(at)
                .expect("time");
        };
        write(&a, 100, 1);
        write(&b, 100, 2);
        write(&far, 100, 3);
        std::fs::write(dir.path().join(&pack_name), &pack).expect("pack");
        write(&stray, 50, 0);
        write(hand, 70, 0);
        // Under a large cap only the stray pack goes: no region over it.
        let gone = evict(dir.path(), 1 << 30, &far).expect("evict");
        assert_eq!(gone, std::slice::from_ref(&stray));
        // The pack counts: 300 bytes of regions and the pack, one byte
        // over the cap, take the oldest region; the pack stays under the
        // other.
        let cap = 299 + pack.len() as u64;
        let gone = evict(dir.path(), cap, &far).expect("evict");
        assert_eq!(gone, std::slice::from_ref(&a));
        assert!(dir.path().join(&pack_name).exists());
        // The last region over the block goes, the pack with it; the hand
        // placed pack stays, whatever the cap.
        let gone = evict(dir.path(), 0, &far).expect("evict");
        assert_eq!(gone, [b, pack_name]);
        assert_eq!(names(dir.path()), [far, hand.to_string()]);
    }

    /// Audit 1863: two writers evict from one listing (a region's job and
    /// a height fetch). The one that finds a file gone counts it as gone:
    /// the region that landed stays landed, not failed and queued again.
    #[test]
    fn a_region_lands_when_another_writer_removed_what_its_eviction_listed() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let old = want(112_825_000);
        let stale = installed(&serve, dir.path(), &old, leviculum_map::profile::CUT_1);
        let stale_name = stale
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let stray = format!("{AUTO_PREFIX}h11-0-0.lmh");
        std::fs::write(dir.path().join(&stray), [0u8; 50]).expect("stray pack");
        RACED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(dir.path().to_path_buf());
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        // A cap of one byte: the old region and the stray pack go.
        let mut loader = Loader::new(maps, Source::Url(serve.url("o.pmtiles")), 1);
        let w = want(112_615_000);
        loader.ask(w.clone(), 0);
        let steps = settle(&mut loader, 0);
        let [Step::Done { landed, .. }] = steps.as_slice() else {
            panic!("the region must land: {steps:?}");
        };
        let mut evicted = landed.evicted.clone();
        evicted.sort();
        let mut both = vec![stale_name, stray];
        both.sort();
        assert_eq!(evicted, both, "counted as gone");
        assert_eq!(names(dir.path()), [auto_name(&w.bounds, (14, 15))]);
        assert!(!loader.busy(), "nothing queued again");
    }

    /// Audit 1863, without the seam: eight evictions start together over
    /// one directory of 400 orphan packs, each from its own listing, as
    /// the region jobs and the height fetch run. Every one returns Ok and
    /// the directory ends empty; one that took a NotFound for an error
    /// fails here.
    #[test]
    fn evictions_from_one_listing_at_once_all_succeed() {
        let dir = tempfile::tempdir().expect("tmp");
        for n in 0..400u32 {
            let name = format!("{AUTO_PREFIX}h11-{}-{}.lmh", n % 2048, n / 2048);
            std::fs::write(dir.path().join(name), [0u8; 8]).expect("pack");
        }
        let start = Arc::new(std::sync::Barrier::new(8));
        let results: Vec<std::io::Result<Vec<String>>> = (0..8)
            .map(|_| {
                let start = Arc::clone(&start);
                let dir = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    start.wait();
                    evict(&dir, 1 << 30, "")
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().expect("the thread"))
            .collect();
        for r in &results {
            assert!(r.is_ok(), "{r:?}");
        }
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn the_names_an_eviction_reads_back() {
        let b = Bounds {
            west: -39_750_000,
            south: 311_200_000,
            east: -39_700_000,
            north: 311_250_000,
        };
        assert_eq!(auto_bounds(&auto_name(&b, (10, 15))), Some(b));
        assert_eq!(auto_bounds("auto-1_2_3-z1-2.lmr"), None);
        assert_eq!(auto_bounds("dunes.lmr"), None);
        assert_eq!(
            pack_bounds("auto-h11-1001-837.lmh"),
            Some(leviculum_mapcut::heights::block_bounds(11, 1001, 837))
        );
        for bad in [
            "auto-h11-2048-1.lmh",
            "auto-h11-1-2-3.lmh",
            "auto-hx-1-2.lmh",
            "h11-1-2.lmh",
        ] {
            assert_eq!(pack_bounds(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_height_catalogue_lies_beside_the_map_source() {
        assert_eq!(heights_url(None, None), HEIGHTS_DEFAULT);
        assert_eq!(
            heights_url(None, Some("https://maps.example.org/planet.pmtiles")),
            "https://maps.example.org/heights.tsv"
        );
        assert_eq!(
            heights_url(
                Some("https://h.example.org/x.tsv"),
                Some("https://m/p.pmtiles")
            ),
            "https://h.example.org/x.tsv"
        );
    }

    #[test]
    fn the_maps_usage_and_delete_all_take_the_downloaded_packs() {
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        std::fs::write(dir.path().join("auto-h11-1-1.lmh"), [0u8; 40]).expect("pack");
        std::fs::write(dir.path().join("dunes.lmh"), [0u8; 7]).expect("hand pack");
        std::fs::write(dir.path().join("a.lmr"), [0u8; 10]).expect("region");
        assert_eq!(maps.usage(), (1, 50));
        assert_eq!(maps.delete_all(), Ok(1));
        assert_eq!(names(dir.path()), ["dunes.lmh"]);
    }
}
