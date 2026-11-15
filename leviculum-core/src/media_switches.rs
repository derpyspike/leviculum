//! A board's media opt-outs, as one flash sector (1480).
//!
//! Media are on by default (the zero-config decision, #471): a board with
//! no record, an erased sector, or an old partition table runs every
//! carrier its image has. The record exists only to switch one off —
//! `lnflash --ble off` writes it, the ThinkNode M9 reads it at boot from
//! its `media` partition (`leviculum-esp/partitions-m9.csv`). One codec
//! for both sides, here, so the host and the board cannot disagree on
//! the layout.
//!
//! Layout:
//!
//! ```text
//! 0..4  magic "LMED"
//! 4     version (1)
//! 5     opt-out bits: bit 0 = BLE off; the rest reserved, 0
//! 6..8  reserved, 0
//! 8..12 check: the first four bytes of full_hash(bytes 0..8)
//! ```
//!
//! An erased sector (all 0xff) and anything else that does not check
//! means "no opt-outs". Unknown bits survive a decode/encode round trip,
//! so an older firmware does not silently clear a newer flag it does not
//! know.

use crate::crypto::full_hash_parts;

/// The record's size; the rest of its sector stays erased.
pub const RECORD_LEN: usize = 12;

const MAGIC: &[u8; 4] = b"LMED";
const VERSION: u8 = 1;
const BITS_AT: usize = 5;
const CHECK_AT: usize = 8;

/// BLE off (bit 0 of the opt-out byte).
const BLE_OFF: u8 = 1 << 0;

/// Which media a board was told not to run.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MediaSwitches {
    opt_out: u8,
}

impl MediaSwitches {
    /// No opt-outs: every carrier on, the state of a board without a
    /// record.
    pub const fn all_on() -> Self {
        Self { opt_out: 0 }
    }

    /// Whether the BLE carrier is on.
    pub fn ble(self) -> bool {
        self.opt_out & BLE_OFF == 0
    }

    /// Switch the BLE carrier.
    pub fn set_ble(self, on: bool) -> Self {
        let opt_out = if on {
            self.opt_out & !BLE_OFF
        } else {
            self.opt_out | BLE_OFF
        };
        Self { opt_out }
    }

    /// The record as it goes into flash.
    pub fn encode(self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[..4].copy_from_slice(MAGIC);
        out[4] = VERSION;
        out[BITS_AT] = self.opt_out;
        let check = full_hash_parts(&[&out[..CHECK_AT]]);
        out[CHECK_AT..].copy_from_slice(&check[..4]);
        out
    }

    /// The record at the start of `sector`; `None` for an erased sector
    /// or one that does not check — which both mean [`Self::all_on`].
    pub fn decode(sector: &[u8]) -> Option<Self> {
        let rec = sector.get(..RECORD_LEN)?;
        if &rec[..4] != MAGIC || rec[4] != VERSION {
            return None;
        }
        let check = full_hash_parts(&[&rec[..CHECK_AT]]);
        if rec[CHECK_AT..RECORD_LEN] != check[..4] {
            return None;
        }
        Some(Self {
            opt_out: rec[BITS_AT],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_comes_back_as_it_went() {
        let off = MediaSwitches::all_on().set_ble(false);
        let mut sector = [0xffu8; 4096];
        sector[..RECORD_LEN].copy_from_slice(&off.encode());
        let back = MediaSwitches::decode(&sector).unwrap();
        assert!(!back.ble());
        assert!(MediaSwitches::decode(&back.set_ble(true).encode())
            .unwrap()
            .ble());
    }

    #[test]
    fn an_erased_or_damaged_sector_means_everything_on() {
        assert!(MediaSwitches::decode(&[0xff; 4096]).is_none());
        assert!(MediaSwitches::decode(&[0; 4096]).is_none());
        let mut rec = MediaSwitches::all_on().set_ble(false).encode();
        rec[BITS_AT] ^= 1;
        assert!(MediaSwitches::decode(&rec).is_none());
        assert!(MediaSwitches::decode(&rec[..RECORD_LEN - 1]).is_none());
        assert!(MediaSwitches::all_on().ble());
    }

    #[test]
    fn unknown_bits_survive_a_round_trip() {
        // A newer host set a flag this firmware does not know: reading
        // and rewriting the record keeps it.
        let mut rec = MediaSwitches { opt_out: 0b1010 }.encode();
        let read = MediaSwitches::decode(&rec).unwrap();
        assert!(read.ble());
        rec = read.set_ble(false).encode();
        assert_eq!(MediaSwitches::decode(&rec).unwrap().opt_out, 0b1011);
    }
}
