//! The ground map, stage A (order 1743, `docs/src/concepts/map-server.md`
//! "Ground packs"): what the ground is made of, as classes from radar,
//! imagery and elevation, drawn under the map in place of its blank
//! paper.
//!
//! A ground pack is the height pack's container ([`crate::elevation`])
//! with its own magics (`LMGD`, `LMGB`, [`PackKind::Ground`]): a
//! directory piece and one block piece per z9 block, each tile 256 x 256
//! class bytes ([`Class`] as `u8`, 0 outside the cut) in one zstd frame
//! with its checksum. The classes are stored at z11 (66 m at 30 N, the
//! pilot's grid; the radar input is 90 m), so a block is 4 x 4 tiles; z10
//! and z9 are derived on the device, each cell the class most of the
//! cells under it hold ([`mode`]).
//!
//! The map never says a ground can be driven: the legend names ground
//! kinds only ([`Class::name`]).
//!
//! Drawing ([`paint_row`]): each display pixel takes the class colour of
//! the style ([`crate::ground_style`]) blended bilinearly between the four
//! class samples around it, so class borders are soft, then the
//! signature of the class nearest it, scaled by how much of that class
//! surrounds it. Signatures sit on the global pixel grid of the display
//! zoom, so tiles join without a seam. The uncertain class and the
//! ground outside every pack stay the map's own paper.

use crate::elevation::PackKind;
use crate::ground_style::{self, Signature};

/// The pack kind of ground packs.
pub const KIND: PackKind = PackKind::Ground;
/// The zoom ground tiles are stored at.
pub const TILE_ZOOM: u8 = 11;
/// The zoom of a ground block: 4 x 4 tiles, the height packs' block grid.
pub const BLOCK_ZOOM: u8 = 9;
/// The class byte outside the cut (no data).
pub const NONE: u8 = 0;

/// A ground class. The byte in a tile is its discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Class {
    /// Dune sand, erg.
    Dune = 1,
    /// Sand sheet, sandy plain.
    Sheet = 2,
    /// Rock plateau, hamada.
    Hamada = 3,
    /// Gravel plain, reg.
    Reg = 4,
    /// Mountain, steep rock.
    Mountain = 5,
    /// Dry lake, sebkha.
    DryLake = 6,
    /// Wadi bed.
    Wadi = 7,
    /// Vegetation, oasis.
    Vegetation = 8,
    /// Classified, but no rule was sure: left untinted.
    Uncertain = 9,
}

impl Class {
    /// Every class, in byte order.
    pub const ALL: [Class; 9] = [
        Class::Dune,
        Class::Sheet,
        Class::Hamada,
        Class::Reg,
        Class::Mountain,
        Class::DryLake,
        Class::Wadi,
        Class::Vegetation,
        Class::Uncertain,
    ];

    /// The class of a tile byte; `None` for [`NONE`] and unknown bytes.
    pub fn of(b: u8) -> Option<Class> {
        Class::ALL.get(usize::from(b).checked_sub(1)?).copied()
    }

    /// The legend's name: what the ground is, never whether it can be
    /// crossed.
    pub fn name(self) -> &'static str {
        match self {
            Class::Dune => "Dune sand, erg",
            Class::Sheet => "Sand sheet",
            Class::Hamada => "Rock plateau, hamada",
            Class::Reg => "Gravel plain, reg",
            Class::Mountain => "Mountain, steep rock",
            Class::DryLake => "Dry lake, sebkha",
            Class::Wadi => "Wadi bed",
            Class::Vegetation => "Vegetation, oasis",
            Class::Uncertain => "Not classified",
        }
    }
}

/// The legend's source line: where the classes come from and what the
/// relief cannot show. GLO-30 posts are 30 m, so from z14 on the relief
/// is drawn from fewer heights than pixels and is soft; it is not
/// sharpened, since the detail is not in the data.
pub const SOURCE_LINE: &str = "Ground: Sentinel-1 coherence and backscatter (Kellndorfer et al. \
     2022), GeoMAD (Digital Earth Africa) and Copernicus GLO-30, classified at about 90 m. \
     Relief: Copernicus GLO-30, 30 m, soft from zoom 14 on.";

/// The two ground styles; both ship (Lew, 2026-11-12). Defined beside
/// the map's other style switches.
pub use leviculum_mapfill::style::GroundStyle;

/// The style a config word names (`pastel`, `strong`); `off` is `None`
/// inside `Some`, and an unknown word `None`.
pub fn parse_style(s: &str) -> Option<Option<GroundStyle>> {
    match s {
        "pastel" => Some(Some(GroundStyle::Pastel)),
        "strong" => Some(Some(GroundStyle::Strong)),
        "off" => Some(None),
        _ => None,
    }
}

/// The class most of `cells` hold; a tie goes to the lower byte, and
/// [`NONE`] only wins where every cell is [`NONE`], so a block's edge
/// does not eat into the classes at coarser zooms.
pub fn mode(cells: impl Iterator<Item = u8>) -> u8 {
    let mut count = [0u32; 10];
    for c in cells {
        if let Some(n) = count.get_mut(usize::from(c)) {
            *n += 1;
        }
    }
    let mut best = NONE;
    let mut most = 0;
    for (b, &n) in count.iter().enumerate().skip(1) {
        if n > most {
            best = b as u8;
            most = n;
        }
    }
    best
}

/// A colour and the weight it is laid with over the pixel under it.
fn base(style: GroundStyle, class: Class) -> Option<[u8; 3]> {
    let row = ground_style::of(class);
    match style {
        GroundStyle::Pastel => row.pastel,
        GroundStyle::Strong => row.strong,
    }
}

/// One step of the stone signature's hash (the pilot's, in wrapping
/// 64-bit arithmetic), 0..65536.
fn hash(a: u64, b: u64, salt: u64) -> u64 {
    let mut h =
        a.wrapping_mul(73_856_093) ^ b.wrapping_mul(19_349_663) ^ salt.wrapping_mul(83_492_791);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    h & 0xFFFF
}

/// The stone cell's edge in pixels.
const STONE_SPACING: u64 = 7;
/// The share of stone cells that hold a mark, of 65536.
const STONE_KEEP: u64 = 47_186; // 0.72
/// The six stone shapes, offsets from the mark's corner.
const STONE_SHAPES: [&[(u64, u64)]; 6] = [
    &[(0, 0)],
    &[(0, 0), (0, 1)],
    &[(0, 0), (1, 1)],
    &[(0, 0), (0, 1), (1, 0)],
    &[(0, 1), (1, 0), (1, 1)],
    &[(0, 0), (1, 0)],
];

/// Whether global pixel (x, y) of the display zoom carries a stone mark.
fn stone(x: u64, y: u64) -> bool {
    let s = STONE_SPACING;
    // A mark reaches one pixel past its corner, so the cells up and left
    // can reach this pixel too.
    let (cx, cy) = (x / s, y / s);
    for ky in cy.saturating_sub(1)..=cy {
        for kx in cx.saturating_sub(1)..=cx {
            if hash(kx, ky, 1) >= STONE_KEEP {
                continue;
            }
            let jy = hash(kx, ky, 2) * (s - 1) / 65_536;
            let jx = hash(kx, ky, 3) * (s - 1) / 65_536;
            let shape = STONE_SHAPES[(hash(kx, ky, 4) * 6 / 65_536) as usize];
            let (oy, ox) = (ky * s + jy, kx * s + jx);
            if shape.iter().any(|&(dy, dx)| oy + dy == y && ox + dx == x) {
                return true;
            }
        }
    }
    false
}

/// Whether global pixel (x, y) carries `signature`'s mark.
fn marked(signature: Signature, x: u64, y: u64) -> bool {
    let dots = (y.is_multiple_of(6) && x.is_multiple_of(6)) || (y % 6 == 3 && x % 6 == 3);
    match signature {
        Signature::Plain => false,
        Signature::Dots => dots,
        Signature::SparseDots => dots && (y / 6 + x / 6).is_multiple_of(2),
        Signature::Dashes => y.is_multiple_of(7) && !(x / 4).is_multiple_of(3),
        Signature::Grove => y.is_multiple_of(5) && x.is_multiple_of(5),
        Signature::Stones => stone(x, y),
    }
}

/// The colour of one display pixel over `under`, given the four class
/// bytes around it (`[north-west, north-east, south-west, south-east]`)
/// with their bilinear weights, the nearest of them, and the pixel's
/// global position at the display zoom.
pub fn pixel(
    style: GroundStyle,
    under: [u8; 3],
    around: [(u8, f32); 4],
    nearest: u8,
    (x, y): (u64, u64),
) -> [u8; 3] {
    let mut acc = [0.0f32; 3];
    let mut weight = 0.0f32;
    let mut same = 0.0f32;
    for (b, w) in around {
        if b == nearest {
            same += w;
        }
        let Some(rgb) = Class::of(b).and_then(|c| base(style, c)) else {
            continue;
        };
        for (a, &v) in acc.iter_mut().zip(&rgb) {
            *a += w * f32::from(v);
        }
        weight += w;
    }
    let mut out = [0.0f32; 3];
    for ((o, &a), &u) in out.iter_mut().zip(&acc).zip(&under) {
        *o = a + (1.0 - weight) * f32::from(u);
    }
    if let Some(class) = Class::of(nearest) {
        let row = ground_style::of(class);
        let ink = match style {
            GroundStyle::Strong => Some(row.ink_strong),
            GroundStyle::Pastel => row.ink_pastel,
        };
        if let Some(ink) = ink {
            if marked(row.signature, x, y) {
                // The pilot's fade: full marks where the class holds the
                // pixel's whole neighbourhood, none at half of it.
                let a = row.alpha * (same * 1.6 - 0.3).clamp(0.0, 1.0);
                for (o, &v) in out.iter_mut().zip(&ink) {
                    *o = *o * (1.0 - a) + a * f32::from(v);
                }
            }
        }
    }
    let mut px = [0u8; 3];
    for (p, o) in px.iter_mut().zip(&out) {
        *p = (o + 0.5).clamp(0.0, 255.0) as u8;
    }
    px
}

/// Where a view's pixels find their ground classes: the class byte of
/// sample (x, y) on the grid of `zoom` (one sample per Web Mercator
/// pixel at that zoom), [`NONE`] where no pack holds it.
pub trait Classes {
    /// The class byte at sample (x, y) of `zoom`'s grid.
    fn class(&mut self, zoom: u8, x: u32, y: u32) -> u8;
}

/// The grid zoom a view at display zoom `zoom` samples: the stored zoom
/// from it on, the display zoom itself between the block zoom and it;
/// `None` above the block zoom, where a view spans more than a block and
/// draws no ground.
pub fn grid_zoom(zoom: u8) -> Option<u8> {
    (zoom >= BLOCK_ZOOM).then(|| zoom.min(TILE_ZOOM))
}

/// Paint the ground under one row of a view: `px` holds the row's
/// pixels from global pixel (`x0`, `y`) at display `zoom` rightwards
/// (wrapping around the antimeridian) and gets the ground laid over it.
/// Returns whether any pixel of the row lies on a class.
pub fn paint_row(
    style: GroundStyle,
    zoom: u8,
    x0: u64,
    y: u64,
    classes: &mut dyn Classes,
    px: &mut [[u8; 3]],
) -> bool {
    let Some(g) = grid_zoom(zoom) else {
        return false;
    };
    let shift = zoom - g;
    let f = (1u64 << shift) as f32;
    let world = 1u64 << (u32::from(zoom) + 8);
    let grid = 1i64 << (u32::from(g) + 8);
    let at = |s: i64, row: i64, classes: &mut dyn Classes| -> u8 {
        if row < 0 || row >= grid {
            return NONE;
        }
        classes.class(g, s.rem_euclid(grid) as u32, row as u32)
    };
    // Each class's colour and signature in this style, looked up once.
    let mut flat = [None; 10];
    let mut marks = [None; 10];
    for c in Class::ALL {
        flat[c as usize] = base(style, c);
        let row = ground_style::of(c);
        let ink = match style {
            GroundStyle::Strong => Some(row.ink_strong),
            GroundStyle::Pastel => row.ink_pastel,
        };
        if let (Some(ink), true) = (ink, row.signature != Signature::Plain) {
            // A pixel inside the class: the whole neighbourhood is it,
            // so the fade of [`pixel`] is at its full 1.0.
            marks[c as usize] = Some((row.signature, ink, row.alpha));
        }
    }
    // Sample coordinate of the pixel centre, rows: one per row.
    let sy = (y as f32 + 0.5) / f - 0.5;
    let ry = libm::floorf(sy);
    let fy = sy - ry;
    let ry = ry as i64;
    let mut any = false;
    // The four samples are fetched again only when the pixel crosses into
    // another sample column.
    let mut held: Option<(i64, [u8; 4])> = None;
    for (i, p) in px.iter_mut().enumerate() {
        let x = (x0 + i as u64) % world;
        let sx = (x as f32 + 0.5) / f - 0.5;
        let rx = libm::floorf(sx);
        let fx = sx - rx;
        let rx = rx as i64;
        let four = match held {
            Some((col, four)) if col == rx => four,
            _ => {
                let four = [
                    at(rx, ry, classes),
                    at(rx + 1, ry, classes),
                    at(rx, ry + 1, classes),
                    at(rx + 1, ry + 1, classes),
                ];
                held = Some((rx, four));
                four
            }
        };
        if four.iter().all(|&b| Class::of(b).is_none()) {
            continue;
        }
        any = true;
        if four[1..].iter().all(|&b| b == four[0]) {
            // Inside a class, away from its border: the class colour and
            // its signature alone, no blend (most pixels of a view).
            if let Some(c) = flat[usize::from(four[0])] {
                *p = c;
                if let Some((sig, ink, a)) = marks[usize::from(four[0])] {
                    if marked(sig, x, y) {
                        let mut out = [0u8; 3];
                        for ((o, &v), &k) in out.iter_mut().zip(&c).zip(&ink) {
                            *o = (f32::from(v) * (1.0 - a) + a * f32::from(k) + 0.5) as u8;
                        }
                        *p = out;
                    }
                }
            }
            continue;
        }
        let w = [
            (1.0 - fx) * (1.0 - fy),
            fx * (1.0 - fy),
            (1.0 - fx) * fy,
            fx * fy,
        ];
        let nearest = four[usize::from(fy >= 0.5) * 2 + usize::from(fx >= 0.5)];
        let around = [
            (four[0], w[0]),
            (four[1], w[1]),
            (four[2], w[2]),
            (four[3], w[3]),
        ];
        *p = pixel(style, *p, around, nearest, (x, y));
    }
    any
}

/// Where a drawn view finds its ground: filled by the frontend's ground
/// store in the view's own pixel coordinates. `row` lays the ground over
/// the row of pixels at (x, y) rightwards; `false` where no pack covers
/// it, and the row is left as it is.
pub trait Ground {
    fn row(&mut self, x: i32, y: i32, px: &mut [[u8; 3]]) -> bool;
}

/// Lay the ground under `area` of `target`: read each row back, let
/// `ground` paint it, write it again. A target that cannot read its
/// pixels is left as it is. Called by the painter after the land fill
/// and before every other area ([`crate::draw::draw_in_relief`]), so
/// OpenStreetMap's own areas (water, towns, fields) lie over the ground.
pub fn apply<D: leviculum_mapfill::coverage::Coverage>(
    target: &mut D,
    area: embedded_graphics::primitives::Rectangle,
    ground: &mut dyn Ground,
) -> Result<(), D::Error> {
    let w = area.size.width as usize;
    if w == 0 {
        return Ok(());
    }
    let mut px = alloc::vec![[0u8; 3]; w];
    for y in area.top_left.y..area.top_left.y + area.size.height as i32 {
        if !target.row_rgb(area.top_left.x, y, &mut px) {
            return Ok(());
        }
        if !ground.row(area.top_left.x, y, &mut px) {
            continue;
        }
        target.set_row_rgb(area.top_left.x, y, &px)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every class byte names its class, and nothing else does.
    #[test]
    fn bytes_name_their_classes() {
        for c in Class::ALL {
            assert_eq!(Class::of(c as u8), Some(c));
        }
        assert_eq!(Class::of(NONE), None);
        assert_eq!(Class::of(10), None);
        assert_eq!(Class::of(255), None);
    }

    /// The mode takes the majority, ties to the lower byte, and the
    /// outside only where nothing else is.
    #[test]
    fn mode_prefers_the_majority_then_the_lower_byte() {
        assert_eq!(mode([3, 3, 4, 4].into_iter()), 3);
        assert_eq!(mode([4, 4, 4, 3].into_iter()), 4);
        assert_eq!(mode([0, 0, 0, 5].into_iter()), 5);
        assert_eq!(mode([0, 0, 0, 0].into_iter()), NONE);
        assert_eq!(mode([200, 9, 0, 0].into_iter()), 9);
    }
}
