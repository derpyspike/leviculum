//! Ground heights through the facade (order 1758): a height pack
//! (`leviculum_map::elevation`, the directory piece then its block pieces)
//! held whole, and one tile's heights at a time, stored (the tile zoom) or
//! derived as means (down to the block zoom). The map's relief will read
//! through here; today it is the entry point the host measures the phone's
//! decode cost through (`examples/height_decode.rs`).

use leviculum_map::elevation::{PackIndex, TileDecoder, TILE_CELLS};

/// A height pack and the decoder state one tile needs, allocated once.
/// The pack is found and decoded through [`PackIndex`], the one reader
/// the desktop's relief store (`loquax/src/relief.rs`) shares.
pub struct HeightPack {
    bytes: Vec<u8>,
    index: PackIndex,
    decoder: TileDecoder,
    scratch: Vec<i16>,
}

impl HeightPack {
    /// Check the whole pack's layout and keep it.
    pub fn open(bytes: Vec<u8>) -> Result<Self, String> {
        let index = PackIndex::of(&bytes).map_err(|e| format!("height pack: {e:?}"))?;
        Ok(HeightPack {
            bytes,
            index,
            decoder: TileDecoder::new(),
            scratch: vec![0; TILE_CELLS],
        })
    }

    /// The zooms a tile can be asked for: the block zoom to the tile zoom.
    pub fn zooms(&self) -> std::ops::RangeInclusive<u8> {
        self.index.zooms()
    }

    /// Every tile the pack covers at `zoom`, block by block.
    pub fn tiles(&self, zoom: u8) -> Vec<(u32, u32)> {
        self.index.tiles(zoom)
    }

    /// The heights of tile (x, y) at `zoom` into `out` (256 x 256 metres,
    /// north row first).
    pub fn heights(&mut self, zoom: u8, x: u32, y: u32, out: &mut [i16]) -> Result<(), String> {
        if !self.zooms().contains(&zoom) {
            return Err(format!("zoom {zoom} outside the pack"));
        }
        let range = self
            .index
            .block_for(zoom, x, y)
            .ok_or_else(|| format!("z{zoom} {x}/{y}: no block holds it"))?;
        self.index
            .decode(
                &self.bytes[range],
                (zoom, x, y),
                &mut self.decoder,
                out,
                &mut self.scratch,
            )
            .map_err(|e| format!("z{zoom} {x}/{y}: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHEBBI: &[u8] = include_bytes!("../../leviculum-map/fixtures/erg-chebbi.lmh");

    #[test]
    fn the_erg_chebbi_pack_decodes_through_the_facade() {
        let mut pack = HeightPack::open(CHEBBI.to_vec()).expect("open");
        assert_eq!(pack.zooms(), 11..=12);
        assert_eq!(pack.tiles(12).len(), 4);
        assert_eq!(pack.tiles(11), vec![(1001, 837)]);
        assert!(pack.tiles(10).is_empty());
        let mut out = vec![0i16; TILE_CELLS];
        for (x, y) in pack.tiles(12) {
            pack.heights(12, x, y, &mut out).expect("stored tile");
            assert!(out.iter().all(|&h| (650..=950).contains(&h)));
        }
        pack.heights(11, 1001, 837, &mut out).expect("derived tile");
        assert!(pack.heights(12, 0, 0, &mut out).is_err());
        assert!(pack.heights(10, 500, 418, &mut out).is_err());
        assert!(HeightPack::open(CHEBBI[..CHEBBI.len() - 1].to_vec()).is_err());
    }
}
