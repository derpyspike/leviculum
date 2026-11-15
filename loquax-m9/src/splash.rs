//! What the M9 shows from power-on until the UI's first frame (1485): the
//! logo on the panel's ground with "Leviculum Loquax" under it, and the
//! rule that the backlight never lights a panel nothing has been written
//! to.
//!
//! The ST7789's memory holds noise after its reset, so a backlight lit at
//! the end of the panel's init showed that noise for the boot's two
//! seconds (Lew, 2026-11-07). [`power_on`] is the order the board keeps:
//! the splash drawn into the frame, the whole frame written, and only then
//! the backlight on. The splash stays on the panel while the board runs
//! its census and restores its store (1476): nothing flushes until the
//! UI's first redraw, which replaces it.
//!
//! The logo is one asset, [`LOGO`]: `loquax-m9/assets/splash-head.1bpp`,
//! Nessa's head for this panel (1690, 1698) — one bit a pixel, rows in
//! order, the leftmost pixel of a byte in its high bit, set where the
//! drawing is ink. A bit a pixel is what the drawing is: the source's own
//! head and face, each shape drawn as white fur, a black cut or a black
//! outline, not a threshold of the colour icon, whose cream muzzle and dark
//! nose threshold into a bear's snout. It also costs 1,568 B of flash where
//! the jackdaw's RGBA cost 50,176. `scripts/nessa-render.py` writes it from
//! `assets/nessa/nessa-R3-b.svg`; `just nessa-check` holds the tree to it. The bit is drawn in the theme's ink on the theme's ground, so the
//! splash is the colour the UI then draws over it.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use mousefood::ColorTheme;

use crate::font::CELL_PX;
use crate::frame::{Frame, HEIGHT, WIDTH};

/// The logo's side in panel pixels.
pub const LOGO_PX: u32 = 112;

/// The logo, one bit a pixel (see the module's head).
pub const LOGO: &[u8] = include_bytes!("../assets/splash-head.1bpp");

const _: () = assert!(LOGO.len() == (LOGO_PX * LOGO_PX / 8) as usize);

/// The bytes one row of the logo takes: a bit a pixel, packed.
const ROW_BYTES: usize = (LOGO_PX / 8) as usize;

/// The words under the logo, one line: the suite's name and the app's
/// (Lew, 2026-11-07). Sixteen cells of the atlas are 160 of the panel's
/// 320 pixels, so the two stay on one line at the UI's size. No version,
/// no hash: the splash names the app.
pub const WORD: &str = "Leviculum Loquax";

/// Pixels between the logo and the word.
const GAP: u32 = 12;

/// The logo's top-left corner: logo, gap and one cell of text, centred.
pub fn logo_at() -> Point {
    let block = LOGO_PX + GAP + u32::from(CELL_PX.1);
    Point::new(
        ((WIDTH - LOGO_PX) / 2) as i32,
        ((HEIGHT - block) / 2) as i32,
    )
}

/// The panel's ground and ink: the theme the cells are drawn in, so the
/// splash is the colour the UI then draws over it.
fn theme() -> (Rgb565, Rgb565) {
    let theme = ColorTheme::default();
    (theme.background.into(), theme.foreground.into())
}

/// The splash, drawn into the whole of `frame`.
pub fn draw(frame: &mut Frame) {
    let (ground, ink) = theme();
    let _ = frame.clear(ground);
    let at = logo_at();
    let pixels = LOGO.iter().enumerate().flat_map(move |(i, byte)| {
        let (row, column) = (i / ROW_BYTES, (i % ROW_BYTES) * 8);
        (0..8)
            .filter(move |&bit| byte & (0x80 >> bit) != 0)
            .map(move |bit| {
                let (x, y) = ((column + bit) as i32, row as i32);
                Pixel(at + Point::new(x, y), ink)
            })
    });
    let _ = frame.draw_iter(pixels);
    let font = crate::font::atlas();
    let style = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();
    let _ = Text::with_text_style(
        WORD,
        Point::new((WIDTH / 2) as i32, at.y + (LOGO_PX + GAP) as i32),
        MonoTextStyle::new(&font, ink),
        style,
    )
    .draw(frame);
}

/// The panel as the boot order sees it: rows written, the backlight
/// switched. The board's ST7789 is one; the host's test panel records.
pub trait Panel {
    type Error;
    /// Write rows `first..=last` of `frame`, full width.
    fn write(&mut self, frame: &Frame, first: u32, last: u32) -> Result<(), Self::Error>;
    /// Switch the backlight.
    fn light(&mut self, on: bool);
}

/// Power-on (1485): the splash drawn into `frame`, all of it written to
/// `panel`, then the backlight on. A write that fails leaves the backlight
/// off and the frame whole to write again, so the first redraw that does
/// reach the panel sends every row; the board lights the backlight after
/// that one ([`Panel::light`] only ever follows a written frame).
pub fn power_on<P: Panel>(panel: &mut P, frame: &mut Frame) -> Result<(), P::Error> {
    draw(frame);
    let (first, last) = frame.take_dirty().unwrap_or((0, HEIGHT - 1));
    if let Err(e) = panel.write(frame, first, last) {
        frame.forget_flushed();
        return Err(e);
    }
    panel.light(true);
    Ok(())
}

/// What a panel saw, in order: the record the boot rule is checked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Write(u32, u32),
    Light(bool),
}

/// Whether `events` keep the rule: the backlight is never on while a row
/// of the panel has not been written since power-on. `Err` names the
/// first event that breaks it.
pub fn lights_only_written(events: &[Event]) -> Result<(), usize> {
    let mut written = [false; HEIGHT as usize];
    for (i, event) in events.iter().enumerate() {
        match *event {
            Event::Write(first, last) => {
                for row in first..=last.min(HEIGHT - 1) {
                    written[row as usize] = true;
                }
            }
            Event::Light(true) if written.iter().any(|w| !w) => return Err(i),
            Event::Light(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_and_the_word_fit_the_panel() {
        let at = logo_at();
        assert!(at.x >= 0 && at.y >= 0);
        let bottom = at.y as u32 + LOGO_PX + GAP + u32::from(CELL_PX.1);
        assert!(bottom <= HEIGHT, "{bottom}");
        assert!(WORD.len() as u32 * u32::from(CELL_PX.0) <= WIDTH);
    }

    /// The logo's bit at `(x, y)`, read the way [`draw`] reads it.
    fn bit(x: u32, y: u32) -> bool {
        LOGO[y as usize * ROW_BYTES + (x / 8) as usize] & (0x80 >> (x % 8)) != 0
    }

    #[test]
    fn the_ink_sits_inside_the_drawing_and_the_margins_carry_none() {
        // The drawing keeps a black margin all round. A stride or a row
        // order off by one smears the ink into it, and the panel would
        // show that before any golden did.
        for x in 0..LOGO_PX {
            assert!(!bit(x, 0) && !bit(x, LOGO_PX - 1), "row of column {x}");
        }
        for y in 0..LOGO_PX {
            assert!(!bit(0, y) && !bit(LOGO_PX - 1, y), "row {y}");
        }
        assert!(bit(56, 34), "the forehead is fur");
        assert!(!bit(39, 52), "the eye is a hole in it");
    }
}
