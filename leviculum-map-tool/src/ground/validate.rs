//! The ground map's validation against OpenStreetMap, as a repeatable
//! command (`leviculum-map-tool ground-validate`): where the tracks run,
//! per class and per slope, against how much ground each class and slope
//! holds. A track runs on a class `ratio` times as often as its area
//! predicts; the pilot's baseline is 1.54 on gravel plain, 0.50 on dune
//! sand, 0.41 on hamada and 0.40 on slopes over 20°.
//!
//! The tracks come from a Protomaps archive (`kind_detail=track` in the
//! roads layer, any Protomaps build, by file or by range requests), the
//! classes from a ground pack, the slopes from the height pack the cut
//! read. A line is sampled every 20 m of ground, each sample counted in
//! the tile it lies in only, so a line repeated in a neighbour tile's
//! buffer is counted once. Unlike the pilot, built-up areas are not
//! masked out: the pilot dropped tracks within 330 m of residential land.
//!
//! The dry lake is checked against OpenStreetMap's water in a window
//! around Lac Iriki: the share of the mapped lake the class finds, and
//! how much of the class in the window lies on it.

use std::collections::HashMap;
use std::os::unix::fs::FileExt;
use std::path::Path;

use leviculum_map::elevation::{PackIndex, PackKind, TileDecoder, TILE_CELLS, TILE_EDGE};
use leviculum_map::ground::{Class, TILE_ZOOM};
use leviculum_map::region::{Bounds, TileId};
use leviculum_mapcut::mvt::{decode_named, EXTENT};
use leviculum_mapcut::pmtiles::{Archive, RangeRead};
use leviculum_mapcut::Res;
use leviculum_mapfill::fixture::Kind;
use leviculum_mapfill::style::Class as FillClass;

use super::grid::{slope_deg, Grid};
use super::sources::Heights;
use crate::heights::{lat_at, tile_units};

/// The ground pack, z11 tile by tile.
pub struct GroundPack {
    file: std::fs::File,
    index: PackIndex,
    tiles: HashMap<(u32, u32), Option<Vec<u8>>>,
}

impl GroundPack {
    pub fn open(path: &Path) -> Res<Self> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = file.metadata()?.len();
        let index = PackIndex::read_of(PackKind::Ground, len, |at, buf| {
            file.read_exact_at(buf, at).is_ok()
        })
        .map_err(|e| format!("{}: {e:?}", path.display()))?;
        Ok(GroundPack {
            file,
            index,
            tiles: HashMap::new(),
        })
    }

    /// The class byte at global z11 pixel (x, y); 0 where the pack has
    /// none.
    pub fn class(&mut self, x: u32, y: u32) -> Res<u8> {
        let key = (x >> 8, y >> 8);
        if !self.tiles.contains_key(&key) {
            let t = match self.index.block_for(TILE_ZOOM, key.0, key.1) {
                Some(range) => {
                    let mut piece = vec![0u8; range.len()];
                    self.file.read_exact_at(&mut piece, range.start as u64)?;
                    let mut out = vec![0u8; TILE_CELLS];
                    let mut scratch = vec![0u8; TILE_CELLS];
                    self.index
                        .decode_classes(
                            &piece,
                            (TILE_ZOOM, key.0, key.1),
                            &mut TileDecoder::new(),
                            &mut out,
                            &mut scratch,
                        )
                        .map_err(|e| format!("ground tile {key:?}: {e:?}"))?;
                    Some(out)
                }
                None => None,
            };
            self.tiles.insert(key, t);
        }
        Ok(match self.tiles.get(&key) {
            Some(Some(t)) => t[(y as usize & 255) * TILE_EDGE + (x as usize & 255)],
            _ => 0,
        })
    }
}

/// The slope classes the pilot counted, upper edges in degrees.
pub const SLOPE_EDGES: [f32; 4] = [2.0, 5.0, 10.0, 20.0];

fn slope_class(s: f32) -> usize {
    SLOPE_EDGES.iter().take_while(|&&e| s >= e).count()
}

/// The steepest of the four z12 slopes under each z11 pixel of a z11
/// tile, from the height pack (Horn's method over the z12 tiles, a
/// tile's border from its neighbours).
fn slope_tile(heights: &Heights, tx: u32, ty: u32) -> Res<Option<Vec<f32>>> {
    // The z12 window under the z11 tile, one sample of apron.
    let n = 2 * TILE_EDGE + 2;
    let mut d = Grid::new(n, n, f32::NAN);
    let (x0, y0) = (2 * i64::from(tx) * 256 - 1, 2 * i64::from(ty) * 256 - 1);
    let mut any = false;
    for zy in (y0.div_euclid(256))..=((y0 + n as i64 - 1).div_euclid(256)) {
        for zx in (x0.div_euclid(256))..=((x0 + n as i64 - 1).div_euclid(256)) {
            if zx < 0 || zy < 0 {
                continue;
            }
            let Some(t) = heights.tile(zx as u32, zy as u32)? else {
                continue;
            };
            for r in 0..256i64 {
                let y = zy * 256 + r - y0;
                if !(0..n as i64).contains(&y) {
                    continue;
                }
                for c in 0..256i64 {
                    let x = zx * 256 + c - x0;
                    if (0..n as i64).contains(&x) {
                        d.v[y as usize * n + x as usize] = f32::from(t[(r * 256 + c) as usize]);
                        any = true;
                    }
                }
            }
        }
    }
    if !any {
        return Ok(None);
    }
    let hole: Vec<bool> = d.v.iter().map(|v| v.is_nan()).collect();
    super::grid::nearest_fill(&mut d, &hole);
    let circ = 40_075_016.686f64;
    let res: Vec<f32> = (0..n)
        .map(|r| {
            let y = (2 * u64::from(ty) * 256 + r as u64).saturating_sub(1);
            let lat = lat_at(y * (tile_units(12) >> 8));
            (circ * lat.to_radians().cos() / (256.0 * 4096.0)) as f32
        })
        .collect();
    let s = slope_deg(&d, &res);
    let mut out = vec![0f32; TILE_CELLS];
    for y in 0..TILE_EDGE {
        for x in 0..TILE_EDGE {
            let i = (2 * y + 1) * n + 2 * x + 1;
            out[y * TILE_EDGE + x] = s.v[i].max(s.v[i + 1]).max(s.v[i + n]).max(s.v[i + n + 1]);
        }
    }
    Ok(Some(out))
}

/// One count of where tracks run against where ground is.
#[derive(Clone, Debug, Default)]
pub struct Shares {
    /// Track samples per key.
    pub track: HashMap<usize, u64>,
    /// Pixels per key.
    pub area: HashMap<usize, u64>,
}

impl Shares {
    /// (track share, area share, ratio) of `key`.
    pub fn ratio(&self, key: usize) -> (f64, f64, f64) {
        let t: u64 = self.track.values().sum();
        let a: u64 = self.area.values().sum();
        let ts = *self.track.get(&key).unwrap_or(&0) as f64 / t.max(1) as f64;
        let as_ = *self.area.get(&key).unwrap_or(&0) as f64 / a.max(1) as f64;
        (ts, as_, if as_ > 0.0 { ts / as_ } else { f64::NAN })
    }
}

/// The validation's result.
#[derive(Clone, Debug, Default)]
pub struct Validation {
    /// Track kilometres sampled inside the box on classified ground.
    pub km: f64,
    pub classes: Shares,
    pub west: Shares,
    pub east: Shares,
    pub slopes: Shares,
    /// Lac Iriki: OSM water pixels in the window, dry-lake pixels in the
    /// window, both, all z11.
    pub lake: (u64, u64, u64),
    /// Pixels per class byte in the Erg Chebbi window ([`CHEBBI`]).
    pub chebbi: [u64; 10],
    /// Track tiles read and their bytes.
    pub tiles: usize,
    pub bytes: u64,
}

/// Lac Iriki's window: about 30 by 20 km around the lake bed.
pub const IRIKI: [f64; 4] = [-6.75, 29.82, -6.05, 30.10];
/// Erg Chebbi's star-dune core, where the pilot's mountain rule painted
/// grey specks on the slip faces.
pub const CHEBBI: [f64; 4] = [-4.02, 31.06, -3.96, 31.20];
/// The meridian splitting the box in halves (the pilot's).
pub const SPLIT_LON: f64 = -5.65;

fn e7(v: f64) -> i32 {
    (v * 1e7).round() as i32
}

/// Global z11 pixel of a point in tile `t` at extent coordinates (x, y).
fn z11(t: TileId, x: f64, y: f64) -> (f64, f64) {
    let k = (1u64 << TILE_ZOOM) as f64 / (1u64 << t.z) as f64;
    (
        (f64::from(t.x) + x / f64::from(EXTENT)) * 256.0 * k,
        (f64::from(t.y) + y / f64::from(EXTENT)) * 256.0 * k,
    )
}

/// Ground metres per z11 pixel at global z11 row `gy`.
fn metres_per_px(gy: f64) -> f64 {
    let lat = lat_at((gy.max(0.0) as u64) * (tile_units(TILE_ZOOM) >> 8));
    40_075_016.686 * lat.to_radians().cos() / (256.0 * 2048.0)
}

/// Even-odd scanline fill of rings (global z11 pixel coordinates) into
/// `mask` over the window whose top-left pixel is (wx, wy), `w` wide.
fn fill_rings(rings: &[Vec<(f64, f64)>], mask: &mut [bool], (wx, wy): (i64, i64), w: usize) {
    let h = mask.len() / w;
    for row in 0..h {
        let yc = (wy + row as i64) as f64 + 0.5;
        let mut xs = Vec::new();
        for ring in rings {
            for k in 0..ring.len() {
                let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                if (a.1 <= yc) != (b.1 <= yc) {
                    xs.push(a.0 + (yc - a.1) / (b.1 - a.1) * (b.0 - a.0));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks_exact(2) {
            let c0 = ((pair[0] - 0.5).ceil() as i64 - wx).max(0);
            let c1 = ((pair[1] - 0.5).floor() as i64 - wx).min(w as i64 - 1);
            for c in c0..=c1 {
                mask[row * w + c as usize] = true;
            }
        }
    }
}

/// Validate `ground` over `bbox` against the tracks of `archive` read at
/// `zoom`.
pub fn validate<R: RangeRead>(
    ground: &mut GroundPack,
    heights: &Heights,
    archive: &Archive<R>,
    zoom: u8,
    bbox: [f64; 4],
) -> Res<Validation> {
    let [west, south, east, north] = bbox;
    let mut v = Validation::default();
    // Area shares: every z11 pixel of the box with a class.
    let px = |lon: f64, lat: f64| -> (u32, u32) {
        let w = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_degrees(
            lat, lon,
        ));
        let u = (tile_units(TILE_ZOOM) >> 8) as u32;
        (w.x / u, w.y / u)
    };
    let (x0, y0) = px(west, north);
    let (x1, y1) = (px(east, south).0, px(east, south).1);
    let (split, _) = px(SPLIT_LON, north);
    let (c0, c1) = (px(CHEBBI[0], CHEBBI[3]), px(CHEBBI[2], CHEBBI[1]));
    for y in c0.1..=c1.1 {
        for x in c0.0..=c1.0 {
            v.chebbi[usize::from(ground.class(x, y)?.min(9))] += 1;
        }
    }
    let mut slopes: HashMap<(u32, u32), Option<Vec<f32>>> = HashMap::new();
    let mut slope_at = |x: u32, y: u32| -> Res<Option<f32>> {
        let key = (x >> 8, y >> 8);
        if let std::collections::hash_map::Entry::Vacant(e) = slopes.entry(key) {
            e.insert(slope_tile(heights, key.0, key.1)?);
        }
        Ok(slopes
            .get(&key)
            .and_then(|t| t.as_ref())
            .map(|t| t[(y as usize & 255) * TILE_EDGE + (x as usize & 255)]))
    };
    for y in y0..=y1 {
        for x in x0..=x1 {
            let c = ground.class(x, y)?;
            if c == 0 {
                continue;
            }
            let k = usize::from(c);
            *v.classes.area.entry(k).or_default() += 1;
            let half = if x < split { &mut v.west } else { &mut v.east };
            *half.area.entry(k).or_default() += 1;
            if let Some(s) = slope_at(x, y)? {
                *v.slopes.area.entry(slope_class(s)).or_default() += 1;
            }
        }
    }
    // Tracks.
    let bounds = Bounds {
        west: e7(west),
        south: e7(south),
        east: e7(east),
        north: e7(north),
    };
    let located = archive.locate_in(&bounds, zoom..=zoom)?;
    let iriki_px = (px(IRIKI[0], IRIKI[3]), px(IRIKI[2], IRIKI[1]));
    let (iw, ih) = (
        (iriki_px.1 .0 - iriki_px.0 .0 + 1) as usize,
        (iriki_px.1 .1 - iriki_px.0 .1 + 1) as usize,
    );
    let mut water = vec![false; iw * ih];
    for chunk in located.chunks(256) {
        let fetched = archive.fetch(chunk)?;
        v.bytes += fetched.bytes;
        for t in fetched.tiles() {
            v.tiles += 1;
            let Some(mvt) = fetched.mvt(t)? else {
                continue;
            };
            let d = decode_named(&mvt, None, false, true)?;
            for f in &d.features {
                let k = (1u64 << TILE_ZOOM) as f64 / (1u64 << t.z) as f64 * 256.0;
                let (tx0, ty0) = (f64::from(t.x) * k, f64::from(t.y) * k);
                let over_iriki = tx0 < f64::from(iriki_px.1 .0 + 1)
                    && tx0 + k > f64::from(iriki_px.0 .0)
                    && ty0 < f64::from(iriki_px.1 .1 + 1)
                    && ty0 + k > f64::from(iriki_px.0 .1);
                if f.kind == Kind::Area && f.class == FillClass::Water {
                    if !over_iriki {
                        continue;
                    }
                    let rings: Vec<Vec<(f64, f64)>> = f
                        .parts
                        .iter()
                        .flatten()
                        .map(|r| {
                            r.iter()
                                .map(|p| z11(t, f64::from(p[0]), f64::from(p[1])))
                                .collect()
                        })
                        .collect();
                    // Only the tile's own square: a polygon is clipped
                    // per tile, with a buffer.
                    let mut m = vec![false; iw * ih];
                    fill_rings(
                        &rings,
                        &mut m,
                        (i64::from(iriki_px.0 .0), i64::from(iriki_px.0 .1)),
                        iw,
                    );
                    for (i, b) in m.into_iter().enumerate() {
                        let (cx, cy) = (
                            (iriki_px.0 .0 as usize + i % iw) as f64 + 0.5,
                            (iriki_px.0 .1 as usize + i / iw) as f64 + 0.5,
                        );
                        if b && cx >= tx0 && cx < tx0 + k && cy >= ty0 && cy < ty0 + k {
                            water[i] = true;
                        }
                    }
                    continue;
                }
                if f.kind != Kind::Line
                    || !f
                        .attrs
                        .iter()
                        .any(|(k, val)| *k == "kind_detail" && val == "track")
                {
                    continue;
                }
                for line in f.parts.iter().flatten() {
                    for seg in line.windows(2) {
                        let a = z11(t, f64::from(seg[0][0]), f64::from(seg[0][1]));
                        let b = z11(t, f64::from(seg[1][0]), f64::from(seg[1][1]));
                        let len_px = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
                        let m = len_px * metres_per_px((a.1 + b.1) / 2.0);
                        let steps = ((m / 20.0) as usize).max(1);
                        for s in 0..steps {
                            let f_ = (s as f64 + 0.5) / steps as f64;
                            let (ex, ey) = (
                                f64::from(seg[0][0]) + f_ * f64::from(seg[1][0] - seg[0][0]),
                                f64::from(seg[0][1]) + f_ * f64::from(seg[1][1] - seg[0][1]),
                            );
                            if ex < 0.0
                                || ey < 0.0
                                || ex >= f64::from(EXTENT)
                                || ey >= f64::from(EXTENT)
                            {
                                continue;
                            }
                            let (gx, gy) = z11(t, ex, ey);
                            let (x, y) = (gx as u32, gy as u32);
                            if x < x0 || x > x1 || y < y0 || y > y1 {
                                continue;
                            }
                            let c = ground.class(x, y)?;
                            if c == 0 {
                                continue;
                            }
                            v.km += 0.02;
                            let k = usize::from(c);
                            *v.classes.track.entry(k).or_default() += 1;
                            let half = if x < split { &mut v.west } else { &mut v.east };
                            *half.track.entry(k).or_default() += 1;
                            if let Some(s) = slope_at(x, y)? {
                                *v.slopes.track.entry(slope_class(s)).or_default() += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut both = 0;
    let mut lake = 0;
    let mut osm = 0;
    for (i, &w) in water.iter().enumerate() {
        let c = ground.class(
            iriki_px.0 .0 + (i % iw) as u32,
            iriki_px.0 .1 + (i / iw) as u32,
        )?;
        let l = c == Class::DryLake as u8;
        osm += u64::from(w);
        lake += u64::from(l);
        both += u64::from(w && l);
    }
    v.lake = (osm, lake, both);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_ring_fills_its_pixels() {
        let ring = vec![(2.0, 2.0), (6.0, 2.0), (6.0, 5.0), (2.0, 5.0)];
        let mut m = vec![false; 10 * 8];
        fill_rings(&[ring], &mut m, (0, 0), 10);
        let n = m.iter().filter(|&&b| b).count();
        assert_eq!(n, 4 * 3);
        assert!(m[3 * 10 + 3]);
        assert!(!m[5 * 10 + 6]);
    }

    #[test]
    fn slope_classes_are_the_pilots() {
        assert_eq!(slope_class(1.0), 0);
        assert_eq!(slope_class(2.0), 1);
        assert_eq!(slope_class(7.0), 2);
        assert_eq!(slope_class(15.0), 3);
        assert_eq!(slope_class(25.0), 4);
    }
}
