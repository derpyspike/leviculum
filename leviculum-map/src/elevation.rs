//! Height pieces: ground heights shipped to the device instead of finished
//! shade pictures (order 1740, K6a; `docs/src/concepts/map-server.md`,
//! "Height packs").
//!
//! A height pack is a directory piece and one block piece per block. A
//! block is the square of tiles at the tile zoom (z12) that one tile at the
//! block zoom (z9) covers, 8 x 8 of them; every block piece stands alone,
//! so a device fetches the blocks of its view in any order, and the coarser
//! zooms from the block zoom to the tile zoom are averaged out of the block
//! on the device ([`Block::heights`]), never shipped.
//!
//! A tile is 256 x 256 heights in whole metres, row-major, north row first,
//! each at the centre of its Web Mercator pixel. It is stored as the
//! residual of a 2D predictor (left plus up minus up-left; the first row
//! predicts from the left, the first column from above, the first cell
//! from zero), all in wrapping 16-bit arithmetic so every `i16` grid round
//! trips, zigzag-mapped to `u16`, split into byte planes (all high bytes,
//! then all low bytes) and compressed as one zstd frame that carries its
//! content checksum.
//!
//! All integers little-endian.
//!
//! ```text
//! directory piece
//!   0  "LMHD"
//!   4  u16 version (1)        6  u16 header length (40)
//!   8  u8 tile zoom           9  u8 block zoom (tile zoom - 4 ..= tile zoom)
//!  10  u16 tile edge (256)   12  u32 block count
//!  16  i32 west, south, east, north, degrees x 10^7: the box the cut was
//!      asked for (the blocks cover it and may reach past it)
//!  32  u32 attribution length  36 u32 notice length
//!  40  attribution, notice     UTF-8
//!      blocks, in piece order, 8 bytes each: u32 x, u32 y at the block zoom
//!
//! block piece
//!   0  "LMHB"
//!   4  u16 version (1)        6  u16 header length (24)
//!   8  u8 tile zoom           9  u8 block zoom      10  u16 tile edge (256)
//!  12  u32 x, 16 u32 y at the block zoom
//!  20  u16 tile count ((2^(tile zoom - block zoom))^2)  22 u16 0
//!  24  u32 compressed length per tile, row-major within the block
//!      the tiles' zstd frames, in the same order
//! ```
//!
//! A reader refuses, before it allocates anything sized by the piece: a
//! wrong magic, a version it does not know, a header length that is not the
//! version's, zooms or an edge it does not read, a tile count that is not
//! the zooms', lengths that do not add up to the piece's own length, and
//! block coordinates outside the block zoom's grid. A tile is refused when
//! its frame does not decode to exactly 256 x 256 x 2 bytes, has no content
//! checksum, or the checksum disagrees; the decoder's window is capped at
//! one tile, so a hostile frame header cannot make it allocate more.

use alloc::vec;
use alloc::vec::Vec;

use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
use ruzstd::io::Read;

/// The directory piece's first four bytes.
pub const DIRECTORY_MAGIC: [u8; 4] = *b"LMHD";
/// A block piece's first four bytes.
pub const BLOCK_MAGIC: [u8; 4] = *b"LMHB";

/// What a pack's tiles hold. The container (directory piece, block
/// pieces, one zstd frame per tile, the refusals) is the same for every
/// kind; the magics tell the kinds apart, so a reader of one kind
/// refuses a pack of the other by its first four bytes, and the tile
/// payload is the kind's own: two byte planes of height residuals for
/// [`PackKind::Heights`], one ground class byte per cell for
/// [`PackKind::Ground`] (order 1743, [`crate::ground`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackKind {
    /// Heights in whole metres (`LMHD`, `LMHB`).
    Heights,
    /// Ground classes (`LMGD`, `LMGB`).
    Ground,
}

impl PackKind {
    /// The directory piece's magic.
    pub const fn directory_magic(self) -> [u8; 4] {
        match self {
            PackKind::Heights => DIRECTORY_MAGIC,
            PackKind::Ground => *b"LMGD",
        }
    }

    /// A block piece's magic.
    pub const fn block_magic(self) -> [u8; 4] {
        match self {
            PackKind::Heights => BLOCK_MAGIC,
            PackKind::Ground => *b"LMGB",
        }
    }

    /// One tile's decoded bytes.
    pub const fn tile_bytes(self) -> usize {
        match self {
            PackKind::Heights => TILE_BYTES,
            PackKind::Ground => TILE_CELLS,
        }
    }
}
/// The format version this writer writes and this reader reads.
pub const VERSION: u16 = 1;
/// A tile's edge in heights.
pub const TILE_EDGE: usize = 256;
/// The heights in one tile.
pub const TILE_CELLS: usize = TILE_EDGE * TILE_EDGE;
/// One tile's decoded bytes: two byte planes.
pub const TILE_BYTES: usize = TILE_CELLS * 2;
/// The tile zoom of the packs this tree cuts.
pub const TILE_ZOOM: u8 = 12;
/// The block zoom of the packs this tree cuts: one block is 8 x 8 tiles.
pub const BLOCK_ZOOM: u8 = 9;
/// The deepest zoom a reader takes as tile zoom: block coordinates and
/// pixel positions stay well inside `u32`.
pub const MAX_TILE_ZOOM: u8 = 20;
/// The most zooms a block spans below its tile zoom: 16 x 16 tiles, and a
/// derived height is the mean of at most 16 x 16 cells.
pub const MAX_ZOOM_SPAN: u8 = 4;

const DIRECTORY_HEADER: usize = 40;
const BLOCK_HEADER: usize = 24;
const DIRECTORY_ENTRY: usize = 8;

/// Why a height piece was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElevationError {
    /// Shorter than its fixed header.
    TooShort,
    /// Not the magic of the piece asked for.
    Magic,
    /// A version this reader does not read.
    Version(u16),
    /// The header length is not the version's.
    HeaderLength,
    /// Tile zoom deeper than [`MAX_TILE_ZOOM`], block zoom deeper than the
    /// tile zoom or more than [`MAX_ZOOM_SPAN`] above it.
    Zooms,
    /// A tile edge other than [`TILE_EDGE`].
    TileEdge,
    /// A tile count that is not the zooms' square.
    TileCount,
    /// The lengths do not add up to the piece's own length.
    Length,
    /// Block coordinates outside the block zoom's grid, or a block that is
    /// not the one the directory names at that place.
    Coordinates,
    /// The tiles were asked for as a kind the pack does not hold
    /// (heights of a ground pack, or the reverse).
    Kind,
    /// The attribution or the notice is not UTF-8.
    Text,
    /// A height asked for outside the block, or at a zoom it does not span.
    Outside,
    /// An output buffer that is not one tile.
    Buffer,
    /// One tile's frame was refused.
    Tile {
        /// The tile's place in the block, row-major.
        index: u16,
        /// What was wrong with it.
        fault: TileFault,
    },
}

/// Why one tile's frame was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileFault {
    /// The frame does not decode (a broken header, block, or a window
    /// larger than one tile).
    Frame,
    /// It decodes to more or fewer bytes than one tile.
    Size,
    /// The frame has no content checksum.
    NoChecksum,
    /// The checksum disagrees with the decoded bytes.
    Checksum,
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn i32le(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The checks every piece's zoom pair passes; returns the tiles per block
/// side.
fn check_zooms(tile_zoom: u8, block_zoom: u8, edge: u16) -> Result<u32, ElevationError> {
    if tile_zoom > MAX_TILE_ZOOM || block_zoom > tile_zoom || tile_zoom - block_zoom > MAX_ZOOM_SPAN
    {
        return Err(ElevationError::Zooms);
    }
    if usize::from(edge) != TILE_EDGE {
        return Err(ElevationError::TileEdge);
    }
    Ok(1 << (tile_zoom - block_zoom))
}

/// The common header of both pieces: magic, version, header length.
fn check_header(b: &[u8], magic: [u8; 4], header: usize) -> Result<(), ElevationError> {
    if b.len() < 8 {
        return Err(ElevationError::TooShort);
    }
    if b[..4] != magic {
        return Err(ElevationError::Magic);
    }
    let version = u16le(b, 4);
    if version != VERSION {
        return Err(ElevationError::Version(version));
    }
    if usize::from(u16le(b, 6)) != header {
        return Err(ElevationError::HeaderLength);
    }
    if b.len() < header {
        return Err(ElevationError::TooShort);
    }
    Ok(())
}

/// A parsed directory piece.
#[derive(Clone, Copy, Debug)]
pub struct Directory<'a> {
    kind: PackKind,
    bytes: &'a [u8],
    tile_zoom: u8,
    block_zoom: u8,
    count: usize,
    bounds: [i32; 4],
    attribution: &'a str,
    notice: &'a str,
    entries_at: usize,
}

impl<'a> Directory<'a> {
    /// Parse the directory at the start of `bytes`, returning it and its
    /// length: the blocks follow it in a whole height file.
    pub fn parse_prefix(bytes: &'a [u8]) -> Result<(Self, usize), ElevationError> {
        Self::parse_prefix_of(PackKind::Heights, bytes)
    }

    /// [`Directory::parse_prefix`] for a pack of `kind`: another kind's
    /// magic is refused.
    pub fn parse_prefix_of(
        kind: PackKind,
        bytes: &'a [u8],
    ) -> Result<(Self, usize), ElevationError> {
        check_header(bytes, kind.directory_magic(), DIRECTORY_HEADER)?;
        let tile_zoom = bytes[8];
        let block_zoom = bytes[9];
        check_zooms(tile_zoom, block_zoom, u16le(bytes, 10))?;
        let count = u32le(bytes, 12) as usize;
        let bounds = [
            i32le(bytes, 16),
            i32le(bytes, 20),
            i32le(bytes, 24),
            i32le(bytes, 28),
        ];
        let attribution_len = u32le(bytes, 32) as u64;
        let notice_len = u32le(bytes, 36) as u64;
        // u64 throughout: a hostile count or length cannot wrap.
        let len = DIRECTORY_HEADER as u64
            + attribution_len
            + notice_len
            + count as u64 * DIRECTORY_ENTRY as u64;
        if len > bytes.len() as u64 {
            return Err(ElevationError::Length);
        }
        let len = len as usize;
        let text_end = DIRECTORY_HEADER + attribution_len as usize;
        let attribution = core::str::from_utf8(&bytes[DIRECTORY_HEADER..text_end])
            .map_err(|_| ElevationError::Text)?;
        let notice_end = text_end + notice_len as usize;
        let notice =
            core::str::from_utf8(&bytes[text_end..notice_end]).map_err(|_| ElevationError::Text)?;
        let grid = 1u64 << block_zoom;
        for n in 0..count {
            let at = notice_end + n * DIRECTORY_ENTRY;
            if u64::from(u32le(bytes, at)) >= grid || u64::from(u32le(bytes, at + 4)) >= grid {
                return Err(ElevationError::Coordinates);
            }
        }
        Ok((
            Directory {
                kind,
                bytes: &bytes[..len],
                tile_zoom,
                block_zoom,
                count,
                bounds,
                attribution,
                notice,
                entries_at: notice_end,
            },
            len,
        ))
    }

    /// Parse a directory piece that is exactly `bytes`.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ElevationError> {
        Self::parse_of(PackKind::Heights, bytes)
    }

    /// [`Directory::parse`] for a pack of `kind`.
    pub fn parse_of(kind: PackKind, bytes: &'a [u8]) -> Result<Self, ElevationError> {
        let (directory, len) = Self::parse_prefix_of(kind, bytes)?;
        if len != bytes.len() {
            return Err(ElevationError::Length);
        }
        Ok(directory)
    }

    /// What the pack's tiles hold.
    pub fn kind(&self) -> PackKind {
        self.kind
    }

    /// The zoom the tiles are stored at.
    pub fn tile_zoom(&self) -> u8 {
        self.tile_zoom
    }

    /// The zoom one block is a tile of; the shallowest zoom a device derives.
    pub fn block_zoom(&self) -> u8 {
        self.block_zoom
    }

    /// The box the cut was asked for, degrees x 10^7, west/south/east/north.
    pub fn bounds(&self) -> [i32; 4] {
        self.bounds
    }

    /// The attribution line, shown on the map.
    pub fn attribution(&self) -> &'a str {
        self.attribution
    }

    /// The licence notice.
    pub fn notice(&self) -> &'a str {
        self.notice
    }

    /// How many block pieces follow.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the pack has no blocks.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Block `n`'s coordinates at the block zoom, in piece order.
    pub fn block(&self, n: usize) -> Option<(u32, u32)> {
        if n >= self.count {
            return None;
        }
        let at = self.entries_at + n * DIRECTORY_ENTRY;
        Some((u32le(self.bytes, at), u32le(self.bytes, at + 4)))
    }

    /// The piece number of the block at (x, y) on the block zoom's grid.
    pub fn find(&self, x: u32, y: u32) -> Option<usize> {
        (0..self.count).find(|&n| self.block(n) == Some((x, y)))
    }
}

/// What a block piece's header and length table say, read without its
/// frames: enough to index a pack with bounded reads ([`PackIndex`]).
#[derive(Clone, Copy, Debug)]
struct BlockHead {
    tile_zoom: u8,
    block_zoom: u8,
    side: u32,
    x: u32,
    y: u32,
    /// The whole piece's length.
    len: usize,
}

impl BlockHead {
    /// Read the head from `bytes`, which must hold at least the header
    /// and the length table ([`BlockHead::table_len`]).
    fn parse(kind: PackKind, bytes: &[u8]) -> Result<Self, ElevationError> {
        check_header(bytes, kind.block_magic(), BLOCK_HEADER)?;
        let tile_zoom = bytes[8];
        let block_zoom = bytes[9];
        let side = check_zooms(tile_zoom, block_zoom, u16le(bytes, 10))?;
        let x = u32le(bytes, 12);
        let y = u32le(bytes, 16);
        let grid = 1u64 << block_zoom;
        if u64::from(x) >= grid || u64::from(y) >= grid {
            return Err(ElevationError::Coordinates);
        }
        let count = usize::from(u16le(bytes, 20));
        if count as u32 != side * side {
            return Err(ElevationError::TileCount);
        }
        let table_end = BLOCK_HEADER + 4 * count;
        if bytes.len() < table_end {
            return Err(ElevationError::Length);
        }
        let mut len = table_end as u64;
        for n in 0..count {
            len += u64::from(u32le(bytes, BLOCK_HEADER + 4 * n));
        }
        let len = usize::try_from(len).map_err(|_| ElevationError::Length)?;
        Ok(Self {
            tile_zoom,
            block_zoom,
            side,
            x,
            y,
            len,
        })
    }

    /// How many bytes of a block piece [`BlockHead::parse`] needs, from
    /// its fixed header: the header and the length table.
    fn table_len(kind: PackKind, header: &[u8]) -> Result<usize, ElevationError> {
        check_header(header, kind.block_magic(), BLOCK_HEADER)?;
        Ok(BLOCK_HEADER + 4 * usize::from(u16le(header, 20)))
    }
}

/// A height pack's index: its zooms and each block's position and byte
/// range in the file, read through bounded reads — the directory piece
/// and each block's header and length table, never a tile frame — so a
/// device keeps a 37 MB pack on disk and reads one block when a tile
/// needs it. The one place a pack's tiles are found and decoded: the
/// phone's facade and the desktop's relief store both go through it.
#[derive(Clone, Debug)]
pub struct PackIndex {
    kind: PackKind,
    tile_zoom: u8,
    block_zoom: u8,
    blocks: Vec<((u32, u32), core::ops::Range<usize>)>,
}

impl PackIndex {
    /// Index the pack of `len` bytes that `read_at(offset, buf)` reads
    /// from: it fills `buf` from `offset` and says whether it could. A
    /// read that fails or a piece that does not add up to `len` is
    /// refused as [`ElevationError::Length`]; the pieces' own checks
    /// are [`Directory::parse`]'s and [`Block::parse_prefix`]'s.
    pub fn read(
        len: u64,
        read_at: impl FnMut(u64, &mut [u8]) -> bool,
    ) -> Result<Self, ElevationError> {
        Self::read_of(PackKind::Heights, len, read_at)
    }

    /// [`PackIndex::read`] for a pack of `kind`.
    pub fn read_of(
        kind: PackKind,
        len: u64,
        mut read_at: impl FnMut(u64, &mut [u8]) -> bool,
    ) -> Result<Self, ElevationError> {
        let mut head = [0u8; DIRECTORY_HEADER];
        if len < DIRECTORY_HEADER as u64 || !read_at(0, &mut head) {
            return Err(ElevationError::TooShort);
        }
        check_header(&head, kind.directory_magic(), DIRECTORY_HEADER)?;
        let dir_len = DIRECTORY_HEADER as u64
            + u64::from(u32le(&head, 32))
            + u64::from(u32le(&head, 36))
            + u64::from(u32le(&head, 12)) * DIRECTORY_ENTRY as u64;
        if dir_len > len {
            return Err(ElevationError::Length);
        }
        let mut dir_bytes = vec![0u8; dir_len as usize];
        if !read_at(0, &mut dir_bytes) {
            return Err(ElevationError::Length);
        }
        let dir = Directory::parse_of(kind, &dir_bytes)?;
        let mut blocks = Vec::with_capacity(dir.len());
        let mut at = dir_len;
        let mut fixed = [0u8; BLOCK_HEADER];
        let mut table = Vec::new();
        for n in 0..dir.len() {
            if at + BLOCK_HEADER as u64 > len || !read_at(at, &mut fixed) {
                return Err(ElevationError::Length);
            }
            let need = BlockHead::table_len(kind, &fixed)?;
            if at + need as u64 > len {
                return Err(ElevationError::Length);
            }
            table.resize(need, 0);
            if !read_at(at, &mut table) {
                return Err(ElevationError::Length);
            }
            let block = BlockHead::parse(kind, &table)?;
            if Some((block.x, block.y)) != dir.block(n)
                || block.tile_zoom != dir.tile_zoom()
                || block.block_zoom != dir.block_zoom()
            {
                return Err(ElevationError::Coordinates);
            }
            let end = at + block.len as u64;
            if end > len {
                return Err(ElevationError::Length);
            }
            blocks.push(((block.x, block.y), at as usize..end as usize));
            at = end;
        }
        if at != len {
            return Err(ElevationError::Length);
        }
        Ok(Self {
            kind,
            tile_zoom: dir.tile_zoom(),
            block_zoom: dir.block_zoom(),
            blocks,
        })
    }

    /// Index a pack held whole in memory.
    pub fn of(bytes: &[u8]) -> Result<Self, ElevationError> {
        Self::of_kind(PackKind::Heights, bytes)
    }

    /// [`PackIndex::of`] for a pack of `kind`.
    pub fn of_kind(kind: PackKind, bytes: &[u8]) -> Result<Self, ElevationError> {
        Self::read_of(kind, bytes.len() as u64, |at, buf| {
            let Ok(at) = usize::try_from(at) else {
                return false;
            };
            match bytes.get(at..at + buf.len()) {
                Some(src) => {
                    buf.copy_from_slice(src);
                    true
                }
                None => false,
            }
        })
    }

    /// What the pack's tiles hold.
    pub fn kind(&self) -> PackKind {
        self.kind
    }

    /// The zoom the tiles are stored at.
    pub fn tile_zoom(&self) -> u8 {
        self.tile_zoom
    }

    /// The shallowest zoom a tile can be derived at.
    pub fn block_zoom(&self) -> u8 {
        self.block_zoom
    }

    /// The zooms a tile can be asked for: the block zoom to the tile zoom.
    pub fn zooms(&self) -> core::ops::RangeInclusive<u8> {
        self.block_zoom..=self.tile_zoom
    }

    /// The byte range of the block piece holding tile (x, y) at `zoom`,
    /// `None` where the pack does not hold it or `zoom` is outside
    /// [`zooms`](Self::zooms).
    pub fn block_for(&self, zoom: u8, x: u32, y: u32) -> Option<core::ops::Range<usize>> {
        if !self.zooms().contains(&zoom) {
            return None;
        }
        let shift = zoom - self.block_zoom;
        let at = (x >> shift, y >> shift);
        self.blocks
            .iter()
            .find(|(p, _)| *p == at)
            .map(|(_, r)| r.clone())
    }

    /// Every tile the pack covers at `zoom`, block by block.
    pub fn tiles(&self, zoom: u8) -> Vec<(u32, u32)> {
        if !self.zooms().contains(&zoom) {
            return Vec::new();
        }
        let per = 1u32 << (zoom - self.block_zoom);
        let mut out = Vec::new();
        for &((bx, by), _) in &self.blocks {
            for y in by * per..(by + 1) * per {
                for x in bx * per..(bx + 1) * per {
                    out.push((x, y));
                }
            }
        }
        out
    }

    /// The heights of tile `(zoom, x, y)` into `out`, from `piece`, the
    /// bytes of [`block_for`](Self::block_for)'s range.
    pub fn decode(
        &self,
        piece: &[u8],
        (zoom, x, y): (u8, u32, u32),
        decoder: &mut TileDecoder,
        out: &mut [i16],
        scratch: &mut [i16],
    ) -> Result<(), ElevationError> {
        Block::parse_of(self.kind, piece)?.heights(decoder, zoom, x, y, out, scratch)
    }

    /// The ground classes of tile `(zoom, x, y)` into `out`, from
    /// `piece`, as [`decode`](Self::decode) for a ground pack
    /// ([`Block::classes`]).
    pub fn decode_classes(
        &self,
        piece: &[u8],
        (zoom, x, y): (u8, u32, u32),
        decoder: &mut TileDecoder,
        out: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<(), ElevationError> {
        Block::parse_of(self.kind, piece)?.classes(decoder, zoom, x, y, out, scratch)
    }
}

/// A parsed block piece, its tiles still compressed.
#[derive(Clone, Copy, Debug)]
pub struct Block<'a> {
    kind: PackKind,
    bytes: &'a [u8],
    tile_zoom: u8,
    block_zoom: u8,
    x: u32,
    y: u32,
    side: u32,
}

impl<'a> Block<'a> {
    /// Parse the block at the start of `bytes`, returning it and its length.
    pub fn parse_prefix(bytes: &'a [u8]) -> Result<(Self, usize), ElevationError> {
        Self::parse_prefix_of(PackKind::Heights, bytes)
    }

    /// [`Block::parse_prefix`] for a pack of `kind`.
    pub fn parse_prefix_of(
        kind: PackKind,
        bytes: &'a [u8],
    ) -> Result<(Self, usize), ElevationError> {
        let head = BlockHead::parse(kind, bytes)?;
        if head.len > bytes.len() {
            return Err(ElevationError::Length);
        }
        Ok((
            Block {
                kind,
                bytes: &bytes[..head.len],
                tile_zoom: head.tile_zoom,
                block_zoom: head.block_zoom,
                x: head.x,
                y: head.y,
                side: head.side,
            },
            head.len,
        ))
    }

    /// Parse a block piece that is exactly `bytes`.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ElevationError> {
        Self::parse_of(PackKind::Heights, bytes)
    }

    /// [`Block::parse`] for a pack of `kind`.
    pub fn parse_of(kind: PackKind, bytes: &'a [u8]) -> Result<Self, ElevationError> {
        let (block, len) = Self::parse_prefix_of(kind, bytes)?;
        if len != bytes.len() {
            return Err(ElevationError::Length);
        }
        Ok(block)
    }

    /// What the block's tiles hold.
    pub fn kind(&self) -> PackKind {
        self.kind
    }

    /// The block's coordinates at the block zoom.
    pub fn position(&self) -> (u32, u32) {
        (self.x, self.y)
    }

    /// The zoom the tiles are stored at.
    pub fn tile_zoom(&self) -> u8 {
        self.tile_zoom
    }

    /// The zoom the block is one tile of.
    pub fn block_zoom(&self) -> u8 {
        self.block_zoom
    }

    /// Tiles per block side at the tile zoom.
    pub fn side(&self) -> u32 {
        self.side
    }

    /// Tile `index`'s compressed frame (row-major within the block).
    pub fn frame(&self, index: usize) -> Option<&'a [u8]> {
        let count = (self.side * self.side) as usize;
        if index >= count {
            return None;
        }
        let mut at = BLOCK_HEADER + 4 * count;
        for n in 0..index {
            at += u32le(self.bytes, BLOCK_HEADER + 4 * n) as usize;
        }
        let len = u32le(self.bytes, BLOCK_HEADER + 4 * index) as usize;
        self.bytes.get(at..at + len)
    }

    /// The heights of the tile (x, y) at `zoom`, which lies between the
    /// block zoom and the tile zoom and inside this block, into `out`
    /// (256 x 256, row-major, north row first). At the tile zoom the
    /// stored tile; above it each height is the mean of the 2^d x 2^d
    /// stored heights it covers (d zooms up), rounded half up, so z11 to
    /// z9 cost no bytes on the wire. Decodes one stored tile at a time:
    /// the working memory is the decoder's and one tile in `scratch`.
    pub fn heights(
        &self,
        decoder: &mut TileDecoder,
        zoom: u8,
        x: u32,
        y: u32,
        out: &mut [i16],
        scratch: &mut [i16],
    ) -> Result<(), ElevationError> {
        if self.kind != PackKind::Heights {
            return Err(ElevationError::Kind);
        }
        if out.len() != TILE_CELLS || scratch.len() != TILE_CELLS {
            return Err(ElevationError::Buffer);
        }
        let (d, first_x, first_y) = self.under(zoom, x, y)?;
        let stored = 1u32 << d; // stored tiles per side under this tile
        if d == 0 {
            let index = (first_y * self.side + first_x) as usize;
            return self.decode(decoder, index, out);
        }
        let px = TILE_EDGE >> d; // output pixels per stored tile side
        let cell = 1usize << d; // stored heights per output pixel side
        let n = (cell * cell) as i32;
        for ty in 0..stored {
            for tx in 0..stored {
                let index = ((first_y + ty) * self.side + first_x + tx) as usize;
                self.decode(decoder, index, scratch)?;
                for oy in 0..px {
                    for ox in 0..px {
                        let mut sum = 0i32;
                        for cy in 0..cell {
                            let row = (oy * cell + cy) * TILE_EDGE + ox * cell;
                            for v in &scratch[row..row + cell] {
                                sum += i32::from(*v);
                            }
                        }
                        let at = (ty as usize * px + oy) * TILE_EDGE + tx as usize * px + ox;
                        out[at] = (2 * sum + n).div_euclid(2 * n) as i16;
                    }
                }
            }
        }
        Ok(())
    }

    /// Where tile (x, y) at `zoom` lies in the block: the zooms `d` it
    /// sits above the tile zoom and the first stored tile under it.
    fn under(&self, zoom: u8, x: u32, y: u32) -> Result<(u8, u32, u32), ElevationError> {
        if zoom < self.block_zoom || zoom > self.tile_zoom {
            return Err(ElevationError::Outside);
        }
        // The tile's place inside the block at its own zoom.
        let shift = zoom - self.block_zoom;
        let (bx, by) = (u64::from(self.x) << shift, u64::from(self.y) << shift);
        let per = 1u64 << shift;
        let (x, y) = (u64::from(x), u64::from(y));
        if x < bx || x >= bx + per || y < by || y >= by + per {
            return Err(ElevationError::Outside);
        }
        let d = self.tile_zoom - zoom;
        Ok((d, ((x - bx) << d) as u32, ((y - by) << d) as u32))
    }

    /// The ground classes of the tile (x, y) at `zoom` into `out` (256 x
    /// 256 class bytes, row-major, north row first), for a ground pack.
    /// At the tile zoom the stored tile; above it each cell is the class
    /// most of the 2^d x 2^d stored cells under it hold
    /// ([`crate::ground::mode`]), so the coarser zooms cost no bytes on
    /// the wire, as for heights.
    pub fn classes(
        &self,
        decoder: &mut TileDecoder,
        zoom: u8,
        x: u32,
        y: u32,
        out: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<(), ElevationError> {
        if self.kind != PackKind::Ground {
            return Err(ElevationError::Kind);
        }
        if out.len() != TILE_CELLS || scratch.len() != TILE_CELLS {
            return Err(ElevationError::Buffer);
        }
        let (d, first_x, first_y) = self.under(zoom, x, y)?;
        let fault = |index: usize| {
            move |fault| ElevationError::Tile {
                index: index as u16,
                fault,
            }
        };
        let frame = |index: usize| self.frame(index).ok_or(fault(index)(TileFault::Size));
        if d == 0 {
            let index = (first_y * self.side + first_x) as usize;
            return decoder
                .decode_bytes(frame(index)?, out)
                .map_err(fault(index));
        }
        let stored = 1u32 << d;
        let px = TILE_EDGE >> d;
        let cell = 1usize << d;
        for ty in 0..stored {
            for tx in 0..stored {
                let index = ((first_y + ty) * self.side + first_x + tx) as usize;
                decoder
                    .decode_bytes(frame(index)?, scratch)
                    .map_err(fault(index))?;
                for oy in 0..px {
                    for ox in 0..px {
                        let at = (ty as usize * px + oy) * TILE_EDGE + tx as usize * px + ox;
                        out[at] = crate::ground::mode((0..cell).flat_map(|cy| {
                            let row = (oy * cell + cy) * TILE_EDGE + ox * cell;
                            scratch[row..row + cell].iter().copied()
                        }));
                    }
                }
            }
        }
        Ok(())
    }

    fn decode(
        &self,
        decoder: &mut TileDecoder,
        index: usize,
        out: &mut [i16],
    ) -> Result<(), ElevationError> {
        let fault = |fault| ElevationError::Tile {
            index: index as u16,
            fault,
        };
        let frame = self.frame(index).ok_or(fault(TileFault::Size))?;
        decoder.decode(frame, out).map_err(fault)
    }
}

/// Decodes one tile's frame; holds the zstd decoder and the byte planes so
/// a device allocates them once.
pub struct TileDecoder {
    frame: FrameDecoder,
    planes: Vec<u8>,
}

impl Default for TileDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl TileDecoder {
    /// A decoder whose window is capped at one tile.
    pub fn new() -> Self {
        let mut frame = FrameDecoder::new();
        frame.set_max_window_size(TILE_BYTES as u64);
        TileDecoder {
            frame,
            planes: vec![0; TILE_BYTES],
        }
    }

    /// Decode one tile's zstd frame into `out` (256 x 256 heights).
    pub fn decode(&mut self, frame: &[u8], out: &mut [i16]) -> Result<(), TileFault> {
        if out.len() != TILE_CELLS {
            return Err(TileFault::Size);
        }
        self.inflate(frame, TILE_BYTES)?;
        planes_to_heights(&self.planes, out);
        Ok(())
    }

    /// Decode one tile's zstd frame of `out.len()` bytes into `out` as
    /// they are: a ground tile's class bytes ([`PackKind::Ground`]). A
    /// frame of any other length is refused as for heights.
    pub fn decode_bytes(&mut self, frame: &[u8], out: &mut [u8]) -> Result<(), TileFault> {
        if out.len() > TILE_BYTES {
            return Err(TileFault::Size);
        }
        self.inflate(frame, out.len())?;
        out.copy_from_slice(&self.planes[..out.len()]);
        Ok(())
    }

    /// Inflate `frame` into the first `want` bytes of the plane buffer,
    /// refusing a frame that is not exactly `want` bytes or whose
    /// checksum is missing or wrong.
    fn inflate(&mut self, frame: &[u8], want: usize) -> Result<(), TileFault> {
        let mut source = frame;
        self.frame.init(&mut source).map_err(|_| TileFault::Frame)?;
        let mut written = 0usize;
        loop {
            // At most one tile's bytes per round (plus the one block that
            // crosses it), drained before the next: a frame of endless
            // blocks cannot grow the decoder's buffer.
            self.frame
                .decode_blocks(&mut source, BlockDecodingStrategy::UptoBytes(TILE_BYTES))
                .map_err(|_| TileFault::Frame)?;
            loop {
                let room = &mut self.planes[written..want];
                if room.is_empty() {
                    break;
                }
                let n = self.frame.read(room).map_err(|_| TileFault::Frame)?;
                if n == 0 {
                    break;
                }
                written += n;
            }
            if self.frame.can_collect() > 0 {
                // More bytes than one tile.
                return Err(TileFault::Size);
            }
            if self.frame.is_finished() {
                break;
            }
            if source.is_empty() {
                return Err(TileFault::Frame);
            }
        }
        if written != want || !source.is_empty() {
            return Err(TileFault::Size);
        }
        let stored = self
            .frame
            .get_checksum_from_data()
            .ok_or(TileFault::NoChecksum)?;
        if self.frame.get_calculated_checksum() != Some(stored) {
            return Err(TileFault::Checksum);
        }
        Ok(())
    }
}

fn zigzag(r: i16) -> u16 {
    ((r << 1) ^ (r >> 15)) as u16
}

fn unzigzag(z: u16) -> i16 {
    ((z >> 1) as i16) ^ -((z & 1) as i16)
}

fn predict(h: &[i16], at: usize) -> i16 {
    let (row, col) = (at / TILE_EDGE, at % TILE_EDGE);
    match (row, col) {
        (0, 0) => 0,
        (0, _) => h[at - 1],
        (_, 0) => h[at - TILE_EDGE],
        _ => h[at - 1]
            .wrapping_add(h[at - TILE_EDGE])
            .wrapping_sub(h[at - TILE_EDGE - 1]),
    }
}

/// One tile's heights as the bytes the zstd frame compresses: predictor
/// residuals, zigzagged, high plane then low plane. `planes` is
/// [`TILE_BYTES`] long.
pub fn heights_to_planes(heights: &[i16], planes: &mut [u8]) {
    for at in 0..TILE_CELLS {
        let z = zigzag(heights[at].wrapping_sub(predict(heights, at)));
        planes[at] = (z >> 8) as u8;
        planes[TILE_CELLS + at] = z as u8;
    }
}

/// The inverse of [`heights_to_planes`].
pub fn planes_to_heights(planes: &[u8], heights: &mut [i16]) {
    for at in 0..TILE_CELLS {
        let z = u16::from(planes[at]) << 8 | u16::from(planes[TILE_CELLS + at]);
        heights[at] = unzigzag(z).wrapping_add(predict(heights, at));
    }
}

/// Write a block piece from its tiles' compressed frames, row-major.
pub fn write_block(
    tile_zoom: u8,
    block_zoom: u8,
    x: u32,
    y: u32,
    frames: &[Vec<u8>],
) -> Result<Vec<u8>, ElevationError> {
    write_block_of(PackKind::Heights, tile_zoom, block_zoom, x, y, frames)
}

/// [`write_block`] for a pack of `kind`.
pub fn write_block_of(
    kind: PackKind,
    tile_zoom: u8,
    block_zoom: u8,
    x: u32,
    y: u32,
    frames: &[Vec<u8>],
) -> Result<Vec<u8>, ElevationError> {
    let side = check_zooms(tile_zoom, block_zoom, TILE_EDGE as u16)?;
    if frames.len() as u32 != side * side {
        return Err(ElevationError::TileCount);
    }
    let grid = 1u64 << block_zoom;
    if u64::from(x) >= grid || u64::from(y) >= grid {
        return Err(ElevationError::Coordinates);
    }
    let mut out = Vec::with_capacity(
        BLOCK_HEADER + 4 * frames.len() + frames.iter().map(Vec::len).sum::<usize>(),
    );
    out.extend_from_slice(&kind.block_magic());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(BLOCK_HEADER as u16).to_le_bytes());
    out.push(tile_zoom);
    out.push(block_zoom);
    out.extend_from_slice(&(TILE_EDGE as u16).to_le_bytes());
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    for frame in frames {
        let len = u32::try_from(frame.len()).map_err(|_| ElevationError::Length)?;
        out.extend_from_slice(&len.to_le_bytes());
    }
    for frame in frames {
        out.extend_from_slice(frame);
    }
    Ok(out)
}

/// What a directory piece says besides its block list.
#[derive(Clone, Debug)]
pub struct DirectoryInfo<'a> {
    /// The tile zoom.
    pub tile_zoom: u8,
    /// The block zoom.
    pub block_zoom: u8,
    /// The box the cut was asked for, degrees x 10^7, west/south/east/north.
    pub bounds: [i32; 4],
    /// The attribution line.
    pub attribution: &'a str,
    /// The licence notice.
    pub notice: &'a str,
}

/// Write a directory piece naming `blocks` (x, y at the block zoom) in
/// piece order.
pub fn write_directory(
    info: &DirectoryInfo<'_>,
    blocks: &[(u32, u32)],
) -> Result<Vec<u8>, ElevationError> {
    write_directory_of(PackKind::Heights, info, blocks)
}

/// [`write_directory`] for a pack of `kind`.
pub fn write_directory_of(
    kind: PackKind,
    info: &DirectoryInfo<'_>,
    blocks: &[(u32, u32)],
) -> Result<Vec<u8>, ElevationError> {
    check_zooms(info.tile_zoom, info.block_zoom, TILE_EDGE as u16)?;
    let attribution = u32::try_from(info.attribution.len()).map_err(|_| ElevationError::Length)?;
    let notice = u32::try_from(info.notice.len()).map_err(|_| ElevationError::Length)?;
    let count = u32::try_from(blocks.len()).map_err(|_| ElevationError::Length)?;
    let mut out = Vec::with_capacity(
        DIRECTORY_HEADER
            + info.attribution.len()
            + info.notice.len()
            + DIRECTORY_ENTRY * blocks.len(),
    );
    out.extend_from_slice(&kind.directory_magic());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(DIRECTORY_HEADER as u16).to_le_bytes());
    out.push(info.tile_zoom);
    out.push(info.block_zoom);
    out.extend_from_slice(&(TILE_EDGE as u16).to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for part in info.bounds {
        out.extend_from_slice(&part.to_le_bytes());
    }
    out.extend_from_slice(&attribution.to_le_bytes());
    out.extend_from_slice(&notice.to_le_bytes());
    out.extend_from_slice(info.attribution.as_bytes());
    out.extend_from_slice(info.notice.as_bytes());
    for (x, y) in blocks {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
    }
    Ok(out)
}

/// A whole height file: the directory piece followed by its block pieces
/// in the directory's order. Returns the directory and each piece's byte
/// range (the directory first): the split the map store cuts it at.
pub fn split(file: &[u8]) -> Result<(Directory<'_>, Vec<core::ops::Range<usize>>), ElevationError> {
    split_of(PackKind::Heights, file)
}

/// [`split`] for a pack of `kind`.
pub fn split_of(
    kind: PackKind,
    file: &[u8],
) -> Result<(Directory<'_>, Vec<core::ops::Range<usize>>), ElevationError> {
    let (directory, mut at) = Directory::parse_prefix_of(kind, file)?;
    let mut ranges = Vec::with_capacity(directory.len().min(file.len() / BLOCK_HEADER) + 1);
    ranges.push(0..at);
    for n in 0..directory.len() {
        let (block, len) = Block::parse_prefix_of(kind, &file[at..])?;
        if Some(block.position()) != directory.block(n)
            || block.tile_zoom() != directory.tile_zoom()
            || block.block_zoom() != directory.block_zoom()
        {
            return Err(ElevationError::Coordinates);
        }
        ranges.push(at..at + len);
        at += len;
    }
    if at != file.len() {
        return Err(ElevationError::Length);
    }
    Ok((directory, ranges))
}
