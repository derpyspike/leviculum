//! The load walk (1811): a test that loads the map as a user does, on the
//! window's real path — the same [`App`] the window runs, its scheduler
//! ([`loquax::ui::viewload`]), the shell's loader ([`loquax::mapload`])
//! and the frame painter ([`super::draw`]) — against a local range server
//! ([`leviculum_mapcut::serve::Serve`]) with a configurable rate, latency
//! and injectable faults, so it is deterministic and offline.
//!
//! The walk runs scripted sessions and records, per frame interval: the
//! view, the loader's queue (every box with rank and age), the running
//! loads, the bytes the source sent, and at every scripted pause a frame
//! rendered through the window's own [`super::draw`]. The rules, each
//! named, each with one threshold, in the render walk's style (1801):
//!
//! - `view_filled_at_rest`: once the user rests at a zoom the autoload
//!   serves ([`loquax::ui::viewload::MIN_ZOOM`] up) and the source is
//!   reachable, no part of the view lacks its zoom's data
//!   ([`loquax::ui::viewload::uncovered`] is 0 and the frame owes no
//!   tile) within the rest's derived bound (1821): [`FILL_FAST_MS`] at
//!   full speed, where the link is not the cost; at a limited rate
//!   [`FILL_BASE_MS`] plus twice the time the bytes the view itself
//!   needs ([`view_bytes`]) take at that rate.
//! - `screen_first`: while a share of the view above
//!   [`loquax::ui::viewload::GAP_SHARE`] lacks its data, no load starts
//!   for anything else; a load already running when the lack began is the
//!   allowed exception, and how long the screen waited behind it is
//!   measured and reported.
//! - `nothing_lost`: every box queued is later finished or dropped by a
//!   named rule (the away cap of [`loquax::mapload::MAX_AWAY`], a screen
//!   box the view left); never silently gone, and never stuck: while
//!   boxes wait, one must start, end or leave within [`STALL_MS`].
//! - `no_regress`: at the same view (centre and zoom), a later rest never
//!   shows a larger uncovered share than an earlier one.
//! - `same_view_same_picture`: the same view at a filled rest gives the
//!   same picture (the screen's pixels, margins cut), whatever the path.
//! - `no_spin`: no box starts while the same box runs, none starts more
//!   than [`SPIN_STARTS`] times without failing between, and no box
//!   starts again after it landed while its file is still there.
//! - `fault_recovers`: after an injected fault ends, the view fills with
//!   no user input within [`RECOVER_MS`]; a backoff must not block what
//!   is on screen once the source answers again.
//! - `frame_never_blank`: no drawn frame of a session — every frame of
//!   [`Walk::step`], not only the pause records (1821) — is blank (one
//!   colour over more than [`BLANK_SHARE`] of the screen) where the
//!   frame before it, at the same view, had a picture; a view that
//!   moved onto ground not loaded yet is legitimately empty and is not
//!   judged.
//! - `no_busy_wake`: at no frame does the loader, polled and idle, name
//!   a wake of 0 ms (1835, the audit of 1821): a wake it names is an
//!   instant at which a load can start, so a frame-driven frontend (the
//!   phone, `MapShown::retry_in_ms`) woken at it never redraws for
//!   nothing until the next real retry.
//! - `pack_after_region` and `relief_drawn` (1868, the audit of 1863),
//!   the desert sessions alone: over a desert archive with a height
//!   catalogue beside it (the committed GLO-30 blocks of Erg Chebbi and
//!   the Bani escarpment), the loader runs with
//!   [`loquax::mapload::Loader::with_heights`] as the shell's does; the
//!   block under the place lands as a pack after a region over it, and
//!   the rested view with the relief on differs from the same view drawn
//!   with it off on at least [`RELIEF_SHARE`] of the screen.
//!
//! - `loading_is_visible` (1812): at rest (no input for [`SETTLE_MS`]),
//!   every point of the view the sampling of
//!   [`loquax::ui::viewload::uncovered`] finds without its zoom's data
//!   lies in a piece of the frame the loads' treatment
//!   ([`loquax::ui::maploading::paint`]) was drawn over.
//! - `progress_moves_with_bytes` (1812): a running area's bar is the
//!   loader's bytes done of bytes planned at that instant, never another
//!   number; it shows loading while bytes moved within the treatment's
//!   stall time ([`TREAT_STALL_MS`]) and says it stands still once none
//!   came for longer.
//! - `no_treatment_when_complete` (1812): a pause that filled shows no
//!   treatment on the screen.
//! - `idle_draws_nothing` (1812): no frame is drawn only because the
//!   stripes' step moved unless the frame on screen when it was asked
//!   showed a moving treatment.
//!
//! With `LOQUAX_LOAD_WALK=DIR` the pictures go to `DIR/full/`, the
//! verdicts to `DIR/rows-load.tsv` and the per-frame traces to
//! `DIR/trace-<session>.tsv`, for `just load-walk`'s contact sheet.
//! `LOQUAX_LOAD_WALK_SET=short|quick|full` picks the sessions (short is
//! the per-batch default that runs in `cargo test`). The selftest
//! (`the_load_walk_selftest_fires_every_rule`) injects each fault class
//! into the walk itself and each rule must fire; `cargo test` runs the
//! cheapest injection, `LOQUAX_LOAD_WALK_SELFTEST=1` all of them.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::Point;
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::region::Bounds;
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapcut::serve::Serve;
use loquax::mapload::{
    Held, LoadState, Loader, Rank, Source, Step, Watch, MAX_AWAY, SCREEN_RETRY_MAX_MS,
};
use loquax::picture::Maps;
use loquax::ui::map::MapWant;
use loquax::ui::maploading::{milli_of, Kind, Loads, STALL_MS as TREAT_STALL_MS};
use loquax::ui::viewload::{gaps, uncovered, GAP_SHARE, MIN_ZOOM};

use super::first_tile;
use super::{draw, prefetch, App, Frames, Job, Message, Order, Pointer, Screen, Want};

/// The window's map pane the walk drives, in pixels.
pub const WALK_SIZE: (u32, u32) = (1120, 700);
/// The walk's archive: two neighbouring areas down to street level, the
/// Grunewald fixture's z15 tile in every slot from z10 to z15 over a box
/// twice [`first_tile::FIRST_TILE_BOX`]'s width (about 28 by 16 km), so a
/// pan crosses several cells and both areas lie inside.
pub const WIDE_BOX: Bounds = Bounds {
    west: 131_600_000,
    south: 524_000_000,
    east: 135_600_000,
    north: 525_400_000,
};
/// The western area's middle: inside the box, so a z13 view and its
/// screen box stay over archive data (past the box the world is empty,
/// which is no loader's fault).
pub const AREA_A: (f64, f64) = (52.4715, 13.3000);
/// The eastern area's middle, a neighbouring area of the same archive.
pub const AREA_B: (f64, f64) = (52.4715, 13.4600);
/// The slow source: the order's 200 kB/s, 40 ms to the first byte.
pub const SLOW_RATE: u64 = 200_000;
/// A rest must fill within this at full speed.
pub const FILL_FAST_MS: u64 = 15_000;
/// The link-free part of a limited rate's fill bound (1821): the cut,
/// the write, the verify and the draw.
pub const FILL_BASE_MS: u64 = 10_000;
/// After a fault ends the view must fill, untouched, within this.
pub const RECOVER_MS: u64 = 25_000;
/// While boxes wait, one must start, end or leave within this.
pub const STALL_MS: u64 = 20_000;
/// A box started more often than this without a failure between spins.
pub const SPIN_STARTS: usize = 2;
/// A frame whose most common colour covers more of the screen than this
/// is blank.
pub const BLANK_SHARE: f64 = 0.99;
/// The share of a rested desert view the relief must move: the block lies
/// under most of a z13 view at the place, and a relief that shades less
/// than a fifth of it was not drawn.
pub const RELIEF_SHARE: f64 = 0.2;
/// The view rests this long after the last input before
/// `loading_is_visible` judges its frames (1812): the scheduler's next
/// decision ([`loquax::ui::viewload::DECIDE_MS`]) and two frames.
pub const SETTLE_MS: u64 = 300;
/// While a finger drags, during a wheel step's glide and in the settle
/// right after a drag, no step of the walk takes longer than this (order
/// 1943, its audit): input never waits on drawing. A moving curved frame
/// rides the memo in a few milliseconds; the whole-frame redraw it
/// replaced took about a second over a store of overlapping regions,
/// 11 to 20 s of frozen window on Lew's desktop. The settle and the
/// wheel's rest draw the real frame whole, inside the step as the
/// window's worker does; the bound is the reviewer's 50 ms with the
/// margin a shared, loaded test host needs (measured in the report of
/// order 1967).
pub const MOVING_GAP_MS: u64 = 250;
/// The steps after a drag's last move that `input_never_waits` judges
/// as its settle: the frame at rest is asked and drawn within them.
pub const SETTLE_STEPS: u32 = 20;

/// Where a walk starts and what its maps directory holds.
struct Ground {
    /// The view's middle and zoom.
    at: World,
    zoom: u8,
    /// The autoload cache's cap.
    cache_bytes: u64,
    /// Old regions installed first ([`prefill_cache`]).
    prefill: usize,
    /// The pane, [`WALK_SIZE`] but for the patchwork session (order
    /// 1943), whose desktop pane puts zoom 10 in the blend window.
    size: (u32, u32),
}

/// A desert place of the height sessions (1868): its name and the
/// committed GLO-30 pack whose one block it lies in the middle of.
pub struct Desert {
    pub name: &'static str,
    pub pack: &'static [u8],
}

/// Erg Chebbi, the z11 block of `erg-chebbi.lmh`.
pub const ERG_CHEBBI: Desert = Desert {
    name: "erg-chebbi",
    pack: include_bytes!("../../../leviculum-map/fixtures/erg-chebbi.lmh"),
};
/// The Bani escarpment, the z11 block of `bani-z11.lmh`.
pub const BANI: Desert = Desert {
    name: "bani",
    pack: include_bytes!("../../../leviculum-map/fixtures/bani-z11.lmh"),
};

impl Desert {
    /// The block's ground: the place is its middle.
    pub fn block(&self) -> Bounds {
        let (dir, _) =
            leviculum_map::elevation::Directory::parse_prefix(self.pack).expect("the pack");
        let (x, y) = dir.block(0).expect("one block");
        leviculum_mapcut::heights::block_bounds(dir.block_zoom(), x, y)
    }

    /// The place, the block's middle.
    pub fn at(&self) -> World {
        let b = self.block();
        project(LatLon::from_degrees(
            (f64::from(b.south) + f64::from(b.north)) / 2e7,
            (f64::from(b.west) + f64::from(b.east)) / 2e7,
        ))
    }

    /// The archive's box: the place and 0.2 degrees east and west, 0.12
    /// north and south, so a z13 view and its screen box stay inside.
    pub fn area(&self) -> Bounds {
        let at = self.at();
        let (lon, lat) = (
            leviculum_map::mercator::lon_of_x(at.x),
            leviculum_map::mercator::lat_of_y(at.y),
        );
        Bounds {
            west: lon - 2_000_000,
            south: lat - 1_200_000,
            east: lon + 2_000_000,
            north: lat + 1_200_000,
        }
    }
}

/// One frame interval of the trace.
pub struct FrameRec {
    pub t: u64,
    pub centre: World,
    pub zoom_milli: u32,
    pub uncovered: f64,
    /// The loader's boxes: bounds, rank, running.
    pub order: Vec<(Bounds, Rank, bool)>,
    /// Body bytes the source sent so far, all answers together.
    pub sent: u64,
    /// Tiles the last drawn frame owed.
    pub missing: usize,
    pub drew: bool,
    /// Whether the frame drawn in this interval was blank
    /// ([`BLANK_SHARE`]); `None` when none was drawn (1821).
    pub blank: Option<bool>,
    /// The view lay inside the archive's box: past it the world is
    /// empty by design, and no picture rule judges the frame.
    pub inside: bool,
    /// The loader, polled and idle, still named a wake of 0 ms (1835):
    /// a frame-driven frontend woken at it would start nothing and
    /// redraw for nothing until the next real retry.
    pub busy_wake: bool,
    /// What of the view is still to come as the window is told it after
    /// this interval (1812).
    pub loads: Loads,
    /// The loader's running boxes for the view at the same instant: their
    /// bytes done and planned.
    pub raw: Vec<(Bounds, u64, u64)>,
    /// The drawn frame's points without data and how many of them carry
    /// no treatment, when judged ([`SETTLE_MS`], the view at rest).
    pub treat: Option<(usize, usize)>,
    /// The drawn frame differs from the one before only in the stripes'
    /// step, and the frame on screen when it was asked showed nothing moving.
    pub idle_phase_draw: bool,
    /// Pixels of the drawn frame the loads' treatment changed.
    pub treated_px: u64,
}

/// A loader event on the walk's clock.
pub enum Ev {
    /// Started: its box and the zooms it asks for (1933: a box's levels
    /// are judged against the view's).
    Started(Bounds, (u8, u8)),
    /// Landed: its box, its file name, the names evicted for it.
    Done(Bounds, String, Vec<String>),
    Failed(Bounds, String),
    Skipped(Bounds, &'static str),
    /// A running load paused, went on, or was dropped (1836).
    Held(Bounds, Held),
}

/// A scripted pause's record: the view, its coverage, its picture.
pub struct Check {
    pub label: String,
    pub t: u64,
    pub centre: World,
    pub zoom_milli: u32,
    pub uncovered: f64,
    /// The rest filled within its bound.
    pub filled: bool,
    /// This pause expected a filled view (rest with a reachable source).
    pub expects_fill: bool,
    /// The view lies inside the archive's box, with a margin: outside it
    /// the world is empty and no picture rule judges the pause.
    pub inside: bool,
    pub fill_bound_ms: u64,
    pub fill_took_ms: u64,
    /// The screen's pixels, margins cut, RGBA.
    pub crop: Vec<u8>,
    pub blank: bool,
    /// Pixels of the screen (margins cut) the pause's frame carries a
    /// treatment on (1812).
    pub treated_px: u64,
}

/// What a fault session did: when the fault ended on the walk's clock,
/// and when the view filled after it.
#[derive(Default)]
pub struct Fault {
    pub cleared_at: Option<u64>,
    pub filled_at: Option<u64>,
    /// A box for the screen refused as "backoff" after the fault ended.
    pub backoff_after_clear: bool,
}

/// A fault injected into the walk itself (`--selftest`): each must make
/// its rule fire.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Inject {
    None,
    /// Every decision's first `here` box is dropped before the loader.
    DropWant,
    /// Every decision's `here` boxes are handed over as `ahead`.
    AheadFirst,
    /// The source stops mid-body and never resumes.
    StuckLoad,
    /// A box that landed is asked again by hand every half second.
    DoubleAsk,
    /// A landed region file is deleted after the first filled rest, and
    /// every want dropped from then on.
    DeleteRegion,
    /// The fault never ends inside the session's recover bound.
    NeverRecovers,
    /// One drawn frame between two pauses, at a steady view whose last
    /// frame had a picture, is counted blank (1821): the per-frame
    /// rule's positive control.
    BlankFrame,
    /// The picture of a repeated view is perturbed before it is checked:
    /// the comparator's positive control.
    PerturbPicture,
    /// One idle frame is recorded as woken by a named wake at which
    /// nothing starts (1835): `no_busy_wake`'s positive control, at the
    /// signal the rule reads.
    BusyWake,
    /// A desert session's loader fetches no height packs (1868):
    /// `pack_after_region` and `relief_drawn`'s positive control.
    NoHeights,
    /// The window is never told the loads (1812): `loading_is_visible`.
    HideTreatment,
    /// A loading area's bar shows the clock, not the bytes (1812):
    /// `progress_moves_with_bytes`.
    FakeProgress,
    /// The last loads are kept once the loader has none (1812):
    /// `no_treatment_when_complete`.
    StaleTreatment,
    /// After the first filled pause a frame is asked for the stripes'
    /// next step while nothing loads (1812): `idle_draws_nothing`.
    IdleTicks,
    /// Every drag step sleeps 300 ms, a frame that waits on drawing
    /// (order 1943): `input_never_waits`'s positive control.
    SlowFrame,
    /// The curved memo is off, so every moving curved frame draws whole
    /// (order 1943; until its audit the maps' epoch churning under the
    /// drag did that, which the memo now rides out):
    /// `input_rides_the_memo`'s positive control.
    NoMemo,
}

/// One verdict row, the render walk's columns.
pub struct Verdict {
    pub session: String,
    pub rule: &'static str,
    pub pass: bool,
    pub value: String,
    pub limit: String,
    pub note: String,
    /// The picture it is about, if one.
    pub image: String,
    pub zoom: String,
}

pub struct Walk {
    pub serve: Serve,
    _dir: tempfile::TempDir,
    pub app: App,
    rx: tokio::sync::mpsc::UnboundedReceiver<Want>,
    pub loader: Loader,
    pub maps: Maps,
    start: Instant,
    pub name: String,
    pub rate: u64,
    pub frames: Vec<FrameRec>,
    pub events: Vec<(u64, Ev)>,
    pub checks: Vec<Check>,
    pub fault: Fault,
    inject: Inject,
    last_wheel: Option<Instant>,
    /// [`Inject::DeleteRegion`] has deleted the landed regions.
    deleted: bool,
    /// [`Inject::BlankFrame`] has blanked its one drawn frame.
    blanked: bool,
    /// [`Inject::BusyWake`] has marked its one idle frame.
    busy_woken: bool,
    last_ask: Option<Instant>,
    done_wants: Vec<MapWant>,
    /// The last drawn frame said a name still fades.
    last_fading: bool,
    /// The archive's box ([`inside_archive`]).
    pub area: Bounds,
    /// A desert session (1868): its loader fetches height packs, and the
    /// block of its place, whose pack must land.
    pub desert: Option<(String, Bounds)>,
    /// Every height fetch that ended: when, the region's box, the packs
    /// it wrote or why it failed.
    pub heights: Vec<(u64, Bounds, Result<Vec<String>, String>)>,
    /// The share of the rested view the relief moved ([`Walk::relief`]).
    pub relief: Option<f64>,
    /// The loader's report as the map shows it (1812), as the shell's.
    watch: Watch,
    /// The walk's clock at the last input (1812, [`SETTLE_MS`]).
    last_input_ms: u64,
    /// [`Inject::StaleTreatment`]'s kept loads.
    stale: Option<Loads>,
    /// A finger drags right now (order 1943): the steps inside a
    /// [`Walk::drag`] are the moving frames the input rules judge.
    dragging: bool,
    /// Steps taken while a finger dragged.
    pub drag_steps: u64,
    /// The longest such step, milliseconds: drawing included, since the
    /// walk draws inside its step as the window's worker does.
    pub drag_gap_max: u64,
    /// A wheel step's glide runs right now (audit of 1943).
    wheeling: bool,
    /// Steps taken in a wheel step's glide, and the longest of them.
    pub wheel_steps: u64,
    pub wheel_gap_max: u64,
    /// Steps of the settle after a drag still to judge ([`SETTLE_STEPS`]).
    settle_left: u32,
    /// Steps judged as a drag's settle, and the longest of them.
    pub settle_steps: u64,
    pub settle_gap_max: u64,
    /// Regions land and tiles arrive under every moving step (audit of
    /// 1943): `Maps::changed` and `Maps::arrived` each drag and wheel
    /// step, as an autoload landing under the finger does.
    pub churn: bool,
    /// Pictures of moving frames ([`Walk::shoot`]): a label and the
    /// screen's pixels.
    pub shots: Vec<(String, Vec<u8>, f64)>,
    /// The last step's length, milliseconds, drawing included.
    pub last_step_ms: u64,
    /// Every step's length, microseconds, drawing included: the render
    /// walk's window variant takes its frame times from it.
    pub step_us: Vec<u64>,
    /// Keep every n-th drag step's frame as a picture named after the
    /// prefix ([`Walk::shoot`]).
    pub shoot_every: Option<(u32, String)>,
    /// The settle draws of [`settle_ladder`]: zoom and milliseconds.
    pub settles: Vec<(u8, u64)>,
    /// Whole curved frames drawn while a finger dragged: a moving frame
    /// rides the memo instead (order 1943).
    pub drag_curved_drawn: u64,
}

impl Walk {
    /// A walk over a fresh maps directory, the source at `rate` bytes a
    /// second (0 for full speed) with `latency` to each first byte, the
    /// view over [`AREA_A`] at `zoom`.
    pub fn new(name: &str, rate: u64, latency_ms: u64, zoom: u8, inject: Inject) -> Self {
        Self::with_cache(name, rate, latency_ms, zoom, inject, 1 << 30, 0)
    }

    /// As [`Walk::new`], with the autoload cache capped at `cache_bytes`
    /// and `prefill` copies of a far region installed first, so eviction
    /// runs ([`loquax::mapload::evict`]).
    pub fn with_cache(
        name: &str,
        rate: u64,
        latency_ms: u64,
        zoom: u8,
        inject: Inject,
        cache_bytes: u64,
        prefill: usize,
    ) -> Self {
        let serve = Serve::start(vec![("w.pmtiles".into(), wide_archive())]).expect("serve");
        let at = project(LatLon::from_degrees(AREA_A.0, AREA_A.1));
        let ground = Ground {
            at,
            zoom,
            cache_bytes,
            prefill,
            size: WALK_SIZE,
        };
        Self::over(serve, name, rate, latency_ms, ground, inject)
    }

    /// A desert session's walk (1868): an archive over `place`'s box, its
    /// height catalogue and the two committed packs beside it, the loader
    /// fetching height packs as the shell's does, the view at the place.
    pub fn desert(place: &Desert, rate: u64, latency_ms: u64, zoom: u8, inject: Inject) -> Self {
        let area = place.area();
        let mut catalogue = leviculum_mapcut::heights::Catalogue::default();
        catalogue
            .add_pack("chebbi.lmh", ERG_CHEBBI.pack)
            .expect("chebbi");
        catalogue.add_pack("bani.lmh", BANI.pack).expect("bani");
        let serve = Serve::start(vec![
            ("w.pmtiles".into(), archive_over(&area)),
            ("chebbi.lmh".into(), ERG_CHEBBI.pack.to_vec()),
            ("bani.lmh".into(), BANI.pack.to_vec()),
            ("heights.tsv".into(), catalogue.write().into_bytes()),
        ])
        .expect("serve");
        let ground = Ground {
            at: place.at(),
            zoom,
            cache_bytes: 1 << 30,
            prefill: 0,
            size: WALK_SIZE,
        };
        let tag = if rate == 0 { "" } else { "-slow" };
        let name = format!("desert-{}{tag}", place.name);
        let mut walk = Self::over(serve, &name, rate, latency_ms, ground, inject);
        walk.area = area;
        let (dir, _) =
            leviculum_map::elevation::Directory::parse_prefix(place.pack).expect("the pack");
        let (x, y) = dir.block(0).expect("one block");
        let pack = format!(
            "{}h{}-{x}-{y}.lmh",
            loquax::picture::AUTO_PREFIX,
            dir.block_zoom()
        );
        walk.desert = Some((pack, place.block()));
        let loader = std::mem::replace(
            &mut walk.loader,
            Loader::new(Maps::default(), Source::Url(String::new()), 0),
        );
        walk.loader = match inject {
            Inject::NoHeights => loader,
            _ => loader.with_heights(walk.serve.url("heights.tsv")),
        };
        walk
    }

    /// The walk over `serve`'s `w.pmtiles` and `ground`.
    fn over(
        serve: Serve,
        name: &str,
        rate: u64,
        latency_ms: u64,
        ground: Ground,
        inject: Inject,
    ) -> Self {
        let Ground {
            at,
            zoom,
            cache_bytes,
            prefill,
            size,
        } = ground;
        let dir = tempfile::tempdir().expect("home");
        let path = dir.path().join("maps");
        std::fs::create_dir_all(&path).expect("maps");
        let url = serve.url("w.pmtiles");
        if prefill > 0 {
            prefill_cache(&path, &url, prefill);
        }
        if rate > 0 || latency_ms > 0 {
            serve.throttle(rate, Duration::from_millis(latency_ms));
        }
        let maps = Maps::new(Some(path));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(maps.clone()), Some(tx));
        app.screen = Screen::Map;
        let (w, h) = size;
        app.apply(Message::MapSize(iced::Size::new(w as f32, h as f32)));
        let mut seen = app.seen.clone();
        seen.autoload = true;
        seen.maps = maps.epoch();
        app.apply(Message::Seen(Box::new(seen)));
        app.view = ExactView::new(at, u32::from(zoom) * MILLI, w, h);
        app.hold();
        // The walk's view is the user's: a region landing fits nothing.
        app.fitted = true;
        let loader = Loader::new(maps.clone(), Source::Url(url), cache_bytes);
        Self {
            serve,
            _dir: dir,
            app,
            rx,
            loader,
            maps,
            start: Instant::now(),
            name: name.to_string(),
            rate,
            frames: Vec::new(),
            events: Vec::new(),
            checks: Vec::new(),
            fault: Fault::default(),
            inject,
            last_wheel: None,
            deleted: false,
            blanked: false,
            busy_woken: false,
            last_ask: None,
            done_wants: Vec::new(),
            last_fading: false,
            area: WIDE_BOX,
            desert: None,
            heights: Vec::new(),
            relief: None,
            watch: Watch::default(),
            last_input_ms: 0,
            stale: None,
            dragging: false,
            drag_steps: 0,
            drag_gap_max: 0,
            drag_curved_drawn: 0,
            wheeling: false,
            wheel_steps: 0,
            wheel_gap_max: 0,
            settle_left: 0,
            settle_steps: 0,
            settle_gap_max: 0,
            churn: false,
            shots: Vec::new(),
            last_step_ms: 0,
            settles: Vec::new(),
            step_us: Vec::new(),
            shoot_every: None,
        }
    }

    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn view_now(&self) -> leviculum_map::viewport::Viewport {
        self.app.view.at_zoom(self.app.view.tile_zoom())
    }

    pub fn uncovered_now(&self) -> f64 {
        uncovered(&self.view_now(), &self.maps.headers())
    }

    /// One frame interval, as the window runs one: the wheel's rest
    /// timer, the glide's tick, the worker's next job (a frame drawn or a
    /// prefetch batch), the shell's half (the wants to the loader, the
    /// loads polled, a landing seen), and the trace's row.
    pub fn step(&mut self) {
        let t = self.now_ms();
        let began = Instant::now();
        if self.dragging {
            // The temporal control (1943): a drag frame that waits.
            if self.inject == Inject::SlowFrame {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        if self.churn && (self.dragging || self.wheeling) {
            // A region lands and another gains tiles under the input.
            self.maps.changed();
            self.maps.arrived();
        }
        self.app.tick(Instant::now());
        if self
            .last_wheel
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(super::WHEEL_REST_MS))
        {
            self.last_wheel = None;
            self.app.apply(Message::WheelRest);
        }
        let mut drew = false;
        let mut blank = None;
        let mut missing = self.frames.last().map_or(0, |f| f.missing);
        let mut treat = None;
        let mut idle_phase_draw = false;
        let mut treated_px = 0;
        let job = match (self.inject, self.app.next_job()) {
            // The positive control (1812): once a pause filled, a frame is
            // asked for the stripes' next step though nothing loads, as a
            // window would that ticked its animation while idle.
            (Inject::IdleTicks, None) if self.checks.iter().any(|c| c.filled) => {
                let step = self.app.shown.as_ref().map_or(0, |s| s.order.phase) + 1;
                self.app
                    .order()
                    .map(|o| Job::Draw(Box::new(Order { phase: step, ..o })))
            }
            (_, job) => job,
        };
        match job {
            Some(Job::Draw(order)) => {
                let before = self.app.shown.as_ref().map(|s| s.order.clone());
                let moved_before = self.app.shown.as_ref().is_some_and(|s| {
                    s.painted
                        .pieces
                        .iter()
                        .any(|p| matches!(p.kind, Kind::Loading { .. }))
                });
                let shown = draw(&self.app.frames, *order);
                // 1812: a frame drawn only for the stripes' next step must
                // show stripes that move.
                let phase_only = before.is_some_and(|b| {
                    b.phase != shown.order.phase
                        && Order {
                            phase: shown.order.phase,
                            ..b
                        } == shown.order
                });
                // Asked while the frame on screen showed nothing moving:
                // the frame that finds the stripes gone (their area came
                // to be held) is the one the window learns it from.
                idle_phase_draw = phase_only && !moved_before;
                treated_px = shown.painted.pixels;
                drew = true;
                missing = shown.missing;
                self.last_fading = shown.fading;
                let mut is_blank = frame_blank(&shown);
                // The selftest's injection (1821): one drawn frame
                // between two pauses, at a view whose last drawn frame
                // had a picture, counts as blank; the rule must fire.
                if self.inject == Inject::BlankFrame && !self.blanked && !self.checks.is_empty() {
                    let steady = self.frames.iter().rev().find(|f| f.drew).is_some_and(|f| {
                        f.centre == self.app.view.centre
                            && f.zoom_milli == self.app.view.zoom_milli()
                            && f.blank == Some(false)
                    });
                    if steady {
                        is_blank = true;
                        self.blanked = true;
                    }
                }
                blank = Some(is_blank);
                self.app.apply(Message::Painted(shown));
            }
            Some(Job::Prefetch(now)) => {
                let more = prefetch(&self.app.frames, now);
                self.app.apply(Message::Prefetched(more));
            }
            None => {}
        }
        if self.inject == Inject::DoubleAsk {
            self.double_ask(t);
        }
        let mut steps = Vec::new();
        while let Ok(want) = self.rx.try_recv() {
            match want {
                Want::MapView(mut w) => {
                    match self.inject {
                        Inject::DropWant => {
                            w.here.clear();
                        }
                        Inject::AheadFirst => {
                            let mut here = std::mem::take(&mut w.here);
                            here.extend(std::mem::take(&mut w.ahead));
                            w.ahead = here;
                        }
                        Inject::DeleteRegion if self.deleted => {
                            w.here.clear();
                            w.ahead.clear();
                        }
                        _ => {}
                    }
                    steps.extend(self.loader.schedule(w.here, w.ahead, t));
                }
                Want::Map(w) => steps.extend(self.loader.ask(w, t)),
                _ => {}
            }
        }
        steps.extend(self.loader.poll(t));
        let mut landed = false;
        for step in &steps {
            let b = step.want().bounds;
            match step {
                Step::Started(w) => {
                    self.events.push((t, Ev::Started(b, w.zooms)));
                }
                Step::Done {
                    want, landed: l, ..
                } => {
                    landed = true;
                    self.done_wants.push(want.clone());
                    self.events
                        .push((t, Ev::Done(b, l.name.clone(), l.evicted.clone())));
                }
                Step::Failed { detail, .. } => {
                    self.events.push((t, Ev::Failed(b, detail.clone())));
                }
                Step::Skipped { why, .. } => self.events.push((t, Ev::Skipped(b, why))),
                Step::Held { how, .. } => self.events.push((t, Ev::Held(b, *how))),
                // A desert session's packs (1868): a pack written is a
                // landing every painter reads again.
                Step::Heights { landed: l, .. } => {
                    let packs = l.as_ref().map(|l| l.packs.clone()).map_err(Clone::clone);
                    landed |= packs.as_ref().is_ok_and(|p| !p.is_empty());
                    self.heights.push((t, b, packs));
                }
            }
        }
        if landed {
            let mut seen = self.app.seen.clone();
            seen.maps = self.maps.epoch();
            self.app.apply(Message::Seen(Box::new(seen)));
        }
        // What of the view is still to come (1812), as the shell tells
        // the window: one report, so the bar and the bytes are the same
        // instant's.
        let report = self.loader.report(t);
        let mut loads = self.watch.of(&report, self.loader.offline(), t);
        let raw: Vec<(Bounds, u64, u64)> = report
            .iter()
            .filter_map(|(b, rank, state)| match state {
                LoadState::Running {
                    bytes_done, bytes, ..
                } if rank.on_screen() => Some((*b, *bytes_done, *bytes)),
                _ => None,
            })
            .collect();
        match self.inject {
            Inject::HideTreatment => loads = Loads::default(),
            Inject::FakeProgress => {
                for a in &mut loads.areas {
                    if let Kind::Loading { milli } = &mut a.kind {
                        *milli = ((t / 7) % 1000) as u16;
                    }
                }
            }
            Inject::StaleTreatment => match loads.areas.is_empty() {
                true => loads = self.stale.clone().unwrap_or(loads),
                false => self.stale = Some(loads.clone()),
            },
            _ => {}
        }
        if loads != self.app.seen.loads {
            let mut seen = self.app.seen.clone();
            seen.loads = loads.clone();
            self.app.apply(Message::Seen(Box::new(seen)));
        }
        // 1812: at rest, every point without data carries the treatment
        // in the frame on screen, once that frame is the one for what the
        // window knows (its loads, its regions, its view): a change is
        // drawn in the next interval, and judged then.
        if let Some(shown) = self.app.shown.as_ref() {
            let current = shown.order.loads == self.app.seen.loads
                && shown.order.epoch == self.app.seen.maps
                && shown.order.view == self.app.view;
            let settled = t.saturating_sub(self.last_input_ms) >= SETTLE_MS && !self.app.moving();
            if current
                && settled
                && self.app.view.tile_zoom() >= MIN_ZOOM
                && inside_archive(&self.area, &self.view_now().bounds())
            {
                let lack = gaps(&self.view_now(), &self.maps.headers());
                let bare = lack
                    .iter()
                    .filter(|&&p| {
                        let (x, y) = shown.view.to_screen(p);
                        !shown.painted.pieces.iter().any(|q| q.contains(x, y))
                    })
                    .count();
                treat = Some((lack.len(), bare));
            }
        }
        // The wake the loader would name a frame-driven frontend now,
        // right after its own poll: 0 while idle means a wake at which
        // nothing starts (1835) — the phone would redraw every 50 ms.
        let mut busy_wake = !self.loader.busy() && self.loader.next_due_ms(t) == Some(0);
        if self.inject == Inject::BusyWake
            && !self.busy_woken
            && !self.loader.busy()
            && !self.checks.is_empty()
        {
            busy_wake = true;
            self.busy_woken = true;
        }
        self.frames.push(FrameRec {
            t,
            centre: self.app.view.centre,
            zoom_milli: self.app.view.zoom_milli(),
            uncovered: self.uncovered_now(),
            order: self.loader.order(),
            sent: self.serve.sent(),
            missing,
            drew,
            blank,
            inside: inside_archive(&self.area, &self.view_now().bounds()),
            busy_wake,
            loads,
            raw,
            treat,
            idle_phase_draw,
            treated_px,
        });
        // A drag, wheel or settle step, drawing included (order 1943 and
        // its audit): input never waits on drawing, so the longest such
        // step is a rule.
        let ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_step_ms = ms;
        self.step_us
            .push(u64::try_from(began.elapsed().as_micros()).unwrap_or(u64::MAX));
        if self.dragging {
            self.drag_steps += 1;
            self.drag_gap_max = self.drag_gap_max.max(ms);
        } else if self.wheeling {
            self.wheel_steps += 1;
            self.wheel_gap_max = self.wheel_gap_max.max(ms);
        } else if self.settle_left > 0 {
            self.settle_left -= 1;
            self.settle_steps += 1;
            self.settle_gap_max = self.settle_gap_max.max(ms);
        }
        let next = t + 16;
        let now = self.now_ms();
        if next > now {
            std::thread::sleep(Duration::from_millis(next - now));
        }
    }

    /// [`Inject::DoubleAsk`]: the first box that landed, asked again by
    /// hand every half second, its file still on disk.
    fn double_ask(&mut self, t: u64) {
        let due = self
            .last_ask
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(500));
        if !due {
            return;
        }
        if let Some(want) = self.done_wants.first().cloned() {
            self.last_ask = Some(Instant::now());
            let steps = self.loader.ask(want, t);
            for step in &steps {
                if let Step::Started(w) = step {
                    self.events.push((t, Ev::Started(w.bounds, w.zooms)));
                }
            }
        }
    }

    /// `frames` frame intervals.
    pub fn run(&mut self, frames: u32) {
        for _ in 0..frames {
            self.step();
        }
    }

    /// One wheel notch of `milli` thousandths of a step at the middle,
    /// then the glide's frames.
    pub fn wheel(&mut self, milli: i32, settle_frames: u32) {
        self.last_input_ms = self.now_ms();
        let (w, h) = self.app.view.size();
        self.app.apply(Message::Pointer(Pointer::Wheel(
            Point::new(w as f32 / 2.0, h as f32 / 2.0),
            milli,
        )));
        self.last_wheel = Some(Instant::now());
        self.wheeling = true;
        self.run(settle_frames);
        self.wheeling = false;
    }

    /// `frames` more steps of a wheel step's glide.
    pub fn wheel_steps_run(&mut self, frames: u32) {
        self.wheeling = true;
        self.run(frames);
        self.wheeling = false;
    }

    /// The screen's pixels of the frame shown now, kept as `label`
    /// ([`Walk::shots`]): a moving frame's picture.
    pub fn shoot(&mut self, label: &str) {
        if let Some(shown) = self.app.shown.as_ref() {
            let (crop, bare) = present(shown, &self.app);
            self.shots.push((label.to_string(), crop, bare));
        }
    }

    /// A drag by `dx, dy` a frame for `frames` frames, pointer held.
    pub fn drag(&mut self, dx: f32, dy: f32, frames: u32) {
        self.dragging = true;
        let curved = self.curved_drawn_now();
        let (w, h) = self.app.view.size();
        let (mut x, mut y) = (w as f32 / 2.0, h as f32 / 2.0);
        self.app
            .apply(Message::Pointer(Pointer::Down(Point::new(x, y))));
        for k in 0..frames {
            self.last_input_ms = self.now_ms();
            x += dx;
            y += dy;
            self.app
                .apply(Message::Pointer(Pointer::Move(Point::new(x, y))));
            self.step();
            if let Some((n, prefix)) = self.shoot_every.clone() {
                if k % n.max(1) == 0 {
                    self.shoot(&format!("{prefix}-f{k:03}"));
                }
            }
            // The pointer stays inside the pane: lift and press again.
            let out = x < 10.0 || y < 10.0 || x > w as f32 - 10.0 || y > h as f32 - 10.0;
            if out && k + 1 < frames {
                self.app.apply(Message::Pointer(Pointer::Up));
                (x, y) = (w as f32 / 2.0, h as f32 / 2.0);
                self.app
                    .apply(Message::Pointer(Pointer::Down(Point::new(x, y))));
            }
        }
        self.app.apply(Message::Pointer(Pointer::Up));
        self.drag_curved_drawn += self.curved_drawn_now().saturating_sub(curved);
        self.dragging = false;
        self.settle_left = SETTLE_STEPS;
    }

    /// Whole curved frames the tile cache has drawn so far (1943).
    fn curved_drawn_now(&self) -> u64 {
        let frames = self
            .app
            .frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        frames.tile_stats().curved_drawn
    }

    /// The view jumped to `(lat, lon)` at `zoom`, as "show where I am"
    /// moves it.
    pub fn jump(&mut self, to: (f64, f64), zoom: u8) {
        self.last_input_ms = self.now_ms();
        self.app.settle();
        let at = project(LatLon::from_degrees(to.0, to.1));
        let (w, h) = self.app.view.size();
        self.app.view = ExactView::new(at, u32::from(zoom) * MILLI, w, h);
        self.app.hold();
    }

    /// At rest until the view is filled at its zoom and the frame owes
    /// nothing, within the bound derived from what the view needs
    /// ([`Walk::fill_bound_for`], 1821); then the pause's picture.
    pub fn rest(&mut self, label: &str, expects: bool) {
        let bound = self.fill_bound_for();
        self.rest_bounded(label, bound, expects);
    }

    /// As [`Walk::rest`], with an explicit bound: a fault session's
    /// pause, which expects no fill. `expects` says the rule holds the
    /// rest to its bound (a reachable source).
    pub fn rest_bounded(&mut self, label: &str, bound_ms: u64, expects: bool) {
        // Below the autoload's floor nothing is asked and nothing fills:
        // a short pause and its picture, no bound waited out.
        if self.app.view.tile_zoom() < MIN_ZOOM {
            self.run(30);
            self.checkpoint(label, None, bound_ms, false);
            return;
        }
        let from = self.now_ms();
        let until = from + bound_ms;
        let mut filled_at = None;
        while self.now_ms() < until {
            self.step();
            let last = self.frames.last().expect("a frame");
            if last.uncovered == 0.0
                && last.missing == 0
                && !self.last_fading
                && !self.loader.loading_here()
            {
                filled_at = Some(self.now_ms());
                break;
            }
        }
        // A landing's last progress and the names' fade settle.
        self.run(4);
        self.checkpoint(label, filled_at.map(|at| at - from), bound_ms, expects);
    }

    /// The pause's picture through the window's own painter, cut to the
    /// screen, and its record.
    pub fn checkpoint(
        &mut self,
        label: &str,
        fill_took: Option<u64>,
        bound_ms: u64,
        expects: bool,
    ) {
        let t = self.now_ms();
        // A wheel's glide still on its way ends here: the pause's picture
        // is the view the user asked for.
        self.app.settle();
        let Some(order) = self.app.order() else {
            return;
        };
        let mut shown = draw(&self.app.frames, order);
        for _ in 0..60 {
            if !shown.fading && shown.missing == 0 {
                break;
            }
            self.step();
            let Some(order) = self.app.order() else {
                break;
            };
            shown = draw(&self.app.frames, order);
        }
        let crop = crop_screen(&shown, &self.app);
        let mut treated_px = treated_on_screen(&shown);
        // The positive control (1812): the stale loads painted as a
        // painter would that forgot the regions holding the view, the
        // treatment left over complete data.
        if self.inject == Inject::StaleTreatment {
            let mut scratch =
                vec![0u8; shown.view.size().0 as usize * shown.view.size().1 as usize * 4];
            let forgot = loquax::ui::maploading::paint(
                &mut scratch,
                &shown.view,
                &shown.order.loads,
                &[],
                &loquax::ui::maploading::Brush {
                    phase: shown.order.phase,
                    dark: false,
                    scale: shown.order.scale,
                    margin: shown.order.margin,
                    look: loquax::ui::maploading::Look::default(),
                },
            );
            treated_px = pieces_on_screen(&shown, &forgot.pieces);
        }
        let mut crop = crop;
        if self.inject == Inject::PerturbPicture {
            let seen_before = self.checks.iter().any(|c| {
                c.centre == self.app.view.centre && c.zoom_milli == self.app.view.zoom_milli()
            });
            if seen_before {
                for px in crop.iter_mut().take(4096) {
                    *px ^= 0x55;
                }
            }
        }
        let blank = blank_share(&crop) > BLANK_SHARE;
        let inside = inside_archive(&self.area, &self.view_now().bounds());
        self.checks.push(Check {
            label: label.to_string(),
            t,
            centre: self.app.view.centre,
            zoom_milli: self.app.view.zoom_milli(),
            uncovered: self.uncovered_now(),
            filled: fill_took.is_some(),
            expects_fill: expects && inside && self.app.view.tile_zoom() >= MIN_ZOOM,
            inside,
            fill_bound_ms: bound_ms,
            fill_took_ms: fill_took.unwrap_or(bound_ms),
            crop,
            blank,
            treated_px,
        });
        if self.inject == Inject::DeleteRegion && !self.deleted && fill_took.is_some() {
            if let Some(dir) = self.maps.dir() {
                for entry in std::fs::read_dir(dir).expect("maps").flatten() {
                    if entry.file_name().to_string_lossy().ends_with(".lmr") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
                self.maps.changed();
                self.deleted = true;
            }
        }
    }

    /// The fill bound of a rest over the view now (1821, the audit of
    /// 1811): at full speed the flat [`FILL_FAST_MS`] — the link is not
    /// the cost — and at a limited rate [`FILL_BASE_MS`] plus twice the
    /// time the view's own bytes ([`view_bytes`]) take at that rate,
    /// twice for the requests and the directories. The bound is what
    /// the view NEEDS, not what today's cells fetch: a rest waiting
    /// behind a giant unpausable cell load fails it, which is the
    /// measured finding sharpenings 1 and 2 of 1811 are for.
    pub fn fill_bound_for(&self) -> u64 {
        match self.rate {
            0 => FILL_FAST_MS,
            rate => FILL_BASE_MS + 2 * view_bytes(&self.view_now()) * 1000 / rate,
        }
    }

    /// The drain budget for this walk's link: wide enough for the loads
    /// the session left running.
    pub fn fill_bound(&self) -> u64 {
        match self.rate {
            0 => FILL_FAST_MS,
            _ => 45_000,
        }
    }

    /// Step until the loader is idle, at most `bound_ms`: the session's
    /// drain, so `nothing_lost` sees every box resolved.
    pub fn drain(&mut self, bound_ms: u64) {
        let until = self.now_ms() + bound_ms;
        while self.now_ms() < until && self.loader.busy() {
            self.step();
        }
        self.run(4);
    }

    /// The share of the rested view's pixels the relief moves (1868): the
    /// view drawn as the window draws it, with the relief, and again with
    /// the config's `map_relief` off; recorded for `relief_drawn`.
    pub fn relief(&mut self) {
        let mut crops = Vec::new();
        for relief in [true, false] {
            let mut seen = self.app.seen.clone();
            seen.style.relief = relief;
            self.app.apply(Message::Seen(Box::new(seen)));
            self.app.settle();
            let Some(order) = self.app.order() else {
                return;
            };
            let mut shown = draw(&self.app.frames, order);
            for _ in 0..60 {
                if !shown.fading && shown.missing == 0 {
                    break;
                }
                self.step();
                let Some(order) = self.app.order() else {
                    break;
                };
                shown = draw(&self.app.frames, order);
            }
            crops.push(crop_screen(&shown, &self.app));
        }
        let mut seen = self.app.seen.clone();
        seen.style.relief = true;
        self.app.apply(Message::Seen(Box::new(seen)));
        let (on, off) = (&crops[0], &crops[1]);
        let moved = on
            .chunks_exact(4)
            .zip(off.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        self.relief = Some(moved as f64 / (on.len() / 4).max(1) as f64);
    }

    /// Mark the walk's fault as ended now; [`rules`] holds the recovery
    /// to [`RECOVER_MS`] from here.
    pub fn fault_cleared(&mut self) {
        self.fault.cleared_at = Some(self.now_ms());
    }

    /// At rest, hands off, until the view fills or [`RECOVER_MS`] passes
    /// after the fault's end.
    pub fn await_recovery(&mut self) {
        let Some(cleared) = self.fault.cleared_at else {
            return;
        };
        while self.now_ms() < cleared + RECOVER_MS {
            self.step();
            self.sticky_backoff();
            let last = self.frames.last().expect("a frame");
            if last.uncovered == 0.0 && last.missing == 0 && !self.loader.loading_here() {
                self.fault.filled_at = Some(self.now_ms());
                break;
            }
        }
        let filled = self.fault.filled_at.is_some();
        if !filled {
            // The recovery bound passed with the view still lacking: an
            // on-screen box sitting in a retry wait is what kept it so.
            let view = self.view_now().bounds();
            self.fault.backoff_after_clear |=
                self.loader
                    .report(self.now_ms())
                    .iter()
                    .any(|(b, rank, state)| {
                        *rank <= Rank::Here
                            && overlaps(&view, b)
                            && matches!(state, LoadState::Retrying { .. })
                    });
        }
        self.checkpoint("after-fault", self.fault.filled_at, RECOVER_MS, false);
    }

    /// A retry that keeps an on-screen box waiting after the fault ended
    /// (1821, the audit of 1811; the old `Skipped "backoff"` detection
    /// died with the loader's retry queue): a box of the view booked
    /// past the screen ladder's last rung ([`SCREEN_RETRY_MAX_MS`])
    /// cannot heal the view in time, whatever else happens.
    fn sticky_backoff(&mut self) {
        let view = self.view_now().bounds();
        for (b, rank, state) in self.loader.report(self.now_ms()) {
            if rank <= Rank::Here && overlaps(&view, &b) {
                if let LoadState::Retrying { in_ms } = state {
                    if in_ms > SCREEN_RETRY_MAX_MS {
                        self.fault.backoff_after_clear = true;
                    }
                }
            }
        }
    }
}

/// The two-area archive: [`first_tile`]'s z15 tile in every slot of
/// [`WIDE_BOX`] from z10 to z15, padded to a dense city tile's size.
pub fn wide_archive() -> Vec<u8> {
    archive_over(&WIDE_BOX)
}

/// [`first_tile`]'s z15 tile in every slot of `area` from z10 to z15: the
/// walk's archive over any box (the desert sessions', 1868).
pub fn archive_over(area: &Bounds) -> Vec<u8> {
    use leviculum_mapcut::pmtiles::{padded, Archive};
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../leviculum-mapcut/fixtures/grunewald.pmtiles");
    let archive = Archive::open(&path).expect("the fixture");
    let z15 = archive
        .tiles()
        .expect("tiles")
        .into_iter()
        .find(|t| t.z == 15)
        .expect("a z15 tile");
    let mvt = archive.mvt(z15).expect("read").expect("the tile");
    let meta = archive.metadata().expect("metadata");
    padded(&mvt, area, 10..=15, first_tile::FIRST_TILE_PAD, &meta).expect("the archive")
}

/// The policy sessions' wide coarse ground (1933): about 2.8 by 1.4
/// degrees around both areas. The tiered archive holds the coarse levels
/// over it and the street detail only over [`WIDE_BOX`], as the real
/// source holds the world coarse and every street deep.
pub const BIG_BOX: Bounds = Bounds {
    west: 120_000_000,
    south: 517_000_000,
    east: 148_000_000,
    north: 531_000_000,
};

/// The z3 tile x=3 y=3, pulled in a little so [`tiered`] cuts no edge
/// tiles of the neighbours: the session standing for Morocco at z6
/// (1933) plays inside it.
pub const MOROCCO_BOX: Bounds = Bounds {
    west: -449_000_000,
    south: 5_000_000,
    east: -1_000_000,
    north: 409_000_000,
};

/// An archive of tiers (1933): the Grunewald z15 tile in every slot of
/// each tier's box at its zooms, padded as
/// [`leviculum_mapcut::pmtiles::padded`] pads, so no two blobs are stored
/// once and a box's bytes grow with its tiles.
pub fn tiered(tiers: &[(Bounds, core::ops::RangeInclusive<u8>)]) -> Vec<u8> {
    use leviculum_map::region::TileId;
    use leviculum_mapcut::cut::tile_range;
    use leviculum_mapcut::pmtiles::{tile_id, write, NONE};
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../leviculum-mapcut/fixtures/grunewald.pmtiles");
    let archive = leviculum_mapcut::pmtiles::Archive::open(&path).expect("the fixture");
    let z15 = archive
        .tiles()
        .expect("tiles")
        .into_iter()
        .find(|t| t.z == 15)
        .expect("a z15 tile");
    let mvt = archive.mvt(z15).expect("read").expect("the tile");
    let meta = archive.metadata().expect("metadata");
    let mut tiles = std::collections::BTreeMap::new();
    for (bounds, zooms) in tiers {
        for z in zooms.clone() {
            let (xs, ys) = tile_range(bounds, z);
            for x in xs.into_iter().flatten() {
                for y in ys.clone() {
                    let t = TileId { z, x, y };
                    let mut blob = mvt.clone();
                    // Field 15, length-delimited: unknown to the Tile
                    // message, [`first_tile::FIRST_TILE_PAD`] bytes that
                    // differ from tile to tile.
                    let pad = first_tile::FIRST_TILE_PAD;
                    blob.push(15 << 3 | 2);
                    let mut len = pad as u64;
                    while len >= 0x80 {
                        blob.push((len as u8) | 0x80);
                        len >>= 7;
                    }
                    blob.push(len as u8);
                    let mut v = tile_id(t).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                    blob.extend((0..pad).map(|_| {
                        v ^= v << 13;
                        v ^= v >> 7;
                        v ^= v << 17;
                        v as u8
                    }));
                    tiles.insert(t, blob);
                }
            }
        }
    }
    let mut out = Vec::new();
    write(&tiles, NONE, &meta, &mut out).expect("the archive");
    out
}

/// A walk over the tiered archive of `tiers`, the view at `at` and
/// `zoom`, its judged area `area` (1933).
pub fn tiered_walk(
    name: &str,
    tiers: &[(Bounds, core::ops::RangeInclusive<u8>)],
    area: Bounds,
    at: (f64, f64),
    zoom: u8,
    rate: u64,
    latency_ms: u64,
) -> Walk {
    let serve = Serve::start(vec![("w.pmtiles".into(), tiered(tiers))]).expect("serve");
    let ground = Ground {
        at: project(LatLon::from_degrees(at.0, at.1)),
        zoom,
        cache_bytes: 1 << 30,
        prefill: 0,
        size: WALK_SIZE,
    };
    let mut walk = Walk::over(serve, name, rate, latency_ms, ground, Inject::None);
    walk.area = area;
    walk
}

/// Install `copies` old autoloaded regions of a far corner of the
/// archive, so a small cache cap evicts them: one region cut at full
/// speed, copied under names of its own, each a day old.
fn prefill_cache(maps: &std::path::Path, url: &str, copies: usize) {
    let mut loader = Loader::new(
        Maps::new(Some(maps.to_path_buf())),
        Source::Url(url.to_string()),
        1 << 30,
    );
    let east = project(LatLon::from_degrees(52.475, 13.52));
    let grown = leviculum_map::viewport::Viewport::new(east, 15, 600, 400);
    loader.ask(
        MapWant {
            bounds: grown.bounds(),
            zooms: (13, 15),
            floor: 15,
            reason: loquax::ui::map::LoadReason::View,
            centre: east,
        },
        0,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let name = loop {
        let steps = loader.poll(0);
        if let Some(Step::Done { landed, .. }) =
            steps.iter().find(|s| !matches!(s, Step::Started(_)))
        {
            break landed.name.clone();
        }
        assert!(
            !steps.iter().any(|s| matches!(s, Step::Failed { .. })),
            "the prefill failed: {steps:?}"
        );
        assert!(Instant::now() < deadline, "the prefill never landed");
        std::thread::sleep(Duration::from_millis(10));
    };
    let old = std::time::SystemTime::now() - Duration::from_secs(86_400);
    for n in 0..copies {
        let copy = maps.join(format!("auto-prefill-{n}.lmr"));
        std::fs::copy(maps.join(&name), &copy).expect("copy");
        std::fs::File::open(&copy)
            .expect("open")
            .set_modified(old)
            .expect("mtime");
    }
    std::fs::remove_file(maps.join(&name)).expect("the original goes");
}

/// The screen's pixels of `shown`, margins cut, RGBA.
/// Pixels of `shown`'s screen, its margins cut, its treatment's pieces
/// cover (1812).
fn treated_on_screen(shown: &super::Shown) -> u64 {
    pieces_on_screen(shown, &shown.painted.pieces)
}

/// Pixels of `shown`'s screen, its margins cut, `pieces` cover.
fn pieces_on_screen(shown: &super::Shown, pieces: &[loquax::ui::maploading::Piece]) -> u64 {
    let (mx, my) = shown.order.margin;
    let (w, h) = shown.view.size();
    let (x0, y0) = (mx as i32, my as i32);
    let (x1, y1) = (w as i32 - mx as i32, h as i32 - my as i32);
    pieces
        .iter()
        .map(|p| {
            let cw = (p.x1.min(x1) - p.x0.max(x0)).max(0);
            let ch = (p.y1.min(y1) - p.y0.max(y0)).max(0);
            (cw as u64) * (ch as u64)
        })
        .sum()
}

/// The screen's pixels as the window presents `shown` over `app`'s
/// view (`MapCanvas::draw` in gui.rs): placed and scaled
/// ([`loquax::mapframe::onto`]), nearest while the view moves or at
/// scale one and linear at rest, and the map's ground where the frame
/// does not reach (1441); with the share of the screen the frame does
/// not reach.
fn present(shown: &super::Shown, app: &App) -> (Vec<u8>, f64) {
    let (fw, fh) = shown.view.size();
    let pixels: Vec<u8> = match &shown.handle {
        super::Handle::Rgba { pixels, .. } => pixels.to_vec(),
        other => panic!("not raw pixels: {other:?}"),
    };
    let (x0, y0, k) = loquax::mapframe::onto(&shown.view, &app.view);
    let nearest = (k - 1.0).abs() < 1e-9 || app.moving();
    let [gr, gg, gb] = leviculum_mapfill::frame::rgb888(app.map_style().theme.palette().background);
    let (w, h) = app.view.size();
    let at = |x: usize, y: usize, c: usize| f64::from(pixels[(y * fw as usize + x) * 4 + c]);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    let mut bare = 0usize;
    for sy in 0..h {
        let fy = (f64::from(sy) + 0.5 - y0) / k - 0.5;
        for sx in 0..w {
            let fx = (f64::from(sx) + 0.5 - x0) / k - 0.5;
            if fx <= -0.5 || fy <= -0.5 || fx >= f64::from(fw) - 0.5 || fy >= f64::from(fh) - 0.5 {
                out.extend_from_slice(&[gr, gg, gb, 0xff]);
                bare += 1;
                continue;
            }
            if nearest {
                let (x, y) = (fx.round().max(0.0) as usize, fy.round().max(0.0) as usize);
                let (x, y) = (x.min(fw as usize - 1), y.min(fh as usize - 1));
                out.extend((0..3).map(|c| at(x, y, c) as u8));
                out.push(0xff);
                continue;
            }
            let (xa, ya) = (fx.floor().max(0.0) as usize, fy.floor().max(0.0) as usize);
            let (xb, yb) = ((xa + 1).min(fw as usize - 1), (ya + 1).min(fh as usize - 1));
            let (tx, ty) = (
                (fx - fx.floor()).clamp(0.0, 1.0),
                (fy - fy.floor()).clamp(0.0, 1.0),
            );
            for c in 0..3 {
                let top = at(xa, ya, c) * (1.0 - tx) + at(xb, ya, c) * tx;
                let bottom = at(xa, yb, c) * (1.0 - tx) + at(xb, yb, c) * tx;
                out.push((top * (1.0 - ty) + bottom * ty).round() as u8);
            }
            out.push(0xff);
        }
    }
    (out, bare as f64 / f64::from(w * h))
}

fn crop_screen(shown: &super::Shown, app: &App) -> Vec<u8> {
    let (fw, fh) = shown.view.size();
    let pixels: Vec<u8> = match &shown.handle {
        super::Handle::Rgba { pixels, .. } => pixels.to_vec(),
        other => panic!("not raw pixels: {other:?}"),
    };
    let (x0, y0, k) = loquax::mapframe::onto(&shown.view, &app.view);
    assert!((k - 1.0).abs() < 1e-6, "a rest frame is unscaled: {k}");
    let (w, h) = app.view.size();
    let mut crop = Vec::with_capacity((w * h * 4) as usize);
    for sy in 0..h {
        for sx in 0..w {
            let fx = (f64::from(sx) - x0).round() as i64;
            let fy = (f64::from(sy) - y0).round() as i64;
            if (0..i64::from(fw)).contains(&fx) && (0..i64::from(fh)).contains(&fy) {
                let i = ((fy as u32 * fw + fx as u32) * 4) as usize;
                crop.extend_from_slice(&pixels[i..i + 4]);
            } else {
                crop.extend_from_slice(&[0xff, 0x00, 0xff, 0xff]);
            }
        }
    }
    crop
}

/// The bytes the view itself needs from the source (1821): the screen's
/// own box at the view's zoom and the step above it (its cut, profile 2,
/// carries z-1..z), over the padded archive's uniform tiles
/// ([`first_tile::first_tile_blob`] each), a tile of slack on each axis.
pub fn view_bytes(view: &leviculum_map::viewport::Viewport) -> u64 {
    let (w, h) = view.size();
    let tiles = |shift: u32| {
        let span = |px: u32| u64::from((px >> shift) / 256 + 2);
        span(w) * span(h)
    };
    (tiles(0) + tiles(1)) * first_tile::first_tile_blob()
}

/// Whether `view` lies inside the archive's box `area` ([`WIDE_BOX`],
/// or a desert's) with a margin of 0.03 degrees (the screen box's
/// growth): past the archive the world is empty, which is no loader's
/// fault, so no fill or picture rule judges a view that reaches out.
pub fn inside_archive(area: &Bounds, view: &Bounds) -> bool {
    const MARGIN: i32 = 300_000;
    let shrunk = Bounds {
        west: area.west + MARGIN,
        south: area.south + MARGIN,
        east: area.east - MARGIN,
        north: area.north - MARGIN,
    };
    shrunk.holds(view)
}

/// Whether two boxes overlap (edges included, no antimeridian walks
/// here: the fixture's box does not cross it).
fn overlaps(a: &Bounds, b: &Bounds) -> bool {
    a.west <= b.east && b.west <= a.east && a.south <= b.north && b.south <= a.north
}

/// The map's own clear colour in the walk's table, as frame pixels: a
/// frame that lost its picture shows this and nothing else, while a
/// legitimately uniform picture (the fixture's unbroken forest, drawn
/// coarse while finer tiles arrive) is drawn data of another colour.
/// The first cut of 1821 called every uniform frame blank and flagged
/// the forest stand-in on a slow host.
fn ground_rgba() -> [u8; 4] {
    let [r, g, b] = leviculum_mapfill::frame::rgb888(
        leviculum_mapfill::style::MapTheme::Light
            .palette()
            .background,
    );
    [r, g, b, 0xff]
}

/// Whether the drawn frame `shown` is blank: the ground colour
/// ([`ground_rgba`]) over more than [`BLANK_SHARE`] of it (1821).
/// Every 16th pixel is read, so a session of thousands of frames is
/// not spent counting.
fn frame_blank(shown: &super::Shown) -> bool {
    let super::Handle::Rgba { pixels, .. } = &shown.handle else {
        return false;
    };
    // The map's own pixels (1812): the loads' treatment says the data is
    // still to come, it is no picture of the map. Plain ground under it,
    // in any of the colours the treatment makes of the ground, is ground;
    // a stand-in between the stripes is still a picture.
    let width = shown.view.size().0 as usize;
    let ground = ground_rgba();
    let dark = shown.order.style.theme == leviculum_mapfill::style::MapTheme::Dark;
    let tints: std::collections::HashSet<[u8; 3]> =
        loquax::ui::maploading::ground_tints([ground[0], ground[1], ground[2]], dark)
            .into_iter()
            .collect();
    let mut hits = 0u32;
    let mut total = 0u32;
    for (n, px) in pixels.chunks_exact(4).enumerate().step_by(16) {
        let plain = px == ground || {
            let (x, y) = ((n % width) as f64 + 0.5, (n / width) as f64 + 0.5);
            shown.painted.pieces.iter().any(|p| p.contains(x, y))
                && tints.contains(&[px[0], px[1], px[2]])
        };
        hits += u32::from(plain);
        total += 1;
    }
    f64::from(hits) / f64::from(total.max(1)) > BLANK_SHARE
}

/// The share of `rgba` the map's own clear colour covers
/// ([`ground_rgba`], 1821).
pub fn blank_share(rgba: &[u8]) -> f64 {
    let ground = ground_rgba();
    let mut hits = 0u32;
    let mut total = 0u32;
    for px in rgba.chunks_exact(4).step_by(4) {
        hits += u32::from(px == ground);
        total += 1;
    }
    f64::from(hits) / f64::from(total.max(1))
}

fn hash_of(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// The verdicts of one finished walk, every rule applied to its trace.
pub fn rules(walk: &Walk) -> Vec<Verdict> {
    let mut out = Vec::new();
    let name = walk.name.clone();
    let v = |rule: &'static str,
             pass: bool,
             value: String,
             limit: String,
             note: String,
             image: String,
             zoom: String| Verdict {
        session: name.clone(),
        rule,
        pass,
        value,
        limit,
        note,
        image,
        zoom,
    };

    // view_filled_at_rest: every pause that expected a fill, filled.
    for (n, c) in walk.checks.iter().enumerate() {
        if !c.expects_fill {
            continue;
        }
        out.push(v(
            "view_filled_at_rest",
            c.filled,
            format!(
                "{:.3} uncovered after {} ms (t={})",
                c.uncovered, c.fill_took_ms, c.t
            ),
            format!("0 within {} ms", c.fill_bound_ms),
            c.label.clone(),
            image_name(walk, n),
            format!("{}", c.zoom_milli / 1000),
        ));
    }

    // screen_first: no load starts for anything else while the screen
    // lacks data; loads already running are measured, not failed.
    let started_rank: HashMap<u64, Vec<(Bounds, Rank)>> = HashMap::new();
    let _ = started_rank;
    let mut lack_from: Option<u64> = None;
    let mut wrong_starts = Vec::new();
    let mut waited_ms = 0u64;
    let mut last_t = walk.frames.first().map_or(0, |f| f.t);
    for f in &walk.frames {
        let lacking = f.uncovered > GAP_SHARE && f.zoom_milli / 1000 >= u32::from(MIN_ZOOM);
        match (lacking, lack_from) {
            (true, None) => lack_from = Some(f.t),
            (false, Some(_)) => lack_from = None,
            _ => {}
        }
        if let Some(from) = lack_from {
            let on_screen_runs = f
                .order
                .iter()
                .any(|(_, rank, running)| *running && *rank <= Rank::Here);
            let off_screen_runs = f
                .order
                .iter()
                .any(|(_, r, running)| *running && *r > Rank::Here);
            if !on_screen_runs && off_screen_runs {
                waited_ms += f.t - last_t;
            }
            // A load for elsewhere that STARTED during the lack.
            for (t, ev) in &walk.events {
                if *t < from || *t > f.t {
                    continue;
                }
                if let Ev::Started(b, _) = ev {
                    let rank = f
                        .order
                        .iter()
                        .find(|(ob, _, running)| *running && ob == b)
                        .map(|(_, r, _)| *r);
                    if rank.is_some_and(|r| r > Rank::Here) && *t == f.t {
                        wrong_starts.push((*t, *b));
                    }
                }
            }
        }
        last_t = f.t;
    }
    out.push(v(
        "screen_first",
        wrong_starts.is_empty(),
        format!(
            "{} off-screen starts while the screen lacked",
            wrong_starts.len()
        ),
        "0".to_string(),
        format!(
            "screen waited {waited_ms} ms behind running off-screen loads (the measured exception)"
        ),
        String::new(),
        String::new(),
    ));

    // nothing_lost: queue disappearances explained, no stall.
    let mut lost = Vec::new();
    for pair in walk.frames.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        for (bounds, rank, running) in &a.order {
            if *running {
                continue;
            }
            let still = b.order.iter().any(|(ob, _, _)| ob == bounds);
            let ended = walk.events.iter().any(|(t, ev)| {
                (*t >= a.t && *t <= b.t)
                    && matches!(ev, Ev::Started(eb, _) | Ev::Done(eb, _, _) | Ev::Failed(eb, _) if eb == bounds)
            });
            if still || ended {
                continue;
            }
            let away_full = b
                .order
                .iter()
                .filter(|(_, r, running)| !running && *r == Rank::Away)
                .count()
                >= MAX_AWAY;
            let named = match rank {
                Rank::Screen => "screen-left",
                Rank::Away if away_full => "away-cap",
                Rank::Away => "away-replaced",
                _ if away_full => "away-cap",
                _ => "",
            };
            if named.is_empty() {
                lost.push((a.t, *bounds, *rank));
            }
        }
    }
    // Stalls: while boxes wait, bytes must flow or a load must start,
    // end or fail within STALL_MS. A slow load making progress is slow,
    // not stuck.
    let mut stall = None;
    let mut last_progress = walk.frames.first().map_or(0, |f| f.t);
    let mut prev_sent = walk.frames.first().map_or(0, |f| f.sent);
    let mut prev_t = last_progress;
    for f in &walk.frames {
        let event = walk.events.iter().any(|(t, ev)| {
            *t > prev_t
                && *t <= f.t
                && matches!(ev, Ev::Started(..) | Ev::Done(..) | Ev::Failed(..))
        });
        if event || f.sent != prev_sent {
            last_progress = f.t;
        }
        let waiting = f.order.iter().any(|(_, _, running)| !running);
        if waiting && f.t.saturating_sub(last_progress) > STALL_MS {
            stall = Some((last_progress, f.t));
            break;
        }
        if !waiting {
            last_progress = f.t;
        }
        prev_sent = f.sent;
        prev_t = f.t;
    }
    out.push(v(
        "nothing_lost",
        lost.is_empty() && stall.is_none(),
        format!(
            "{} unexplained drops{}",
            lost.len(),
            stall.map_or(String::new(), |(a, b)| format!(
                ", queue stalled {} ms from t={a}",
                b - a
            ))
        ),
        format!("0 drops, no stall over {STALL_MS} ms"),
        lost.iter()
            .take(3)
            .map(|(t, b, r)| format!("t={t} {b:?} as {r:?}"))
            .collect::<Vec<_>>()
            .join("; "),
        String::new(),
        String::new(),
    ));

    // no_regress: the same rest view never shows less later.
    let mut regress = Vec::new();
    for (i, a) in walk.checks.iter().enumerate() {
        for (j, b) in walk.checks.iter().enumerate().skip(i + 1) {
            if a.inside
                && a.centre == b.centre
                && a.zoom_milli == b.zoom_milli
                && b.uncovered > a.uncovered + 0.001
            {
                regress.push((i, j, a.uncovered, b.uncovered));
            }
        }
    }
    out.push(v(
        "no_regress",
        regress.is_empty(),
        format!("{} revisits showed less", regress.len()),
        "0".to_string(),
        regress
            .iter()
            .take(3)
            .map(|(i, j, a, b)| {
                format!(
                    "{} {:.2} -> {} {:.2}",
                    walk.checks[*i].label, a, walk.checks[*j].label, b
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
        regress
            .first()
            .map(|(_, j, _, _)| image_name(walk, *j))
            .unwrap_or_default(),
        String::new(),
    ));

    // same_view_same_picture: filled rests at the same view match.
    let mut differs = Vec::new();
    for (i, a) in walk.checks.iter().enumerate() {
        for (j, b) in walk.checks.iter().enumerate().skip(i + 1) {
            if a.inside
                && a.centre == b.centre
                && a.zoom_milli == b.zoom_milli
                && a.filled
                && b.filled
                && hash_of(&a.crop) != hash_of(&b.crop)
            {
                differs.push((i, j));
            }
        }
    }
    out.push(v(
        "same_view_same_picture",
        differs.is_empty(),
        format!("{} filled revisits differed", differs.len()),
        "0".to_string(),
        differs
            .iter()
            .take(3)
            .map(|(i, j)| format!("{} vs {}", walk.checks[*i].label, walk.checks[*j].label))
            .collect::<Vec<_>>()
            .join("; "),
        differs
            .first()
            .map(|(_, j)| image_name(walk, *j))
            .unwrap_or_default(),
        String::new(),
    ));

    // no_spin: no double start, no start after done with the file there,
    // no restart loop.
    let mut spins = Vec::new();
    let mut runs: Vec<Bounds> = Vec::new();
    let mut starts: HashMap<String, usize> = HashMap::new();
    let mut landed_files: HashMap<String, Bounds> = HashMap::new();
    for (t, ev) in &walk.events {
        match ev {
            Ev::Started(b, _) => {
                if runs.contains(b) {
                    spins.push(format!("t={t} started while running: {b:?}"));
                }
                if landed_files.values().any(|lb| lb == b) {
                    spins.push(format!("t={t} started again after landing: {b:?}"));
                }
                runs.push(*b);
                let k = format!("{b:?}");
                let n = starts.entry(k.clone()).or_default();
                *n += 1;
                if *n > SPIN_STARTS {
                    spins.push(format!("t={t} start {n} of {k}"));
                }
            }
            Ev::Done(b, file, evicted) => {
                runs.retain(|rb| rb != b);
                landed_files.insert(file.clone(), *b);
                for name in evicted {
                    landed_files.remove(name);
                }
            }
            Ev::Failed(b, _) => {
                runs.retain(|rb| rb != b);
                starts.remove(&format!("{b:?}"));
            }
            // A dropped load ends there; it starts again later.
            Ev::Held(b, Held::Dropped) => runs.retain(|rb| rb != b),
            Ev::Skipped(..) | Ev::Held(..) => {}
        }
    }
    out.push(v(
        "no_spin",
        spins.is_empty(),
        format!("{} spins", spins.len()),
        "0".to_string(),
        spins.iter().take(3).cloned().collect::<Vec<_>>().join("; "),
        String::new(),
        String::new(),
    ));

    // fault_recovers: only for walks that cleared a fault.
    if let Some(cleared) = walk.fault.cleared_at {
        let took = walk.fault.filled_at.map(|at| at - cleared);
        let pass = took.is_some() && !walk.fault.backoff_after_clear;
        out.push(v(
            "fault_recovers",
            pass,
            match took {
                Some(ms) => format!("filled {ms} ms after the fault ended"),
                None => "never filled".to_string(),
            },
            format!("within {RECOVER_MS} ms, no backoff on screen"),
            match walk.fault.backoff_after_clear {
                true => "a box on screen was refused as backoff after the source answered".into(),
                false => String::new(),
            },
            String::new(),
            String::new(),
        ));
    }

    // frame_never_blank: every drawn frame of the session (1821), the
    // pause records included. A blank frame at the very view whose last
    // drawn frame had a picture is a lost picture; a view that moved
    // onto ground not loaded yet is legitimately empty and not judged.
    let mut blank_checks = Vec::new();
    for (i, pair) in walk.checks.windows(2).enumerate() {
        if pair[1].inside && !pair[0].blank && pair[1].blank {
            blank_checks.push(i + 1);
        }
    }
    let mut blank_frames = Vec::new();
    let mut last_drawn: Option<&FrameRec> = None;
    for f in walk.frames.iter().filter(|f| f.drew) {
        if let (Some(prev), Some(blank)) = (last_drawn, f.blank) {
            if f.inside
                && blank
                && prev.blank == Some(false)
                && prev.centre == f.centre
                && prev.zoom_milli == f.zoom_milli
            {
                blank_frames.push((f.t, prev.treated_px));
            }
        }
        last_drawn = Some(f);
    }
    // no_busy_wake: the loader, polled and idle, never names a wake of
    // 0 ms (1835): the wake it names is an instant something can start.
    let busy_wakes: Vec<u64> = walk
        .frames
        .iter()
        .filter(|f| f.busy_wake)
        .map(|f| f.t)
        .collect();
    out.push(v(
        "no_busy_wake",
        busy_wakes.is_empty(),
        format!("{} idle frames with a 0 ms wake named", busy_wakes.len()),
        "0".to_string(),
        busy_wakes
            .iter()
            .take(3)
            .map(|t| format!("t={t}"))
            .collect::<Vec<_>>()
            .join("; "),
        String::new(),
        String::new(),
    ));

    out.push(v(
        "frame_never_blank",
        blank_checks.is_empty() && blank_frames.is_empty(),
        format!(
            "{} blank pauses, {} blank drawn frames after a picture",
            blank_checks.len(),
            blank_frames.len()
        ),
        "0".to_string(),
        blank_checks
            .iter()
            .map(|i| walk.checks[*i].label.clone())
            .chain(
                blank_frames
                    .iter()
                    .take(3)
                    .map(|(t, px)| format!("frame t={t} (the frame before: {px} px treated)")),
            )
            .take(3)
            .collect::<Vec<_>>()
            .join("; "),
        blank_checks
            .first()
            .map(|i| image_name(walk, *i))
            .unwrap_or_default(),
        String::new(),
    ));
    // loading_is_visible (1812): at rest, every point without data
    // carries the treatment in the drawn frame.
    let judged: Vec<&FrameRec> = walk.frames.iter().filter(|f| f.treat.is_some()).collect();
    let lacking: Vec<&FrameRec> = judged
        .iter()
        .copied()
        .filter(|f| f.treat.is_some_and(|(n, _)| n > 0))
        .collect();
    let bare: Vec<&FrameRec> = lacking
        .iter()
        .copied()
        .filter(|f| f.treat.is_some_and(|(_, b)| b > 0))
        .collect();
    out.push(v(
        "loading_is_visible",
        bare.is_empty(),
        format!(
            "{} of {} rested frames lacking data show a point without treatment ({} rested frames)",
            bare.len(),
            lacking.len(),
            judged.len()
        ),
        "0".to_string(),
        bare.iter()
            .take(3)
            .map(|f| {
                let (n, b) = f.treat.unwrap_or_default();
                format!("t={} {b} of {n} bare", f.t)
            })
            .collect::<Vec<_>>()
            .join("; "),
        String::new(),
        String::new(),
    ));

    // progress_moves_with_bytes (1812): the bar is the bytes, it says
    // loading while they move and stands still once they stop.
    let mut wrong = Vec::new();
    let mut moved_at: HashMap<(i32, i32, i32, i32), (u64, u64)> = HashMap::new();
    let mut shown_running = 0usize;
    for f in &walk.frames {
        for &(b, done, bytes) in &f.raw {
            let key = (b.west, b.south, b.east, b.north);
            let since = match moved_at.get(&key) {
                Some(&(was, since)) if was == done => since,
                _ => f.t,
            };
            moved_at.insert(key, (done, since));
            let Some(area) = f.loads.areas.iter().find(|a| a.bounds == b) else {
                continue;
            };
            shown_running += 1;
            let want = milli_of(done, bytes);
            let quiet = f.t - since;
            let ok = match area.kind {
                Kind::Loading { milli } => milli == want && quiet < TREAT_STALL_MS + 1_000,
                Kind::Stalled { milli, .. } => milli == want && quiet + 1_000 >= TREAT_STALL_MS,
                _ => false,
            };
            if !ok {
                wrong.push(format!(
                    "t={} {:?} for {done} of {bytes} bytes, quiet {quiet} ms",
                    f.t, area.kind
                ));
            }
        }
        moved_at.retain(|k, _| {
            f.raw
                .iter()
                .any(|(b, ..)| (b.west, b.south, b.east, b.north) == *k)
        });
    }
    out.push(v(
        "progress_moves_with_bytes",
        wrong.is_empty(),
        format!(
            "{} of {} running areas shown otherwise than their bytes",
            wrong.len(),
            shown_running
        ),
        format!("0; stalled after {TREAT_STALL_MS} ms without a byte"),
        wrong.iter().take(3).cloned().collect::<Vec<_>>().join("; "),
        String::new(),
        String::new(),
    ));

    // no_treatment_when_complete (1812): a filled pause shows none.
    let stale: Vec<usize> = walk
        .checks
        .iter()
        .enumerate()
        .filter(|(_, c)| c.filled && c.treated_px > 0)
        .map(|(n, _)| n)
        .collect();
    out.push(v(
        "no_treatment_when_complete",
        stale.is_empty(),
        format!(
            "{} of {} filled pauses carry a treatment",
            stale.len(),
            walk.checks.iter().filter(|c| c.filled).count()
        ),
        "0".to_string(),
        stale
            .iter()
            .take(3)
            .map(|n| {
                format!(
                    "{} ({} px)",
                    walk.checks[*n].label, walk.checks[*n].treated_px
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
        stale
            .first()
            .map(|n| image_name(walk, *n))
            .unwrap_or_default(),
        String::new(),
    ));

    // idle_draws_nothing (1812): no frame for the stripes' step without
    // stripes that move in it.
    let idle: Vec<u64> = walk
        .frames
        .iter()
        .filter(|f| f.idle_phase_draw)
        .map(|f| f.t)
        .collect();
    out.push(v(
        "idle_draws_nothing",
        idle.is_empty(),
        format!(
            "{} frames drawn for the stripes' step with none moving",
            idle.len()
        ),
        "0".to_string(),
        idle.iter()
            .take(3)
            .map(|t| format!("t={t}"))
            .collect::<Vec<_>>()
            .join("; "),
        String::new(),
        String::new(),
    ));

    // pack_after_region, relief_drawn: the desert sessions (1868).
    if let Some((pack, block)) = &walk.desert {
        let landed = walk.heights.iter().find_map(|(t, _, r)| match r {
            Ok(packs) if packs.contains(pack) => Some(*t),
            _ => None,
        });
        let region = walk.events.iter().find_map(|(t, ev)| match ev {
            Ev::Done(b, ..) if overlaps(b, block) => Some(*t),
            _ => None,
        });
        let failed: Vec<String> = walk
            .heights
            .iter()
            .filter_map(|(_, _, r)| r.as_ref().err().cloned())
            .take(2)
            .collect();
        out.push(v(
            "pack_after_region",
            matches!((region, landed), (Some(r), Some(p)) if r <= p),
            format!("region over the block at {region:?} ms, {pack} at {landed:?} ms"),
            "the pack lands, after a region over its block".to_string(),
            failed.join("; "),
            String::new(),
            String::new(),
        ));
        let share = walk.relief.unwrap_or(0.0);
        let image = walk
            .checks
            .len()
            .checked_sub(1)
            .map(|n| image_name(walk, n))
            .unwrap_or_default();
        out.push(v(
            "relief_drawn",
            share >= RELIEF_SHARE,
            format!("{share:.3} of the rested view moved with the relief on"),
            format!(">= {RELIEF_SHARE:.3}"),
            "the same view drawn with map_relief off".to_string(),
            image,
            walk.checks
                .last()
                .map(|c| format!("{}", c.zoom_milli / 1000))
                .unwrap_or_default(),
        ));
    }

    // Order 1943, in every session that dragged: input never waits on
    // drawing (the walk draws inside its step, as the window's worker
    // does), and a moving curved frame rides the memo instead of a
    // whole-frame redraw.
    if !walk.settles.is_empty() {
        let (w, h) = walk.app.view.size();
        let mp = f64::from(w) * f64::from(h) / 1e6;
        let bound = (SETTLE_DRAW_MS_PER_MP * mp).round() as u64;
        for &(zoom, ms) in &walk.settles {
            out.push(v(
                "settle_draw_in_bound",
                ms <= bound,
                format!("settle draw at z{zoom} took {ms} ms ({w}x{h})"),
                format!("{bound} ms ({SETTLE_DRAW_MS_PER_MP} ms a megapixel)"),
                "a new view at rest over overlapping regions, caches warm".to_string(),
                String::new(),
                format!("{zoom}"),
            ));
        }
    }
    if walk.drag_steps > 0 || walk.wheel_steps > 0 {
        let longest = walk
            .drag_gap_max
            .max(walk.wheel_gap_max)
            .max(walk.settle_gap_max);
        out.push(v(
            "input_never_waits",
            longest <= MOVING_GAP_MS,
            format!(
                "longest step {longest} ms: drag {} ms over {} steps, wheel {} ms over {}, settle {} ms over {}",
                walk.drag_gap_max,
                walk.drag_steps,
                walk.wheel_gap_max,
                walk.wheel_steps,
                walk.settle_gap_max,
                walk.settle_steps
            ),
            format!("{MOVING_GAP_MS} ms"),
            String::new(),
            String::new(),
            String::new(),
        ));
        out.push(v(
            "input_rides_the_memo",
            walk.drag_curved_drawn == 0,
            format!(
                "{} whole curved draws while a finger dragged",
                walk.drag_curved_drawn
            ),
            "0".to_string(),
            "a moving curved frame rides the memo and owes the real one".to_string(),
            String::new(),
            String::new(),
        ));
    }
    out
}

/// The picture name of check `n` of `walk`.
pub fn image_name(walk: &Walk, n: usize) -> String {
    format!(
        "load-{}-{:02}-{}",
        walk.name,
        n,
        walk.checks[n].label.replace(' ', "_")
    )
}

/// Write the walk's pictures and per-frame trace under `out`, and append
/// its verdict rows to `rows`.
pub fn write_out(
    walk: &Walk,
    verdicts: &[Verdict],
    out: Option<&std::path::Path>,
    rows: &mut String,
) {
    for v in verdicts {
        rows.push_str(&format!(
            "load\tsession\t{}\t{}x{}\t\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            v.session,
            WALK_SIZE.0,
            WALK_SIZE.1,
            v.zoom,
            v.image,
            v.rule,
            v.value.replace(['\t', '\n'], " "),
            v.limit.replace(['\t', '\n'], " "),
            if v.pass { "pass" } else { "FAIL" },
            v.note.replace(['\t', '\n'], " "),
        ));
    }
    let Some(out) = out else {
        return;
    };
    let full = out.join("full");
    std::fs::create_dir_all(&full).expect("the pictures directory");
    let (w, h) = walk.app.view.size();
    for (n, c) in walk.checks.iter().enumerate() {
        let mut png = Vec::new();
        let mut e = png::Encoder::new(&mut png, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut writer = e.write_header().expect("header");
        writer.write_image_data(&c.crop).expect("data");
        writer.finish().expect("finish");
        std::fs::write(full.join(format!("{}.png", image_name(walk, n))), png)
            .expect("the picture");
    }
    for (label, crop, _) in &walk.shots {
        write_png(
            &full.join(format!("{}-{label}.png", walk.name)),
            crop,
            (w, h),
        );
    }
    let mut trace = String::from(
        "t\tcentre_x\tcentre_y\tzoom_milli\tuncovered\tsent\tmissing\tdrew\tbusy_wake\tqueue\n",
    );
    for f in &walk.frames {
        let queue: Vec<String> = f
            .order
            .iter()
            .map(|(b, r, running)| {
                format!(
                    "{}{:?}@{},{}",
                    if *running { "*" } else { "" },
                    r,
                    b.west,
                    b.south
                )
            })
            .collect();
        trace.push_str(&format!(
            "{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\n",
            f.t,
            f.centre.x,
            f.centre.y,
            f.zoom_milli,
            f.uncovered,
            f.sent,
            f.missing,
            u8::from(f.drew),
            u8::from(f.busy_wake),
            queue.join(" ")
        ));
    }
    trace.push_str("\nt\tevent\tbounds\tdetail\n");
    for (t, ev) in &walk.events {
        let (kind, b, detail) = match ev {
            Ev::Started(b, z) => ("started", b, format!("z{}-{}", z.0, z.1)),
            Ev::Done(b, name, evicted) => ("done", b, format!("{name} evicted={evicted:?}")),
            Ev::Failed(b, why) => ("failed", b, why.clone()),
            Ev::Skipped(b, why) => ("skipped", b, (*why).to_string()),
            Ev::Held(b, how) => (how.word(), b, String::new()),
        };
        trace.push_str(&format!("{t}\t{kind}\t{},{}\t{detail}\n", b.west, b.south));
    }
    std::fs::write(out.join(format!("trace-{}.tsv", walk.name)), trace).expect("the trace");
}

// ---------------------------------------------------------------------
// The sessions: what a user does.
// ---------------------------------------------------------------------

/// Wheel in step by step from the world to street level, resting at
/// every step (session 1).
pub fn wheel_in_rested(rate: u64, latency: u64, from: u8, to: u8, inject: Inject) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("wheel-rested{tag}"), rate, latency, from, inject);
    let bound = walk.fill_bound();
    for z in from + 1..=to {
        walk.wheel(1000, 14);
        walk.rest(&format!("z{z}"), true);
    }
    walk.drain(bound);
    walk
}

/// The same without waiting: steps faster than loads finish (session 2).
pub fn wheel_in_rushed(rate: u64, latency: u64) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(
        &format!("wheel-rushed{tag}"),
        rate,
        latency,
        4,
        Inject::None,
    );
    let bound = walk.fill_bound();
    for _ in 5..=16 {
        walk.wheel(1000, 4);
    }
    walk.rest("final", true);
    walk.drain(bound);
    walk
}

/// In three steps, out two, in one, repeated (session 3).
pub fn in_out_in(rate: u64, latency: u64, rounds: u32, inject: Inject) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("in-out-in{tag}"), rate, latency, 13, inject);
    let bound = walk.fill_bound();
    walk.rest("start-z13", true);
    for round in 0..rounds {
        for _ in 0..3 {
            walk.wheel(1000, 14);
        }
        walk.rest(&format!("r{round}-in3-z16"), true);
        for _ in 0..2 {
            walk.wheel(-1000, 14);
        }
        walk.rest(&format!("r{round}-out2-z14"), true);
        walk.wheel(1000, 14);
        walk.rest(&format!("r{round}-in1-z15"), true);
        walk.wheel(-2000, 14);
        walk.rest(&format!("r{round}-back-z13"), true);
    }
    walk.drain(bound);
    walk
}

/// Pan across several boxes at a fixed zoom, slowly and fast (session 4).
pub fn pan_slow_fast(rate: u64, latency: u64) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("pan{tag}"), rate, latency, 15, Inject::None);
    let bound = walk.fill_bound();
    walk.rest("start-z15", true);
    // The view goes east: the pointer west, a quarter screen a second.
    walk.drag(-8.0, 0.0, 150);
    walk.rest("slow-east", true);
    walk.drag(-60.0, 0.0, 40);
    walk.rest("fast-east", true);
    walk.drain(bound);
    walk
}

/// Pan while zooming (session 5).
pub fn pan_while_zooming(rate: u64, latency: u64) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("pan-zoom{tag}"), rate, latency, 13, Inject::None);
    let bound = walk.fill_bound();
    for round in 0..3 {
        walk.drag(-30.0, 0.0, 10);
        walk.wheel(700, 8);
        walk.drag(-30.0, 10.0, 10);
        walk.wheel(if round == 2 { 700 } else { -700 }, 8);
        let _ = round;
    }
    walk.rest("after", true);
    walk.drain(bound);
    walk
}

/// Jump far away while loads run, and come back (session 6).
pub fn jump_away_and_back(rate: u64, latency: u64) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("jump{tag}"), rate, latency, 15, Inject::None);
    let bound = walk.fill_bound();
    walk.rest("start-z15", true);
    // Move on so loads run, and jump while they do.
    walk.drag(-40.0, 0.0, 20);
    walk.run(8);
    walk.jump(AREA_B, 15);
    walk.rest("away-z15", true);
    walk.jump(AREA_A, 15);
    walk.rest("back-z15", true);
    walk.drain(bound);
    walk
}

/// A fault that heals (session 7): the source answers 503 while the user
/// wheels in, then serves again; the view must fill untouched.
pub fn recovers_503(rate: u64, latency: u64, heals: bool) -> Walk {
    let mut walk = Walk::new(
        if heals {
            "fault-503"
        } else {
            "fault-503-selftest"
        },
        rate,
        latency,
        13,
        if heals {
            Inject::None
        } else {
            Inject::NeverRecovers
        },
    );
    walk.serve.fail(true);
    walk.wheel(1000, 14);
    walk.wheel(1000, 14);
    walk.rest_bounded("faulty", 6_000, false);
    if heals {
        walk.serve.fail(false);
    }
    walk.fault_cleared();
    walk.await_recovery();
    walk.drain(walk.fill_bound());
    walk
}

/// A link that breaks mid-load and comes back (session 7).
pub fn recovers_drop(rate: u64, latency: u64) -> Walk {
    let mut walk = Walk::new("fault-drop", rate, latency, 14, Inject::None);
    walk.run(20);
    walk.serve.drop_at(Some(walk.serve.sent() + 150_000));
    walk.wheel(1000, 14);
    walk.rest_bounded("faulty", 8_000, false);
    walk.serve.drop_at(None);
    walk.fault_cleared();
    walk.await_recovery();
    walk.drain(walk.fill_bound());
    walk
}

/// A request that stalls and is released (session 7).
pub fn recovers_stall(rate: u64, latency: u64) -> Walk {
    let mut walk = Walk::new("fault-stall", rate, latency, 15, Inject::None);
    walk.serve.hold_at(walk.serve.sent() + 120_000);
    walk.drag(-60.0, 0.0, 30);
    walk.rest_bounded("faulty", 8_000, false);
    walk.serve.release();
    walk.fault_cleared();
    walk.await_recovery();
    walk.drain(walk.fill_bound());
    walk
}

/// A start with a full cache at the cap (session 8): old regions must be
/// evicted by the cap's name, and the view still fills.
pub fn cache_at_cap(rate: u64, latency: u64) -> Walk {
    let mut walk = Walk::with_cache("cache-cap", rate, latency, 15, Inject::None, 3 << 20, 24);
    let bound = walk.fill_bound();
    walk.rest("start-z15", true);
    walk.drag(-40.0, 0.0, 40);
    walk.rest("east", true);
    walk.drain(bound);
    walk
}

/// A detailed visit, then four wheel steps out (1933): the view must
/// fill whole from cuts that carry the view's levels alone, coarse and
/// small, never the street detail of a wide box. The bounds are explicit:
/// what the view needs at 200 kB/s is its own levels over view and
/// margin, about 3 to 4 MB a rest.
pub fn zoom_out_from_visit(rate: u64, latency: u64) -> Walk {
    let mut walk = tiered_walk(
        "zoom-out-visit",
        &[(BIG_BOX, 7..=11), (WIDE_BOX, 12..=15)],
        BIG_BOX,
        AREA_A,
        15,
        rate,
        latency,
    );
    walk.rest_bounded("visit-z15", 60_000, true);
    for _ in 0..4 {
        walk.wheel(-1000, 14);
    }
    walk.rest_bounded("out4-z11", 60_000, true);
    walk.drain(walk.fill_bound());
    walk
}

/// 1933, load walk step "zoom out from a detailed visit": after the
/// visit, four wheel steps out; the view share reaches 1.0 within the
/// bound, and no box cut for it carries more than the three zoom levels
/// above the view zoom it was cut for — its deepest zoom is that view's
/// own, so its span says it. And once the view has settled at z11, no
/// box deeper than z11 starts (the audit of 1933 put this back after
/// 1933 dropped it red): a box queued for a z12-z15 view the reader
/// left would fetch street detail no screen draws, so the loader drops
/// it at the next decision ([`loquax::mapload::Loader::schedule`]).
#[test]
fn zoom_out_from_a_detailed_visit_loads_only_the_view_levels() {
    let walk = zoom_out_from_visit(SLOW_RATE, 40);
    let visit = walk.checks.first().expect("the visit's pause");
    assert!(visit.filled, "the visit itself fills");
    let out = walk.checks.last().expect("the zoom-out's pause");
    eprintln!(
        "SMOKE zoom-out-visit out_filled={} out_took_ms={} sent_bytes={}",
        out.filled,
        out.fill_took_ms,
        walk.serve.sent()
    );
    assert!(out.expects_fill, "the z11 view lies over the archive");
    assert!(
        out.filled,
        "the zoomed-out view fills whole within {} ms (uncovered {:.3})",
        out.fill_bound_ms, out.uncovered
    );
    // No box with more than three zoom levels above the view zoom it
    // was cut for: a box's deepest zoom is that view's (its floor), so
    // its span says it. A queued box starts after the view moved on,
    // which is why the view at its start is not the measure.
    let mut cut = 0;
    for (t, ev) in &walk.events {
        let Ev::Started(b, zooms) = ev else { continue };
        if *t <= visit.t {
            continue;
        }
        cut += 1;
        assert!(
            zooms.1 - zooms.0 <= 3,
            "a box bundles z{}-{} over {b:?}",
            zooms.0,
            zooms.1
        );
    }
    assert!(cut > 0, "the zoom out loaded its coarse levels");
    // The view settled at z11 (its zoom last changed, plus the paced
    // decision that sees it): every box that starts after that carries
    // nothing deeper than the view's own zoom.
    let settled = walk
        .frames
        .windows(2)
        .rev()
        .find(|w| w[0].zoom_milli != w[1].zoom_milli)
        .map(|w| w[1].t)
        .unwrap_or(0)
        + loquax::ui::viewload::DECIDE_MS;
    let deep: Vec<String> = walk
        .events
        .iter()
        .filter_map(|(t, ev)| match ev {
            Ev::Started(b, zooms) if *t > settled && zooms.1 > 11 => {
                Some(format!("t={t} z{}-{} {b:?}", zooms.0, zooms.1))
            }
            _ => None,
        })
        .collect();
    assert!(
        deep.is_empty(),
        "a box cut for a view the reader left starts under the z11 view:\n{}",
        deep.join("\n")
    );
}

/// 1933, load walk step "wheel over loaded ground", as the audit of
/// 1933 pinned it down: the ground is what ONE rest at ONE zoom loads
/// (a drained rest at every rung turned "a region is loaded" into
/// "every zoom was loaded"). One wheel step in over that ground fetches
/// only levels deeper than the ground's own — never z12 and below
/// again — and once that step's levels landed, ten wheel steps between
/// the two rungs cut no box and fetch 0 bytes.
#[test]
fn wheel_steps_over_loaded_ground_fetch_nothing() {
    let mut walk = tiered_walk(
        "wheel-loaded",
        &[(BIG_BOX, 7..=11), (WIDE_BOX, 12..=15)],
        BIG_BOX,
        AREA_A,
        12,
        SLOW_RATE,
        40,
    );
    walk.rest_bounded("z12", 60_000, true);
    walk.drain(walk.fill_bound());
    let ground = walk.now_ms();
    // One step in: only levels the ground does not hold come down.
    walk.wheel(1000, 14);
    walk.rest_bounded("z13", 60_000, true);
    walk.drain(walk.fill_bound());
    let refetch: Vec<String> = walk
        .events
        .iter()
        .filter_map(|(t, ev)| match ev {
            Ev::Started(b, z) if *t > ground && z.0 <= 12 => {
                Some(format!("t={t} z{}-{} {b:?}", z.0, z.1))
            }
            _ => None,
        })
        .collect();
    // Ten steps between the rungs the ground now holds: nothing moves.
    let t0 = walk.now_ms();
    let sent0 = walk.serve.sent();
    for milli in [
        -1000, 1000, -1000, 1000, -1000, 1000, -1000, 1000, -1000, 1000,
    ] {
        walk.wheel(milli, 14);
    }
    walk.run(30);
    let cut: Vec<String> = walk
        .events
        .iter()
        .filter(|(t, ev)| *t > t0 && matches!(ev, Ev::Started(..)))
        .map(|(t, ev)| match ev {
            Ev::Started(b, z) => format!("t={t} z{}-{} {b:?}", z.0, z.1),
            _ => String::new(),
        })
        .collect();
    let sent = walk.serve.sent() - sent0;
    eprintln!(
        "SMOKE wheel-loaded wheel_bytes={sent} wheel_cuts={} step_in_refetches={}",
        cut.len(),
        refetch.len()
    );
    assert!(
        refetch.is_empty(),
        "the step in fetched levels the ground already holds:\n{}",
        refetch.join("\n")
    );
    assert!(
        cut.is_empty(),
        "wheel steps over loaded ground cut boxes:\n{}",
        cut.join("\n")
    );
    assert_eq!(sent, 0, "wheel steps over loaded ground fetched bytes");
    for c in &walk.checks {
        assert!(c.filled, "{} fills before the wheel phase", c.label);
    }
}

/// The audit of 1933, its deciding test: after one rest at z12 has
/// landed its ground, one wheel step in cuts only boxes whose levels
/// are all deeper than the ground's — z9 to z12 never come down a
/// second time ([`loquax::ui::viewload`] clips a cut's shallow end to
/// the levels held).
#[test]
fn zoom_in_one_step_cuts_no_level_already_held() {
    let mut walk = tiered_walk(
        "wheel-in-held",
        &[(BIG_BOX, 7..=11), (WIDE_BOX, 12..=15)],
        BIG_BOX,
        AREA_A,
        12,
        SLOW_RATE,
        40,
    );
    walk.rest_bounded("z12", 60_000, true);
    walk.drain(walk.fill_bound());
    let t0 = walk.now_ms();
    let sent0 = walk.serve.sent();
    walk.wheel(1000, 14);
    walk.rest_bounded("z13", 60_000, true);
    walk.drain(walk.fill_bound());
    let again: Vec<String> = walk
        .events
        .iter()
        .filter_map(|(t, ev)| match ev {
            Ev::Started(b, z) if *t > t0 && z.0 <= 12 => {
                Some(format!("t={t} z{}-{} {b:?}", z.0, z.1))
            }
            _ => None,
        })
        .collect();
    eprintln!(
        "SMOKE wheel-in-held step_bytes={} started_shallow={}",
        walk.serve.sent() - sent0,
        again.len()
    );
    assert!(
        again.is_empty(),
        "a zoom-in step fetched levels already held:\n{}",
        again.join("\n")
    );
}

/// 1933, the measurement standing for Morocco at z6: a zoomed-out view
/// over wide coarse ground fills whole, coarse first, at 200 kB/s. The
/// SMOKE line quotes the bytes and the time to full share.
#[test]
fn a_zoomed_out_view_fills_whole_coarse_first() {
    let mut walk = tiered_walk(
        "morocco-z6",
        &[(MOROCCO_BOX, 3..=6)],
        MOROCCO_BOX,
        (31.5, -14.0),
        6,
        SLOW_RATE,
        40,
    );
    walk.rest_bounded("z6", 60_000, true);
    let c = walk.checks.last().expect("the pause");
    eprintln!(
        "SMOKE morocco-z6 filled={} took_ms={} bound_ms={} sent_bytes={} uncovered={:.3}",
        c.filled,
        c.fill_took_ms,
        c.fill_bound_ms,
        walk.serve.sent(),
        c.uncovered
    );
    assert!(
        c.filled,
        "a z6 view fills whole (uncovered {:.3} after {} ms, {} bytes)",
        c.uncovered,
        c.fill_bound_ms,
        walk.serve.sent()
    );
}

/// The patchwork store (order 1943): `count` overlapping cuts of the
/// walk's archive with mixed zoom spans, named `auto-patch-*`, written
/// synchronously into `maps` before the first frame — the shape of
/// Lew's 94-region store of 2026-11-16, which the single-fixture
/// sessions could not see. One line names the store's size.
fn cut_patchwork(maps: &std::path::Path, count: usize) {
    cut_store(
        maps,
        count,
        &wide_archive(),
        &[(10, 15), (11, 14), (12, 15), (13, 15)],
        &WIDE_BOX,
        1,
    );
}

/// `count` overlapping cuts of `archive` with the zoom `spans` in turn,
/// inside `area`, their boxes `scale` times the patchwork's
/// ([`cut_patchwork`]), named `auto-patch-*`.
fn cut_store(
    maps: &std::path::Path,
    count: usize,
    archive: &[u8],
    spans: &[(u8, u8)],
    area: &Bounds,
    scale: i32,
) {
    use leviculum_mapcut::load::{plan, Loading, Want};
    use leviculum_mapcut::pmtiles::Archive;
    let source = maps.join("patchwork.pmtiles");
    std::fs::write(&source, archive).expect("the archive");
    let archive = Archive::open(&source).expect("open");
    let widths: [i32; 4] = [3_000_000, 2_200_000, 1_400_000, 1_800_000];
    let heights: [i32; 4] = [1_400_000, 1_000_000, 800_000, 1_200_000];
    let mut bytes = 0u64;
    for i in 0..count {
        let (z0, z1) = spans[i % spans.len()];
        let (bw, bh) = (
            widths[i % widths.len()] * scale,
            heights[(i / 2) % heights.len()] * scale,
        );
        let west = (area.west + i as i32 * 170_000 * scale).min(area.east - bw);
        let south = (area.south + ((i as i32 * 3) % 5) * 120_000 * scale).min(area.north - bh);
        let want = Want {
            bounds: Bounds {
                west,
                south,
                east: west + bw,
                north: south + bh,
            },
            zooms: z0..=z1,
            floor: 0,
            cap: 64 << 20,
            profile: leviculum_map::profile::CUT_2,
            locale: None,
            buildings: false,
            pois: false,
        };
        let planned = plan(&archive, &want).expect("plan");
        let path = maps.join(format!("auto-patch-{i:02}.lmr"));
        let out = std::fs::File::create(&path).expect("create");
        let mut loading = Loading::begin(&archive, &want, planned, "test", 1, out).expect("begin");
        while loading.step().expect("round").is_some() {}
        loading.finish().expect("finish");
        bytes += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    }
    std::fs::remove_file(&source).expect("the archive goes");
    eprintln!("SMOKE patchwork store regions={count} bytes={bytes}");
}

/// A desktop pane over a store of many overlapping regions with mixed
/// spans (order 1943): zoom 10 lies in this pane's blend window, so the
/// whole-frame curved path carries the store, as it did on Lew's
/// screen. The view reaches past the archive's box, so no fill is
/// expected and the autoload is off: the session judges drawing — a
/// drag at the curved zoom, at the tile zoom above, and the wheel
/// between them — by `input_never_waits` and `input_rides_the_memo`.
pub fn patchwork_sized(name: &str, count: usize, inject: Inject) -> Walk {
    let mut walk = patchwork_store(name, count, inject);
    walk.drag(-40.0, 0.0, 25);
    walk.rest_bounded("panned", 1_500, false);
    walk.wheel(1000, 14);
    walk.rest_bounded("z11", 1_500, false);
    walk.drag(-40.0, 10.0, 20);
    walk.wheel(-2000, 14);
    walk.rest_bounded("out-z9", 1_500, false);
    assert_curved(&walk, inject);
    walk
}

/// The patchwork session under loading (audit of 1943): regions land
/// and tiles arrive under every drag and wheel step ([`Walk::churn`]),
/// at the curved zoom and the tile zoom above it. Its moving frames are
/// kept as pictures: a half-screen drag at z10 and a wheel step from
/// z10 to z9 in its glide.
pub fn patchwork_loading() -> Walk {
    let mut walk = patchwork_store("patchwork-loading", 16, Inject::None);
    walk.churn = true;
    // 23 moves of 40 px: half the 1800 px pane.
    walk.drag(-40.0, 0.0, 23);
    walk.shoot("half-screen-drag-z10");
    walk.rest_bounded("panned", 1_500, false);
    walk.wheel(-1000, 4);
    walk.shoot("wheel-z10-to-z9");
    walk.run(14);
    walk.rest_bounded("out-z9", 1_500, false);
    walk.drag(-40.0, 10.0, 20);
    walk.rest_bounded("panned-z9", 1_500, false);
    walk.wheel(2000, 14);
    walk.rest_bounded("z11", 1_500, false);
    walk.drag(40.0, -10.0, 20);
    walk.rest_bounded("panned-z11", 1_500, false);
    walk.wheel(-1000, 14);
    walk.rest_bounded("back-z10", 1_500, false);
    assert_curved(&walk, Inject::None);
    walk
}

/// The session is about the curved path: a change that kept zoom 10
/// out of this pane's blend window would judge nothing.
fn assert_curved(walk: &Walk, inject: Inject) {
    let stats = {
        let frames = walk
            .app
            .frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        frames.tile_stats()
    };
    assert!(
        stats.curved_drawn > 0,
        "the patchwork session never drew a curved frame"
    );
    assert!(
        stats.curved_blit > 0 || inject != Inject::None,
        "no moving frame rode the memo: the drags stayed outside the blend"
    );
}

/// The patchwork store written and the view settled at z10 over it,
/// the autoload off ([`patchwork_sized`]); [`Inject::NoMemo`] turns the
/// curved memo off.
fn patchwork_store(name: &str, count: usize, inject: Inject) -> Walk {
    patchwork_store_at(name, count, inject, (1800, 1320))
}

/// [`patchwork_store`] on a pane of `size`.
fn patchwork_store_at(name: &str, count: usize, inject: Inject, size: (u32, u32)) -> Walk {
    let serve = Serve::start(vec![("w.pmtiles".into(), wide_archive())]).expect("serve");
    let at = project(LatLon::from_degrees(AREA_A.0, AREA_A.1));
    let ground = Ground {
        at,
        zoom: 10,
        cache_bytes: 1 << 30,
        prefill: 0,
        size,
    };
    let mut walk = Walk::over(serve, name, 0, 0, ground, inject);
    let dir = walk.maps.dir().expect("the maps directory").to_path_buf();
    cut_patchwork(&dir, count);
    walk.maps.changed();
    let mut seen = walk.app.seen.clone();
    seen.autoload = false;
    walk.app.apply(Message::Seen(Box::new(seen)));
    if inject == Inject::NoMemo {
        walk.app
            .frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_curved_memo(false);
    }
    walk.rest_bounded("settled-z10", 1_500, false);
    walk
}

/// The settle draw's bound per megapixel of the pane (audit of 1943,
/// order 1943 point 3): a settled view at z8 to z11 over overlapping
/// regions, every cache warm but the view itself new, is drawn whole
/// in at most this many milliseconds a megapixel, 95 ms on the
/// 1800x1320 pane. Lew measured the freeze at about 1 s for a z10 frame
/// on his 1440p desktop; 1943's fixes brought it to 11 ms warm. The
/// walk measures 9 to 15 ms released (report of order 1967); the bound
/// keeps about six times that for a shared, loaded test host.
pub const SETTLE_DRAW_MS_PER_MP: f64 = 40.0;

/// A settled view at each of z8, z9, z10 and z11 over a store of
/// overlapping regions whose spans reach those zooms (audit of 1943):
/// each zoom is visited once to warm the caches, then a view 64 px
/// east at the same zoom rests, and its longest step — the draw at
/// rest, inside the step as the window's worker draws — is kept in
/// [`Walk::settles`] for `settle_draw_in_bound`.
pub fn settle_ladder() -> Walk {
    let tiers = [(BIG_BOX, 7..=11), (WIDE_BOX, 12..=15)];
    let serve = Serve::start(vec![("w.pmtiles".into(), tiered(&tiers))]).expect("serve");
    // The store's middle: its cuts lie about 12.0 to 13.6 E, 51.7 to
    // 52.2 N.
    let at = (51.95, 12.8);
    let ground = Ground {
        at: project(LatLon::from_degrees(at.0, at.1)),
        zoom: 8,
        cache_bytes: 1 << 30,
        prefill: 0,
        size: (1800, 1320),
    };
    let mut walk = Walk::over(serve, "settle-ladder", 0, 0, ground, Inject::None);
    let dir = walk.maps.dir().expect("the maps directory").to_path_buf();
    cut_store(
        &dir,
        16,
        &tiered(&tiers),
        &[(7, 11), (8, 12), (9, 13), (10, 15)],
        &BIG_BOX,
        3,
    );
    walk.maps.changed();
    let mut seen = walk.app.seen.clone();
    seen.autoload = false;
    walk.app.apply(Message::Seen(Box::new(seen)));
    for zoom in 8..=11u8 {
        walk.jump(at, zoom);
        walk.rest_bounded(&format!("warm-z{zoom}"), 3_000, false);
    }
    for zoom in 8..=11u8 {
        // 64 px east at this zoom: a view no memo holds, over regions
        // every cache has read.
        let middle = project(LatLon::from_degrees(at.0, at.1));
        let east = leviculum_map::mercator::unproject(World {
            x: middle.x + (64u32 << (24 - u32::from(zoom))),
            y: middle.y,
        });
        let to = (f64::from(east.lat_e7) / 1e7, f64::from(east.lon_e7) / 1e7);
        walk.jump(to, zoom);
        let mut longest = 0u64;
        for _ in 0..60 {
            walk.step();
            longest = longest.max(walk.last_step_ms);
        }
        walk.settles.push((zoom, longest));
        walk.rest_bounded(&format!("settled-z{zoom}"), 3_000, false);
    }
    walk
}

/// The patchwork session of the quick and full sets.
pub fn patchwork() -> Walk {
    patchwork_sized("patchwork", 16, Inject::None)
}

/// The sessions of a set: `short` is the per-batch `cargo test` default,
/// `quick` what fits two minutes released, `full` everything.
/// A desert place loaded as a user does (1868, the audit of 1863): the
/// view rests over the place at z13 until it fills, the loader brings
/// the region and then the height pack of the block under it, the view
/// rests again with the pack drawn, and the relief is measured against
/// the same view drawn without it. One SMOKE line a step for the report.
pub fn desert_rest(place: &Desert, rate: u64, latency: u64, inject: Inject) -> Walk {
    let mut walk = Walk::desert(place, rate, latency, 13, inject);
    let bound = walk.fill_bound();
    walk.rest("z13", true);
    walk.drain(bound);
    walk.wheel(1000, 14);
    walk.rest("z14", true);
    walk.drain(bound);
    walk.relief();
    for (t, ev) in &walk.events {
        if let Ev::Done(b, name, _) = ev {
            eprintln!(
                "SMOKE {} region t_ms={t} name={name} box={},{},{},{}",
                place.name, b.west, b.south, b.east, b.north
            );
        }
    }
    for (t, _, r) in &walk.heights {
        eprintln!("SMOKE {} heights t_ms={t} result={r:?}", place.name);
    }
    eprintln!(
        "SMOKE {} relief_share={:.3} sent_bytes={}",
        place.name,
        walk.relief.unwrap_or(0.0),
        walk.serve.sent()
    );
    walk
}

pub fn run_set(set: &str) -> Vec<(Walk, Vec<Verdict>)> {
    let mut walks: Vec<Walk> = Vec::new();
    match set {
        // The per-batch set runs in every `cargo test` and must be
        // green on its own: no revisit sessions here while the painter's
        // same-view finding (1811, the report's mechanism 2) is open;
        // `quick` and `full` keep them and are the measuring tool.
        "short" => {
            walks.push(wheel_in_rested(0, 0, 12, 14, Inject::None));
            walks.push(pan_slow_fast(0, 0));
        }
        "quick" => {
            walks.push(wheel_in_rested(0, 0, 4, 16, Inject::None));
            walks.push(in_out_in(0, 0, 2, Inject::None));
            walks.push(pan_slow_fast(0, 0));
            walks.push(desert_rest(&ERG_CHEBBI, 0, 0, Inject::None));
            walks.push(desert_rest(&BANI, 0, 0, Inject::None));
            walks.push(patchwork());
            walks.push(patchwork_loading());
            walks.push(settle_ladder());
        }
        "full" => {
            walks.push(wheel_in_rested(0, 0, 4, 16, Inject::None));
            walks.push(wheel_in_rushed(0, 0));
            walks.push(in_out_in(0, 0, 3, Inject::None));
            walks.push(pan_slow_fast(0, 0));
            walks.push(pan_while_zooming(0, 0));
            walks.push(jump_away_and_back(0, 0));
            walks.push(wheel_in_rested(SLOW_RATE, 40, 12, 16, Inject::None));
            walks.push(in_out_in(SLOW_RATE, 40, 1, Inject::None));
            walks.push(pan_slow_fast(SLOW_RATE, 40));
            walks.push(recovers_503(0, 0, true));
            walks.push(recovers_drop(0, 0));
            walks.push(recovers_stall(0, 0));
            walks.push(cache_at_cap(0, 0));
            walks.push(desert_rest(&ERG_CHEBBI, 0, 0, Inject::None));
            walks.push(desert_rest(&BANI, 0, 0, Inject::None));
            walks.push(patchwork());
            walks.push(patchwork_loading());
            walks.push(settle_ladder());
        }
        // One session alone, for a look at it (1812).
        "patchwork" => {
            walks.push(patchwork());
        }
        "patchwork-loading" => {
            walks.push(patchwork_loading());
        }
        "settle-ladder" => {
            walks.push(settle_ladder());
        }
        "wheel-slow" => {
            walks.push(wheel_in_rested(SLOW_RATE, 40, 12, 16, Inject::None));
        }
        other => panic!("no load walk set named {other}"),
    }
    walks
        .into_iter()
        .map(|w| {
            let verdicts = rules(&w);
            (w, verdicts)
        })
        .collect()
}

/// Run `set`, write rows and pictures when `LOQUAX_LOAD_WALK` names a
/// directory, and return the failures.
pub fn run_and_report(set: &str) -> Vec<String> {
    let out = std::env::var_os("LOQUAX_LOAD_WALK").map(std::path::PathBuf::from);
    let mut rows = String::new();
    let mut failed = Vec::new();
    for (walk, verdicts) in run_set(set) {
        for v in &verdicts {
            eprintln!(
                "load-walk {} {}: {} ({}; limit {}) {}",
                v.session,
                v.rule,
                if v.pass { "pass" } else { "FAIL" },
                v.value,
                v.limit,
                v.note
            );
            if !v.pass {
                failed.push(format!(
                    "{} {}: {} (limit {}) {}",
                    v.session, v.rule, v.value, v.limit, v.note
                ));
            }
        }
        write_out(&walk, &verdicts, out.as_deref(), &mut rows);
    }
    if let Some(out) = &out {
        std::fs::create_dir_all(out).expect("the out directory");
        std::fs::write(out.join(format!("rows-load-{set}.tsv")), &rows).expect("the rows");
    }
    failed
}

/// The selftest's cases: each walk-level fault and the rule it must turn
/// red (1811). The cheap first one runs in every `cargo test`.
pub fn selftest_cases() -> Vec<(&'static str, Inject, &'static str)> {
    vec![
        ("drop-want", Inject::DropWant, "view_filled_at_rest"),
        ("ahead-first", Inject::AheadFirst, "screen_first"),
        ("stuck-load", Inject::StuckLoad, "nothing_lost"),
        ("double-ask", Inject::DoubleAsk, "no_spin"),
        ("delete-region", Inject::DeleteRegion, "no_regress"),
        ("never-recovers", Inject::NeverRecovers, "fault_recovers"),
        ("blank-frame", Inject::BlankFrame, "frame_never_blank"),
        (
            "perturb-picture",
            Inject::PerturbPicture,
            "same_view_same_picture",
        ),
        ("busy-wake", Inject::BusyWake, "no_busy_wake"),
        ("no-heights", Inject::NoHeights, "pack_after_region"),
        ("no-heights", Inject::NoHeights, "relief_drawn"),
        (
            "hide-treatment",
            Inject::HideTreatment,
            "loading_is_visible",
        ),
        (
            "fake-progress",
            Inject::FakeProgress,
            "progress_moves_with_bytes",
        ),
        (
            "stale-treatment",
            Inject::StaleTreatment,
            "no_treatment_when_complete",
        ),
        ("idle-ticks", Inject::IdleTicks, "idle_draws_nothing"),
        ("slow-frame", Inject::SlowFrame, "input_never_waits"),
        ("no-memo", Inject::NoMemo, "input_rides_the_memo"),
    ]
}

/// One selftest case run: a small session with the fault injected.
pub fn selftest_walk(inject: Inject) -> Walk {
    match inject {
        Inject::StuckLoad => {
            let mut walk = Walk::new("selftest-stuck", 0, 0, 13, Inject::None);
            walk.serve.hold_at(walk.serve.sent() + 60_000);
            walk.wheel(1000, 14);
            walk.rest_bounded("held", 4_000, false);
            let frames = (2 * STALL_MS / 16) as u32;
            walk.run(frames);
            walk
        }
        Inject::NeverRecovers => recovers_503(0, 0, false),
        // 1812: a slow source, so the view rests while it lacks data and
        // the treatment's rules have frames to judge.
        Inject::HideTreatment
        | Inject::FakeProgress
        | Inject::StaleTreatment
        | Inject::IdleTicks => {
            let mut walk = Walk::new("selftest-treat", SLOW_RATE, 40, 12, inject);
            let bound = walk.fill_bound();
            walk.wheel(1000, 14);
            walk.rest_bounded("z13", bound, true);
            walk.run(40);
            walk.wheel(1000, 14);
            walk.rest_bounded("z14", bound, true);
            walk
        }
        Inject::NoHeights => desert_rest(&ERG_CHEBBI, 0, 0, Inject::NoHeights),
        // 1943: the small patchwork, dragged at the curved zoom.
        Inject::SlowFrame | Inject::NoMemo => patchwork_sized("selftest-patch", 6, inject),
        Inject::DeleteRegion | Inject::PerturbPicture => {
            let mut walk = Walk::new(
                match inject {
                    Inject::DeleteRegion => "selftest-delete",
                    _ => "selftest-perturb",
                },
                0,
                0,
                13,
                inject,
            );
            let bound = walk.fill_bound();
            walk.rest("first-z13", true);
            walk.wheel(1000, 14);
            walk.rest_bounded("z14", bound, true);
            walk.wheel(-1000, 14);
            walk.rest("second-z13", true);
            walk
        }
        inject => {
            let mut walk = Walk::new("selftest", 0, 0, 12, inject);
            let bound = match inject {
                Inject::DropWant => 4_000,
                _ => walk.fill_bound(),
            };
            walk.wheel(1000, 14);
            walk.rest_bounded("z13", bound, true);
            walk.wheel(1000, 14);
            walk.rest_bounded("z14", bound, true);
            walk.drain(6_000);
            walk
        }
    }
}

/// The load walk, as `just load-walk` and the per-batch `cargo test` run
/// it: `LOQUAX_LOAD_WALK_SET` picks the sessions (default `short`), a
/// red rule fails the test after writing its rows.
#[test]
fn the_load_walk_holds_its_rules() {
    let set = std::env::var("LOQUAX_LOAD_WALK_SET").unwrap_or_else(|_| "short".to_string());
    let failed = run_and_report(&set);
    assert!(
        failed.is_empty(),
        "the load walk's rules failed:\n{}",
        failed.join("\n")
    );
}

/// The positive controls: each fault injected into the walk itself must
/// turn its rule red. `cargo test` runs the cheapest case;
/// `LOQUAX_LOAD_WALK_SELFTEST=1` runs them all (`just load-walk
/// --selftest`).
#[test]
fn the_load_walk_selftest_fires_every_rule() {
    let which = std::env::var("LOQUAX_LOAD_WALK_SELFTEST").ok();
    let out = std::env::var_os("LOQUAX_LOAD_WALK").map(std::path::PathBuf::from);
    let cases = selftest_cases();
    // Unset runs the cheapest case; `1` runs them all; a case's name
    // runs its cases alone (order 1943: a new rule's control, run by
    // hand, fits the per-batch clock).
    let cases = match which.as_deref() {
        None => cases[..1].to_vec(),
        Some("1") => cases,
        Some(name) => cases.into_iter().filter(|(n, _, _)| *n == name).collect(),
    };
    let mut rows = String::new();
    let mut wrong = Vec::new();
    for (name, inject, rule) in cases {
        let walk = selftest_walk(inject);
        let verdicts = rules(&walk);
        let fired = verdicts.iter().any(|v| v.rule == rule && !v.pass);
        eprintln!(
            "load-walk selftest {name}: {rule} {}",
            if fired { "fired" } else { "DID NOT FIRE" }
        );
        rows.push_str(&format!(
            "load\tselftest\t{name}\t{}x{}\t\t\t\t{rule}\t{}\t1.000\t{}\tthe injected fault must turn its rule red\n",
            WALK_SIZE.0,
            WALK_SIZE.1,
            u8::from(fired),
            if fired { "pass" } else { "FAIL" }
        ));
        if !fired {
            wrong.push(format!("{name}: {rule} did not fire"));
        }
    }
    if let Some(out) = &out {
        std::fs::create_dir_all(out).expect("the out directory");
        std::fs::write(out.join("rows-load-selftest.tsv"), &rows).expect("the rows");
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// One scene of 1812's comparison: where, how deep, which map, which
/// screen.
struct LooksScene {
    name: &'static str,
    /// The source: the walk's synthetic city, the Erg Chebbi desert, or a
    /// real archive's bytes at its place.
    ground: LooksGround,
    zoom: u8,
    dark: bool,
    /// Logical width and height, and screen pixels a logical one.
    size: (f32, f32, f32),
    /// A region installed before the walk starts, its file's bytes: the
    /// data a coarser or a neighbouring load leaves on screen, which the
    /// treatment must not hide. Empty for none.
    standin: &'static [u8],
}

#[derive(Clone, Copy)]
enum LooksGround {
    Synthetic,
    Desert,
    Real(&'static [u8], (f64, f64)),
}

/// The Protomaps sample's Oltrarno, south of the Arno in Florence: a city.
const OLTRARNO: &[u8] = include_bytes!("../../../leviculum-mapcut/fixtures/oltrarno.pmtiles");
/// The Grunewald forest in the west of Berlin: open land with paths.
const GRUNEWALD: &[u8] = include_bytes!("../../../leviculum-mapcut/fixtures/grunewald.pmtiles");
/// Central Florence, z11 to z15, north of the Oltrarno.
const FIRENZE: &[u8] = include_bytes!("../../../leviculum-map/fixtures/firenze.lmr");

impl Walk {
    /// A walk over a real archive's `bytes` at `at`, the source at
    /// `rate` bytes a second (1812's comparison pictures).
    pub fn real(name: &str, bytes: &[u8], at: (f64, f64), rate: u64, zoom: u8) -> Self {
        let serve = Serve::start(vec![("w.pmtiles".into(), bytes.to_vec())]).expect("serve");
        let ground = Ground {
            at: project(LatLon::from_degrees(at.0, at.1)),
            zoom,
            cache_bytes: 1 << 30,
            prefill: 0,
            size: WALK_SIZE,
        };
        Self::over(serve, name, rate, 40, ground, Inject::None)
    }
}

/// The screen's pixels of a frame's `pixels` drawn as `frame`, cut to
/// `screen` (as [`crop_screen`] does).
fn crop_pixels(pixels: &[u8], frame: &ExactView, screen: &ExactView) -> Vec<u8> {
    let (fw, fh) = frame.size();
    let (x0, y0, _) = loquax::mapframe::onto(frame, screen);
    let (w, h) = screen.size();
    let mut crop = Vec::with_capacity((w * h * 4) as usize);
    for sy in 0..h {
        for sx in 0..w {
            let fx = (f64::from(sx) - x0).round() as i64;
            let fy = (f64::from(sy) - y0).round() as i64;
            if (0..i64::from(fw)).contains(&fx) && (0..i64::from(fh)).contains(&fy) {
                let i = ((fy as u32 * fw + fx as u32) * 4) as usize;
                crop.extend_from_slice(&pixels[i..i + 4]);
            } else {
                crop.extend_from_slice(&[0xff, 0x00, 0xff, 0xff]);
            }
        }
    }
    crop
}

fn write_png(path: &std::path::Path, rgba: &[u8], (w, h): (u32, u32)) {
    let mut png = Vec::new();
    let mut e = png::Encoder::new(&mut png, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let mut writer = e.write_header().expect("header");
    writer.write_image_data(rgba).expect("data");
    writer.finish().expect("finish");
    std::fs::write(path, png).expect("the picture");
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// The window variant's frame gate (audit of 1943): a moving frame of
/// the desktop map, drawn on the window's path inside the blend window
/// (the memo ridden, the names placed, the loads polled), within one
/// frame of a 30 Hz display. The window shows the last frame until the
/// next lands and draws on its own worker. Measured released on the
/// shared coder host while other lanes built (order 1967): p50 3 to
/// 9 ms, single steps up to 26 ms, varying run to run with the host's
/// load; a 60 Hz gate (16.7 ms) failed one run in two on one step, so
/// it would judge the host, not the map.
pub const WINDOW_FRAME_MS: f64 = 1000.0 / 30.0;

/// One window-variant row in the render walk's columns (variant, kind,
/// sequence, size, centre, zoom, image, rule, value, limit, verdict,
/// note).
#[allow(clippy::too_many_arguments)]
fn window_row(
    rows: &mut String,
    failed: &mut Vec<String>,
    sequence: &str,
    size: &str,
    zoom: f64,
    image: &str,
    rule: &str,
    value: f64,
    limit: f64,
    note: &str,
) {
    let pass = value <= limit;
    if !pass {
        failed.push(format!(
            "{sequence} {size} {image} {rule}: {value:.3} > {limit:.3} {note}"
        ));
    }
    rows.push_str(&format!(
        "window\tframe\t{sequence}\t{size}\tpatchwork\t{zoom:.3}\t{image}\t{rule}\t{value:.3}\t{limit:.3}\t{}\t{note}\n",
        if pass { "pass" } else { "FAIL" }
    ));
}

/// The render walk's desktop `window` map variant (audit of 1943): the
/// window's own path (the App, its tile cache with the curved memo on,
/// headless), over the patchwork store, at the desktop pane and at
/// 1440p, with zoom 10 inside the blend window. Sequences: a pan, a
/// wheel step out (z10 to z9) and back in, and the pan again while
/// regions land under it. Rules: `frame_p95` over the moving frames
/// ([`WINDOW_FRAME_MS`]), `input_rides_the_memo` (no whole curved draw
/// while a finger drags), and per kept picture `view_covered` (no pixel
/// the frame does not reach) and `sea_strip` (no more of the screen in
/// the theme's water colour than the same view at rest shows). Rows go
/// to `rows-window-map.tsv`, pictures to `full/`, under `out`.
pub fn window_variant(out: &std::path::Path) -> Vec<String> {
    let full = out.join("full");
    std::fs::create_dir_all(&full).expect("the pictures directory");
    let mut rows = String::new();
    let mut failed = Vec::new();
    for (size, px, moves) in [
        ("1800x1320", (1800, 1320), 23u32),
        ("2560x1440", (2560, 1440), 32),
    ] {
        let mut walk = patchwork_store_at(&format!("window-{size}"), 16, Inject::None, px);
        // The walk's window draws the light map.
        let water = leviculum_mapfill::frame::rgb888(leviculum_map::outline::water(
            leviculum_mapfill::style::MapTheme::Light,
        ));
        let share_of = |rgba: &[u8], c: [u8; 3]| {
            let n = rgba.chunks_exact(4).filter(|p| p[..3] == c).count();
            n as f64 / (rgba.len() / 4) as f64
        };
        let sequence = |walk: &mut Walk,
                        name: &str,
                        play: &mut dyn FnMut(&mut Walk),
                        rows: &mut String,
                        failed: &mut Vec<String>| {
            walk.step_us.clear();
            walk.shots.clear();
            let drawn_before = walk.curved_drawn_now();
            walk.shoot_every = Some((4, format!("window-{name}-{size}")));
            play(walk);
            walk.shoot_every = None;
            let moving_ms: Vec<f64> = walk.step_us.iter().map(|&us| us as f64 / 1e3).collect();
            eprintln!("WINDOW_STEPS {name} {size} {moving_ms:.1?}");
            let drew = walk.drag_curved_drawn;
            let drawn_moving = walk.curved_drawn_now() - drawn_before;
            walk.run(SETTLE_STEPS);
            walk.shoot(&format!("window-{name}-{size}-rest"));
            let zoom = f64::from(walk.app.view.zoom_milli()) / 1000.0;
            let mut sorted = moving_ms.clone();
            sorted.sort_by(f64::total_cmp);
            let q = |f: f64| {
                sorted
                    .get(((sorted.len().max(1) - 1) as f64 * f).round() as usize)
                    .copied()
                    .unwrap_or(0.0)
            };
            window_row(
                rows,
                failed,
                name,
                size,
                zoom,
                "",
                "frame_p95",
                q(0.95),
                WINDOW_FRAME_MS,
                &format!(
                    "p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms, {} frames, target 30 Hz, one frame {:.1} ms; {drawn_moving} whole curved draws among them",
                    q(0.5),
                    q(0.95),
                    q(1.0),
                    sorted.len(),
                    WINDOW_FRAME_MS
                ),
            );
            window_row(
                rows,
                failed,
                name,
                size,
                zoom,
                "",
                "input_rides_the_memo",
                drew as f64,
                0.0,
                "whole curved draws while a finger dragged, this walk so far",
            );
            let rest = walk
                .shots
                .last()
                .map(|(_, c, _)| share_of(c, water))
                .unwrap_or(0.0);
            let (w, h) = walk.app.view.size();
            for (label, crop, bare) in &walk.shots {
                let zoom = f64::from(walk.app.view.zoom_milli()) / 1000.0;
                write_png(&full.join(format!("{label}.png")), crop, (w, h));
                // A wheel's glide scales the frame of the view it ends on
                // towards the view (`App::order`): stepping in, that
                // frame lies inside the screen and the ground shows
                // round it by design (1441), so only pans and rests are
                // held to full cover.
                if !name.starts_with("wheel") || label.ends_with("-rest") {
                    window_row(
                        rows,
                        failed,
                        name,
                        size,
                        zoom,
                        label,
                        "view_covered",
                        *bare,
                        0.0,
                        "share of the screen the frame does not reach",
                    );
                }
                window_row(
                    rows,
                    failed,
                    name,
                    size,
                    zoom,
                    label,
                    "sea_strip",
                    share_of(crop, water),
                    rest + 0.02,
                    "share in the theme's water colour, at most the rest's and 2 %",
                );
            }
        };
        sequence(
            &mut walk,
            "pan",
            &mut |w| w.drag(-40.0, 0.0, moves),
            &mut rows,
            &mut failed,
        );
        sequence(
            &mut walk,
            "wheel-out",
            &mut |w| {
                w.wheel(-1000, 2);
                for i in 0..3 {
                    w.shoot(&format!("window-wheel-out-{size}-f{i:03}"));
                    w.wheel_steps_run(2);
                }
            },
            &mut rows,
            &mut failed,
        );
        sequence(
            &mut walk,
            "wheel-in",
            &mut |w| {
                w.wheel(1000, 2);
                for i in 0..3 {
                    w.shoot(&format!("window-wheel-in-{size}-f{i:03}"));
                    w.wheel_steps_run(2);
                }
            },
            &mut rows,
            &mut failed,
        );
        walk.churn = true;
        sequence(
            &mut walk,
            "pan-loading",
            &mut |w| w.drag(40.0, 0.0, moves),
            &mut rows,
            &mut failed,
        );
        walk.churn = false;
        assert_curved(&walk, Inject::None);
    }
    std::fs::write(out.join("rows-window-map.tsv"), rows).expect("the rows");
    failed
}

/// The render walk's window variant, written when `LOQUAX_RENDER_WALK`
/// names a directory (`just render-walk` runs it released); without it
/// the test does nothing, since frame times of a debug build judge
/// nothing.
#[test]
fn the_window_map_walks_inside_the_blend() {
    let Some(out) = std::env::var_os("LOQUAX_RENDER_WALK").map(PathBuf::from) else {
        return;
    };
    let failed = window_variant(&out);
    assert!(
        failed.is_empty(),
        "the window variant's rules failed:\n{}",
        failed.join("\n")
    );
}

/// 1812's comparison, a measuring tool: real loading scenes (the walk's
/// city and the Erg Chebbi desert, light and dark, the desktop's and a
/// phone's screen, zooms from the globe to the ground), each caught while
/// a load for the screen runs with bytes arriving, drawn without the
/// treatment and with each [`loquax::ui::maploading::Look`], with the
/// frame's time with and without it. `LOQUAX_LOAD_LOOKS=DIR` writes the
/// pictures, `looks.tsv` and `index.html` there; without it the test
/// does nothing.
#[test]
fn the_loading_looks_side_by_side() {
    use leviculum_mapfill::style::MapTheme;
    use loquax::ui::maploading::{paint, Brush, Look};
    let Some(out) = std::env::var_os("LOQUAX_LOAD_LOOKS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&out).expect("the out directory");
    let desk = (1120.0, 700.0, 1.0);
    let florence = LooksGround::Real(OLTRARNO, (43.7475, 11.2720));
    let grunewald = LooksGround::Real(GRUNEWALD, (52.4715, 13.2280));
    let florence_between = LooksGround::Real(OLTRARNO, (43.7620, 11.2660));
    let phone = (412.0, 915.0, 2.625);
    let scenes = [
        LooksScene {
            name: "globe-z3",
            ground: LooksGround::Synthetic,
            zoom: 3,
            dark: false,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "city-z11",
            ground: LooksGround::Synthetic,
            zoom: 11,
            dark: false,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "city-z13",
            ground: LooksGround::Synthetic,
            zoom: 13,
            dark: false,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "city-z13-dark",
            ground: LooksGround::Synthetic,
            zoom: 13,
            dark: true,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "city-z15",
            ground: LooksGround::Synthetic,
            zoom: 15,
            dark: false,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "city-z14-phone",
            ground: LooksGround::Synthetic,
            zoom: 14,
            dark: false,
            size: phone,
            standin: &[],
        },
        LooksScene {
            name: "city-z14-phone-dark",
            ground: LooksGround::Synthetic,
            zoom: 14,
            dark: true,
            size: phone,
            standin: &[],
        },
        LooksScene {
            name: "desert-z12",
            ground: LooksGround::Desert,
            zoom: 12,
            dark: false,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "desert-z12-dark",
            ground: LooksGround::Desert,
            zoom: 12,
            dark: true,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "desert-z13-phone",
            ground: LooksGround::Desert,
            zoom: 13,
            dark: false,
            size: phone,
            standin: &[],
        },
        LooksScene {
            name: "florence-z15",
            ground: florence,
            zoom: 15,
            dark: false,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "florence-z15-dark",
            ground: florence,
            zoom: 15,
            dark: true,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "florence-z15-phone",
            ground: florence,
            zoom: 15,
            dark: false,
            size: phone,
            standin: &[],
        },
        LooksScene {
            name: "grunewald-z15",
            ground: grunewald,
            zoom: 15,
            dark: false,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "grunewald-z15-dark",
            ground: grunewald,
            zoom: 15,
            dark: true,
            size: desk,
            standin: &[],
        },
        LooksScene {
            name: "grunewald-z14-phone-dark",
            ground: grunewald,
            zoom: 14,
            dark: true,
            size: phone,
            standin: &[],
        },
        // Florence's centre installed (firenze.lmr), the Oltrarno south
        // of it loading: half the screen has its data.
        LooksScene {
            name: "florence-mixed-z13-dark",
            ground: florence_between,
            zoom: 13,
            dark: true,
            size: desk,
            standin: FIRENZE,
        },
        LooksScene {
            name: "florence-mixed-z13-phone",
            ground: florence_between,
            zoom: 13,
            dark: false,
            size: phone,
            standin: FIRENZE,
        },
        LooksScene {
            name: "florence-mixed-z13-phone-dark",
            ground: florence_between,
            zoom: 13,
            dark: true,
            size: phone,
            standin: FIRENZE,
        },
        LooksScene {
            name: "florence-mixed-z13",
            ground: florence_between,
            zoom: 13,
            dark: false,
            size: desk,
            standin: FIRENZE,
        },
    ];
    let looks = [
        ("stripes", Look::Stripes),
        ("veil", Look::Veil),
        ("edge", Look::Edge),
    ];
    let mut rows = String::from(
        "scene\tsize\tzoom\tmilli\tdraw_ms_without_p50\tdraw_ms_with_p50\tlook\tpaint_ms_p50\tpainted_px\tpicture\n",
    );
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>1812 loading looks</title>\
         <style>body{font:13px sans-serif}img{max-width:420px;border:1px solid #888}\
         td{vertical-align:top;padding:4px}</style><h1>1812: the loading looks</h1><table>",
    );
    for scene in &scenes {
        let mut walk = match scene.ground {
            LooksGround::Desert => {
                Walk::desert(&ERG_CHEBBI, SLOW_RATE, 40, scene.zoom, Inject::None)
            }
            LooksGround::Synthetic => {
                Walk::new(scene.name, SLOW_RATE, 40, scene.zoom, Inject::None)
            }
            // The fixtures are small: a slow link so the load is caught
            // with part of the region drawn and the rest to come.
            LooksGround::Real(bytes, at) => Walk::real(scene.name, bytes, at, 6_000, scene.zoom),
        };
        if !scene.standin.is_empty() {
            let dir = walk.maps.dir().expect("maps").to_path_buf();
            std::fs::write(dir.join("standin.lmr"), scene.standin).expect("the stand-in");
            walk.maps.changed();
            let mut seen = walk.app.seen.clone();
            seen.maps = walk.maps.epoch();
            walk.app.apply(Message::Seen(Box::new(seen)));
        }
        let (lw, lh, k) = scene.size;
        walk.app.apply(Message::Scale(k));
        walk.app.apply(Message::MapSize(iced::Size::new(lw, lh)));
        let mut seen = walk.app.seen.clone();
        if scene.dark {
            seen.map_flag = Some(leviculum_ui::theme::ThemeFlag::Dark);
            seen.style = seen.style.in_theme(MapTheme::Dark);
        }
        walk.app.apply(Message::Seen(Box::new(seen)));
        walk.app.hold();
        // Until a load for the screen runs with bytes arriving, between a
        // tenth and nine tenths of the way; the globe asks for none.
        let mut milli = 0u16;
        for _ in 0..2_500 {
            walk.step();
            let running = walk.app.seen.loads.areas.iter().find_map(|a| match a.kind {
                Kind::Loading { milli } if (100..900).contains(&milli) => Some(milli),
                _ => None,
            });
            if let Some(m) = running {
                milli = m;
                break;
            }
            if scene.zoom < MIN_ZOOM && walk.frames.len() > 60 {
                break;
            }
        }
        let Some(order) = walk.app.order() else {
            continue;
        };
        let bare = Order {
            loads: Loads::default(),
            phase: 0,
            ..order.clone()
        };
        let mut without = Vec::new();
        let mut with = Vec::new();
        let mut last = None;
        for _ in 0..9 {
            let shown = draw(&walk.app.frames, bare.clone());
            without.push(shown.ms);
            last = Some(shown);
            with.push(draw(&walk.app.frames, order.clone()).ms);
        }
        let Some(shown) = last else {
            continue;
        };
        let base: Vec<u8> = match &shown.handle {
            super::Handle::Rgba { pixels, .. } => pixels.to_vec(),
            other => panic!("not raw pixels: {other:?}"),
        };
        let screen = walk.app.view;
        let size = screen.size();
        let dark = order.style.theme == MapTheme::Dark;
        let held = loquax::ui::viewload::holding(&walk.maps.headers(), order.view.tile_zoom());
        let name = format!("looks-{}-none.png", scene.name);
        write_png(
            &out.join(&name),
            &crop_pixels(&base, &shown.view, &screen),
            size,
        );
        html.push_str(&format!(
            "<tr><td>{}<br>{}x{} z{}<br>{} ‰ loaded<br>draw {:.2} ms without, {:.2} ms with</td>\
             <td><a href='{name}'><img src='{name}'></a><br>none</td>",
            scene.name,
            size.0,
            size.1,
            scene.zoom,
            milli,
            median(without.clone()),
            median(with.clone())
        ));
        for (word, look) in looks {
            let mut times = Vec::new();
            let mut rgba = base.clone();
            let mut painted = Default::default();
            for _ in 0..9 {
                rgba.copy_from_slice(&base);
                let at = Instant::now();
                painted = paint(
                    &mut rgba,
                    &shown.view,
                    &order.loads,
                    &held,
                    &Brush {
                        phase: order.phase,
                        dark,
                        scale: order.scale,
                        margin: order.margin,
                        look,
                    },
                );
                times.push(at.elapsed().as_secs_f64() * 1e3);
            }
            let painted: loquax::ui::maploading::Painted = painted;
            let name = format!("looks-{}-{word}.png", scene.name);
            write_png(
                &out.join(&name),
                &crop_pixels(&rgba, &shown.view, &screen),
                size,
            );
            rows.push_str(&format!(
                "{}\t{}x{}\t{}\t{}\t{:.3}\t{:.3}\t{word}\t{:.3}\t{}\t{name}\n",
                scene.name,
                size.0,
                size.1,
                scene.zoom,
                milli,
                median(without.clone()),
                median(with.clone()),
                median(times.clone()),
                painted.pixels
            ));
            html.push_str(&format!(
                "<td><a href='{name}'><img src='{name}'></a><br>{word}, paint {:.3} ms</td>",
                median(times)
            ));
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    std::fs::write(out.join("looks.tsv"), rows).expect("the rows");
    std::fs::write(out.join("index.html"), html).expect("the sheet");
}
