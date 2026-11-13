//! The map's scale bar (1701, Lew 2026-11-11: "how much is 500 m"): a
//! round length, 1, 2 or 5 times a power of ten metres, that fits a width
//! the frontend gives, the length it takes on screen, and its word.
//!
//! One rule for every target: the window and the phone draw it as a line
//! in pixels, the terminal in cells ([`cells`]), the M9 in its panel's
//! pixels. Each asks with its own view, so the bar follows zoom and
//! latitude live: the length is measured at the view's centre, where the
//! projection's stretch is the one the reader looks at.

use alloc::format;
use alloc::string::String;

use leviculum_map::viewport::{ExactView, Viewport};

use super::radar;

/// The longest a bar may be on the window and the phone, in logical
/// pixels: about a thumb's width, so it reads at a glance and never
/// reaches the credit beside it.
pub const MAX_PX: f64 = 96.0;

/// The longest a bar may be in the terminal, in cells, its word aside.
pub const MAX_CELLS: u16 = 10;

/// A scale bar: the length it stands for, the length it takes, its word.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleBar {
    /// The round length in metres: 1, 2 or 5 times a power of ten.
    pub metres: u32,
    /// Its length on screen, in the unit the width was asked in.
    pub len: f64,
    /// "500 m", "2 km": the same in German and English.
    pub word: String,
}

/// The bar for a screen whose unit spans `metres_per_unit` metres, at
/// most `max` units long. None where not even a metre fits, or the view
/// gives no length (a degenerate view, a pole).
pub fn fit(metres_per_unit: f64, max: f64) -> Option<ScaleBar> {
    if !metres_per_unit.is_finite() || metres_per_unit <= 0.0 || max.is_nan() || max < 1.0 {
        return None;
    }
    let reach = metres_per_unit * max;
    if reach < 1.0 {
        return None;
    }
    let metres = radar::ring_step(reach);
    Some(ScaleBar {
        metres,
        len: f64::from(metres) / metres_per_unit,
        word: word(metres),
    })
}

/// The bar of a pixel frontend's view (the window, the phone): `scale`
/// screen pixels to a logical one, `len` in logical pixels.
pub fn of_exact(view: &ExactView, scale: f64) -> Option<ScaleBar> {
    let per_px = radar::metres_per_unit(view.centre) * view.unit();
    fit(per_px * scale, MAX_PX)
}

/// The bar of a view in whole zoom steps (the terminal, the M9):
/// `px_per_unit` of the view's pixels to the unit asked in (a cell's
/// width, or one), at most `max` of them.
pub fn of_viewport(view: &Viewport, px_per_unit: f64, max: f64) -> Option<ScaleBar> {
    let unit = libm::exp2(24.0 - f64::from(view.zoom()));
    let per_px = radar::metres_per_unit(view.centre) * unit;
    fit(per_px * px_per_unit, max)
}

/// A round length's word: metres below a kilometre, whole kilometres
/// above.
pub fn word(metres: u32) -> String {
    match metres {
        m if m < 1_000 => format!("{m} m"),
        m => format!("{} km", m / 1_000),
    }
}

/// The bar as the terminal draws it: `├───┤ 500 m`, its length rounded
/// to whole cells and two at least; `|---| 500 m` in ASCII.
pub fn cells(bar: &ScaleBar, ascii: bool) -> String {
    let n = (libm::round(bar.len).max(2.0) as usize).min(usize::from(MAX_CELLS));
    let (left, mid, right) = match ascii {
        true => ('|', '-', '|'),
        false => ('├', '─', '┤'),
    };
    let mut line = String::with_capacity(n * 3 + bar.word.len() + 1);
    line.push(left);
    for _ in 0..n - 2 {
        line.push(mid);
    }
    line.push(right);
    line.push(' ');
    line.push_str(&bar.word);
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::viewport::MILLI;

    /// Florence's Duomo, the map's fixture place.
    fn duomo() -> leviculum_map::mercator::World {
        project(LatLon::from_e6(43_773_100, 11_255_900))
    }

    /// The order's test: the bar's length on screen, laid between two
    /// points of the view, is the distance it says, at several zooms,
    /// fractional ones among them.
    #[test]
    fn the_bar_is_as_long_as_the_distance_it_names() {
        for zoom_milli in [5_000, 9_500, 12_000, 14_300, 16_000, 18_700] {
            let view = ExactView::new(duomo(), zoom_milli, 1080, 1920);
            let bar = of_exact(&view, 1.0).expect("a bar");
            assert!(bar.len <= MAX_PX, "{zoom_milli}: {bar:?}");
            // At least two fifths of the room: the next round length
            // (at most 2.5 times this one) would not have fitted.
            assert!(bar.len > MAX_PX * 0.4, "{zoom_milli}: {bar:?}");
            let (cx, cy) = (540.0, 960.0);
            let a = view.to_world(cx - bar.len / 2.0, cy);
            let b = view.to_world(cx + bar.len / 2.0, cy);
            let measured = radar::metres(a, b);
            let off = (measured - f64::from(bar.metres)).abs() / f64::from(bar.metres);
            assert!(off < 0.005, "{zoom_milli}: {measured} m for {bar:?}");
        }
    }

    /// The same zoom further north is fewer metres to the pixel: the bar
    /// follows the latitude.
    #[test]
    fn the_bar_follows_the_latitude() {
        let at = |lat_e6| ExactView::new(project(LatLon::from_e6(lat_e6, 0)), 14 * MILLI, 800, 600);
        let equator = of_exact(&at(0), 1.0).expect("a bar");
        let north = of_exact(&at(60_000_000), 1.0).expect("a bar");
        // At 60° a metre is twice the pixels it is at the equator.
        let per_m = |b: &ScaleBar| b.len / f64::from(b.metres);
        let ratio = per_m(&north) / per_m(&equator);
        assert!((ratio - 2.0).abs() < 0.01, "{ratio}: {equator:?} {north:?}");
    }

    /// A screen with twice the pixels to a logical one gives the same
    /// bar in logical pixels.
    #[test]
    fn the_scale_of_the_screen_does_not_change_the_bar() {
        let one = ExactView::new(duomo(), 15 * MILLI, 800, 600);
        let two = ExactView::new(duomo(), 16 * MILLI, 1600, 1200);
        let (a, b) = (of_exact(&one, 1.0), of_exact(&two, 2.0));
        let (a, b) = (a.expect("a bar"), b.expect("a bar"));
        assert_eq!(a.metres, b.metres);
        assert!((a.len - b.len).abs() < 1e-6, "{a:?} {b:?}");
    }

    #[test]
    fn the_lengths_are_round_and_their_words_plain() {
        for (per, metres, said) in [
            (0.1, 5, "5 m"),
            (1.0, 50, "50 m"),
            (10.0, 500, "500 m"),
            (25.0, 2_000, "2 km"),
            (1_000.0, 50_000, "50 km"),
        ] {
            let bar = fit(per, MAX_PX).expect("a bar");
            assert_eq!((bar.metres, bar.word.as_str()), (metres, said), "{per}");
        }
        assert_eq!(fit(0.001, MAX_PX), None, "not a metre fits");
        assert_eq!(fit(f64::NAN, MAX_PX), None);
        assert_eq!(fit(0.0, MAX_PX), None);
    }

    #[test]
    fn the_terminal_draws_whole_cells() {
        // A cell of two braille dots across at z14 over Florence.
        let view = Viewport::new(duomo(), 14, 160, 96);
        let bar = of_viewport(&view, 2.0, f64::from(MAX_CELLS)).expect("a bar");
        let line = cells(&bar, false);
        assert!(line.starts_with('├'), "{line}");
        assert!(line.ends_with(&format!("┤ {}", bar.word)), "{line}");
        let n = line.chars().take_while(|c| *c != ' ').count();
        assert_eq!(n as f64, libm::round(bar.len).max(2.0), "{line} {bar:?}");
        let short = ScaleBar {
            metres: 1,
            len: 0.4,
            word: word(1),
        };
        assert_eq!(cells(&short, true), "|| 1 m");
    }
}
