//! One type ramp for the chat, the lists and the map (1700, Lew
//! 2026-11-11): on a small dense screen the chat's text had a good size
//! and the window's map was far too small, because the two named their
//! sizes apart (the chat 17 to 20, the map 11 to 14). Both follow the
//! same window factor; what differed were the design sizes. Now every
//! text size is a step of this ramp, in the phone's sp, which the window
//! takes as logical pixels (loquax-ui.md, "Pixel screens"):
//!
//! - the chat ([`super::chat::size`]): a bubble's body, a list row's
//!   name and last line, a bubble's time;
//! - the map (`crate::mapnames::size_of`, `crate::screenmap::LABEL`): a
//!   pin's label is the body, place, park and water names the secondary
//!   line, road names the caption, and no map text below the caption.
//!
//! The reader's [`TextSize`] (Settings, Appearance) scales the ramp as
//! one value for the chat and the map together.

/// A chat's title in its bar.
pub const TITLE: f32 = 20.0;
/// A name in the list.
pub const NAME: f32 = 18.0;
/// A bubble's text, the compose field's, a pin's label.
pub const BODY: f32 = 17.0;
/// A list row's last line; place, park and water names on the map.
pub const SECONDARY: f32 = 15.0;
/// A bubble's time, an unread count; road names on the map. The least
/// text the chat draws, and so the least the map draws.
pub const CAPTION: f32 = 12.0;

/// What the map's sizes beside its text grow by: they were drawn beside a
/// 14 px pin label, which is now the [`BODY`], so the halos, the margins,
/// the markers, the hit areas and the lines grow by the same ratio and
/// the picture keeps its proportions.
pub const MAP_RATIO: f32 = BODY / 14.0;

/// The chat's smallest button, its side in logical pixels: a bar's icon,
/// 24 with 8 around (`leviculum_ui_iced::control::icon_action`). A pin on
/// the map answers a click as far out as this reaches.
pub const LEAST_BUTTON: f32 = 40.0;

/// The reader's text size: one value for the chat and the map, the
/// ramp's sizes times its [`factor`](TextSize::factor). Normal is the
/// ramp as it stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextSize {
    Small,
    #[default]
    Normal,
    Large,
}

impl TextSize {
    /// The three, smallest first: the order the settings offer them.
    pub const ALL: [TextSize; 3] = [TextSize::Small, TextSize::Normal, TextSize::Large];

    /// What the ramp's sizes are multiplied by.
    pub const fn factor(self) -> f32 {
        match self {
            TextSize::Small => 0.875,
            TextSize::Normal => 1.0,
            TextSize::Large => 1.25,
        }
    }

    /// The config's word (`text_size`).
    pub const fn word(self) -> &'static str {
        match self {
            TextSize::Small => "small",
            TextSize::Normal => "normal",
            TextSize::Large => "large",
        }
    }

    /// The size a config word names.
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.word() == word)
    }

    /// Its words on the settings' page.
    pub const fn words(self) -> super::strings::StrId {
        use super::strings::StrId;
        match self {
            TextSize::Small => StrId::TextSmall,
            TextSize::Normal => StrId::TextNormal,
            TextSize::Large => StrId::TextLarge,
        }
    }
}
