//! The ground map's parameter table (order 1743): every class colour of
//! both styles, every signature ink, and the relief's shadow floor, in
//! one place. The renderers ([`crate::ground`], [`crate::relief`]) read
//! their numbers from here and nowhere else, so a colour trial changes
//! this file alone.
//!
//! The values are the final ones of the MapOut-style trial
//! (`mapout-r2/README_R2.md`, "Colour table (R2 and R')", Lew
//! 2026-11-12): the dune yellow (240, 200, 112) with its dots
//! (198, 138, 56), hamada a light cool grey with its stone signature. The
//! strong style is that table as it stands. The pastel style is the
//! pilot's blend of it toward paper (`render_ground.py` v2: 55 % class
//! colour and 45 % warm paper, hamada 62 % toward a cool paper so it
//! stays cool against reg), worked out once and written down here, and
//! it carries the stone signature alone, as the pilot's pastel did. The
//! pastel stone ink is this order's own value (the trial gave only the
//! strong one): the strong ink moved toward the cool paper by the same
//! share as the pastel hamada, so the marks keep about the strong
//! style's contrast against their ground. Both styles ship; Lew compares
//! them in the field (2026-11-12).

use crate::ground::Class;

/// A class signature: marks laid over the class colour on the global
/// Web Mercator pixel grid of the display zoom, so they join across
/// tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signature {
    /// None.
    Plain,
    /// Dots on a diagonal grid of 6 pixels (dune sand).
    Dots,
    /// The dots on alternate 6-pixel squares only (sand sheet).
    SparseDots,
    /// Dashes every 7 rows, two of three 4-pixel runs (dry lake).
    Dashes,
    /// Dots on a 5-pixel grid (vegetation).
    Grove,
    /// Stones: one small angular mark per 7 x 7 cell at a hashed place,
    /// 28 % of the cells empty (hamada).
    Stones,
}

/// One class's row of the table.
#[derive(Clone, Copy, Debug)]
pub struct ClassStyle {
    /// The class.
    pub class: Class,
    /// Its colour in the strong style; `None` leaves the map's own paper
    /// (the uncertain class is untinted).
    pub strong: Option<[u8; 3]>,
    /// Its colour in the pastel style.
    pub pastel: Option<[u8; 3]>,
    /// Its signature.
    pub signature: Signature,
    /// The signature's ink in the strong style and its opacity.
    pub ink_strong: [u8; 3],
    /// The signature's ink in the pastel style; `None` where the pastel
    /// style draws no signature for the class.
    pub ink_pastel: Option<[u8; 3]>,
    /// The ink's opacity, 0..1, both styles.
    pub alpha: f32,
}

/// The darkest multiplier the relief lays on a shaded face (the ambient
/// lift, R′'s "shadow floor"): [`crate::relief::shade_rgb`] reads it.
pub const SHADOW_FLOOR: f32 = 0.62;

/// The table, one row per [`Class`] (a test holds that every class has
/// exactly one).
pub const CLASSES: [ClassStyle; 9] = [
    ClassStyle {
        class: Class::Dune,
        strong: Some([240, 200, 112]),
        pastel: Some([240, 216, 163]),
        signature: Signature::Dots,
        ink_strong: [198, 138, 56],
        ink_pastel: None,
        alpha: 0.55,
    },
    ClassStyle {
        class: Class::Sheet,
        strong: Some([246, 231, 190]),
        pastel: Some([243, 233, 206]),
        signature: Signature::SparseDots,
        ink_strong: [214, 176, 104],
        ink_pastel: None,
        alpha: 0.5,
    },
    ClassStyle {
        class: Class::Hamada,
        strong: Some([200, 204, 206]),
        pastel: Some([213, 216, 217]),
        signature: Signature::Stones,
        ink_strong: [150, 157, 164],
        ink_pastel: Some([170, 176, 182]),
        alpha: 0.8,
    },
    ClassStyle {
        class: Class::Reg,
        strong: Some([228, 219, 200]),
        pastel: Some([233, 227, 212]),
        signature: Signature::Plain,
        ink_strong: [0, 0, 0],
        ink_pastel: None,
        alpha: 0.0,
    },
    ClassStyle {
        class: Class::Mountain,
        strong: Some([214, 204, 194]),
        pastel: Some([226, 218, 208]),
        signature: Signature::Plain,
        ink_strong: [0, 0, 0],
        ink_pastel: None,
        alpha: 0.0,
    },
    ClassStyle {
        class: Class::DryLake,
        strong: Some([250, 250, 246]),
        pastel: Some([246, 244, 237]),
        signature: Signature::Dashes,
        ink_strong: [140, 168, 190],
        ink_pastel: None,
        alpha: 0.7,
    },
    ClassStyle {
        class: Class::Wadi,
        strong: Some([200, 216, 226]),
        pastel: Some([218, 225, 226]),
        signature: Signature::Plain,
        ink_strong: [0, 0, 0],
        ink_pastel: None,
        alpha: 0.0,
    },
    ClassStyle {
        class: Class::Vegetation,
        strong: Some([164, 198, 128]),
        pastel: Some([198, 215, 172]),
        signature: Signature::Grove,
        ink_strong: [80, 130, 60],
        ink_pastel: None,
        alpha: 0.7,
    },
    ClassStyle {
        class: Class::Uncertain,
        strong: None,
        pastel: None,
        signature: Signature::Plain,
        ink_strong: [0, 0, 0],
        ink_pastel: None,
        alpha: 0.0,
    },
];

/// The row of `class`.
pub fn of(class: Class) -> &'static ClassStyle {
    // The table is in class order; a test holds it there.
    &CLASSES[class as usize - 1]
}
