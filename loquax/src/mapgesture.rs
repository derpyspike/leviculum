//! The map's gestures in geographic terms (order 1818, 1841): a drag,
//! a wheel step or a pinch keeps the ground DRAWN under the pointer,
//! through the view's own [`Blend`], wherever the view is blended. One
//! definition for every frontend — the phone and the window through
//! [`crate::mapframe`], which re-exports it, and the terminal
//! ([`crate::ui::map`]), which carries no `screenmap` (audit of 1818,
//! condition 8: its wheel zoomed about the Mercator point, 99 px off the
//! drawn one at 32 N 6 W z4). `no_std` with `alloc`, as the core is.

use leviculum_map::blend::Blend;
use leviculum_map::viewport::{ExactView, MILLI};

/// The world's width in Mercator world units.
const WORLD_F: f64 = 4_294_967_296.0;

/// `v` wrapped onto the world's width, as `f64::rem_euclid` (not in
/// `core`).
fn wrap(v: f64) -> f64 {
    v - WORLD_F * libm::floor(v / WORLD_F)
}

/// The blend a drag on `view` turns, while the view is not yet Mercator
/// (1762); `None` where a drag pans.
pub fn turns(view: &ExactView) -> Option<Blend> {
    let (w, h) = view.size();
    let b = Blend::new(view.centre, view.zoom(), w, h);
    (b.t() < 1.0).then_some(b)
}

/// `view` dragged by `(dx, dy)` screen pixels in [`ExactView::pan`]'s
/// sense: at globe zooms the drag turns
/// the sphere about its centre, which stays in the middle of the screen
/// ([`Blend::turned`], the grabbed middle following the finger, north up,
/// no flip over a pole); elsewhere it pans in whole Mercator pixels, the
/// contract the fling's room accounting rests on, so a name drawn again
/// is the same pixels (`mapframe::moved`). In a blended view a pixel pan slides
/// the drawn ground against the finger (audit of 1818 condition 2:
/// 9.7 px at 32 N 6 W z3.5), and NO finger-less rule can fix that — the
/// picture does not move rigidly under any recentring; the centre that
/// holds the middle's ground and the one that holds a pointer 400 px
/// away lie 13.5 px apart there (measured 2026-11-14) — so every drag
/// that HAS a pointer goes through [`drag_about`] instead; this entry
/// is for the pointer-less rest: the fling's coast, programmatic pans.
pub fn drag(view: &mut ExactView, dx: i32, dy: i32) {
    match turns(view) {
        Some(b) => view.centre = b.turned(f64::from(dx), f64::from(dy)),
        None => view.pan(dx, dy),
    }
}

/// [`drag`] with the pointer: `(x, y)` is where the finger or the
/// cursor IS, after it moved by `(-dx, -dy)`, and the ground drawn under
/// it before the move — at `(x + dx, y + dy)` — follows it there, held
/// through the blend's own [`Blend::to_world`]/[`Blend::to_screen`] by
/// the same damped walk a pinch holds its focus with (audit of 1818,
/// condition 2). A blended picture moves rigidly under no recentring
/// (see [`drag`]), so only the entry that knows the pointer can keep
/// the ground under it; a pointer off the world falls back to [`drag`].
/// At `t = 1` it pans in whole pixels, as [`drag`]. The pan is
/// fractional: a blended frame's leftover encodes sub-pixel pans, and
/// re-picking the held ground on a whole-pixel grid walked it half a
/// pixel a frame (order 1833); at `t = 1` the pan rounds to whole
/// pixels, as a name drawn again must be the same pixels (`mapframe::moved`).
pub fn drag_about(view: &mut ExactView, dx: f64, dy: f64, x: f64, y: f64) {
    #[allow(clippy::cast_possible_truncation)]
    let whole = (
        libm::round(dx).clamp(-1e9, 1e9) as i32,
        libm::round(dy).clamp(-1e9, 1e9) as i32,
    );
    let (w, h) = view.size();
    let b = Blend::new(view.centre, view.zoom(), w, h);
    if b.t() >= 1.0 {
        view.pan(whole.0, whole.1);
        // The fraction goes into the centre in world units: dropping it
        // froze a third of a pixel into the view at every frame at rest,
        // which the zoom then magnified (order 1833). A whole-pixel pan
        // has none and stays whole pixels.
        let frac = (dx - f64::from(whole.0), dy - f64::from(whole.1));
        if frac != (0.0, 0.0) {
            let unit = libm::pow(2.0, 24.0 - f64::from(view.zoom_milli()) / 1000.0);
            view.centre = leviculum_map::mercator::World {
                x: wrap(f64::from(view.centre.x) + frac.0 * unit) as u32,
                y: (f64::from(view.centre.y) + frac.1 * unit).clamp(0.0, WORLD_F - 1.0) as u32,
            };
        }
        return;
    }
    let held = b.to_world(x + dx, y + dy);
    let Some(held) = held else {
        drag(view, whole.0, whole.1);
        return;
    };
    let held = held_refined(&b, x + dx, y + dy, held);
    // Start where the finger-less drag lands, a pixel or a few from the
    // hold; the walk removes the rest.
    drag(view, whole.0, whole.1);
    walk_held(view, held, x, y);
}

/// The ground drawn under the screen point `(x, y)` of `view`, through
/// the blend's own inverse and refined to the world grid's floor
/// (`held_refined`): the point a gesture holds. A caller that anchors
/// on this ground (order 1833) must pick the SAME point the gesture
/// maths holds — the raw inverse answers a few thousandths of a pixel
/// away, and a pinch magnifies that seed by its whole zoom span.
/// `None` off the world.
pub fn ground_under(view: &ExactView, x: f64, y: f64) -> Option<leviculum_map::mercator::World> {
    let (w, h) = view.size();
    let b = Blend::new(view.centre, view.zoom(), w, h);
    let held = b.to_world(x, y)?;
    Some(held_refined(&b, x, y, held))
}

/// Whether `view` is drawn blended (`t < 1`): there gesture maths must
/// go through the blend ([`zoom_about`], [`drag`]), since Mercator's
/// inverse is not the drawn picture's.
pub fn blends(view: &ExactView) -> bool {
    let (w, h) = view.size();
    Blend::new(view.centre, view.zoom(), w, h).t() < 1.0
}

/// A held point's zoom walk stops within this of the asked pixel, and
/// gives up after this many steps: each step turns the centre by the
/// held point's screen error ([`Blend::turned`]), which near the view's
/// middle removes almost all of it, so a handful of steps reach this.
/// The bound is far under a pixel because a pinch compounds it: what an
/// event leaves over, every later event's zoom magnifies about the held
/// point — a residual of a THOUSANDTH of a pixel an event still summed
/// to 1.7 px over a z4-to-z10 pinch at 55 millizooms an event (the
/// geometric series of `eps · 2^(z_end - z)`); at a millionth the whole
/// ladder keeps within a fiftieth of a pixel.
const HELD_EPS_PX: f64 = 1e-6;
const HELD_STEPS: u32 = 24;

/// `held`, which [`Blend::to_world`] put within a thirty-second of a
/// pixel of `(x, y)`, pushed to the world grid's floor by Newton on the
/// forward mapping in continuous world coordinates.
pub(crate) fn held_refined(
    b: &Blend,
    x: f64,
    y: f64,
    held: leviculum_map::mercator::World,
) -> leviculum_map::mercator::World {
    use leviculum_map::mercator::World;
    let place = |ux: f64, uy: f64| World {
        x: wrap(ux) as u32,
        y: uy.clamp(0.0, WORLD_F - 1.0) as u32,
    };
    let (mut ux, mut uy) = (f64::from(held.x), f64::from(held.y));
    let h = b.unit();
    for _ in 0..3 {
        let (px, py) = b.to_screen(place(ux, uy));
        let (fx, fy) = (px - x, py - y);
        let (qx, qy) = b.to_screen(place(ux + h, uy));
        let (rx, ry) = b.to_screen(place(ux, uy + h));
        let (j11, j21) = ((qx - px) / h, (qy - py) / h);
        let (j12, j22) = ((rx - px) / h, (ry - py) / h);
        let det = j11 * j22 - j12 * j21;
        if libm::fabs(det) < 1e-18 {
            break;
        }
        ux -= (fx * j22 - fy * j12) / det;
        uy = (uy - (fy * j11 - fx * j21) / det).clamp(0.0, WORLD_F - 1.0);
    }
    place(ux, uy)
}

/// `view` zoomed to `zoom_milli` about the screen point `(x, y)`: the
/// GEOGRAPHIC point drawn under `(x, y)` stays under it, as a pinch's
/// fingers expect, in flat and in blended views alike. Where both the old and
/// the new view are Mercator exactly (`t = 1`) this is
/// [`ExactView::zoom_about`], unchanged. In a blended view that rule
/// kept the MERCATOR point of the focus — which is not the point DRAWN
/// there: the blend bends away from the centre and the globe's world
/// term scales by `cos φ₀^(1-t)` — so every step of a continuous pinch
/// drifted the view a little, and over the handover the drift grew to
/// Lew's thousand-kilometre jump ("I wanted to zoom in on Morocco and
/// was in Spain", order 1818). Here the point under the fingers is the
/// blend's own ([`Blend::to_world`]), and at the new zoom the centre is
/// turned ([`Blend::turned`], the same move a drag makes) until that
/// point lands under the fingers again, within `HELD_EPS_PX`. A focus
/// off the world (off the globe's disk) zooms about the centre, which
/// both mappings pin to the middle pixel.
pub fn zoom_about(view: &mut ExactView, zoom_milli: u32, x: f64, y: f64) {
    let (w, h) = view.size();
    let before = Blend::new(view.centre, view.zoom(), w, h);
    let zoom = f64::from(zoom_milli) / f64::from(MILLI);
    if before.t() >= 1.0 && Blend::new(view.centre, zoom, w, h).t() >= 1.0 {
        view.zoom_about(zoom_milli, x, y);
        return;
    }
    let held = before.to_world(x, y);
    *view = ExactView::new(view.centre, zoom_milli, w, h);
    let Some(held) = held else {
        return;
    };
    // The inverse answers within a thirty-second of a pixel; a few more
    // Newton steps here take that to the world grid's own floor. The
    // walk below holds the held point where the fingers are, so any
    // residual between the held point and the fingers' true ground
    // turns into ground error — which every later event of a
    // continuous pinch magnifies, a thousandth of a pixel an event
    // summing to two pixels over a world-to-street pinch.
    let held = held_refined(&before, x, y, held);
    // The held point is aimed at its own drawn pixel — which is the
    // focus within the inverse's guarantee — with its offset from the
    // focus scaled by the zoom, as the zoom scales everything about the
    // fingers. Aiming at the focus itself would move the view by the
    // inverse's residual on every event; keeping the offset unscaled
    // would zoom about the residual's point instead of the fingers'.
    // Either way a continuous pinch compounds what an event seeds by
    // every later event's scale, so the seed has to vanish, not merely
    // stay small.
    // The scale change is the blends' own — their pixel unit carries the
    // globe's `cos φ₀^(1-t)` besides the zoom's power of two.
    let k = before.unit() / Blend::new(view.centre, zoom, w, h).unit();
    let (hx, hy) = before.to_screen(held);
    let (tx, ty) = (x + (hx - x) * k, y + (hy - y) * k);
    walk_held(view, held, tx, ty);
}

/// Turn `view` until the world point `held` is drawn at `(tx, ty)`,
/// within [`HELD_EPS_PX`]: Newton on the turn. The raw `turned(e)` step
/// answers with only a part of the error where the held point sits far
/// from the centre (a third at 51° N near the globe floor), and a pinch
/// compounds what a capped walk leaves. The Jacobian of the held point's
/// place against a turn, by finite differences of a pixel, converges in
/// a handful of steps at any latitude — DAMPED. Near the limb the
/// Jacobian is ill-conditioned (the held point moves almost along one
/// line under any turn), and the raw solve asked for a turn of
/// thousands of pixels: from a 4.7 px error at 51° N with the focus at
/// nine tenths of the disk's radius, one undamped step threw the centre
/// three quarters of the way to the pole, the clamp pinned it there,
/// and the walk ended 93 px off (audit of 1818, condition 1, 4.49 px a
/// pinch event). A step is capped at the view's half perimeter and
/// halved until the error shrinks; a step that cannot shrink it keeps
/// the best centre seen.
///
/// An exact hold is not always possible: a held ground at or past
/// Mercator's edge latitude pulls the centre onto the edge, where
/// [`Blend::turned`]'s never-flip clamp stops it (north stays up,
/// 1762). The walk then ends at the constrained best, measured 5.7 px
/// an event at 480 x 320 with the focus at nine tenths of the disk's
/// radius (2026-11-14); the hold is exact again as soon as the centre
/// leaves the edge.
fn walk_held(view: &mut ExactView, held: leviculum_map::mercator::World, tx: f64, ty: f64) {
    let (w, h) = view.size();
    for _ in 0..HELD_STEPS {
        let b = Blend::new(view.centre, view.zoom(), w, h);
        let (px, py) = b.to_screen(held);
        let (ex, ey) = (px - tx, py - ty);
        if ex.abs() < HELD_EPS_PX && ey.abs() < HELD_EPS_PX {
            break;
        }
        let step = 1.0;
        let east = Blend::new(b.turned(step, 0.0), view.zoom(), w, h);
        let south = Blend::new(b.turned(0.0, step), view.zoom(), w, h);
        let (qx, qy) = east.to_screen(held);
        let (rx, ry) = south.to_screen(held);
        let (j11, j21) = ((qx - px) / step, (qy - py) / step);
        let (j12, j22) = ((rx - px) / step, (ry - py) / step);
        let det = j11 * j22 - j12 * j21;
        if libm::fabs(det) < 1e-12 {
            break;
        }
        let mut dx = (ex * j22 - ey * j12) / det;
        let mut dy = (ey * j11 - ex * j21) / det;
        let cap = f64::from(w) + f64::from(h);
        let asked = libm::hypot(dx, dy);
        if asked > cap {
            dx *= cap / asked;
            dy *= cap / asked;
        }
        let err = libm::hypot(ex, ey);
        let mut stepped = false;
        for _ in 0..8 {
            let centre = b.turned(-dx, -dy);
            let c = Blend::new(centre, view.zoom(), w, h);
            let (nx, ny) = c.to_screen(held);
            if libm::hypot(nx - tx, ny - ty) < err {
                view.centre = centre;
                stepped = true;
                break;
            }
            dx /= 2.0;
            dy /= 2.0;
        }
        if !stepped {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};

    /// Audit of 1818 condition 9: [`blends`] answers for the view the
    /// globe draws (the disk and the handover band) and not for the flat
    /// map past it, at every size a frontend has; a constant answer
    /// either way is red here.
    #[test]
    fn blends_is_the_blends_own_t() {
        let at = project(LatLon::from_degrees(51.0, 10.0));
        for (w, h) in [(480u32, 320u32), (1224, 2197), (1600, 2560)] {
            let mut seen = (0usize, 0usize);
            for zm in (0u32..=16_000).step_by(250) {
                let view = ExactView::new(at, zm, w, h);
                let t = Blend::new(view.centre, view.zoom(), w, h).t();
                assert_eq!(blends(&view), t < 1.0, "{w}x{h} z{zm}");
                assert_eq!(turns(&view).is_some(), t < 1.0, "{w}x{h} z{zm}");
                match blends(&view) {
                    true => seen.0 += 1,
                    false => seen.1 += 1,
                }
            }
            assert!(seen.0 > 0 && seen.1 > 0, "{w}x{h}: {seen:?}");
            assert!(blends(&ExactView::new(at, 2_000, w, h)));
            assert!(!blends(&ExactView::new(at, 14_000, w, h)));
        }
    }
}
