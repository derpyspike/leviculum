//! The view watchdog (1934, P0): what the user sees in the window, when
//! they do not zoom or pan, must be whole and right within a short time.
//! Lew, 2026-11-16: "At the latest after a few seconds the picture should
//! be done, even if everything has to be loaded. If it is not done after
//! 5 seconds, that is a bug. Build a check for it, at least in debug
//! builds, and we run debug builds all the time."
//!
//! The definitions are those of the map concept
//! (`docs/src/concepts/loquax-map.md`, "The view is whole"):
//!
//! - A view is **settled** once no pan, zoom or rotate input came for
//!   [`SETTLE_MS`]: a frontend says the view moves, or the view changed.
//! - A settled view is **whole** when every screen cell
//!   ([`super::viewload::screen_cells`]) draws from data at the view's
//!   own zoom, the frame owes no tile and no cell is grey.
//!
//! [`ViewWatch`] is fed one [`Observed`] per frame (and per wake it asks
//! for, [`ViewWatch::next_due_ms`]) and answers with the events a
//! frontend logs ([`ViewEvent::line`]): `VIEW_SETTLED` once a view
//! settles, `VIEW_WHOLE` once it is whole, `VIEW_INCOMPLETE` at
//! [`WHOLE_BOUND_MS`] when it is not and every [`AGAIN_MS`] after while it
//! stays so, and `VIEW_REGRESSED` whenever a drawn cell of a settled view
//! stops being drawn (1930 drives that one to zero). In a debug build a
//! frontend also shows [`ViewWatch::badge`] in a corner, never in a
//! release build.
//!
//! The watch is pure: no clock, no log, no screen. The frontends hand it
//! their monotonic milliseconds and say its lines.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_map::mercator::{unproject, World};
use leviculum_map::region::Header;
use leviculum_map::viewport::Viewport;

use super::viewload::{screen_cells, Cell, CellState};

/// A view is settled once no input moved it for this long (1934).
pub const SETTLE_MS: u64 = 300;
/// The longest a settled view may take to be whole (1934). Lew,
/// 2026-11-16: "If it is not done after 5 seconds, that is a bug." The
/// bound of the real network; a test over a throttled source scales its
/// own from the bytes the view needs.
pub const WHOLE_BOUND_MS: u64 = 5_000;
/// A view that stays incomplete is said again this often.
pub const AGAIN_MS: u64 = 10_000;
/// The first missing cells a `VIEW_INCOMPLETE` names by their tile key.
pub const NAMED: usize = 4;
/// The most `VIEW_REGRESSED` lines one settled view says; past it, one
/// line with the count, so a frame that loses the screen is no flood.
pub const REGRESSED_MAX: usize = 16;

/// What a frontend saw at one frame.
#[derive(Clone, Copy, Debug)]
pub struct Observed<'a> {
    /// The view drawn, at its load zoom.
    pub view: &'a Viewport,
    /// Input moves it now: a drag, the wheel, a pinch, a glide.
    pub moving: bool,
    /// The regions on disk the frame draws from.
    pub regions: &'a [Header],
    /// Tiles the frame owes (a region still being written, stood in).
    pub pending: usize,
    /// Cells the frontend drew grey (a tile cache's empty cell).
    pub grey: usize,
    /// The maps directory's epoch: a new one is a region that landed or
    /// left.
    pub epoch: u64,
    /// Archive bytes the autoload fetched so far, a running count
    /// (`loquax::picture::Maps::fetched`).
    pub fetched: u64,
}

/// What a `VIEW_INCOMPLETE` says of the loads and the cache, asked only
/// when it is said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// The loads, running, paused and queued, with their boxes and zooms.
    pub loads: String,
    /// The tile cache, used of its cap, as `used/cap` in bytes, or `-`.
    pub cache: String,
    /// The autoloaded regions on disk, bytes.
    pub disk: u64,
}

/// Why a drawn cell stopped being drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// A region left the maps directory.
    Evict,
    /// A region landed or changed.
    Land,
    /// Nothing on disk changed: the frame drew it otherwise.
    Redraw,
}

impl Cause {
    pub fn word(self) -> &'static str {
        match self {
            Self::Evict => "evict",
            Self::Land => "land",
            Self::Redraw => "redraw",
        }
    }
}

/// One line the watch says.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewEvent {
    Settled {
        view: (f64, f64, u8),
        cells: usize,
    },
    Whole {
        ms: u64,
        bytes: u64,
    },
    Incomplete {
        ms: u64,
        missing: usize,
        grey: usize,
        standin: usize,
        outline: usize,
        share: f64,
        first: Vec<Cell>,
        context: Context,
    },
    Regressed {
        cell: Cell,
        from: CellState,
        to: CellState,
        cause: Cause,
    },
    /// More regressions of one settled view than [`REGRESSED_MAX`]: the
    /// rest counted, not named.
    RegressedMore {
        more: usize,
        cause: Cause,
    },
}

/// A tile key as the log writes it.
pub fn key(c: &Cell) -> String {
    format!("{}/{}/{}", c.z, c.x, c.y)
}

impl ViewEvent {
    /// The event's name, its catalogue key.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Settled { .. } => "VIEW_SETTLED",
            Self::Whole { .. } => "VIEW_WHOLE",
            Self::Incomplete { .. } => "VIEW_INCOMPLETE",
            Self::Regressed { .. } | Self::RegressedMore { .. } => "VIEW_REGRESSED",
        }
    }

    /// The structured line (`NAME key=val ...`).
    pub fn line(&self) -> String {
        match self {
            Self::Settled { view, cells } => format!(
                "VIEW_SETTLED view={:.5},{:.5},{} cells={cells}",
                view.0, view.1, view.2
            ),
            Self::Whole { ms, bytes } => format!("VIEW_WHOLE ms={ms} bytes={bytes}"),
            Self::Incomplete {
                ms,
                missing,
                grey,
                standin,
                outline,
                share,
                first,
                context,
            } => format!(
                "VIEW_INCOMPLETE ms={ms} missing={missing} grey={grey} standin={standin} \
                 outline={outline} share={share:.2} first={} loads={} cache={} disk={}",
                match first.is_empty() {
                    true => String::from("-"),
                    false => first.iter().map(key).collect::<Vec<_>>().join(","),
                },
                match context.loads.is_empty() {
                    true => "-",
                    false => context.loads.as_str(),
                },
                context.cache,
                context.disk
            ),
            Self::Regressed {
                cell,
                from,
                to,
                cause,
            } => format!(
                "VIEW_REGRESSED cell={} from={} to={} cause={}",
                key(cell),
                from.word(),
                to.word(),
                cause.word()
            ),
            Self::RegressedMore { more, cause } => format!(
                "VIEW_REGRESSED cell=+{more} from=drawn to=- cause={}",
                cause.word()
            ),
        }
    }
}

/// The view a frame shows, as the watch compares them.
type Key = (World, u8, (u32, u32));

/// A settled view's watch.
#[derive(Clone, Debug)]
struct Settled {
    at_ms: u64,
    fetched: u64,
    whole: bool,
    whole_said: bool,
    next_ms: u64,
    /// The cells drawn at the last frame.
    drawn: Vec<Cell>,
    epoch: u64,
    regions: usize,
    regressed: usize,
    /// Said incomplete and not whole since: seconds and share, for the
    /// badge.
    late: Option<(u64, f64)>,
}

/// The watchdog of one map view.
#[derive(Clone, Debug, Default)]
pub struct ViewWatch {
    key: Option<Key>,
    input_ms: u64,
    /// [`Observed::fetched`] at the last input: a view's bytes count from
    /// the moment it was asked, the settle's 300 ms included.
    input_fetched: u64,
    settled: Option<Settled>,
}

impl ViewWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The frame `seen` at `now_ms`: the lines to say. `context` is asked
    /// only for a `VIEW_INCOMPLETE`.
    pub fn observe(
        &mut self,
        now_ms: u64,
        seen: &Observed<'_>,
        context: impl FnOnce() -> Context,
    ) -> Vec<ViewEvent> {
        let view = seen.view;
        let key = (view.centre, view.zoom(), view.size());
        if seen.moving || self.key != Some(key) {
            self.key = Some(key);
            self.input_ms = now_ms;
            self.input_fetched = seen.fetched;
            self.settled = None;
            return Vec::new();
        }
        let mut out = Vec::new();
        let cells = screen_cells(view, seen.regions);
        let settled = match &mut self.settled {
            Some(s) => s,
            None => {
                if now_ms.saturating_sub(self.input_ms) < SETTLE_MS {
                    return Vec::new();
                }
                let p = unproject(view.centre);
                out.push(ViewEvent::Settled {
                    view: (
                        f64::from(p.lon_e7) / 1e7,
                        f64::from(p.lat_e7) / 1e7,
                        view.zoom(),
                    ),
                    cells: cells.len(),
                });
                // The view settled 300 ms after the last input, whenever
                // the first frame after that came.
                let at_ms = self.input_ms + SETTLE_MS;
                self.settled.insert(Settled {
                    at_ms,
                    fetched: self.input_fetched,
                    whole: false,
                    whole_said: false,
                    next_ms: at_ms + WHOLE_BOUND_MS,
                    drawn: Vec::new(),
                    epoch: seen.epoch,
                    regions: seen.regions.len(),
                    regressed: 0,
                    late: None,
                })
            }
        };
        // A drawn cell no longer drawn.
        let cause = match (
            seen.regions.len() < settled.regions,
            seen.epoch != settled.epoch,
        ) {
            (true, _) => Cause::Evict,
            (false, true) => Cause::Land,
            (false, false) => Cause::Redraw,
        };
        let mut more = 0;
        for cell in &settled.drawn {
            let now = cells
                .iter()
                .find(|(c, _)| c == cell)
                .map_or(CellState::Outline, |(_, s)| *s);
            if now == CellState::Drawn {
                continue;
            }
            match settled.regressed < REGRESSED_MAX {
                true => out.push(ViewEvent::Regressed {
                    cell: *cell,
                    from: CellState::Drawn,
                    to: now,
                    cause,
                }),
                false => more += 1,
            }
            settled.regressed += 1;
        }
        if more > 0 {
            out.push(ViewEvent::RegressedMore { more, cause });
        }
        settled.drawn = cells
            .iter()
            .filter(|(_, s)| *s == CellState::Drawn)
            .map(|(c, _)| *c)
            .collect();
        settled.epoch = seen.epoch;
        settled.regions = seen.regions.len();
        let standin = cells
            .iter()
            .filter(|(_, s)| *s == CellState::StandIn)
            .count()
            + seen.pending;
        let outline = cells
            .iter()
            .filter(|(_, s)| *s == CellState::Outline)
            .count();
        let missing = standin + outline + seen.grey;
        settled.whole = missing == 0;
        if settled.whole {
            settled.late = None;
            if !settled.whole_said {
                settled.whole_said = true;
                out.push(ViewEvent::Whole {
                    ms: now_ms - settled.at_ms,
                    bytes: seen.fetched.saturating_sub(settled.fetched),
                });
            }
        } else if now_ms >= settled.next_ms {
            let ms = now_ms - settled.at_ms;
            let share = missing as f64 / (cells.len() + seen.pending + seen.grey).max(1) as f64;
            settled.next_ms = now_ms + AGAIN_MS;
            settled.late = Some((ms / 1000, share));
            out.push(ViewEvent::Incomplete {
                ms,
                missing,
                grey: seen.grey,
                standin,
                outline,
                share,
                first: cells
                    .iter()
                    .filter(|(_, s)| *s != CellState::Drawn)
                    .map(|(c, _)| *c)
                    .take(NAMED)
                    .collect(),
                context: context(),
            });
        }
        out
    }

    /// The next instant the watch has something to decide, for a
    /// frontend that draws only when asked: the settle after the last
    /// input, or the next `VIEW_INCOMPLETE`. `None` once the settled view
    /// is whole. With `repeats` off, for a frontend whose every wake is a
    /// frame drawn (the phone): no wake for the settle, which is dated to
    /// the input anyway, and none after the first `VIEW_INCOMPLETE`, whose
    /// repeats ride on the frames it draws anyway, so a view that stays
    /// incomplete offline costs it no frame every 10 s.
    pub fn next_due_ms(&self, repeats: bool) -> Option<u64> {
        match &self.settled {
            // A settle needs no frame of its own: it is dated to the input.
            None if !repeats => self.key.map(|_| self.input_ms + SETTLE_MS + WHOLE_BOUND_MS),
            None => self.key.map(|_| self.input_ms + SETTLE_MS),
            Some(s) if s.whole => None,
            Some(s) if s.late.is_some() && !repeats => None,
            Some(s) => Some(s.next_ms),
        }
    }

    /// The debug badge (1934): the seconds since the view settled and the
    /// share still missing, while a `VIEW_INCOMPLETE` was said and the
    /// view is not whole since. A frontend shows it in debug builds only.
    pub fn badge(&self) -> Option<String> {
        let (s, share) = self.settled.as_ref()?.late?;
        Some(format!("{s} s {:.0} %", share * 100.0))
    }

    /// Whether the settled view is whole now.
    pub fn whole(&self) -> bool {
        self.settled.as_ref().is_some_and(|s| s.whole)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};

    fn region(view: &Viewport, zooms: (u8, u8)) -> Header {
        Header {
            bounds: view.bounds(),
            min_zoom: zooms.0,
            max_zoom: zooms.1,
            profile: leviculum_map::profile::CUT_2,
            attribution: (0, 0),
            notice: (0, 0),
            tiles: 0,
            index: 0,
            len: 4096,
            labels: None,
            buildings: None,
            pois: None,
            streamed: false,
        }
    }

    fn view() -> Viewport {
        Viewport::new(project(LatLon::from_degrees(31.1, -4.0)), 12, 1120, 700)
    }

    fn seen<'a>(view: &'a Viewport, regions: &'a [Header], moving: bool) -> Observed<'a> {
        Observed {
            view,
            moving,
            regions,
            pending: 0,
            grey: 0,
            epoch: regions.len() as u64,
            fetched: 0,
        }
    }

    fn names(events: &[ViewEvent]) -> Vec<&'static str> {
        events.iter().map(ViewEvent::name).collect()
    }

    /// Settled 300 ms after the last input, whole when a region holds the
    /// screen at its zoom: one `VIEW_SETTLED`, one `VIEW_WHOLE`, nothing
    /// more while it stays so, and no badge.
    #[test]
    fn a_view_that_fills_in_time_is_whole_once() {
        let v = view();
        let mut w = ViewWatch::new();
        assert!(w
            .observe(0, &seen(&v, &[], true), Context::default)
            .is_empty());
        assert!(w
            .observe(100, &seen(&v, &[], false), Context::default)
            .is_empty());
        assert_eq!(
            w.next_due_ms(true),
            Some(300),
            "300 ms after the last moving frame"
        );
        let e = w.observe(300, &seen(&v, &[], false), Context::default);
        assert_eq!(names(&e), ["VIEW_SETTLED"]);
        assert!(e[0]
            .line()
            .starts_with("VIEW_SETTLED view=-4.00000,31.10000,12 cells="));
        let r = [region(&v, (12, 12))];
        let mut s = seen(&v, &r, false);
        s.fetched = 1234;
        let e = w.observe(1_900, &s, Context::default);
        assert_eq!(
            e,
            [ViewEvent::Whole {
                ms: 1_600,
                bytes: 1234
            }]
        );
        assert_eq!(e[0].line(), "VIEW_WHOLE ms=1600 bytes=1234");
        assert!(w.observe(9_000, &s, Context::default).is_empty());
        assert_eq!(w.next_due_ms(true), None);
        assert_eq!(w.badge(), None);
    }

    /// Not whole 5 s after settling: one `VIEW_INCOMPLETE` with the
    /// counts, the first cells and the context, again 10 s later, the
    /// badge meanwhile; a stand-in (a coarser region) counts as missing.
    #[test]
    fn a_view_not_whole_after_five_seconds_is_said_and_again() {
        let v = view();
        let r = [region(&v, (8, 10))];
        let mut w = ViewWatch::new();
        w.observe(0, &seen(&v, &r, false), Context::default);
        assert_eq!(
            names(&w.observe(300, &seen(&v, &r, false), Context::default)),
            ["VIEW_SETTLED"]
        );
        assert!(w
            .observe(5_299, &seen(&v, &r, false), Context::default)
            .is_empty());
        assert_eq!(w.badge(), None);
        let ctx = || Context {
            loads: "running:z12-12".into(),
            cache: "1/2".into(),
            disk: 7,
        };
        let e = w.observe(5_300, &seen(&v, &r, false), ctx);
        let ViewEvent::Incomplete {
            ms,
            missing,
            standin,
            outline,
            share,
            first,
            ..
        } = &e[0]
        else {
            panic!("{e:?}");
        };
        assert_eq!((*ms, *outline), (5_000, 0));
        assert!(*missing > 0 && missing == standin);
        assert_eq!(*share, 1.0);
        assert_eq!(first.len(), NAMED);
        let line = e[0].line();
        assert!(
            line.contains(" loads=running:z12-12 cache=1/2 disk=7"),
            "{line}"
        );
        assert_eq!(w.badge().as_deref(), Some("5 s 100 %"));
        assert_eq!(w.next_due_ms(true), Some(15_300), "the repeat");
        let mut phone = ViewWatch::new();
        phone.observe(1_000, &seen(&v, &r, false), Context::default);
        assert_eq!(phone.next_due_ms(false), Some(6_300), "no settle wake");
        // Settled late, dated to the input: incomplete at 5 s after it.
        assert_eq!(
            names(&phone.observe(6_300, &seen(&v, &r, false), Context::default)),
            ["VIEW_SETTLED", "VIEW_INCOMPLETE"]
        );
        assert_eq!(w.next_due_ms(false), None, "no frame for a repeat");
        assert!(w
            .observe(15_299, &seen(&v, &r, false), Context::default)
            .is_empty());
        assert_eq!(
            names(&w.observe(15_300, &seen(&v, &r, false), Context::default)),
            ["VIEW_INCOMPLETE"]
        );
        // Owed tiles and grey cells count too.
        let held = [region(&v, (12, 12))];
        let mut s = seen(&v, &held, false);
        s.pending = 2;
        s.grey = 1;
        assert!(w.observe(15_400, &s, Context::default).is_empty());
        let e = w.observe(25_400, &s, Context::default);
        assert!(
            matches!(
                e[0],
                ViewEvent::Incomplete {
                    missing: 3,
                    grey: 1,
                    standin: 2,
                    outline: 0,
                    ..
                }
            ),
            "{e:?}"
        );
    }

    /// Input starts the settle again: a view that moves before 5 s is
    /// never said incomplete.
    #[test]
    fn input_restarts_the_settle() {
        let v = view();
        let mut w = ViewWatch::new();
        w.observe(0, &seen(&v, &[], false), Context::default);
        w.observe(300, &seen(&v, &[], false), Context::default);
        let mut moved = v;
        moved.pan(10, 0);
        assert!(w
            .observe(4_000, &seen(&moved, &[], false), Context::default)
            .is_empty());
        assert!(w
            .observe(5_400, &seen(&moved, &[], false), Context::default)
            .iter()
            .all(|e| e.name() == "VIEW_SETTLED"));
        assert!(w
            .observe(9_000, &seen(&moved, &[], false), Context::default)
            .is_empty());
    }

    /// A drawn cell of a settled view that stops being drawn is a
    /// `VIEW_REGRESSED`, its cause the directory's change.
    #[test]
    fn a_drawn_cell_lost_is_a_regression() {
        let v = view();
        let held = [region(&v, (12, 12))];
        let mut w = ViewWatch::new();
        w.observe(0, &seen(&v, &held, false), Context::default);
        assert_eq!(
            names(&w.observe(300, &seen(&v, &held, false), Context::default)),
            ["VIEW_SETTLED", "VIEW_WHOLE"]
        );
        let e = w.observe(400, &seen(&v, &[], false), Context::default);
        assert!(!e.is_empty());
        assert!(
            e.iter().all(|e| matches!(
                e,
                ViewEvent::Regressed {
                    from: CellState::Drawn,
                    to: CellState::Outline,
                    cause: Cause::Evict,
                    ..
                }
            )),
            "{e:?}"
        );
        assert!(
            e[0].line().starts_with("VIEW_REGRESSED cell=12/"),
            "{}",
            e[0].line()
        );
        // Lost already: no second line for the same loss.
        assert!(w
            .observe(500, &seen(&v, &[], false), Context::default)
            .is_empty());
    }
}
