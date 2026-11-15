//! Height packs reaching the device (order 1863): the catalogue a map
//! server publishes beside its height packs, and the one-block pack a
//! device writes from a block it fetched (`docs/src/concepts/map-server.md`,
//! "Height packs on the device").
//!
//! The server keeps height packs as `leviculum-map-tool heights` cuts
//! them (one file per area, a directory piece and its block pieces,
//! `leviculum_map::elevation`) and publishes a catalogue, a text file of
//! tab-separated lines, beside them:
//!
//! ```text
//! lmh-catalogue 1
//! pack morocco.lmh 37063251 950
//! block morocco.lmh 9 245 208 950 1359631 -77343750 313536369 -70312500 319521622
//! ```
//!
//! The fields are separated by one tab each, shown here as spaces:
//!
//! - the first line names the format and its version;
//! - `pack`: a pack file's name (one path segment, beside the
//!   catalogue), its length and its directory piece's length;
//! - `block`: a block of a pack listed before: its block zoom, x and y,
//!   its byte offset and length in the pack file, and its bounds in
//!   degrees x 10^7 (west, south, east, north), so a reader that does not
//!   project can still tell where it lies.
//!
//! A line of any other first word is skipped: a later version may add
//! kinds of lines, a reader of this one draws from what it knows.
//!
//! The grid is the packs' own: one block, the z12 tiles under one tile of
//! the block zoom, is the unit a device fetches. It is the smallest piece
//! a pack already stands alone in, and a fixed grid serves boxes the
//! device chooses as it pans: no two boxes fetch the same heights twice,
//! which one pack per region box would.
//!
//! A device fetches a block by one range request into the pack file and
//! writes it as a pack of its own ([`local_pack`]): a directory naming the
//! one block, the block's own bounds as the box, the pack's attribution
//! and notice, then the block piece unchanged.

use std::fmt::Write as _;

use leviculum_map::elevation::{self, Block, Directory, DirectoryInfo, PackIndex, TileDecoder};
use leviculum_map::mercator::{lat_of_y, lon_of_x, x_of_lon, y_of_lat};
use leviculum_map::region::Bounds;

use crate::Res;

/// The catalogue's first word.
pub const CATALOGUE_MAGIC: &str = "lmh-catalogue";
/// The catalogue version this writer writes and this reader reads.
pub const CATALOGUE_VERSION: u32 = 1;
/// The catalogue's usual file name beside the packs and the region
/// archive.
pub const CATALOGUE_NAME: &str = "heights.tsv";

/// One pack file of a catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFile {
    /// Its name beside the catalogue.
    pub name: String,
    /// Its length in bytes.
    pub bytes: u64,
    /// Its directory piece's length: the bytes a device reads for the
    /// attribution and notice.
    pub directory: u64,
}

/// One block of a catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEntry {
    /// The pack it is in, an index into [`Catalogue::packs`].
    pub pack: usize,
    /// The block zoom.
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
    /// Its byte offset in the pack file.
    pub offset: u64,
    /// Its length.
    pub len: u64,
}

impl BlockEntry {
    /// The ground the block covers.
    pub fn bounds(&self) -> Bounds {
        block_bounds(self.zoom, self.x, self.y)
    }

    /// The file name a device keeps the block under, without a prefix.
    pub fn local_name(&self) -> String {
        format!("h{}-{}-{}.lmh", self.zoom, self.x, self.y)
    }
}

/// A server's height catalogue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalogue {
    pub packs: Vec<PackFile>,
    pub blocks: Vec<BlockEntry>,
}

/// The ground of tile (x, y) at `zoom`, degrees x 10^7.
pub fn block_bounds(zoom: u8, x: u32, y: u32) -> Bounds {
    let shift = 32 - u32::from(zoom.min(31));
    let edge = |n: u32| -> u32 { (u64::from(n) << shift).min(u64::from(u32::MAX)) as u32 };
    // The east and south edge of the grid's last tile is the world's end,
    // one unit short of 2^32.
    let far = |n: u32| -> u32 {
        let v = (u64::from(n) + 1) << shift;
        v.min(u64::from(u32::MAX)) as u32
    };
    let east = match far(x) {
        u32::MAX => 1_800_000_000,
        v => lon_of_x(v),
    };
    Bounds {
        west: lon_of_x(edge(x)),
        south: lat_of_y(far(y)),
        east,
        north: lat_of_y(edge(y)),
    }
}

/// Whether `name` may be a pack file's name: one path segment of
/// letters, digits, `.`, `_` and `-`, not starting with a dot.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Catalogue {
    /// Add the pack file `name` holding `bytes`: every block of it, at
    /// its byte range, after the whole file was checked to split cleanly.
    pub fn add_pack(&mut self, name: &str, bytes: &[u8]) -> Res<()> {
        if !plain_name(name) {
            return Err(format!("{name:?} is no plain file name").into());
        }
        if self.packs.iter().any(|p| p.name == name) {
            return Err(format!("{name} is listed twice").into());
        }
        let (directory, ranges) = elevation::split(bytes).map_err(|e| format!("{name}: {e:?}"))?;
        let pack = self.packs.len();
        self.packs.push(PackFile {
            name: name.to_string(),
            bytes: bytes.len() as u64,
            directory: ranges.first().map_or(0, |r| r.end as u64),
        });
        for (n, range) in ranges.iter().skip(1).enumerate() {
            let Some((x, y)) = directory.block(n) else {
                return Err(format!("{name}: block {n} is not in the directory").into());
            };
            self.blocks.push(BlockEntry {
                pack,
                zoom: directory.block_zoom(),
                x,
                y,
                offset: range.start as u64,
                len: range.len() as u64,
            });
        }
        Ok(())
    }

    /// The catalogue as its text.
    pub fn write(&self) -> String {
        let mut out = format!("{CATALOGUE_MAGIC}\t{CATALOGUE_VERSION}\n");
        for pack in &self.packs {
            let _ = writeln!(
                out,
                "pack\t{}\t{}\t{}",
                pack.name, pack.bytes, pack.directory
            );
        }
        for b in &self.blocks {
            let bounds = b.bounds();
            let _ = writeln!(
                out,
                "block\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                self.packs[b.pack].name,
                b.zoom,
                b.x,
                b.y,
                b.offset,
                b.len,
                bounds.west,
                bounds.south,
                bounds.east,
                bounds.north
            );
        }
        out
    }

    /// Read a catalogue's text. Refused: another first line or version, a
    /// pack name that is not one plain segment or listed twice, a block of
    /// a pack not listed before it, a block outside its pack's bytes or
    /// its zoom's grid, a number that does not parse.
    pub fn parse(text: &str) -> Res<Self> {
        let mut lines = text.lines().enumerate();
        let first: Vec<&str> = lines
            .next()
            .map(|(_, l)| l.split('\t').collect())
            .unwrap_or_default();
        match first.as_slice() {
            [magic, version] if *magic == CATALOGUE_MAGIC => {
                let version: u32 = version.trim().parse()?;
                if version != CATALOGUE_VERSION {
                    return Err(format!("catalogue version {version}").into());
                }
            }
            _ => return Err("not a height catalogue".into()),
        }
        let mut catalogue = Self::default();
        for (n, line) in lines {
            let at = |e: &dyn core::fmt::Display| format!("catalogue line {}: {e}", n + 1);
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["pack", name, bytes, directory, ..] => {
                    if !plain_name(name) {
                        return Err(at(&"no plain file name").into());
                    }
                    if catalogue.packs.iter().any(|p| p.name == *name) {
                        return Err(at(&"pack listed twice").into());
                    }
                    let bytes: u64 = bytes.parse().map_err(|e| at(&e))?;
                    let directory: u64 = directory.parse().map_err(|e| at(&e))?;
                    if directory > bytes {
                        return Err(at(&"directory longer than the pack").into());
                    }
                    catalogue.packs.push(PackFile {
                        name: name.to_string(),
                        bytes,
                        directory,
                    });
                }
                ["block", name, zoom, x, y, offset, len, ..] => {
                    let pack = catalogue
                        .packs
                        .iter()
                        .position(|p| p.name == *name)
                        .ok_or_else(|| at(&"block of a pack not listed"))?;
                    let zoom: u8 = zoom.parse().map_err(|e| at(&e))?;
                    let x: u32 = x.parse().map_err(|e| at(&e))?;
                    let y: u32 = y.parse().map_err(|e| at(&e))?;
                    let offset: u64 = offset.parse().map_err(|e| at(&e))?;
                    let len: u64 = len.parse().map_err(|e| at(&e))?;
                    if zoom > elevation::MAX_TILE_ZOOM
                        || u64::from(x) >= 1 << zoom
                        || u64::from(y) >= 1 << zoom
                    {
                        return Err(at(&"block outside its grid").into());
                    }
                    let file = &catalogue.packs[pack];
                    if offset < file.directory
                        || len == 0
                        || offset.checked_add(len).is_none_or(|end| end > file.bytes)
                    {
                        return Err(at(&"block outside its pack").into());
                    }
                    catalogue.blocks.push(BlockEntry {
                        pack,
                        zoom,
                        x,
                        y,
                        offset,
                        len,
                    });
                }
                _ => {}
            }
        }
        Ok(catalogue)
    }

    /// The blocks under `bounds`, each place once (the first listed): what
    /// a device fetches for a region of that box. A box across the
    /// antimeridian (west east of east) has none.
    pub fn under(&self, bounds: &Bounds) -> Vec<BlockEntry> {
        let mut out: Vec<BlockEntry> = Vec::new();
        if bounds.west >= bounds.east || bounds.south >= bounds.north {
            return out;
        }
        for b in &self.blocks {
            let shift = 32 - u32::from(b.zoom.min(31));
            let x0 = x_of_lon(bounds.west) >> shift;
            // The east and south edges are exclusive: a box ending on a
            // block's edge does not reach into it.
            let x1 = x_of_lon(bounds.east).saturating_sub(1) >> shift;
            let y0 = y_of_lat(bounds.north) >> shift;
            let y1 = y_of_lat(bounds.south).saturating_sub(1) >> shift;
            let inside = (x0..=x1).contains(&b.x) && (y0..=y1).contains(&b.y);
            if inside && !out.iter().any(|o| (o.zoom, o.x, o.y) == (b.zoom, b.x, b.y)) {
                out.push(*b);
            }
        }
        out
    }
}

/// The pack a device writes for `entry`: a directory naming the one block
/// with the block's own bounds as the box, the attribution and notice of
/// `directory` (the pack file's directory piece), then `block` (the bytes
/// the catalogue's range named). Every tile of the block is decoded once,
/// so a frame that does not decode or whose checksum disagrees is refused
/// here and never lands; the result is checked to index as a pack.
pub fn local_pack(directory: &[u8], block: &[u8], entry: &BlockEntry) -> Res<Vec<u8>> {
    let directory = Directory::parse(directory).map_err(|e| format!("directory: {e:?}"))?;
    let parsed = Block::parse(block).map_err(|e| format!("block: {e:?}"))?;
    if parsed.position() != (entry.x, entry.y)
        || parsed.block_zoom() != entry.zoom
        || directory.block_zoom() != entry.zoom
        || parsed.tile_zoom() != directory.tile_zoom()
    {
        return Err(format!(
            "the block is not {}/{}/{} of its pack",
            entry.zoom, entry.x, entry.y
        )
        .into());
    }
    let mut decoder = TileDecoder::new();
    let mut out = vec![0i16; elevation::TILE_CELLS];
    let mut scratch = vec![0i16; elevation::TILE_CELLS];
    let side = parsed.side();
    let tz = parsed.tile_zoom();
    for ty in 0..side {
        for tx in 0..side {
            parsed
                .heights(
                    &mut decoder,
                    tz,
                    entry.x * side + tx,
                    entry.y * side + ty,
                    &mut out,
                    &mut scratch,
                )
                .map_err(|e| format!("block tile {tx},{ty}: {e:?}"))?;
        }
    }
    let b = entry.bounds();
    let mut pack = elevation::write_directory(
        &DirectoryInfo {
            tile_zoom: directory.tile_zoom(),
            block_zoom: directory.block_zoom(),
            bounds: [b.west, b.south, b.east, b.north],
            attribution: directory.attribution(),
            notice: directory.notice(),
        },
        &[(entry.x, entry.y)],
    )
    .map_err(|e| format!("directory: {e:?}"))?;
    pack.extend_from_slice(block);
    PackIndex::of(&pack).map_err(|e| format!("the pack does not index: {e:?}"))?;
    Ok(pack)
}
