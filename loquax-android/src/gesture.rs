//! MapView.java's gesture arithmetic, mirrored (order 1833).
//!
//! The device does NOT send each pinch event to the library: MapView
//! accumulates every gesture event since the last frame into one affine
//! transform of the drawn frame, `screen = scale * p + off`, and each
//! frame request carries that transform encoded as "pan, then zoom
//! about the focus" (`MapView.java`, `zoomAbout`, `onScroll`,
//! `drawFrames`, `place`). The render walk and the facade tests that
//! judge the PHONE's pinch must drive the library through this
//! arithmetic — driving `zoom_about` directly, as the walk did before
//! this order, judges a path the device never takes, which is how the
//! walk stayed green (0 of 670) while the APK lost Morocco by 300 km
//! (the reviewer's device finding of 2026-11-14).
//!
//! Every method cites the lines of `MapView.java` it mirrors; a change
//! there changes this file in the same commit, or the walk drifts from
//! the device again.

use crate::schema::{MapRequest, MapShown};

/// `MapFrame.OVERSCAN` (`MapFrame.java:21`).
const OVERSCAN: f64 = 0.25;

/// `MapFrame.margin` (`MapFrame.java:31-33`): none while the last frame
/// turned the globe, a quarter of the side otherwise.
pub fn margin(side: u32, turns: bool) -> u32 {
    match turns {
        true => 0,
        #[allow(clippy::cast_sign_loss)]
        false => (side as f64 * OVERSCAN).round() as u32,
    }
}

/// `MapFrame.dragSince` (`MapFrame.java:64-67`, order 1883): one axis of
/// the drag made since the request, which the frame answering it did not
/// absorb. `off - pinch` is the drag share; a zoom scales it as it scales
/// `scale`.
fn drag_since(
    off: f64,
    pinch: f64,
    scale: f64,
    asked_off: f64,
    asked_pinch: f64,
    asked_scale: f64,
) -> f64 {
    (off - pinch) - scale / asked_scale * (asked_off - asked_pinch)
}

/// `MapFrame.turnsOnly` (`MapFrame.java:78-80`, order 1883): the landing
/// frame answers a pure turn, drawn in place whatever the held ground says.
#[allow(clippy::float_cmp)]
fn turns_only(
    turns: bool,
    asked_scale: f64,
    scale: f64,
    zoom_before: i64,
    zoom_after: i64,
) -> bool {
    turns && asked_scale == 1.0 && scale == 1.0 && zoom_before == zoom_after
}

/// `MapFrame.zoomBy` (`MapFrame.java:90-99`): the factor a pinch may
/// scale by, never below the zoom where the world fills the screen.
fn zoom_by(scale: f64, by: f64, zoom_milli: i64, min_zoom_milli: i64) -> f64 {
    if min_zoom_milli <= 0 || scale <= 0.0 {
        return by;
    }
    #[allow(clippy::cast_precision_loss)]
    let least = 2f64.powf((min_zoom_milli - zoom_milli) as f64 / 1000.0);
    if scale * by >= least {
        return by;
    }
    least / scale
}

/// What the last request carried (`MapView.java`, the asked* fields).
#[derive(Clone, Copy)]
struct Asked {
    off: (f64, f64),
    /// The about point (to a thousandth) and the pan in full precision:
    /// the ground drawn at (about + pan) of the then-current frame is
    /// the one the library held, the anchor `place` uses.
    about: (f64, f64),
    pan: (f64, f64),
    /// The pinch share and the scale when the request went
    /// (`askedPinchX/Y`, `askedScale`, order 1883).
    pinch: (f64, f64),
    scale: f64,
}

impl Default for Asked {
    /// Nothing asked yet: Java's fields start at zero, `askedScale` at 1.
    fn default() -> Self {
        Self {
            off: (0.0, 0.0),
            about: (0.0, 0.0),
            pan: (0.0, 0.0),
            pinch: (0.0, 0.0),
            scale: 1.0,
        }
    }
}

/// One `MapView`'s gesture state between frames.
pub struct Gesture {
    /// The view's size in screen pixels.
    w: u32,
    h: u32,
    /// `scale`, `offX`, `offY` (`MapView.java:221-230`).
    scale: f64,
    off: (f64, f64),
    /// The pinch events' share of `off` (order 1833).
    pinch: (f64, f64),
    /// The last gesture's focus, `focusX/focusY/focused`.
    focus: Option<(f64, f64)>,
    asked: Asked,
    /// The last frame's `x`, `y`, `zoom_milli`, `min_zoom_milli`,
    /// `turns`, as `place` keeps them.
    shown: Option<(u32, u32, u32)>,
    min_zoom_milli: i64,
    turns: bool,
    /// The last frame was drawn blended (`t < 1`), as `MapView.blended`.
    blended: bool,
    /// A pinch is in progress, as `ScaleGestureDetector.isInProgress()`:
    /// from its first scale event until the fingers lift, which the
    /// mirror reads as the next frame asked at rest.
    scaling: bool,
}

impl Gesture {
    /// A view of `w` x `h` screen pixels before its first frame.
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            scale: 1.0,
            off: (0.0, 0.0),
            pinch: (0.0, 0.0),
            focus: None,
            asked: Asked::default(),
            shown: None,
            min_zoom_milli: 0,
            turns: false,
            blended: false,
            scaling: false,
        }
    }

    /// `zoomAbout` (`MapView.java:413-431`): scale what is on screen by
    /// `by` about the screen point `(fx, fy)` — a pinch event, a double
    /// tap, a fly step.
    pub fn on_scale(&mut self, by: f64, fx: f64, fy: f64) {
        let by = match self.shown {
            Some((_, _, zoom)) => zoom_by(self.scale, by, i64::from(zoom), self.min_zoom_milli),
            None => by,
        };
        self.scale *= by;
        self.off.0 = by * (self.off.0 - fx) + fx;
        self.off.1 = by * (self.off.1 - fy) + fy;
        self.pinch.0 = by * (self.pinch.0 - fx) + fx;
        self.pinch.1 = by * (self.pinch.1 - fy) + fy;
        self.focus = Some((fx, fy));
        self.scaling = true;
    }

    /// `onScroll` (`MapView.java`, the gesture detector's listener): the
    /// event's pointer 0 at `(fx, fy)` moved the map by `(-dx, -dy)` —
    /// Android hands the distance the content is to move against. While
    /// a pinch is in progress the focus stays the scale detector's
    /// (order 1840): pointer 0 is up to half the fingers' span from it.
    pub fn on_scroll(&mut self, dx: f64, dy: f64, fx: f64, fy: f64) {
        self.off.0 -= dx;
        self.off.1 -= dy;
        if !self.scaling || self.focus.is_none() {
            self.focus = Some((fx, fy));
        }
    }

    /// One touch event as `MapView.onTouchEvent` hands it on: the gesture
    /// detector first, its `onScroll` when the focal point moved (`dx`,
    /// `dy`, pointer 0 at `(px, py)`), then the scale detector's `onScale`
    /// while a pinch is in progress (`by` about the focal point `(fx,
    /// fy)`). The order is Java's since order 1840: the event's movement,
    /// then its zoom about the focal point's new place.
    pub fn on_touch(
        &mut self,
        scroll: Option<(f64, f64, f64, f64)>,
        scale: Option<(f64, f64, f64)>,
    ) {
        if let Some((dx, dy, px, py)) = scroll {
            self.on_scroll(dx, dy, px, py);
        }
        if let Some((by, fx, fy)) = scale {
            self.on_scale(by, fx, fy);
        }
    }

    /// The frame request `drawFrames` builds (`MapView.java:1080-1151`):
    /// the gesture since the last frame as pan, zoom and the focus, over
    /// the last frame's view. The caller fills the rest of the request.
    pub fn ask(&mut self) -> MapRequest {
        let w = self.w;
        let h = self.h;
        let mx = margin(w, self.turns);
        let my = margin(h, self.turns);
        // The about point whole with its fraction as thousandths, the
        // pan's fraction too (MapView.java drawFrames, orders 1833 and
        // 1840): re-picking the held ground on a whole-pixel grid walked
        // it half a pixel a frame, and a zoom about the rounded pixel is
        // not the one shown about the fingers.
        #[allow(clippy::cast_precision_loss)]
        let (fx, fy) = self.focus.unwrap_or((w as f64 / 2.0, h as f64 / 2.0));
        let (ax, ay) = (fx.round(), fy.round());
        let (ax_milli, ay_milli) = (((fx - ax) * 1000.0).round(), ((fy - ay) * 1000.0).round());
        let (px, py) = (ax + ax_milli / 1000.0, ay + ay_milli / 1000.0);
        let pan_xf = (px - self.off.0) / self.scale - px;
        let pan_yf = (py - self.off.1) / self.scale - py;
        let pan_x = pan_xf.round();
        let pan_y = pan_yf.round();
        self.asked = Asked {
            off: self.off,
            about: (px, py),
            pan: (pan_xf, pan_yf),
            pinch: self.pinch,
            scale: self.scale,
        };
        let by_milli = (1000.0 * self.scale.log2()).round();
        #[allow(clippy::cast_possible_truncation)]
        MapRequest {
            w,
            h,
            margin_x: mx,
            margin_y: my,
            pan_x: pan_x as i32,
            pan_y: pan_y as i32,
            pan_x_milli: ((pan_xf - pan_x) * 1000.0).round() as i32,
            pan_y_milli: ((pan_yf - pan_y) * 1000.0).round() as i32,
            pan_about: self.focus.is_some(),
            zoom_by_milli: by_milli as i32,
            about_x: ax as i32,
            about_y: ay as i32,
            about_x_milli: ax_milli as i32,
            about_y_milli: ay_milli as i32,
            x: self.shown.map(|(x, _, _)| x),
            y: self.shown.map(|(_, y, _)| y),
            zoom_milli: self.shown.map(|(_, _, z)| z),
            moving: true,
            ..MapRequest::default()
        }
    }

    /// `place` (`MapView.java:1320-1392`) and the fields `drawFrames`
    /// keeps from the answer: what the fingers did since the ask stays,
    /// the rest is absorbed by the frame. `moving` is a finger down or a
    /// fly running, as the Java reads `touching || flying || coasting`.
    pub fn place(&mut self, shown: &MapShown, moving: bool) {
        let first = self.shown.is_none();
        let z1 = i64::from(shown.zoom_milli);
        if !moving {
            // Asked at rest: the fingers lifted, the pinch ended.
            self.scaling = false;
        }
        if first || !moving {
            self.scale = 1.0;
            self.off = (0.0, 0.0);
            self.pinch = (0.0, 0.0);
            self.focus = None;
        } else {
            let zoom0 = self.shown.map_or(z1, |(_, _, z)| i64::from(z));
            #[allow(clippy::cast_precision_loss)]
            let a = 2f64.powf((zoom0 - z1) as f64 / 1000.0);
            if self.blended || shown.blended {
                // MapView.place's turning branch: the leftover anchored
                // where the frame says the held ground sits, so the held
                // ground keeps displaying where the fingers put it; an
                // absorbed pure drag anchors on the focus and leaves
                // zero, the frame in place (1762, order 1833).
                let sigma = self.scale * a;
                // The drag since the request: the next frame's turn,
                // never a slide of this one (order 1883).
                let asked = self.asked;
                let drag = (
                    drag_since(
                        self.off.0,
                        self.pinch.0,
                        self.scale,
                        asked.off.0,
                        asked.pinch.0,
                        asked.scale,
                    ),
                    drag_since(
                        self.off.1,
                        self.pinch.1,
                        self.scale,
                        asked.off.1,
                        asked.pinch.1,
                        asked.scale,
                    ),
                );
                // A pure turn is drawn in place: the held ground anchors
                // only a frame that answers a zoom (order 1883).
                let pure = turns_only(shown.turns, asked.scale, self.scale, zoom0, z1);
                match (shown.held_x_micro, shown.held_y_micro) {
                    (Some(hx), Some(hy)) if !pure => {
                        #[allow(clippy::cast_precision_loss)]
                        let held = (hx as f64 / 1e6, hy as f64 / 1e6);
                        let held_old = (
                            self.asked.about.0 + self.asked.pan.0,
                            self.asked.about.1 + self.asked.pan.1,
                        );
                        self.off.0 = self.scale * held_old.0 + self.off.0 - sigma * held.0;
                        self.off.1 = self.scale * held_old.1 + self.off.1 - sigma * held.1;
                    }
                    _ => {
                        self.off.0 -= sigma * self.asked.off.0;
                        self.off.1 -= sigma * self.asked.off.1;
                    }
                }
                self.pinch = (self.off.0 - drag.0, self.off.1 - drag.1);
                self.scale = sigma;
            } else {
                // Java's `unit` reads the OLD frame's zoom: `zoomMilli`
                // is replaced only after `place` returns.
                #[allow(clippy::cast_precision_loss)]
                let unit = 2f64.powf(24.0 - zoom0 as f64 / 1000.0);
                let (x0, y0) = self.shown.map_or((shown.x, shown.y), |(x, y, _)| (x, y));
                let dx = f64::from(shown.x.wrapping_sub(x0) as i32);
                let dy = f64::from(shown.y) - f64::from(y0);
                let bx = (1.0 - a) * (f64::from(self.w) / 2.0) + dx / unit;
                let by = (1.0 - a) * (f64::from(self.h) / 2.0) + dy / unit;
                self.off.0 += self.scale * bx;
                self.off.1 += self.scale * by;
                self.scale *= a;
                self.pinch = self.off;
            }
        }
        self.shown = Some((shown.x, shown.y, shown.zoom_milli));
        self.min_zoom_milli = i64::from(shown.min_zoom_milli);
        self.turns = shown.turns;
        self.blended = shown.blended;
    }

    /// The last frame's zoom in thousandths, as `MapView.zoom()`.
    pub fn zoom_milli(&self) -> Option<u32> {
        self.shown.map(|(_, _, z)| z)
    }

    /// Where Java draws the last frame's view pixel `(x, y)` right now
    /// (`MapView.onDraw` with `shownX`/`shownY`): the leftover gesture
    /// transform applied — the pinch share alone while the globe turns,
    /// whose drag share the next frame absorbs as a turn.
    pub fn drawn(&self, x: f64, y: f64) -> (f64, f64) {
        let off = match self.turns {
            true => self.pinch,
            false => self.off,
        };
        (off.0 + self.scale * x, off.1 + self.scale * y)
    }
}
