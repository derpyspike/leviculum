//! Where the map is still loading, shown in place (1812).
//!
//! Lew 2026-11-13: "you wait and ask yourself: is it loading or not?" must
//! never happen again. Every area of the view whose data is still to come
//! carries a quiet treatment in the picture itself, in one of three looks
//! that differ at a glance:
//!
//! - **loading now** ([`Kind::Loading`]): blue diagonal stripes that move
//!   while bytes arrive, a blue edge, and a bar along the area's bottom
//!   filled to the bytes done of the bytes planned. Nothing is faked: the
//!   fraction is the loader's own count, and a load that receives nothing
//!   for [`STALL_MS`] stops moving and turns amber ([`Kind::Stalled`]),
//!   its line saying for how long no data came.
//! - **waiting its turn** ([`Kind::Waiting`]): grey still stripes and a
//!   dashed grey edge.
//! - **failed** ([`Kind::Failed`]): red still stripes and a dashed red
//!   edge; the line says when it tries again. Offline is said once for
//!   the view ([`Loads::offline`]), never per box.
//!
//! The stripes cover a fifth of the area's pixels, so the data already
//! there (a coarser step's stand-in, the outline) stays readable under
//! them. When the area completes the loader drops its box and the next
//! frame draws the area without it: the map under it does not move, so
//! nothing jumps.
//!
//! The treatment is computed from the loader's report (the shell's
//! `mapload::Watch`), carried in the model ([`Loads`]), painted over a
//! composed frame's RGBA by [`paint`] on the window and the phone, and
//! said in one small line ([`Loads::line`]) on every target, the terminal
//! and the M9 included. Only boxes for what the user looks at (asked by
//! hand, the screen, under it) are drawn: a box next to the screen or one
//! the view left is not the view's lack.
//!
//! An idle map draws no frame for it: only [`Loads::animates`] asks a
//! frontend for frames, one per [`PHASE_MS`].

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_map::mercator::{project, LatLon};
use leviculum_map::region::Bounds;
use leviculum_map::viewport::ExactView;

use super::strings::{self, Arg, Lang, StrId};

/// A running load that received no byte for this long stands still and
/// says so ([`Kind::Stalled`]).
pub const STALL_MS: u64 = 10_000;

/// The stripes of a load move one step this often: 8 steps a second,
/// only while one loads ([`Loads::animates`]).
pub const PHASE_MS: u64 = 125;

/// What an area of the view is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Bytes arrive: thousandths of the bytes planned that are done.
    Loading { milli: u16 },
    /// It runs, and no byte came for `quiet_s` seconds ([`STALL_MS`]):
    /// the bar stands still where it is.
    Stalled { milli: u16, quiet_s: u32 },
    /// It waits its turn in the queue.
    Waiting,
    /// It failed and tries again in `retry_s` seconds.
    Failed { retry_s: u32 },
}

impl Kind {
    /// Which treatment wins where two areas overlap: the one that says
    /// most about what happens there.
    fn precedence(self) -> u8 {
        match self {
            Self::Loading { .. } => 0,
            Self::Stalled { .. } => 1,
            Self::Failed { .. } => 2,
            Self::Waiting => 3,
        }
    }
}

/// One area of the view whose data is still to come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub bounds: Bounds,
    pub kind: Kind,
    /// Archive bytes fetched of those planned, the loader's own count (0
    /// of 0 until the plan is made).
    pub bytes_done: u64,
    pub bytes: u64,
}

/// What of the view is still to come: carried in the model, sent to the
/// window, painted by [`paint`], said by [`Loads::line`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loads {
    pub areas: Vec<Area>,
    /// The last load found no internet: said once for the view.
    pub offline: bool,
}

/// Thousandths of `bytes` that `done` is, 0 before a plan.
pub fn milli_of(done: u64, bytes: u64) -> u16 {
    match bytes {
        0 => 0,
        _ => (done.min(bytes).saturating_mul(1000) / bytes) as u16,
    }
}

impl Loads {
    /// Nothing of the view is still to come.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty() && !self.offline
    }

    /// Whether a load moves: the only reason a frontend draws frames for
    /// the treatment. Stalled, waiting and failed areas stand still.
    pub fn animates(&self) -> bool {
        self.areas
            .iter()
            .any(|a| matches!(a.kind, Kind::Loading { .. }))
    }

    /// The step of the stripes at `now_ms`: 0 while nothing moves, so an
    /// idle map asks the same frame and draws none.
    pub fn phase(&self, now_ms: u64) -> u32 {
        match self.animates() {
            true => (now_ms / PHASE_MS) as u32,
            false => 0,
        }
    }

    /// How far the running loads are together, in thousandths of the
    /// bytes they planned; `None` when none runs.
    pub fn milli(&self) -> Option<u16> {
        let running = self
            .areas
            .iter()
            .filter(|a| matches!(a.kind, Kind::Loading { .. } | Kind::Stalled { .. }));
        let (mut done, mut bytes, mut any) = (0u64, 0u64, false);
        for a in running {
            any = true;
            done = done.saturating_add(a.bytes_done.min(a.bytes));
            bytes = bytes.saturating_add(a.bytes);
        }
        any.then(|| milli_of(done, bytes))
    }

    /// The one small line every target shows beside the map (the
    /// window's bar, the phone's chip, the terminal's title, the M9's
    /// status): `None` when nothing of the view is to come.
    pub fn line(&self, lang: Lang) -> Option<String> {
        let say = |id: StrId, args: &[Arg]| strings::format(id.text(lang), args, lang);
        if self.offline {
            return Some(String::from(StrId::MapLoadOffline.text(lang)));
        }
        let stalled = self.areas.iter().find_map(|a| match a.kind {
            Kind::Stalled { quiet_s, .. } => Some(quiet_s),
            _ => None,
        });
        let loading = self
            .areas
            .iter()
            .any(|a| matches!(a.kind, Kind::Loading { .. }));
        if let (Some(milli), true) = (self.milli(), loading) {
            return Some(say(
                StrId::MapLoadingPercent,
                &[Arg::Int(i64::from(milli / 10))],
            ));
        }
        if let Some(quiet_s) = stalled {
            return Some(say(StrId::MapLoadStalled, &[Arg::Int(i64::from(quiet_s))]));
        }
        let failed = self
            .areas
            .iter()
            .filter_map(|a| match a.kind {
                Kind::Failed { retry_s } => Some(retry_s),
                _ => None,
            })
            .min();
        if let Some(retry_s) = failed {
            return Some(say(StrId::MapLoadRetry, &[Arg::Int(i64::from(retry_s))]));
        }
        (!self.areas.is_empty()).then(|| String::from(StrId::MapLoadWaiting.text(lang)))
    }

    /// Whether an area failed and waits: what "try now" applies to.
    pub fn failed(&self) -> bool {
        self.areas
            .iter()
            .any(|a| matches!(a.kind, Kind::Failed { .. }))
    }
}

/// How the treatment looks: [`Look::Stripes`] is the one the frontends
/// draw (1812's choice, its report); the others are the variants it was
/// chosen over, kept for the comparison pictures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// Sparse diagonal stripes, an edge, a progress bar.
    #[default]
    Stripes,
    /// A wash of colour over the whole area, an edge, a progress bar.
    Veil,
    /// The edge alone, marching while it loads, and a progress bar.
    Edge,
}

/// One rectangle of a frame that carries an area's treatment, in the
/// frame's pixels, `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub kind: Kind,
}

impl Piece {
    /// Whether the pixel `(x, y)` lies in it.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= f64::from(self.x0)
            && x < f64::from(self.x1)
            && y >= f64::from(self.y0)
            && y < f64::from(self.y1)
    }

    fn empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
}

/// What [`paint`] did to a frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Painted {
    /// The rectangles that carry a treatment, none overlapping.
    pub pieces: Vec<Piece>,
    /// Pixels it changed.
    pub pixels: u64,
}

/// The treatment's ink for `kind`, on a light or a dark map.
fn ink(kind: Kind, dark: bool) -> [u8; 3] {
    match (kind, dark) {
        (Kind::Loading { .. }, false) => [0x1f, 0x6f, 0xd1],
        (Kind::Loading { .. }, true) => [0x5a, 0xa9, 0xff],
        (Kind::Stalled { .. }, false) => [0xc8, 0x80, 0x00],
        (Kind::Stalled { .. }, true) => [0xff, 0xc1, 0x4d],
        (Kind::Failed { .. }, false) => [0xc6, 0x28, 0x28],
        (Kind::Failed { .. }, true) => [0xff, 0x6b, 0x6b],
        (Kind::Waiting, false) => [0x6a, 0x6a, 0x6a],
        (Kind::Waiting, true) => [0xb4, 0xb4, 0xb4],
    }
}

/// `b` over the pixel at `i` with `alpha` of 256.
#[inline]
fn blend(rgba: &mut [u8], i: usize, b: [u8; 3], alpha: u16) {
    let keep = 256 - alpha;
    for c in 0..3 {
        let v = (u16::from(rgba[i + c]) * keep + u16::from(b[c]) * alpha) >> 8;
        rgba[i + c] = v as u8;
    }
}

/// `r` with `cut` taken out: up to four rectangles.
fn subtract(r: Piece, cut: &Piece) -> Vec<Piece> {
    let overlaps = r.x0 < cut.x1 && cut.x0 < r.x1 && r.y0 < cut.y1 && cut.y0 < r.y1;
    if !overlaps {
        return alloc::vec![r];
    }
    let mid = (r.y0.max(cut.y0), r.y1.min(cut.y1));
    [
        Piece { y1: cut.y0, ..r },
        Piece { y0: cut.y1, ..r },
        Piece {
            y0: mid.0,
            y1: mid.1,
            x1: cut.x0,
            ..r
        },
        Piece {
            y0: mid.0,
            y1: mid.1,
            x0: cut.x1,
            ..r
        },
    ]
    .into_iter()
    .map(|p| Piece {
        x0: p.x0.max(r.x0),
        x1: p.x1.min(r.x1),
        y0: p.y0.max(r.y0),
        y1: p.y1.min(r.y1),
        ..p
    })
    .filter(|p| !p.empty())
    .collect()
}

/// Where `b` lies in `view`'s pixels, unclipped: west to east and north
/// to south, the copy of the world nearest the view's centre.
pub fn rect_of(view: &ExactView, b: &Bounds) -> (f64, f64, f64, f64) {
    let nw = project(LatLon {
        lat_e7: b.north,
        lon_e7: b.west,
    });
    let se = project(LatLon {
        lat_e7: b.south,
        lon_e7: b.east,
    });
    let (x0, y0) = view.to_screen(nw);
    let (_, y1) = view.to_screen(se);
    let width = f64::from(se.x.wrapping_sub(nw.x)) / view.unit();
    (x0, y0, x0 + width, y1)
}

/// How one frame draws the treatment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush {
    /// The stripes' step ([`Loads::phase`]).
    pub phase: u32,
    /// The map is the dark one.
    pub dark: bool,
    /// Screen pixels a logical one.
    pub scale: f32,
    /// The frame's pixels past the screen on the left and right, and on
    /// the top and bottom: the bar runs along what the screen shows.
    pub margin: (u32, u32),
    pub look: Look,
}

/// The treatment of `loads` over a frame `view` drew into `rgba` (RGBA
/// rows, the view's size): every area's visible part once, the one that
/// says most where two overlap ([`Kind`]'s order), as `brush` says,
/// never over a box of `held`, the regions that hold the view's zoom
/// already ([`super::viewload::holding`]): a box loads for the whole
/// screen, but where its data is there the map shows it bare. A
/// view on the globe (a curved frame, [`crate::mapgesture::blends`]) is
/// not drawn over: no box is asked for at its zooms by itself, and its
/// data, the world outline, is there.
pub fn paint(
    rgba: &mut [u8],
    view: &ExactView,
    loads: &Loads,
    held: &[Bounds],
    brush: &Brush,
) -> Painted {
    let Brush {
        phase,
        dark,
        scale,
        margin,
        look,
    } = *brush;
    let mut painted = Painted::default();
    if loads.areas.is_empty() || crate::mapgesture::blends(view) {
        return painted;
    }
    let (w, h) = view.size();
    if rgba.len() < (w as usize) * (h as usize) * 4 {
        return painted;
    }
    let (wi, hi) = (w as i32, h as i32);
    // What is there already, rounded inwards: a pixel half outside a
    // region still lacks.
    let there: Vec<Piece> = held
        .iter()
        .map(|b| {
            let (x0, y0, x1, y1) = rect_of(view, b);
            Piece {
                x0: libm::ceil(x0).clamp(-1e9, 1e9) as i32,
                y0: libm::ceil(y0).clamp(-1e9, 1e9) as i32,
                x1: libm::floor(x1).clamp(-1e9, 1e9) as i32,
                y1: libm::floor(y1).clamp(-1e9, 1e9) as i32,
                kind: Kind::Waiting,
            }
        })
        .filter(|p| !p.empty())
        .collect();
    let mut order: Vec<&Area> = loads.areas.iter().collect();
    order.sort_by_key(|a| a.kind.precedence());
    let px = |v: f32| -> i32 { libm::roundf(v * scale.max(0.5)).max(1.0) as i32 };
    let geometry = Geometry {
        period: px(18.0),
        stripe: px(4.0),
        edge: px(2.0),
        bar: px(6.0),
        dash: px(8.0),
        // A dark ground swallows a thin tint: stronger stripes there.
        stripe_alpha: match dark {
            true => 150,
            false => 104,
        },
    };
    for area in order {
        let (fx0, fy0, fx1, fy1) = rect_of(view, &area.bounds);
        let full = Piece {
            x0: libm::floor(fx0).clamp(-1e9, 1e9) as i32,
            y0: libm::floor(fy0).clamp(-1e9, 1e9) as i32,
            x1: libm::ceil(fx1).clamp(-1e9, 1e9) as i32,
            y1: libm::ceil(fy1).clamp(-1e9, 1e9) as i32,
            kind: area.kind,
        };
        let clipped = Piece {
            x0: full.x0.max(0),
            y0: full.y0.max(0),
            x1: full.x1.min(wi),
            y1: full.y1.min(hi),
            ..full
        };
        if clipped.empty() {
            continue;
        }
        let mut free = alloc::vec![clipped];
        for taken in painted.pieces.iter().chain(&there) {
            free = free.iter().flat_map(|p| subtract(*p, taken)).collect();
        }
        if free.is_empty() {
            continue;
        }
        let colour = ink(area.kind, dark);
        let mut changed = 0u64;
        for p in &free {
            changed += fill(rgba, w, p, &full, colour, phase, look, &geometry);
        }
        // The bar along the bottom of what the screen shows of the area.
        let (mx, my) = (margin.0.min(w / 2) as i32, margin.1.min(h / 2) as i32);
        let shown = Piece {
            x0: clipped.x0.max(mx),
            y0: clipped.y0.max(my),
            x1: clipped.x1.min(wi - mx),
            y1: clipped.y1.min(hi - my),
            ..clipped
        };
        if let (Kind::Loading { milli } | Kind::Stalled { milli, .. }, false) =
            (area.kind, shown.empty())
        {
            changed += bar(rgba, w, &free, &shown, milli, colour, dark, &geometry);
        }
        painted.pixels += changed;
        painted.pieces.extend(free);
    }
    painted
}

/// Sizes in screen pixels.
struct Geometry {
    period: i32,
    stripe: i32,
    edge: i32,
    bar: i32,
    dash: i32,
    stripe_alpha: u16,
}

/// One piece's stripes or wash, and the area's edges that lie in it.
#[allow(clippy::too_many_arguments)]
fn fill(
    rgba: &mut [u8],
    w: u32,
    p: &Piece,
    full: &Piece,
    colour: [u8; 3],
    phase: u32,
    look: Look,
    g: &Geometry,
) -> u64 {
    let mut changed = 0u64;
    let moving = matches!(p.kind, Kind::Loading { .. });
    let shift = match moving {
        true => (phase as i32).wrapping_mul(2).rem_euclid(g.period),
        false => 0,
    };
    let row = w as usize * 4;
    for y in p.y0..p.y1 {
        let base = y as usize * row;
        match look {
            Look::Stripes => {
                // The stripes run down to the left: x + y + shift on a
                // whole period's start.
                let start = (-(y + shift)).rem_euclid(g.period);
                let mut x = p.x0 - (p.x0 - start).rem_euclid(g.period);
                while x < p.x1 {
                    for sx in x.max(p.x0)..(x + g.stripe).min(p.x1) {
                        blend(rgba, base + sx as usize * 4, colour, g.stripe_alpha);
                        changed += 1;
                    }
                    x += g.period;
                }
            }
            Look::Veil => {
                let alpha = match moving {
                    true => 48 + (phase % 8) as u16 * 4,
                    false => 56,
                };
                for x in p.x0..p.x1 {
                    blend(rgba, base + x as usize * 4, colour, alpha);
                    changed += 1;
                }
            }
            Look::Edge => {}
        }
        // The area's own edges, where this piece reaches them: solid
        // while it loads, dashed otherwise; the edge look's dashes march.
        let dashed = |t: i32| -> bool {
            let march = match (look, moving) {
                (Look::Edge, true) => phase as i32 * 2,
                _ => 0,
            };
            let solid = moving && look != Look::Edge;
            solid || (t + march).rem_euclid(g.dash * 2) < g.dash
        };
        let edge = match look {
            Look::Edge => g.edge + g.edge / 2,
            _ => g.edge,
        };
        let top = y >= full.y0 && y < full.y0 + edge;
        let bottom = y < full.y1 && y >= full.y1 - edge;
        if top || bottom {
            for x in p.x0..p.x1 {
                if dashed(x) {
                    blend(rgba, base + x as usize * 4, colour, 216);
                    changed += 1;
                }
            }
        } else if dashed(y) {
            for x in [full.x0, full.x1 - edge] {
                for ex in x.max(p.x0)..(x + edge).min(p.x1) {
                    blend(rgba, base + ex as usize * 4, colour, 216);
                    changed += 1;
                }
            }
        }
    }
    changed
}

/// The progress bar along the bottom of `clipped`, in the free pieces:
/// `milli` of its width in the area's ink, the rest a quiet track.
#[allow(clippy::too_many_arguments)]
fn bar(
    rgba: &mut [u8],
    w: u32,
    free: &[Piece],
    clipped: &Piece,
    milli: u16,
    colour: [u8; 3],
    dark: bool,
    g: &Geometry,
) -> u64 {
    let mut changed = 0u64;
    let y1 = clipped.y1;
    let y0 = (y1 - g.bar).max(clipped.y0);
    let width = i64::from(clipped.x1 - clipped.x0);
    let split = clipped.x0 + (width * i64::from(milli.min(1000)) / 1000) as i32;
    let track = match dark {
        true => [0xff, 0xff, 0xff],
        false => [0x00, 0x00, 0x00],
    };
    let row = w as usize * 4;
    for p in free {
        for y in y0.max(p.y0)..y1.min(p.y1) {
            for x in p.x0..p.x1 {
                let i = y as usize * row + x as usize * 4;
                match x < split {
                    true => blend(rgba, i, colour, 240),
                    false => blend(rgba, i, track, 88),
                }
                changed += 1;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::viewport::MILLI;

    /// A flat view of Berlin at zoom 13, 400 by 300.
    fn view() -> ExactView {
        let at = project(LatLon::from_degrees(52.52, 13.40));
        ExactView::new(at, 13 * MILLI, 400, 300)
    }

    /// The bounds of `view`'s pixels `x0..x1` by `y0..y1`.
    fn bounds_of(view: &ExactView, x0: f64, y0: f64, x1: f64, y1: f64) -> Bounds {
        let nw = leviculum_map::mercator::unproject(view.to_world(x0, y0));
        let se = leviculum_map::mercator::unproject(view.to_world(x1, y1));
        Bounds {
            west: nw.lon_e7,
            north: nw.lat_e7,
            east: se.lon_e7,
            south: se.lat_e7,
        }
    }

    fn brush(phase: u32, dark: bool, scale: f32, look: Look) -> Brush {
        Brush {
            phase,
            dark,
            scale,
            margin: (0, 0),
            look,
        }
    }

    fn area(bounds: Bounds, kind: Kind) -> Area {
        Area {
            bounds,
            kind,
            bytes_done: 0,
            bytes: 0,
        }
    }

    fn grey(view: &ExactView) -> Vec<u8> {
        let (w, h) = view.size();
        alloc::vec![0x80; (w * h * 4) as usize]
    }

    #[test]
    fn nothing_to_come_paints_nothing() {
        let v = view();
        let mut rgba = grey(&v);
        let painted = paint(
            &mut rgba,
            &v,
            &Loads::default(),
            &[],
            &brush(7, false, 1.0, Look::Stripes),
        );
        assert_eq!(painted, Painted::default());
        assert!(rgba.iter().all(|&b| b == 0x80));
    }

    #[test]
    fn an_area_carries_its_treatment_in_every_period() {
        let v = view();
        let b = bounds_of(&v, 50.0, 40.0, 250.0, 200.0);
        let loads = Loads {
            areas: alloc::vec![area(b, Kind::Loading { milli: 500 })],
            offline: false,
        };
        let mut rgba = grey(&v);
        let painted = paint(
            &mut rgba,
            &v,
            &loads,
            &[],
            &brush(0, false, 1.0, Look::Stripes),
        );
        assert_eq!(painted.pieces.len(), 1);
        let p = painted.pieces[0];
        assert!((p.x0 - 50).abs() <= 1 && (p.x1 - 250).abs() <= 1, "{p:?}");
        // Every 16-pixel square inside holds a changed pixel: no part of
        // the area looks untouched.
        let w = 400usize;
        for sy in (p.y0..p.y1 - 16).step_by(16) {
            for sx in (p.x0..p.x1 - 16).step_by(16) {
                let any = (sy..sy + 16).any(|y| {
                    (sx..sx + 16).any(|x| rgba[(y as usize * w + x as usize) * 4] != 0x80)
                });
                assert!(any, "untouched square at {sx},{sy}");
            }
        }
        // Outside the area nothing changed.
        assert_eq!(rgba[(10 * w + 10) * 4], 0x80);
        assert_eq!(rgba[(250 * w + 300) * 4], 0x80);
        // The stripes leave most of the area as it was.
        let inside = ((p.x1 - p.x0) * (p.y1 - p.y0)) as u64;
        assert!(
            painted.pixels < inside / 2,
            "{} of {inside}",
            painted.pixels
        );
    }

    #[test]
    fn the_bar_is_filled_to_the_bytes() {
        let v = view();
        let b = bounds_of(&v, 0.0, 0.0, 400.0, 300.0);
        let at = |milli: u16| {
            let loads = Loads {
                areas: alloc::vec![area(b, Kind::Loading { milli })],
                offline: false,
            };
            let mut rgba = grey(&v);
            paint(
                &mut rgba,
                &v,
                &loads,
                &[],
                &brush(0, false, 1.0, Look::Stripes),
            );
            // The bar's middle row: count the area's blue.
            let y = 297usize;
            (0..400usize)
                .filter(|&x| {
                    let i = (y * 400 + x) * 4;
                    rgba[i + 2] > rgba[i] + 40 && rgba[i + 2] > 0xc0
                })
                .count()
        };
        let (none, half, all) = (at(0), at(500), at(1000));
        assert!(none < 5, "{none}");
        assert!((190..=210).contains(&half), "{half}");
        assert!(all > 395, "{all}");
    }

    /// Where a region holds the view already, the map shows it bare: a
    /// box that loads over the whole screen marks only the rest.
    #[test]
    fn what_is_there_stays_bare() {
        let v = view();
        let all = bounds_of(&v, 0.0, 0.0, 400.0, 300.0);
        let west = bounds_of(&v, 0.0, 0.0, 200.0, 300.0);
        let loads = Loads {
            areas: alloc::vec![area(all, Kind::Loading { milli: 0 })],
            offline: false,
        };
        let mut rgba = grey(&v);
        let painted = paint(
            &mut rgba,
            &v,
            &loads,
            &[west],
            &brush(0, false, 1.0, Look::Stripes),
        );
        for y in 0..300usize {
            for x in 0..199usize {
                assert_eq!(rgba[(y * 400 + x) * 4], 0x80, "touched at {x},{y}");
            }
        }
        assert!(painted.pieces.iter().all(|p| p.x0 >= 199), "{painted:?}");
        assert!(painted.pixels > 0);
    }

    /// The bar runs along the bottom of what the screen shows, never in
    /// the frame's margin past it, which the screen crops away.
    #[test]
    fn the_bar_runs_along_the_screen_not_the_margin() {
        let v = view();
        let b = bounds_of(&v, 0.0, 0.0, 400.0, 300.0);
        let loads = Loads {
            areas: alloc::vec![area(b, Kind::Loading { milli: 1000 })],
            offline: false,
        };
        let mut rgba = grey(&v);
        let brush = Brush {
            margin: (50, 40),
            ..brush(0, false, 1.0, Look::Stripes)
        };
        paint(&mut rgba, &v, &loads, &[], &brush);
        let blue = |x: usize, y: usize| {
            let i = (y * 400 + x) * 4;
            rgba[i + 2] > rgba[i] + 40 && rgba[i + 2] > 0xc0
        };
        // The screen's bottom row band (y 254..260), its middle filled.
        assert!(blue(200, 257), "the bar on the screen");
        // The frame's own bottom, in the margin: no bar there.
        assert!(!blue(200, 297), "no bar in the margin");
    }

    #[test]
    fn the_stripes_move_only_while_loading() {
        let v = view();
        let b = bounds_of(&v, 0.0, 0.0, 400.0, 300.0);
        let frame = |kind: Kind, phase: u32| {
            let loads = Loads {
                areas: alloc::vec![area(b, kind)],
                offline: false,
            };
            let mut rgba = grey(&v);
            paint(
                &mut rgba,
                &v,
                &loads,
                &[],
                &brush(phase, false, 1.0, Look::Stripes),
            );
            rgba
        };
        let loading = Kind::Loading { milli: 100 };
        assert_ne!(frame(loading, 0), frame(loading, 1));
        assert_eq!(frame(Kind::Waiting, 0), frame(Kind::Waiting, 1));
        let loads = Loads {
            areas: alloc::vec![area(b, Kind::Waiting)],
            offline: false,
        };
        assert!(!loads.animates());
        assert_eq!(loads.phase(12_345), 0);
    }

    #[test]
    fn three_states_look_different() {
        let v = view();
        let b = bounds_of(&v, 0.0, 0.0, 400.0, 300.0);
        let frame = |kind: Kind| {
            let loads = Loads {
                areas: alloc::vec![area(b, kind)],
                offline: false,
            };
            let mut rgba = grey(&v);
            paint(
                &mut rgba,
                &v,
                &loads,
                &[],
                &brush(0, false, 1.0, Look::Stripes),
            );
            rgba
        };
        let a = frame(Kind::Loading { milli: 0 });
        let b = frame(Kind::Waiting);
        let c = frame(Kind::Failed { retry_s: 5 });
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn overlapping_areas_paint_each_pixel_once() {
        let v = view();
        let wide = bounds_of(&v, 0.0, 0.0, 400.0, 300.0);
        let inner = bounds_of(&v, 100.0, 100.0, 200.0, 200.0);
        let loads = Loads {
            areas: alloc::vec![
                area(wide, Kind::Waiting),
                area(inner, Kind::Loading { milli: 0 })
            ],
            offline: false,
        };
        let mut rgba = grey(&v);
        let painted = paint(
            &mut rgba,
            &v,
            &loads,
            &[],
            &brush(0, false, 1.0, Look::Stripes),
        );
        // The loading area first, whole; the waiting one around it.
        assert_eq!(painted.pieces[0].kind, Kind::Loading { milli: 0 });
        for (i, a) in painted.pieces.iter().enumerate() {
            for b in &painted.pieces[i + 1..] {
                let overlap = a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1;
                assert!(!overlap, "{a:?} {b:?}");
            }
        }
        let area: i64 = painted
            .pieces
            .iter()
            .map(|p| i64::from(p.x1 - p.x0) * i64::from(p.y1 - p.y0))
            .sum();
        assert_eq!(area, 400 * 300);
    }

    #[test]
    fn an_area_off_the_frame_paints_nothing() {
        let v = view();
        let b = bounds_of(&v, 500.0, 0.0, 700.0, 300.0);
        let loads = Loads {
            areas: alloc::vec![area(b, Kind::Loading { milli: 0 })],
            offline: false,
        };
        let mut rgba = grey(&v);
        assert!(paint(
            &mut rgba,
            &v,
            &loads,
            &[],
            &brush(0, false, 1.0, Look::Stripes)
        )
        .pieces
        .is_empty());
    }

    #[test]
    fn every_look_marks_the_area() {
        let v = view();
        let b = bounds_of(&v, 50.0, 50.0, 350.0, 250.0);
        for look in [Look::Stripes, Look::Veil, Look::Edge] {
            let loads = Loads {
                areas: alloc::vec![area(b, Kind::Loading { milli: 300 })],
                offline: false,
            };
            let mut rgba = grey(&v);
            let painted = paint(&mut rgba, &v, &loads, &[], &brush(0, true, 2.0, look));
            assert!(painted.pixels > 0, "{look:?}");
        }
    }

    #[test]
    fn the_line_says_what_happens() {
        let b = Bounds {
            west: 0,
            south: 0,
            east: 1,
            north: 1,
        };
        let mut loads = Loads::default();
        assert_eq!(loads.line(Lang::En), None);
        loads.areas.push(Area {
            bounds: b,
            kind: Kind::Loading { milli: 420 },
            bytes_done: 42,
            bytes: 100,
        });
        assert_eq!(
            loads.line(Lang::En).as_deref(),
            Some("Loading the map · 42 %")
        );
        assert_eq!(loads.line(Lang::De).as_deref(), Some("Karte lädt · 42 %"));
        loads.areas[0].kind = Kind::Stalled {
            milli: 420,
            quiet_s: 12,
        };
        assert_eq!(
            loads.line(Lang::En).as_deref(),
            Some("No map data for 12 s")
        );
        loads.areas[0].kind = Kind::Failed { retry_s: 7 };
        assert_eq!(
            loads.line(Lang::De).as_deref(),
            Some("Karte nicht geladen · neuer Versuch in 7 s")
        );
        loads.areas[0].kind = Kind::Waiting;
        assert_eq!(
            loads.line(Lang::En).as_deref(),
            Some("The map waits its turn")
        );
        loads.offline = true;
        assert_eq!(
            loads.line(Lang::En).as_deref(),
            Some("Offline · the map loads when the network is back")
        );
    }

    #[test]
    fn the_fraction_is_the_bytes() {
        assert_eq!(milli_of(0, 0), 0);
        assert_eq!(milli_of(1, 3), 333);
        assert_eq!(milli_of(5, 3), 1000);
    }
}
