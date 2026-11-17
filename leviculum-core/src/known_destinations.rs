//! Data types and trait for known destinations persistence.
//!
//! Known destinations track peer identities discovered via announces.
//! Each target serializes these in its own format:
//! - std: msgpack (Python Reticulum compatible)
//! - embedded: compact binary records (future)

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::constants::{IDENTITY_KEY_SIZE, TRUNCATED_HASHBYTES};

/// Announce packet hash length (SHA-256).
pub const PACKET_HASH_LEN: usize = 32;

/// The fifth element of a Python known-destinations entry: what the cull
/// reads to decide whether an identity may be dropped.
///
/// `Identity.remember` writes `0` for a freshly remembered destination,
/// `_used_destination_data` writes a `time.time()` stamp, and
/// `_retain_destination_data` writes the `-1` sentinel that pins the entry
/// (`reference/Reticulum/RNS/Identity.py:101-113, 268-292`). The cull spares
/// `-1`, drops a never-announced `0` entry together with its ratchet file,
/// and ages a stamped one out against `DESTINATION_TIMEOUT`
/// (Identity.py:336-366).
///
/// We do not yet drive this field ourselves — the retain RPC keeps its state
/// in the runtime announce cache — but we must carry it faithfully, or a
/// file that passes through us comes back to Python with every pin erased.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum KnownDestUseState {
    /// Never used since it was remembered (Python `0`).
    #[default]
    NeverUsed,
    /// Pinned against the cull (Python `-1`).
    Retained,
    /// Last use, seconds since the Unix epoch (Python `time.time()`), or
    /// any other nonzero value the reference reads as one.
    Used(f64),
}

impl KnownDestUseState {
    /// Read the field as Python classifies it (Identity.py:336-348): exactly
    /// `-1` is the retain sentinel (`== -1`, :344), zero is never-used, and
    /// any other number is a last-use stamp, a negative one included, which
    /// the reference's cull then ages out as a use long past
    /// (`unused_for = now - e[4]`, :348). A NaN matches none of the
    /// reference's tests and reads as never-used, which ages it out by its
    /// announce time as the reference's first arm (:350) does.
    pub fn from_seconds(value: f64) -> Self {
        if value == -1.0 {
            Self::Retained
        } else if value == 0.0 || value.is_nan() {
            Self::NeverUsed
        } else {
            Self::Used(value)
        }
    }
}

/// A known destination entry, a peer identity discovered from an announce.
#[derive(Clone)]
pub struct KnownDestEntry {
    /// Seconds since Unix epoch (Python `time.time()` compatible).
    pub timestamp: f64,
    /// Original announce packet hash (32 bytes).
    pub packet_hash: Vec<u8>,
    /// Combined public key: X25519(32) | Ed25519(32) = 64 bytes.
    pub public_key: [u8; IDENTITY_KEY_SIZE],
    /// Optional application data from the announce.
    pub app_data: Option<Vec<u8>>,
    /// Python's fifth element (Codeberg #321). Preserved across a
    /// load-save round-trip so a pin an application set on the other
    /// stack survives a daemon swap.
    pub use_state: KnownDestUseState,
}

/// Persistent storage for known destination identities.
///
/// Implemented per target:
/// - std: msgpack file (Python Reticulum compatible)
/// - embedded: compact binary records in flash (future)
pub trait KnownDestinationsStore {
    /// Error type for storage operations.
    type Error: core::fmt::Debug;

    /// Load all known destination entries from persistent storage.
    fn load_all(
        &mut self,
    ) -> Result<BTreeMap<[u8; TRUNCATED_HASHBYTES], KnownDestEntry>, Self::Error>;

    /// Save all known destination entries to persistent storage.
    fn save_all(
        &mut self,
        entries: &BTreeMap<[u8; TRUNCATED_HASHBYTES], KnownDestEntry>,
    ) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::KnownDestUseState;

    /// The fifth element as the reference classifies it
    /// (`reference/Reticulum/RNS/Identity.py:336-352`): only exactly `-1`
    /// pins (`== -1`, :344), zero and NaN are never-used (NaN fails every
    /// test there, so only the announce-time arm can cull it), and every
    /// other number, a negative or an infinite one included, is a use.
    #[test]
    fn from_seconds_classifies_the_fifth_element_as_the_reference_does() {
        assert_eq!(
            KnownDestUseState::from_seconds(-1.0),
            KnownDestUseState::Retained
        );
        for used in [-2.0, -0.5, f64::INFINITY, 1_800_000_000.0] {
            assert_eq!(
                KnownDestUseState::from_seconds(used),
                KnownDestUseState::Used(used),
                "{used} is a use"
            );
        }
        for never in [0.0, -0.0, f64::NAN] {
            assert_eq!(
                KnownDestUseState::from_seconds(never),
                KnownDestUseState::NeverUsed,
                "{never} is never-used"
            );
        }
    }
}
