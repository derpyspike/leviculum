//! The render walk's rules (1801): what `just render-walk` checks in every
//! map frame it draws through the facade, and in every gesture it plays.
//!
//! The walk (`examples/render_walk.rs`, driven by `scripts/render-walk.py`)
//! draws the phone's map exactly as the app does: through
//! [`crate::map::render_into`], into a Bitmap sized the way `MapView.java`
//! sizes it ([`JavaBitmap`], the margins of `MapFrame.margin`), so a frame
//! the library draws at another size fails here as it fails on the phone.
//! What the screen then shows is kept as Java keeps it ([`Screen`]): the
//! frame's view when one lands, and when one fails the last good one
//! scaled to the zoom the fingers asked for, which is how a failed globe
//! frame became Lew's "small rectangle" on 2026-11-13.
//!
//! Each rule is named, has one documented threshold, and says pass or fail
//! with the value it measured ([`Verdict`]). The frame rules:
//!
//! - [`BITMAP_IS_FRAME`]: the frame fits the Bitmap Java made (0 misfits).
//! - [`VIEW_COVERED`]: no more than [`BLANK_MAX_PCT`] per cent of the
//!   screen is left undrawn.
//! - [`WORLD_SPANS_VIEW`]: on a turning globe, no more than
//!   [`SPACE_MAX_PCT`] per cent of the middle line across the shorter side
//!   is space: the world at its smallest fills the view (1796).
//! - [`NO_ERROR_TEXT`]: the frame says no error and no notice.
//! - [`NO_MAP_CHIP_OVER_DATA`]: no "no map here" chip where the world's
//!   outline is drawn (zoom at most [`leviculum_map::outline::MAX_ZOOM`]).
//! - [`LABEL_INSIDE_VIEW`]: no shown name straddles the screen's edge.
//! - [`LABEL_CLEAR_OF_CONTROLS`]: no shown name lies under the map's two
//!   round buttons (`activity_conversations.xml`: 56 dp, 16 dp from the
//!   end, 16 dp and 84 dp from the bottom).
//! - [`LABEL_OVERLAP`]: no two shown names overlap.
//! - [`LABEL_CURRENT_ZOOM`]: once a frame at rest has settled, no name is
//!   from a deeper zoom than the frame's, and none is still fading out.
//! - [`LABEL_ON_WORLD`]: on a turning globe, moving or not, no name floats
//!   in space: at most two of its box's corners and middle are space.
//!
//! [`LABEL_INSIDE_VIEW`], [`LABEL_CLEAR_OF_CONTROLS`] and
//! [`LABEL_OVERLAP`] judge moving frames too (order 1818): the names of
//! the last rest frame ride along while a finger moves, and the ride
//! must not push them over the edge, the buttons or each other.
//! [`LABEL_CURRENT_ZOOM`] judges a settled frame only, since names fade
//! by design while the map moves.
//!
//! Two rules watch a gesture frame by frame (order 1818):
//!
//! - [`GLOBE_DISK_FIXED`]: on a turning globe, the disk fitted from the
//!   frame's own pixels ([`disk_fit`]) stays where the gesture's
//!   reference frame at the same zoom put it, centre and radius within
//!   [`DISK_MAX_PX`]: the wobble proof.
//! - [`ZOOM_KEEPS_THE_PLACE`]: through a zoom gesture, the geographic
//!   point under the pinch focus stays under it within
//!   [`PLACE_MAX_PX`], in every frame, blended or not.
//! - [`BARE_PAPER`]: no more than [`BARE_MAX_PCT`] per cent of a gesture
//!   frame's screen is bare paper where the same view at rest shows map
//!   content ([`PaperMask`], [`bare_paper`]; the audit of order 1851: a
//!   pan over Berlin drew a nearly empty frame and no rule saw it).
//!
//! The gesture rules ([`sequence_rules`]):
//!
//! - [`FRAME_P95`]: the 95th percentile of the gesture's frame times is at
//!   most half a frame of the display (8.3 ms at 60 Hz, order 1853: the
//!   rule's own note named that line while it judged against the whole
//!   frame of 1799).
//! - [`HEAP_RETURNS`]: the live heap at the gesture's end is at most
//!   [`HEAP_KEPT_MAX_MB`] above its start.
//! - [`HEAP_PEAK`]: the live heap never grows more than [`HEAP_PEAK_MAX_MB`]
//!   above the gesture's start.
//! - [`VIEW_KEPT`]: a device rotation and back leaves the centre and zoom
//!   where they were.
//!
//! Host only: nothing of this is in the app.

use leviculum_map::labels::Rect;
use leviculum_map::outline::MAX_ZOOM;
use leviculum_map::viewport::MILLI;
use loquax::mapframe::DrawnName;

use crate::map::Canvas;
use crate::schema::MapShown;

/// `MapFrame.OVERSCAN`: the margin past each edge Java asks, as a share
/// of the screen side. A test reads the Java source so the two agree.
pub const OVERSCAN: f32 = 0.25;

/// `MapFrame.margin`: no margin while the last frame turned the globe,
/// a quarter of the side otherwise.
pub fn java_margin(side: u32, turns: bool) -> u32 {
    match turns {
        true => 0,
        false => (side as f32 * OVERSCAN).round() as u32,
    }
}

/// A pixel no frame draws: frames are opaque, this is transparent
/// magenta. The Bitmap is filled with it before each frame, and a screen
/// pixel Java has nothing for is it too.
pub const SENTINEL: [u8; 4] = [255, 0, 255, 0];

/// The Bitmap `MapView` makes before it asks for a frame: of the size Java
/// decided, cleared to [`SENTINEL`] before each frame. A frame of another
/// size fails with the words `bitmap.rs` says on the phone.
pub struct JavaBitmap {
    pub size: (u32, u32),
    pub rgba: Vec<u8>,
}

impl JavaBitmap {
    /// A Bitmap of `w` x `h`, all [`SENTINEL`].
    pub fn new(w: u32, h: u32) -> Self {
        let mut bitmap = Self {
            size: (w, h),
            rgba: Vec::new(),
        };
        bitmap.clear();
        bitmap
    }

    /// Every pixel [`SENTINEL`] again, before the next frame: outside the
    /// frame's time, as Java's spare Bitmap holds an old frame and costs
    /// the library nothing.
    pub fn clear(&mut self) {
        let len = self.size.0 as usize * self.size.1 as usize * 4;
        self.rgba.resize(len, 0);
        for px in self.rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&SENTINEL);
        }
    }
}

impl Canvas for JavaBitmap {
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String> {
        if self.size != (w, h) {
            return Err(format!(
                "map: the bitmap is {}x{}, the frame {w}x{h}",
                self.size.0, self.size.1
            ));
        }
        let len = w as usize * h as usize * 4;
        self.rgba.resize(len, 0);
        Ok(&mut self.rgba)
    }
}

/// What the screen shows, as `MapView` shows it: the view's part of the
/// last good frame, and while frames fail that frame scaled about the
/// screen's middle to the zoom asked since.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    /// The zoom, in thousandths, the picture was drawn at.
    pub zoom_milli: u32,
}

impl Screen {
    /// The view of a frame `fw` x `fh` that landed, its margins `mx`, `my`.
    pub fn landed(
        frame: &[u8],
        fw: u32,
        (mx, my): (u32, u32),
        (w, h): (u32, u32),
        zoom_milli: u32,
    ) -> Self {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            let row = (y + my) as usize * fw as usize * 4;
            let from = row + mx as usize * 4;
            let to = from + w as usize * 4;
            match frame.get(from..to) {
                Some(px) => rgba.extend_from_slice(px),
                None => (0..w).for_each(|_| rgba.extend_from_slice(&SENTINEL)),
            }
        }
        Self {
            w,
            h,
            rgba,
            zoom_milli,
        }
    }

    /// What a failed frame leaves on a `w` x `h` screen asked at
    /// `zoom_milli`: this picture scaled about the middle by the zoom
    /// between, the rest [`SENTINEL`]. Nothing at all before a first frame.
    pub fn failed(&self, (w, h): (u32, u32), zoom_milli: u32) -> Self {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        let by = (f64::from(zoom_milli) - f64::from(self.zoom_milli)) / f64::from(MILLI);
        let scale = by.exp2();
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        let (ox, oy) = (f64::from(self.w) / 2.0, f64::from(self.h) / 2.0);
        for y in 0..h {
            for x in 0..w {
                let sx = (f64::from(x) + 0.5 - cx) / scale + ox;
                let sy = (f64::from(y) + 0.5 - cy) / scale + oy;
                let inside =
                    sx >= 0.0 && sy >= 0.0 && sx < f64::from(self.w) && sy < f64::from(self.h);
                let px = match inside {
                    true => {
                        let i = (sy as usize * self.w as usize + sx as usize) * 4;
                        self.rgba.get(i..i + 4).unwrap_or(&SENTINEL)
                    }
                    false => &SENTINEL,
                };
                rgba.extend_from_slice(px);
            }
        }
        Self {
            w,
            h,
            rgba,
            zoom_milli: self.zoom_milli,
        }
    }
}

/// One rule's answer for one frame or gesture.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub rule: &'static str,
    pub value: f64,
    pub limit: f64,
    pub pass: bool,
    /// What failed, in words: the error, the names.
    pub note: String,
}

impl Verdict {
    fn at_most(rule: &'static str, value: f64, limit: f64, note: String) -> Self {
        Self {
            rule,
            value,
            limit,
            pass: value <= limit,
            note,
        }
    }
}

pub const BITMAP_IS_FRAME: &str = "bitmap_is_frame";
pub const VIEW_COVERED: &str = "view_covered";
pub const WORLD_SPANS_VIEW: &str = "world_spans_view";
pub const NO_ERROR_TEXT: &str = "no_error_text";
pub const NO_MAP_CHIP_OVER_DATA: &str = "no_map_chip_over_data";
pub const LABEL_INSIDE_VIEW: &str = "label_inside_view";
pub const LABEL_CLEAR_OF_CONTROLS: &str = "label_clear_of_controls";
pub const LABEL_OVERLAP: &str = "label_overlap";
pub const LABEL_CURRENT_ZOOM: &str = "label_current_zoom";
pub const LABEL_ON_WORLD: &str = "label_on_world";
pub const FRAME_P95: &str = "frame_p95";
pub const HEAP_RETURNS: &str = "heap_returns";
pub const HEAP_PEAK: &str = "heap_peak";
pub const VIEW_KEPT: &str = "view_kept";
pub const GLOBE_DISK_FIXED: &str = "globe_disk_fixed";
pub const ZOOM_KEEPS_THE_PLACE: &str = "zoom_keeps_the_place";
pub const BARE_PAPER: &str = "bare_paper";

/// Per cent of a gesture frame's screen that may be bare paper where the
/// same view at rest shows map content: a strip a pan uncovers before
/// its tiles land, not an emptied screen.
pub const BARE_MAX_PCT: f64 = 10.0;
/// The side in pixels of a [`PaperMask`]'s block.
pub const MASK_BLOCK: usize = 16;
/// Every how many pixels, each way, a block is sampled: 4 x 4 samples.
pub const MASK_STEP: usize = 4;
/// Samples of 16 that make a block bare paper: names over paper (thin
/// text, a quarter of a block at most) leave a block paper, so a rest
/// frame's names, which fade while a finger moves, count as no content
/// (the first full walk of order 1853 failed five world frames at z5 on
/// their names alone, 10.1 to 11.4 %).
pub const MASK_PAPER_SAMPLES: usize = 12;

/// Where a screen shows bare paper (the table's background, the land
/// where nothing is drawn), one bit per [`MASK_BLOCK`] block: small
/// enough to keep one per suspect frame of a gesture.
#[derive(Clone, Debug, PartialEq)]
pub struct PaperMask {
    bits: Vec<u64>,
    len: usize,
    paper: usize,
}

impl PaperMask {
    /// The mask of `screen` against the colour `paper`.
    pub fn of(screen: &Screen, paper: [u8; 3]) -> Self {
        Self::of_beneath(screen, paper, &[])
    }

    /// The mask of a rest frame's `screen` with its shown `names` (in
    /// frame pixels, the frame's margins `margins`) read as paper: names
    /// fade by design while a finger moves, so a gesture frame without
    /// them misses no map (the first full walk of order 1853 failed z5
    /// world frames on their names alone).
    pub fn of_rest(
        screen: &Screen,
        paper: [u8; 3],
        names: &[DrawnName],
        margins: (u32, u32),
    ) -> Self {
        let boxes: Vec<Rect> = names
            .iter()
            .filter(|n| n.shown && n.alpha > 0)
            .filter_map(|n| name_box(n, margins))
            .collect();
        Self::of_beneath(screen, paper, &boxes)
    }

    fn of_beneath(screen: &Screen, paper: [u8; 3], names: &[Rect]) -> Self {
        let (w, h) = (screen.w as usize, screen.h as usize);
        let mut bits = Vec::new();
        let (mut len, mut count) = (0usize, 0usize);
        let under_name = |x: usize, y: usize| {
            let (x, y) = (x as i32, y as i32);
            names
                .iter()
                .any(|r| r.x0 <= x && x < r.x1 && r.y0 <= y && y < r.y1)
        };
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            x < w && y < h && (screen.rgba.get(i..i + 3) == Some(&paper[..]) || under_name(x, y))
        };
        for by in (0..h).step_by(MASK_BLOCK) {
            for bx in (0..w).step_by(MASK_BLOCK) {
                let samples = (0..MASK_BLOCK)
                    .step_by(MASK_STEP)
                    .flat_map(|dy| (0..MASK_BLOCK).step_by(MASK_STEP).map(move |dx| (dx, dy)))
                    .filter(|&(dx, dy)| at(bx + dx, by + dy))
                    .count();
                let is = samples >= MASK_PAPER_SAMPLES;
                if len % 64 == 0 {
                    bits.push(0);
                }
                if is {
                    if let Some(word) = bits.last_mut() {
                        *word |= 1 << (len % 64);
                    }
                    count += 1;
                }
                len += 1;
            }
        }
        Self {
            bits,
            len,
            paper: count,
        }
    }

    /// Per cent of the blocks that are paper: an upper bound of
    /// [`bare_paper`]'s value against any rest frame.
    pub fn share_pct(&self) -> f64 {
        self.paper as f64 * 100.0 / self.len.max(1) as f64
    }
}

/// [`BARE_PAPER`] for a gesture frame's mask against the mask of the same
/// view drawn at rest: the per cent of the screen that is paper in the
/// gesture frame and map content at rest. Masks of different screens
/// fail outright.
pub fn bare_paper(moving: &PaperMask, rest: &PaperMask) -> Verdict {
    if moving.len != rest.len {
        return Verdict::at_most(
            BARE_PAPER,
            100.0,
            BARE_MAX_PCT,
            format!("masks of {} and {} samples", moving.len, rest.len),
        );
    }
    let bare: u32 = moving
        .bits
        .iter()
        .zip(&rest.bits)
        .map(|(m, r)| (m & !r).count_ones())
        .sum();
    let pct = f64::from(bare) * 100.0 / moving.len.max(1) as f64;
    Verdict::at_most(
        BARE_PAPER,
        pct,
        BARE_MAX_PCT,
        format!(
            "paper {:.1} % moving, {:.1} % at rest",
            moving.share_pct(),
            rest.share_pct()
        ),
    )
}

/// Pixels the fitted disk's centre or radius may move between a
/// gesture's frames at one zoom: the rim's smoothing, nothing a person
/// sees (order 1818, the wobble item).
pub const DISK_MAX_PX: f64 = 1.0;
/// Pixels the ground under a pinch's focus may wander over a zoom
/// gesture (order 1818, the Morocco-to-Spain jump).
pub const PLACE_MAX_PX: f64 = 2.0;

/// Per cent of the screen that may be undrawn: a pixel row's worth of
/// rounding at the edges, nothing a person sees.
pub const BLANK_MAX_PCT: f64 = 0.5;
/// Per cent of the globe's middle line that may be space: its rim's
/// smoothing.
pub const SPACE_MAX_PCT: f64 = 2.0;
/// Megabytes the live heap may keep after a gesture: the tile cache's
/// budget ([`loquax::tiles::BUDGET_MB`]) and the names' sprites with it.
pub const HEAP_KEPT_MAX_MB: f64 = 160.0;
/// Megabytes the live heap may grow during a gesture: the kept budget
/// and a few frames of the largest size in flight.
pub const HEAP_PEAK_MAX_MB: f64 = 320.0;

/// Everything a frame rule reads about one frame.
pub struct FrameFacts<'a> {
    pub shown: &'a MapShown,
    /// What the screen shows after it ([`Screen`]).
    pub screen: &'a Screen,
    /// The screen's size in pixels and its density.
    pub view: (u32, u32),
    pub density: f32,
    /// The names the frame drew, in its pixels ([`crate::map::drawn_names`]).
    pub names: &'a [DrawnName],
    /// The frame is at rest and its fades are done: the label rules apply.
    pub settled: bool,
    /// The space colour of the globe in the frame's table.
    pub space: [u8; 3],
}

/// The map's round buttons on a `w` x `h` screen at `density`, in screen
/// pixels: friends at the bottom end, locate above it.
pub fn controls((w, h): (u32, u32), density: f32) -> [Rect; 2] {
    let dp = |v: f32| (v * density).round() as i32;
    let (w, h) = (w as i32, h as i32);
    let button = |bottom: f32| Rect {
        x0: w - dp(16.0 + 56.0),
        y0: h - dp(bottom + 56.0),
        x1: w - dp(16.0),
        y1: h - dp(bottom),
    };
    [button(16.0), button(84.0)]
}

/// The union of a name's boxes, moved from frame pixels to screen pixels.
fn name_box(name: &DrawnName, (mx, my): (u32, u32)) -> Option<Rect> {
    let (mx, my) = (mx as i32, my as i32);
    let first = name.boxes.first()?;
    let mut r = Rect {
        x0: first.x0,
        y0: first.y0,
        x1: first.x1,
        y1: first.y1,
    };
    for b in &name.boxes {
        r.x0 = r.x0.min(b.x0);
        r.y0 = r.y0.min(b.y0);
        r.x1 = r.x1.max(b.x1);
        r.y1 = r.y1.max(b.y1);
    }
    Some(Rect {
        x0: r.x0 - mx,
        y0: r.y0 - my,
        x1: r.x1 - mx,
        y1: r.y1 - my,
    })
}

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

/// The frame rules of the module's list for one frame.
pub fn frame_rules(f: &FrameFacts<'_>) -> Vec<Verdict> {
    let mut out = Vec::new();
    let error = f.shown.error.clone().unwrap_or_default();
    let misfit = !f.shown.ok && error.contains("the bitmap is");
    out.push(Verdict::at_most(
        BITMAP_IS_FRAME,
        f64::from(u8::from(misfit)),
        0.0,
        if misfit { error.clone() } else { String::new() },
    ));
    let pixels = (f.screen.rgba.len() / 4).max(1);
    let blank = f
        .screen
        .rgba
        .chunks_exact(4)
        .filter(|px| *px == SENTINEL)
        .count()
        + (f.view.0 as usize * f.view.1 as usize).saturating_sub(f.screen.rgba.len() / 4);
    out.push(Verdict::at_most(
        VIEW_COVERED,
        blank as f64 * 100.0 / pixels as f64,
        BLANK_MAX_PCT,
        String::new(),
    ));
    let space = match f.shown.turns && f.shown.ok {
        true => space_on_middle_line(f.screen, f.space),
        false => 0.0,
    };
    out.push(Verdict::at_most(
        WORLD_SPANS_VIEW,
        space,
        SPACE_MAX_PCT,
        String::new(),
    ));
    let words: Vec<String> = [
        (!f.shown.ok && !misfit).then(|| error.clone()),
        f.shown.notice.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    out.push(Verdict::at_most(
        NO_ERROR_TEXT,
        words.len() as f64,
        0.0,
        words.join(" | "),
    ));
    let chip = f.shown.ok && f.shown.no_map && f.shown.zoom_milli <= u32::from(MAX_ZOOM) * MILLI;
    out.push(Verdict::at_most(
        NO_MAP_CHIP_OVER_DATA,
        f64::from(u8::from(chip)),
        0.0,
        String::new(),
    ));
    if f.shown.ok {
        out.extend(label_rules(f));
    }
    if f.shown.turns && f.shown.ok {
        let adrift = names_in_space(f);
        out.push(Verdict::at_most(
            LABEL_ON_WORLD,
            adrift.len() as f64,
            0.0,
            adrift.join(", "),
        ));
    }
    out
}

/// The shown names on screen whose box has three or more of its four
/// corners and its middle on the globe's space colour.
fn names_in_space(f: &FrameFacts<'_>) -> Vec<String> {
    let (w, h) = (f.screen.w as i32, f.screen.h as i32);
    let space = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w || y >= h {
            return false;
        }
        let i = (y as usize * w as usize + x as usize) * 4;
        f.screen.rgba.get(i..i + 3) == Some(&f.space[..])
    };
    f.names
        .iter()
        .filter(|n| n.alpha > 0)
        .filter_map(|n| name_box(n, (f.shown.margin_x, f.shown.margin_y)).map(|b| (n, b)))
        .filter(|(_, b)| b.x1 > 0 && b.y1 > 0 && b.x0 < w && b.y0 < h)
        .filter(|(_, b)| {
            let (x1, y1) = (b.x1 - 1, b.y1 - 1);
            let points = [
                (b.x0, b.y0),
                (x1, b.y0),
                (b.x0, y1),
                (x1, y1),
                ((b.x0 + x1) / 2, (b.y0 + y1) / 2),
            ];
            points.iter().filter(|&&(x, y)| space(x, y)).count() >= 3
        })
        .map(|(n, _)| n.text.clone())
        .collect()
}

/// Per cent of the screen's middle line across its shorter side that is
/// the globe's space colour.
fn space_on_middle_line(screen: &Screen, space: [u8; 3]) -> f64 {
    let (w, h) = (screen.w as usize, screen.h as usize);
    let at = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        screen.rgba.get(i..i + 3)
    };
    let line: Vec<Option<&[u8]>> = match w <= h {
        true => (0..w).map(|x| at(x, h / 2)).collect(),
        false => (0..h).map(|y| at(w / 2, y)).collect(),
    };
    let n = line.len().max(1);
    let spaced = line.iter().filter(|px| **px == Some(&space[..])).count();
    spaced as f64 * 100.0 / n as f64
}

/// The label rules of a settled frame.
fn label_rules(f: &FrameFacts<'_>) -> Vec<Verdict> {
    let margin = (f.shown.margin_x, f.shown.margin_y);
    let screen = Rect {
        x0: 0,
        y0: 0,
        x1: f.view.0 as i32,
        y1: f.view.1 as i32,
    };
    let shown: Vec<(&DrawnName, Rect)> = f
        .names
        .iter()
        .filter(|n| n.alpha > 0)
        .filter_map(|n| name_box(n, margin).map(|b| (n, b)))
        .filter(|(_, b)| intersects(b, &screen))
        .collect();
    let straddle: Vec<&str> = shown
        .iter()
        .filter(|(_, b)| b.x0 < 0 || b.y0 < 0 || b.x1 > screen.x1 || b.y1 > screen.y1)
        .map(|(n, _)| n.text.as_str())
        .collect();
    let buttons = controls(f.view, f.density);
    let under: Vec<&str> = shown
        .iter()
        .filter(|(_, b)| buttons.iter().any(|c| intersects(b, c)))
        .map(|(n, _)| n.text.as_str())
        .collect();
    let mut pairs = Vec::new();
    for (i, (a, _)) in shown.iter().enumerate() {
        for (b, _) in shown.iter().skip(i + 1) {
            let hit = a
                .boxes
                .iter()
                .any(|p| b.boxes.iter().any(|q| intersects(p, q)));
            if hit {
                pairs.push(format!("{}/{}", a.text, b.text));
            }
        }
    }
    let list = |v: &[&str]| v.join(", ");
    let mut out = vec![
        Verdict::at_most(
            LABEL_INSIDE_VIEW,
            straddle.len() as f64,
            0.0,
            list(&straddle),
        ),
        Verdict::at_most(
            LABEL_CLEAR_OF_CONTROLS,
            under.len() as f64,
            0.0,
            list(&under),
        ),
        Verdict::at_most(LABEL_OVERLAP, pairs.len() as f64, 0.0, pairs.join(", ")),
    ];
    // Only a settled frame owes current names: they fade by design
    // while the map moves (order 1818 judges the three rules above on
    // moving frames too).
    if f.settled {
        let stale: Vec<String> = shown
            .iter()
            .filter(|(n, _)| !n.shown || n.min_zoom > f.shown.zoom)
            .map(|(n, _)| format!("{} (z{})", n.text, n.min_zoom))
            .collect();
        out.push(Verdict::at_most(
            LABEL_CURRENT_ZOOM,
            stale.len() as f64,
            0.0,
            stale.join(", "),
        ));
    }
    out
}

/// The globe's disk fitted from a frame's own pixels: its centre and
/// radius in screen pixels, by a least-squares circle (Kåsa) through
/// the space-to-world boundary, sampled down every column from the top
/// edge and up from the bottom, and across every row from the left and
/// right. `None` without enough boundary (no space in view, or no
/// world).
pub fn disk_fit(screen: &Screen, space: [u8; 3]) -> Option<(f64, f64, f64)> {
    let (w, h) = (screen.w as usize, screen.h as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let is_space = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        screen.rgba.get(i..i + 3) == Some(&space[..])
    };
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for x in 0..w {
        if is_space(x, 0) {
            if let Some(y) = (0..h).find(|&y| !is_space(x, y)) {
                pts.push((x as f64 + 0.5, y as f64));
            }
        }
        if is_space(x, h - 1) {
            if let Some(y) = (0..h).rev().find(|&y| !is_space(x, y)) {
                pts.push((x as f64 + 0.5, y as f64 + 1.0));
            }
        }
    }
    for y in 0..h {
        if is_space(0, y) {
            if let Some(x) = (0..w).find(|&x| !is_space(x, y)) {
                pts.push((x as f64, y as f64 + 0.5));
            }
        }
        if is_space(w - 1, y) {
            if let Some(x) = (0..w).rev().find(|&x| !is_space(x, y)) {
                pts.push((x as f64 + 1.0, y as f64 + 0.5));
            }
        }
    }
    if pts.len() < 20 {
        return None;
    }
    // Kåsa: minimise Σ (x² + y² + D x + E y + F)²; centre (-D/2, -E/2).
    let n = pts.len() as f64;
    let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut sxz, mut syz, mut sz) = (0.0, 0.0, 0.0);
    for &(x, y) in &pts {
        let z = x * x + y * y;
        sx += x;
        sy += y;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
        sxz += x * z;
        syz += y * z;
        sz += z;
    }
    let (a11, a12, a13) = (sxx, sxy, sx);
    let (a21, a22, a23) = (sxy, syy, sy);
    let (a31, a32, a33) = (sx, sy, n);
    let (b1, b2, b3) = (sxz, syz, sz);
    let det = a11 * (a22 * a33 - a23 * a32) - a12 * (a21 * a33 - a23 * a31)
        + a13 * (a21 * a32 - a22 * a31);
    if det.abs() < 1e-9 {
        return None;
    }
    let d = (b1 * (a22 * a33 - a23 * a32) - a12 * (b2 * a33 - a23 * b3)
        + a13 * (b2 * a32 - a22 * b3))
        / det;
    let e = (a11 * (b2 * a33 - a23 * b3) - b1 * (a21 * a33 - a23 * a31)
        + a13 * (a21 * b3 - b2 * a31))
        / det;
    let f = (a11 * (a22 * b3 - b2 * a32) - a12 * (a21 * b3 - b2 * a31)
        + b1 * (a21 * a32 - a22 * a31))
        / det;
    let (cx, cy) = (d / 2.0, e / 2.0);
    let r2 = cx * cx + cy * cy + f;
    if r2 <= 0.0 {
        return None;
    }
    Some((cx, cy, r2.sqrt()))
}

/// [`GLOBE_DISK_FIXED`]: how far the disk of `now` is from the disk of
/// the gesture's reference frame at the same zoom, the worst of centre
/// x, centre y and radius.
pub fn globe_disk_fixed(now: (f64, f64, f64), reference: (f64, f64, f64)) -> Verdict {
    let value = (now.0 - reference.0)
        .abs()
        .max((now.1 - reference.1).abs())
        .max((now.2 - reference.2).abs());
    Verdict::at_most(
        GLOBE_DISK_FIXED,
        value,
        DISK_MAX_PX,
        format!(
            "centre ({:.1}, {:.1}) r {:.1}, reference ({:.1}, {:.1}) r {:.1}",
            now.0, now.1, now.2, reference.0, reference.1, reference.2
        ),
    )
}

/// [`GLOBE_DISK_FIXED`] for a CENTRED zoom gesture (audit of 1818,
/// condition 12): the disk grows and shrinks about its centre, so the
/// radius is free, but the fitted centre holds to the rest frame's
/// within [`DISK_MAX_PX`]. The note reports both fits, so the row
/// carries the deviation per gesture frame and size.
pub fn globe_disk_centred(now: (f64, f64, f64), reference: (f64, f64, f64)) -> Verdict {
    let value = (now.0 - reference.0).abs().max((now.1 - reference.1).abs());
    Verdict::at_most(
        GLOBE_DISK_FIXED,
        value,
        DISK_MAX_PX,
        format!(
            "zooming: centre ({:.1}, {:.1}) r {:.1}, at rest ({:.1}, {:.1}) r {:.1}",
            now.0, now.1, now.2, reference.0, reference.1, reference.2
        ),
    )
}

/// [`ZOOM_KEEPS_THE_PLACE`]: how far the ground that started under the
/// pinch focus is from it in this frame, screen pixels.
pub fn zoom_keeps_the_place(off: f64, note: String) -> Verdict {
    Verdict::at_most(ZOOM_KEEPS_THE_PLACE, off, PLACE_MAX_PX, note)
}

/// What a gesture measured: its frame times and the live heap.
#[derive(Clone, Debug, Default)]
pub struct Played {
    pub frame_ms: Vec<f64>,
    /// The display's rate the frames are held to.
    pub hz: f64,
    pub heap_start: usize,
    pub heap_peak: usize,
    pub heap_end: usize,
    /// For a rotation: how far the view moved, in pixels of the screen,
    /// and its zoom in thousandths, from before to after.
    pub moved_px: Option<f64>,
    pub zoom_moved_milli: Option<u32>,
}

/// The `q` quantile of `ms` (nearest rank), 0 for none.
pub fn quantile(ms: &[f64], q: f64) -> f64 {
    let mut v = ms.to_vec();
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => 0.0,
        n => v[((n - 1) as f64 * q).round() as usize],
    }
}

/// The gesture rules of the module's list.
pub fn sequence_rules(p: &Played) -> Vec<Verdict> {
    let mb = |b: usize| b as f64 / (1u64 << 20) as f64;
    // The gate is half a frame of the display (8.3 ms at 60 Hz, order
    // 1853), the line the note always named.
    let whole = 1000.0 / p.hz.max(1.0);
    let p95 = quantile(&p.frame_ms, 0.95);
    let mut out = vec![
        Verdict::at_most(
            FRAME_P95,
            p95,
            whole / 2.0,
            format!(
                "p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms, {} frames, target {:.0} Hz, half frame {:.1} ms",
                quantile(&p.frame_ms, 0.5),
                p95,
                quantile(&p.frame_ms, 1.0),
                p.frame_ms.len(),
                p.hz,
                whole / 2.0
            ),
        ),
        Verdict::at_most(
            HEAP_RETURNS,
            mb(p.heap_end.saturating_sub(p.heap_start)),
            HEAP_KEPT_MAX_MB,
            format!(
                "start {:.0} MB, peak {:.0} MB, end {:.0} MB",
                mb(p.heap_start),
                mb(p.heap_peak),
                mb(p.heap_end)
            ),
        ),
        Verdict::at_most(
            HEAP_PEAK,
            mb(p.heap_peak.saturating_sub(p.heap_start)),
            HEAP_PEAK_MAX_MB,
            String::new(),
        ),
    ];
    if let (Some(px), Some(z)) = (p.moved_px, p.zoom_moved_milli) {
        out.push(Verdict::at_most(
            VIEW_KEPT,
            px + f64::from(z),
            1.0,
            format!("moved {px:.1} px, zoom by {z} milli"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::labels::LabelKind;

    fn shown(zoom: u8) -> MapShown {
        MapShown {
            ok: true,
            w: 200,
            h: 300,
            margin_x: 50,
            margin_y: 75,
            zoom,
            zoom_milli: u32::from(zoom) * MILLI,
            ..MapShown::default()
        }
    }

    fn name(text: &str, min_zoom: u8, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> DrawnName {
        DrawnName {
            text: text.to_string(),
            kind: LabelKind::City,
            min_zoom,
            at: (0.0, 0.0),
            angle: 0.0,
            boxes: vec![Rect { x0, y0, x1, y1 }],
            alpha: 255,
            shown: true,
        }
    }

    fn filled(w: u32, h: u32, px: [u8; 4]) -> Screen {
        Screen {
            w,
            h,
            rgba: px.repeat(w as usize * h as usize),
            zoom_milli: 5000,
        }
    }

    fn verdict(v: &[Verdict], rule: &str) -> Verdict {
        v.iter().find(|v| v.rule == rule).cloned().expect(rule)
    }

    /// A frame with nothing wrong: every rule green.
    fn clean() -> (MapShown, Screen, Vec<DrawnName>) {
        // Frame pixels: the screen starts at (50, 75).
        let names = vec![
            name("A", 5, (60, 85, 100, 95)),
            name("B", 3, (60, 120, 100, 130)),
        ];
        (shown(5), filled(300, 400, [9, 9, 9, 255]), names)
    }

    fn rules(s: &MapShown, screen: &Screen, names: &[DrawnName]) -> Vec<Verdict> {
        frame_rules(&FrameFacts {
            shown: s,
            screen,
            view: (300, 400),
            density: 1.0,
            names,
            settled: true,
            space: [1, 2, 3],
        })
    }

    #[test]
    fn a_clean_frame_passes_every_rule() {
        let (s, screen, names) = clean();
        let v = rules(&s, &screen, &names);
        assert_eq!(v.len(), 9);
        assert!(v.iter().all(|v| v.pass), "{v:?}");
    }

    #[test]
    fn a_bitmap_of_another_size_fails_and_says_so() {
        let mut bitmap = JavaBitmap::new(300, 450);
        let error = bitmap.pixels(200, 300).err().unwrap_or_default();
        assert_eq!(error, "map: the bitmap is 300x450, the frame 200x300");
        assert!(bitmap.pixels(300, 450).is_ok());
        let (_, screen, names) = clean();
        let failed = MapShown {
            ok: false,
            error: Some(error),
            ..shown(5)
        };
        let v = rules(&failed, &screen, &names);
        assert!(!verdict(&v, BITMAP_IS_FRAME).pass);
        assert!(
            verdict(&v, NO_ERROR_TEXT).pass,
            "the misfit is its own rule"
        );
    }

    #[test]
    fn a_blank_half_fails_the_cover() {
        let (s, mut screen, names) = clean();
        let half = screen.rgba.len() / 2;
        for px in screen.rgba[half..].chunks_exact_mut(4) {
            px.copy_from_slice(&SENTINEL);
        }
        let v = verdict(&rules(&s, &screen, &names), VIEW_COVERED);
        assert!(!v.pass && (v.value - 50.0).abs() < 1.0, "{v:?}");
    }

    #[test]
    fn a_failed_frame_shrinks_the_last_picture_to_a_rectangle() {
        let good = filled(300, 400, [9, 9, 9, 255]);
        // Two steps out: a quarter of each side is left, the rest blank.
        let shrunk = good.failed((300, 400), 3000);
        let blank = shrunk
            .rgba
            .chunks_exact(4)
            .filter(|p| *p == SENTINEL)
            .count();
        let share = blank as f64 / (300.0 * 400.0);
        assert!((share - 15.0 / 16.0).abs() < 0.02, "{share}");
        let (s, _, names) = clean();
        assert!(!verdict(&rules(&s, &shrunk, &names), VIEW_COVERED).pass);
        let same = good.failed((300, 400), 5000);
        assert_eq!(same.rgba, good.rgba);
    }

    #[test]
    fn space_across_a_turning_globe_fails_the_span() {
        let (mut s, mut screen, names) = clean();
        s.turns = true;
        assert!(verdict(&rules(&s, &screen, &names), WORLD_SPANS_VIEW).pass);
        // The middle row's left fifth is space: a disk narrower than the view.
        let row = 200 * 300 * 4;
        for px in screen.rgba[row..row + 60 * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[1, 2, 3, 255]);
        }
        let v = verdict(&rules(&s, &screen, &names), WORLD_SPANS_VIEW);
        assert!(!v.pass && (v.value - 20.0).abs() < 0.1, "{v:?}");
        s.turns = false;
        assert!(verdict(&rules(&s, &screen, &names), WORLD_SPANS_VIEW).pass);
    }

    #[test]
    fn a_name_in_space_beside_a_turning_globe_fails_on_world() {
        let (mut s, mut screen, mut names) = clean();
        // The screen's top band (rows 0..40) is space; A sits in it.
        for px in screen.rgba[..300 * 40 * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[1, 2, 3, 255]);
        }
        names.truncate(1);
        assert!(
            rules(&s, &screen, &names)
                .iter()
                .all(|v| v.rule != LABEL_ON_WORLD),
            "a flat map has no space to float in"
        );
        s.turns = true;
        let v = verdict(&rules(&s, &screen, &names), LABEL_ON_WORLD);
        assert!(!v.pass && v.note == "A", "{v:?}");
        // B is on the world.
        let (s, _, names) = clean();
        let s = MapShown { turns: true, ..s };
        let mut screen = filled(300, 400, [9, 9, 9, 255]);
        for px in screen.rgba[..300 * 40 * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[1, 2, 3, 255]);
        }
        let v = verdict(&rules(&s, &screen, &names[1..]), LABEL_ON_WORLD);
        assert!(v.pass, "{v:?}");
    }

    #[test]
    fn a_notice_or_an_error_fails_the_text_rule() {
        let (mut s, screen, names) = clean();
        s.notice = Some("map: this area is loaded already".to_string());
        assert!(!verdict(&rules(&s, &screen, &names), NO_ERROR_TEXT).pass);
        let (_, screen, names) = clean();
        let s = MapShown {
            ok: false,
            error: Some("map: no bitmap to draw into".to_string()),
            ..shown(5)
        };
        assert!(!verdict(&rules(&s, &screen, &names), NO_ERROR_TEXT).pass);
    }

    #[test]
    fn the_no_map_chip_fails_only_where_the_outline_draws() {
        let (mut s, screen, names) = clean();
        s.no_map = true;
        assert!(!verdict(&rules(&s, &screen, &names), NO_MAP_CHIP_OVER_DATA).pass);
        s.zoom_milli = (u32::from(MAX_ZOOM) + 1) * MILLI;
        s.zoom = MAX_ZOOM + 1;
        assert!(verdict(&rules(&s, &screen, &names), NO_MAP_CHIP_OVER_DATA).pass);
    }

    #[test]
    fn a_name_across_the_edge_fails_inside_view() {
        let (s, screen, mut names) = clean();
        names.push(name("Murmansk", 3, (330, 100, 370, 110)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_INSIDE_VIEW);
        assert!(!v.pass && v.note == "Murmansk", "{v:?}");
        // Wholly in the margin: not on screen, not counted.
        let (s, screen, mut names) = clean();
        names.push(name("Far", 3, (0, 0, 40, 10)));
        assert!(verdict(&rules(&s, &screen, &names), LABEL_INSIDE_VIEW).pass);
    }

    #[test]
    fn a_name_under_a_button_fails_the_controls() {
        let (s, screen, mut names) = clean();
        // The friends button of a 300 x 400 screen at density 1 is
        // x 228..284, y 328..384; in frame pixels plus (50, 75).
        names.push(name("Kosovo", 3, (290, 410, 320, 420)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_CLEAR_OF_CONTROLS);
        assert!(!v.pass && v.note == "Kosovo", "{v:?}");
        assert_eq!(
            controls((300, 400), 1.0),
            [
                Rect {
                    x0: 228,
                    y0: 328,
                    x1: 284,
                    y1: 384
                },
                Rect {
                    x0: 228,
                    y0: 260,
                    x1: 284,
                    y1: 316
                }
            ]
        );
    }

    #[test]
    fn two_names_on_one_spot_fail_the_overlap() {
        let (s, screen, mut names) = clean();
        names.push(name("Wümme", 5, (70, 88, 110, 98)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_OVERLAP);
        assert!(!v.pass && v.note == "A/Wümme", "{v:?}");
    }

    #[test]
    fn a_name_of_a_deeper_zoom_or_still_fading_fails_after_the_settle() {
        let (s, screen, mut names) = clean();
        names.push(name("Eckstever Straße", 16, (60, 140, 100, 150)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_CURRENT_ZOOM);
        assert!(!v.pass && v.note == "Eckstever Straße (z16)", "{v:?}");
        let (s, screen, mut names) = clean();
        names[1].shown = false;
        assert!(!verdict(&rules(&s, &screen, &names), LABEL_CURRENT_ZOOM).pass);
        // A moving frame is not judged by its names.
        let (s, screen, mut names) = clean();
        names.push(name("Eckstever Straße", 16, (60, 140, 100, 150)));
        let v = frame_rules(&FrameFacts {
            shown: &s,
            screen: &screen,
            view: (300, 400),
            density: 1.0,
            names: &names,
            settled: false,
            space: [1, 2, 3],
        });
        assert!(v.iter().all(|v| v.rule != LABEL_CURRENT_ZOOM));
    }

    /// A gesture frame emptied to paper where the rest frame shows the
    /// map fails [`BARE_PAPER`]; paper the rest frame shows too (a sea
    /// of nothing at that zoom) does not count, and a thin uncovered
    /// strip passes (the audit of order 1851: a pan frame nearly blank).
    #[test]
    fn a_gesture_frame_of_bare_paper_fails_where_the_rest_has_map() {
        let paper = [245, 241, 233];
        let (w, h) = (64u32, 256u32);
        let screen = |paper_rows: u32| {
            let mut s = filled(w, h, [200, 180, 160, 255]);
            for y in 0..paper_rows {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    s.rgba[i..i + 4].copy_from_slice(&[245, 241, 233, 255]);
                }
            }
            s
        };
        let rest = PaperMask::of(&screen(0), paper);
        let blank = PaperMask::of(&screen(h), paper);
        assert!((blank.share_pct() - 100.0).abs() < 1e-9);
        let v = bare_paper(&blank, &rest);
        assert!(!v.pass && (v.value - 100.0).abs() < 1e-9, "{v:?}");
        // The rest frame is paper there too: nothing is missing.
        assert!(bare_paper(&blank, &blank).pass);
        // A strip of one block row of 16 (6 %) uncovered by a pan passes.
        let strip = PaperMask::of(&screen(16), paper);
        let v = bare_paper(&strip, &rest);
        assert!(v.pass && v.value > 0.0, "{v:?}");
        // Half the screen does not.
        assert!(!bare_paper(&PaperMask::of(&screen(128), paper), &rest).pass);
        // Names over paper at rest (a text row through every block,
        // a quarter of its samples) still leave the rest frame paper.
        let mut named = screen(h);
        for y in (0..h).step_by(16) {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                named.rgba[i..i + 4].copy_from_slice(&[40, 40, 40, 255]);
            }
        }
        assert!(bare_paper(&blank, &PaperMask::of(&named, paper)).pass);
        // Dense names at rest (a whole block of text) are content to
        // the plain mask but paper beneath their boxes.
        let mut dense = screen(h);
        for y in 0..32 {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                dense.rgba[i..i + 4].copy_from_slice(&[40, 40, 40, 255]);
            }
        }
        assert!(!bare_paper(&blank, &PaperMask::of(&dense, paper)).pass);
        let label = name("Berlin", 5, (0, 0, w as i32, 32));
        let rest = PaperMask::of_rest(&dense, paper, &[label], (0, 0));
        assert!(bare_paper(&blank, &rest).pass);
        // Masks of different screens fail outright.
        let other = PaperMask::of(&filled(32, 32, [0, 0, 0, 255]), paper);
        assert!(!bare_paper(&blank, &other).pass);
    }

    #[test]
    fn the_gesture_rules_hold_time_heap_and_view() {
        let mb = 1usize << 20;
        let good = Played {
            frame_ms: vec![4.0; 40],
            hz: 60.0,
            heap_start: 100 * mb,
            heap_peak: 200 * mb,
            heap_end: 120 * mb,
            moved_px: Some(0.0),
            zoom_moved_milli: Some(0),
        };
        let v = sequence_rules(&good);
        assert_eq!(v.len(), 4);
        assert!(v.iter().all(|v| v.pass), "{v:?}");
        let mut slow = good.clone();
        slow.frame_ms = vec![18.0; 40];
        assert!(!verdict(&sequence_rules(&slow), FRAME_P95).pass);
        // Order 1853: the gate is half a frame (8.3 ms at 60 Hz), the
        // line the note names; 9 ms fails, 8 ms passes.
        let mut half = good.clone();
        half.frame_ms = vec![9.0; 40];
        let v = verdict(&sequence_rules(&half), FRAME_P95);
        assert!(!v.pass, "{v:?}");
        assert!((v.limit - 1000.0 / 120.0).abs() < 1e-9, "{v:?}");
        assert!(v.note.contains("half frame 8.3 ms"), "{v:?}");
        half.frame_ms = vec![8.0; 40];
        assert!(verdict(&sequence_rules(&half), FRAME_P95).pass);
        let mut kept = good.clone();
        kept.heap_end = 300 * mb;
        assert!(!verdict(&sequence_rules(&kept), HEAP_RETURNS).pass);
        let mut grown = good.clone();
        grown.heap_peak = 500 * mb;
        assert!(!verdict(&sequence_rules(&grown), HEAP_PEAK).pass);
        let mut lost = good;
        lost.moved_px = Some(12.0);
        assert!(!verdict(&sequence_rules(&lost), VIEW_KEPT).pass);
    }

    /// A disk drawn into space is fitted to within a pixel; the same
    /// disk three pixels over turns [`GLOBE_DISK_FIXED`] red (order
    /// 1818, the wobble item's positive control); all world or all
    /// space fits nothing.
    #[test]
    fn the_disk_fit_finds_a_drawn_circle_and_a_shift_turns_the_rule_red() {
        let space = [1, 2, 3];
        let disk = |cx: f64, cy: f64, r: f64| {
            let mut screen = filled(200, 300, [1, 2, 3, 255]);
            for y in 0..300usize {
                for x in 0..200usize {
                    let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                    if dx * dx + dy * dy <= r * r {
                        let i = (y * 200 + x) * 4;
                        screen.rgba[i..i + 4].copy_from_slice(&[9, 9, 9, 255]);
                    }
                }
            }
            screen
        };
        let at = disk_fit(&disk(100.0, 150.0, 90.0), space).expect("a fit");
        assert!(
            (at.0 - 100.0).abs() < 0.6 && (at.1 - 150.0).abs() < 0.6 && (at.2 - 90.0).abs() < 1.0,
            "{at:?}"
        );
        let same = disk_fit(&disk(100.0, 150.0, 90.0), space).expect("a fit");
        assert!(globe_disk_fixed(same, at).pass);
        let shifted = disk_fit(&disk(103.0, 150.0, 90.0), space).expect("a fit");
        let v = globe_disk_fixed(shifted, at);
        assert!(!v.pass && (v.value - 3.0).abs() < 0.5, "{v:?}");
        assert!(disk_fit(&filled(200, 300, [9, 9, 9, 255]), space).is_none());
        assert!(disk_fit(&filled(200, 300, [1, 2, 3, 255]), space).is_none());
    }

    /// The ground under the pinch focus may wander two pixels, no more
    /// (order 1818, the Morocco-to-Spain jump).
    #[test]
    fn the_place_rule_holds_two_pixels() {
        assert!(zoom_keeps_the_place(1.9, String::new()).pass);
        assert!(!zoom_keeps_the_place(2.1, String::new()).pass);
    }

    /// Order 1818: the edge, button and overlap rules judge a moving
    /// frame too; only the current-zoom rule waits for the settle.
    #[test]
    fn a_moving_frame_is_judged_by_edge_button_and_overlap() {
        let (s, screen, mut names) = clean();
        names.push(name("Murmansk", 3, (330, 100, 370, 110)));
        let v = frame_rules(&FrameFacts {
            shown: &s,
            screen: &screen,
            view: (300, 400),
            density: 1.0,
            names: &names,
            settled: false,
            space: [1, 2, 3],
        });
        assert!(!verdict(&v, LABEL_INSIDE_VIEW).pass);
        assert!(v.iter().all(|v| v.rule != LABEL_CURRENT_ZOOM));
    }

    #[test]
    fn the_quantiles_are_the_nearest_rank() {
        let ms: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(quantile(&ms, 0.5), 51.0);
        assert_eq!(quantile(&ms, 0.95), 95.0);
        assert_eq!(quantile(&ms, 1.0), 100.0);
        assert_eq!(quantile(&[], 0.5), 0.0);
    }

    /// The margins mirror `MapFrame.java`'s: the same share, none while
    /// the globe turns.
    #[test]
    fn the_margins_are_the_java_ones() {
        let java = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/android/src/network/leviculum/loquax/MapFrame.java"
        ))
        .expect("MapFrame.java");
        assert!(
            java.contains(&format!("static final float OVERSCAN = {OVERSCAN}f;")),
            "MapFrame.OVERSCAN moved"
        );
        assert!(java.contains("return turns ? 0 : Math.round(side * OVERSCAN);"));
        assert_eq!(java_margin(1224, false), 306);
        assert_eq!(java_margin(2197, false), 549);
        assert_eq!(java_margin(2197, true), 0);
    }
}
