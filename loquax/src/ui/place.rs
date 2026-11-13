//! A message at its place (1692; `docs/src/concepts/loquax-tracks.md`,
//! "A message at its place"): while a share runs in a chat
//! ([`super::share`]), every message written there carries the sender's
//! last fix, and the map shows those messages where they were written.
//!
//! The rule is the share's switch and nothing new: no share, no place.
//! The place is the newest own fix the shell delivered
//! ([`super::compose::Bar::own_fix`]) at send time, never a photo's EXIF
//! (photos are shrunk, and EXIF is missing or wrong as often as not). A fix
//! older than [`PLACE_FRESH_SECS`] goes as nothing rather than as a stale
//! place. The wire form is the one a position report has: one
//! `FIELD_TELEMETRY`, a Sideband Telemeter with time and location
//! ([`super::position_fields`]), so a Sideband peer reads it and ours reads
//! Sideband's. Unmarked: the message is no report of the share's cadence.

use leviculum_lxmf::Field;

use super::{files, photos, telemetry, voice, UiModel};

/// The oldest a fix may be and still go as a message's place, seconds: the
/// longest gap a running share promises between two reports
/// ([`super::share::STILL_EVERY_S`]). A fix older than that is older than
/// anything the share itself would still send as current.
pub const PLACE_FRESH_SECS: i64 = super::share::STILL_EVERY_S;

/// Whether a message of `body` and `fields` is one the reader wrote: words,
/// a picture, a file or a voice message. A reaction, a report, a group's
/// state or a receipt has none of them and carries no place.
pub fn written(body: &str, fields: &[Field]) -> bool {
    !body.trim().is_empty()
        || photos::picture_parts(fields).is_some()
        || !files::files_of(fields).is_empty()
        || voice::voice_parts(fields).is_some()
}

impl<B> UiModel<B> {
    /// The field a message of `body` and `fields` to `to` gains: the last
    /// fix, while a share runs in that chat, the fix is no older than
    /// [`PLACE_FRESH_SECS`], and the message is one the reader wrote that
    /// carries no telemetry of its own. `None` otherwise.
    pub(super) fn place_field(&self, to: &[u8; 16], body: &str, fields: &[Field]) -> Option<Field> {
        let now = self.now?.unix_secs;
        let share = self.share(to)?;
        if share.over(now) || telemetry::has_telemetry(fields) || !written(body, fields) {
            return None;
        }
        let fix = self.bar.own_fix?;
        if now.saturating_sub(fix.last_update) > PLACE_FRESH_SECS {
            return None;
        }
        super::position_fields(fix).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::constants::FIELD_TELEMETRY;
    use leviculum_lxmf::telemetry::{Location, Telemetry};

    fn hex(bytes: &[u8]) -> alloc::string::String {
        bytes.iter().map(|b| alloc::format!("{b:02x}")).collect()
    }

    /// The place's wire form, byte for byte: the field number, then one
    /// bin around Sideband's Telemeter, a map of `SID_TIME` (1) and
    /// `SID_LOCATION` (2), the location seven elements as `sense.py`
    /// packs them (six big-endian bins, the fix time an int). 51 bytes
    /// with the key; Sideband's own reader takes it as its own.
    #[test]
    fn the_place_on_the_wire_is_sidebands_telemeter() {
        let fix = Location {
            latitude_e6: 43_773_100,
            longitude_e6: 11_256_000,
            altitude_e2: 5_000,
            speed_e2: 120,
            bearing_e2: 9_000,
            accuracy_e2: 500,
            last_update: 1_793_500_000,
        };
        let fields = super::super::position_fields(fix);
        let [(key, value)] = fields.as_slice() else {
            panic!("one field: {fields:?}");
        };
        assert_eq!(*key, FIELD_TELEMETRY);
        assert_eq!(
            hex(value),
            concat!(
                "c4",
                "30", // bin8, 48 bytes: the packed Telemeter
                "82", // map of two sensors
                "01",
                "ce6ae6a360", // SID_TIME: 1 793 500 000
                "02",
                "97", // SID_LOCATION: array of seven
                "c404",
                "029becac", // latitude  43.773100 (e6)
                "c404",
                "00abc0c0", // longitude 11.256000 (e6)
                "c404",
                "00001388", // altitude  50.00 m (e2)
                "c404",
                "00000078", // speed      1.20 m/s (e2)
                "c404",
                "00002328", // bearing   90.00 deg (e2)
                "c402",
                "01f4",       // accuracy   5.00 m (e2)
                "ce6ae6a360", // last_update, the fix time
            )
        );
        assert_eq!(value.len() + 1, 51, "with the one-byte key");
        let mut p = 0;
        let blob = leviculum_lxmf::msgpack::read_bin(value, &mut p).expect("bin");
        let read = Telemetry::decode(blob).expect("a Telemeter");
        assert_eq!(read.location, Some(fix));
        assert_eq!(read.time, Some(fix.last_update));
    }

    #[test]
    fn written_is_words_or_media() {
        assert!(written("hi", &[]));
        assert!(!written("  \n", &[]));
        assert!(written("", &photos::photo_fields("jpg", alloc::vec![1])));
        assert!(written("", &files::file_fields("a.txt", alloc::vec![1])));
        assert!(written(
            "",
            &voice::voice_fields(voice::AM_OPUS_OGG, alloc::vec![1])
        ));
        let reaction = alloc::vec![(leviculum_lxmf::constants::FIELD_REACTION, alloc::vec![0xc0])];
        assert!(!written("", &reaction), "a reaction is no message written");
        let report = super::super::position_fields(Location {
            latitude_e6: 1,
            longitude_e6: 1,
            altitude_e2: 0,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 0,
            last_update: 1,
        });
        assert!(!written("", &report), "a report is no message written");
    }
}
