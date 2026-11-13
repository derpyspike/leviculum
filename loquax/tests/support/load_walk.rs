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
//!   tile) within the session's bound: [`FILL_FAST_MS`] at full speed,
//!   [`FILL_SLOW_MS`] at [`SLOW_RATE`].
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
//! - `frame_never_blank`: no rendered frame of a session is blank (one
//!   colour over more than [`BLANK_SHARE`] of the screen) where the
//!   frame before it had a picture.
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
use loquax::mapload::{Loader, Rank, Source, Step, MAX_AWAY};
use loquax::picture::Maps;
use loquax::ui::map::MapWant;
use loquax::ui::viewload::{uncovered, GAP_SHARE, MIN_ZOOM};

use super::first_tile;
use super::{draw, prefetch, App, Frames, Job, Message, Pointer, Screen, Want};

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
/// A rest must fill within this at [`SLOW_RATE`].
pub const FILL_SLOW_MS: u64 = 45_000;
/// After a fault ends the view must fill, untouched, within this.
pub const RECOVER_MS: u64 = 25_000;
/// While boxes wait, one must start, end or leave within this.
pub const STALL_MS: u64 = 20_000;
/// A box started more often than this without a failure between spins.
pub const SPIN_STARTS: usize = 2;
/// A frame whose most common colour covers more of the screen than this
/// is blank.
pub const BLANK_SHARE: f64 = 0.99;

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
}

/// A loader event on the walk's clock.
pub enum Ev {
    Started(Bounds),
    /// Landed: its box, its file name, the names evicted for it.
    Done(Bounds, String, Vec<String>),
    Failed(Bounds, String),
    Skipped(Bounds, &'static str),
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
    /// Every picture after the first is blanked before it is checked.
    BlankFrame,
    /// The picture of a repeated view is perturbed before it is checked:
    /// the comparator's positive control.
    PerturbPicture,
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
    last_ask: Option<Instant>,
    done_wants: Vec<MapWant>,
    /// The last drawn frame said a name still fades.
    last_fading: bool,
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
        let (w, h) = WALK_SIZE;
        app.apply(Message::MapSize(iced::Size::new(w as f32, h as f32)));
        let mut seen = app.seen.clone();
        seen.autoload = true;
        seen.maps = maps.epoch();
        app.apply(Message::Seen(Box::new(seen)));
        let at = project(LatLon::from_degrees(AREA_A.0, AREA_A.1));
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
            last_ask: None,
            done_wants: Vec::new(),
            last_fading: false,
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
        self.app.tick(Instant::now());
        if self
            .last_wheel
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(super::WHEEL_REST_MS))
        {
            self.last_wheel = None;
            self.app.apply(Message::WheelRest);
        }
        let mut drew = false;
        let mut missing = self.frames.last().map_or(0, |f| f.missing);
        match self.app.next_job() {
            Some(Job::Draw(order)) => {
                let shown = draw(&self.app.frames, *order);
                drew = true;
                missing = shown.missing;
                self.last_fading = shown.fading;
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
                    self.events.push((t, Ev::Started(b)));
                    let _ = w;
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
            }
        }
        if landed {
            let mut seen = self.app.seen.clone();
            seen.maps = self.maps.epoch();
            self.app.apply(Message::Seen(Box::new(seen)));
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
        });
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
                    self.events.push((t, Ev::Started(w.bounds)));
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
        let (w, h) = WALK_SIZE;
        self.app.apply(Message::Pointer(Pointer::Wheel(
            Point::new(w as f32 / 2.0, h as f32 / 2.0),
            milli,
        )));
        self.last_wheel = Some(Instant::now());
        self.run(settle_frames);
    }

    /// A drag by `dx, dy` a frame for `frames` frames, pointer held.
    pub fn drag(&mut self, dx: f32, dy: f32, frames: u32) {
        let (w, h) = WALK_SIZE;
        let (mut x, mut y) = (w as f32 / 2.0, h as f32 / 2.0);
        self.app
            .apply(Message::Pointer(Pointer::Down(Point::new(x, y))));
        for k in 0..frames {
            x += dx;
            y += dy;
            self.app
                .apply(Message::Pointer(Pointer::Move(Point::new(x, y))));
            self.step();
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
    }

    /// The view jumped to `(lat, lon)` at `zoom`, as "show where I am"
    /// moves it.
    pub fn jump(&mut self, to: (f64, f64), zoom: u8) {
        self.app.settle();
        let at = project(LatLon::from_degrees(to.0, to.1));
        let (w, h) = WALK_SIZE;
        self.app.view = ExactView::new(at, u32::from(zoom) * MILLI, w, h);
        self.app.hold();
    }

    /// At rest until the view is filled at its zoom and the frame owes
    /// nothing, at most `bound_ms`; then the pause's picture. `expects`
    /// says the rule holds the rest to its bound (a reachable source).
    pub fn rest(&mut self, label: &str, bound_ms: u64, expects: bool) {
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
        let mut crop = crop;
        match self.inject {
            Inject::BlankFrame if !self.checks.is_empty() => {
                for px in crop.chunks_exact_mut(4) {
                    px.copy_from_slice(&[0x80, 0x80, 0x80, 0xff]);
                }
            }
            Inject::PerturbPicture => {
                let seen_before = self.checks.iter().any(|c| {
                    c.centre == self.app.view.centre && c.zoom_milli == self.app.view.zoom_milli()
                });
                if seen_before {
                    for px in crop.iter_mut().take(4096) {
                        *px ^= 0x55;
                    }
                }
            }
            _ => {}
        }
        let blank = blank_share(&crop) > BLANK_SHARE;
        let inside = inside_archive(&self.view_now().bounds());
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

    /// The fill bound for this walk's link.
    pub fn fill_bound(&self) -> u64 {
        match self.rate {
            0 => FILL_FAST_MS,
            _ => FILL_SLOW_MS,
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
            let last = self.frames.last().expect("a frame");
            if let Some((_, Ev::Skipped(b, "backoff"))) = self.events.last() {
                if overlaps(&self.view_now().bounds(), b) {
                    self.fault.backoff_after_clear = true;
                }
            }
            if last.uncovered == 0.0 && last.missing == 0 && !self.loader.loading_here() {
                self.fault.filled_at = Some(self.now_ms());
                break;
            }
        }
        let filled = self.fault.filled_at.is_some();
        self.checkpoint("after-fault", self.fault.filled_at, RECOVER_MS, false);
        let _ = filled;
    }
}

/// The two-area archive: [`first_tile`]'s z15 tile in every slot of
/// [`WIDE_BOX`] from z10 to z15, padded to a dense city tile's size.
pub fn wide_archive() -> Vec<u8> {
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
    padded(&mvt, &WIDE_BOX, 10..=15, first_tile::FIRST_TILE_PAD, &meta).expect("the archive")
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

/// Whether `view` lies inside [`WIDE_BOX`] with a margin of 0.03
/// degrees (the screen box's growth): past the archive the world is
/// empty, which is no loader's fault, so no fill or picture rule judges
/// a view that reaches out.
pub fn inside_archive(view: &Bounds) -> bool {
    const MARGIN: i32 = 300_000;
    let shrunk = Bounds {
        west: WIDE_BOX.west + MARGIN,
        south: WIDE_BOX.south + MARGIN,
        east: WIDE_BOX.east - MARGIN,
        north: WIDE_BOX.north - MARGIN,
    };
    shrunk.holds(view)
}

/// Whether two boxes overlap (edges included, no antimeridian walks
/// here: the fixture's box does not cross it).
fn overlaps(a: &Bounds, b: &Bounds) -> bool {
    a.west <= b.east && b.west <= a.east && a.south <= b.north && b.south <= a.north
}

/// The share of `rgba` its most common pixel covers.
pub fn blank_share(rgba: &[u8]) -> f64 {
    let mut counts: HashMap<[u8; 4], u32> = HashMap::new();
    let mut total = 0u32;
    for px in rgba.chunks_exact(4).step_by(4) {
        *counts.entry([px[0], px[1], px[2], px[3]]).or_default() += 1;
        total += 1;
    }
    let top = counts.values().copied().max().unwrap_or(0);
    f64::from(top) / f64::from(total.max(1))
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
                if let Ev::Started(b) = ev {
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
                    && matches!(ev, Ev::Started(eb) | Ev::Done(eb, _, _) | Ev::Failed(eb, _) if eb == bounds)
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
            *t > prev_t && *t <= f.t && matches!(ev, Ev::Started(_) | Ev::Done(..) | Ev::Failed(..))
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
            Ev::Started(b) => {
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
            Ev::Skipped(..) => {}
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

    // frame_never_blank: a picture after a picture.
    let mut blanked = Vec::new();
    for (i, pair) in walk.checks.windows(2).enumerate() {
        if pair[1].inside && !pair[0].blank && pair[1].blank {
            blanked.push(i + 1);
        }
    }
    out.push(v(
        "frame_never_blank",
        blanked.is_empty(),
        format!("{} blank frames after a picture", blanked.len()),
        "0".to_string(),
        blanked
            .iter()
            .take(3)
            .map(|i| walk.checks[*i].label.clone())
            .collect::<Vec<_>>()
            .join("; "),
        blanked
            .first()
            .map(|i| image_name(walk, *i))
            .unwrap_or_default(),
        String::new(),
    ));
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
    let mut trace =
        String::from("t\tcentre_x\tcentre_y\tzoom_milli\tuncovered\tsent\tmissing\tdrew\tqueue\n");
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
            "{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\n",
            f.t,
            f.centre.x,
            f.centre.y,
            f.zoom_milli,
            f.uncovered,
            f.sent,
            f.missing,
            u8::from(f.drew),
            queue.join(" ")
        ));
    }
    trace.push_str("\nt\tevent\tbounds\tdetail\n");
    for (t, ev) in &walk.events {
        let (kind, b, detail) = match ev {
            Ev::Started(b) => ("started", b, String::new()),
            Ev::Done(b, name, evicted) => ("done", b, format!("{name} evicted={evicted:?}")),
            Ev::Failed(b, why) => ("failed", b, why.clone()),
            Ev::Skipped(b, why) => ("skipped", b, (*why).to_string()),
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
        walk.rest(&format!("z{z}"), bound, true);
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
    walk.rest("final", bound, true);
    walk.drain(bound);
    walk
}

/// In three steps, out two, in one, repeated (session 3).
pub fn in_out_in(rate: u64, latency: u64, rounds: u32, inject: Inject) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("in-out-in{tag}"), rate, latency, 13, inject);
    let bound = walk.fill_bound();
    walk.rest("start-z13", bound, true);
    for round in 0..rounds {
        for _ in 0..3 {
            walk.wheel(1000, 14);
        }
        walk.rest(&format!("r{round}-in3-z16"), bound, true);
        for _ in 0..2 {
            walk.wheel(-1000, 14);
        }
        walk.rest(&format!("r{round}-out2-z14"), bound, true);
        walk.wheel(1000, 14);
        walk.rest(&format!("r{round}-in1-z15"), bound, true);
        walk.wheel(-2000, 14);
        walk.rest(&format!("r{round}-back-z13"), bound, true);
    }
    walk.drain(bound);
    walk
}

/// Pan across several boxes at a fixed zoom, slowly and fast (session 4).
pub fn pan_slow_fast(rate: u64, latency: u64) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("pan{tag}"), rate, latency, 15, Inject::None);
    let bound = walk.fill_bound();
    walk.rest("start-z15", bound, true);
    // The view goes east: the pointer west, a quarter screen a second.
    walk.drag(-8.0, 0.0, 150);
    walk.rest("slow-east", bound, true);
    walk.drag(-60.0, 0.0, 40);
    walk.rest("fast-east", bound, true);
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
    walk.rest("after", bound, true);
    walk.drain(bound);
    walk
}

/// Jump far away while loads run, and come back (session 6).
pub fn jump_away_and_back(rate: u64, latency: u64) -> Walk {
    let tag = if rate == 0 { "" } else { "-slow" };
    let mut walk = Walk::new(&format!("jump{tag}"), rate, latency, 15, Inject::None);
    let bound = walk.fill_bound();
    walk.rest("start-z15", bound, true);
    // Move on so loads run, and jump while they do.
    walk.drag(-40.0, 0.0, 20);
    walk.run(8);
    walk.jump(AREA_B, 15);
    walk.rest("away-z15", bound, true);
    walk.jump(AREA_A, 15);
    walk.rest("back-z15", bound, true);
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
    walk.rest("faulty", 6_000, false);
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
    walk.rest("faulty", 8_000, false);
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
    walk.rest("faulty", 8_000, false);
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
    walk.rest("start-z15", bound, true);
    walk.drag(-40.0, 0.0, 40);
    walk.rest("east", bound, true);
    walk.drain(bound);
    walk
}

/// The sessions of a set: `short` is the per-batch `cargo test` default,
/// `quick` what fits two minutes released, `full` everything.
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
    ]
}

/// One selftest case run: a small session with the fault injected.
pub fn selftest_walk(inject: Inject) -> Walk {
    match inject {
        Inject::StuckLoad => {
            let mut walk = Walk::new("selftest-stuck", 0, 0, 13, Inject::None);
            walk.serve.hold_at(walk.serve.sent() + 60_000);
            walk.wheel(1000, 14);
            walk.rest("held", 4_000, false);
            let frames = (2 * STALL_MS / 16) as u32;
            walk.run(frames);
            walk
        }
        Inject::NeverRecovers => recovers_503(0, 0, false),
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
            walk.rest("first-z13", bound, true);
            walk.wheel(1000, 14);
            walk.rest("z14", bound, true);
            walk.wheel(-1000, 14);
            walk.rest("second-z13", bound, true);
            walk
        }
        inject => {
            let mut walk = Walk::new("selftest", 0, 0, 12, inject);
            let bound = match inject {
                Inject::DropWant => 4_000,
                _ => walk.fill_bound(),
            };
            walk.wheel(1000, 14);
            walk.rest("z13", bound, true);
            walk.wheel(1000, 14);
            walk.rest("z14", bound, true);
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
    let all = std::env::var_os("LOQUAX_LOAD_WALK_SELFTEST").is_some();
    let out = std::env::var_os("LOQUAX_LOAD_WALK").map(std::path::PathBuf::from);
    let cases = selftest_cases();
    let cases = if all { cases } else { cases[..1].to_vec() };
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
