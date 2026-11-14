//! The relief layer's height data on the device (order 1741): the
//! `*.lmh` height packs beside the regions in the maps directory, a
//! bounded cache of decoded tiles and computed shade planes, and the
//! sampler the painter lays under a view's ground through
//! `leviculum_map::draw::draw_in_relief`.
//!
//! A pack is indexed once — the directory piece and each block's byte
//! range — and dropped; blocks are read back from the file as tiles
//! need them, so the Morocco pilot's 37 MB never sits in memory. The
//! caches hold a few megabytes: decoded height tiles (128 KiB each)
//! and shade planes (132 KiB each), evicted oldest-used first.
//!
//! The relief grid is the pack's own: tiles at the display zoom where
//! the pack stores or derives them (block zoom up to tile zoom), the
//! display sampling bicubic through [`relief::sample`]. A view deeper
//! than the tile zoom samples the z12 grid upsampled, as the study
//! does; a view above the block zoom carries no relief.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leviculum_map::elevation::{split, Block, TileDecoder, TILE_CELLS};
use leviculum_map::relief::{self, assemble, sky_radius_m, ShadePlane};
use leviculum_map::viewport::Viewport;

/// How many decoded height tiles stay around: 32 x 128 KiB = 4 MiB.
const TILE_CACHE: usize = 32;

/// How many shade planes stay around: 24 x 132 KiB = 3.1 MiB — a
/// desert screen at the relief zoom needs about a dozen.
const PLANE_CACHE: usize = 24;

/// One indexed height pack: the file stays on disk, only the block
/// ranges and bounds are held.
struct Pack {
    path: PathBuf,
    tile_zoom: u8,
    block_zoom: u8,
    /// Each block's (x, y) at the block zoom and its byte range.
    blocks: Vec<(u32, u32, core::ops::Range<usize>)>,
}

impl Pack {
    fn open(path: &Path) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        let (dir, ranges) = split(&bytes).ok()?;
        let blocks = (0..dir.len())
            .zip(&ranges[1..])
            .filter_map(|(n, r)| dir.block(n).map(|(x, y)| (x, y, r.clone())))
            .collect();
        Some(Self {
            path: path.to_path_buf(),
            tile_zoom: dir.tile_zoom(),
            block_zoom: dir.block_zoom(),
            blocks,
        })
    }

    /// The bytes of the block holding tile (x, y) at `zoom`, read from
    /// the file.
    fn block_bytes(&self, zoom: u8, x: u32, y: u32) -> Option<Vec<u8>> {
        let shift = zoom.checked_sub(self.block_zoom)?;
        let (bx, by) = (x >> shift, y >> shift);
        let range = self
            .blocks
            .iter()
            .find(|&&(x, y, _)| (x, y) == (bx, by))
            .map(|(_, _, r)| r.clone())?;
        let mut file = fs::File::open(&self.path).ok()?;
        file.seek(SeekFrom::Start(range.start as u64)).ok()?;
        let mut out = vec![0u8; range.len()];
        file.read_exact(&mut out).ok()?;
        Some(out)
    }
}

/// A cache entry with its use stamp, oldest-used evicted first.
type Used<V> = (V, u64);

/// A decoded heights tile, shared between the cache and its users.
type Heights = Arc<Vec<i16>>;

/// The painter's relief side: the packs of the maps directory and the
/// bounded tile and plane caches.
#[derive(Default)]
pub struct ReliefStore {
    packs: Vec<Pack>,
    /// Decoded height tiles by (zoom, x, y).
    tiles: HashMap<(u8, u32, u32), Used<Heights>>,
    /// Shade planes by (zoom, x, y, sky radius in metres).
    planes: HashMap<(u8, u32, u32, u32), Used<Arc<ShadePlane>>>,
    stamp: u64,
}

impl ReliefStore {
    /// Read the `*.lmh` packs of `dir` again: what the painter does
    /// when the maps directory's epoch moved. Caches survive — a
    /// pack's tiles do not change under its name.
    pub fn refresh(&mut self, dir: Option<&Path>) {
        self.packs.clear();
        let Some(dir) = dir else { return };
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "lmh") && p.is_file())
            .collect();
        paths.sort();
        self.packs = paths.iter().filter_map(|p| Pack::open(p)).collect();
    }

    /// Whether any pack is installed at all: the painter skips the
    /// whole pass without one.
    pub fn any(&self) -> bool {
        !self.packs.is_empty()
    }

    /// The relief zoom a view at `zoom` samples, from the first pack:
    /// the display zoom where a pack serves it, else the pack's
    /// nearest. `None` without a pack.
    fn relief_zoom(&self, zoom: u8) -> Option<u8> {
        let p = self.packs.first()?;
        Some(zoom.clamp(p.block_zoom, p.tile_zoom))
    }

    /// The shade plane of relief tile (x, y) at `rz` for a view at
    /// `zoom`, computed once and cached. `None` where no pack covers
    /// the tile.
    fn plane(&mut self, rz: u8, x: u32, y: u32, zoom: u8) -> Option<Arc<ShadePlane>> {
        let sky = sky_radius_m(zoom) as u32;
        self.stamp += 1;
        let stamp = self.stamp;
        if let Some((p, used)) = self.planes.get_mut(&(rz, x, y, sky)) {
            *used = stamp;
            return Some(p.clone());
        }
        // The nine neighbours; the centre must exist, the rest clamp.
        let grid = 1i64 << rz;
        let mut refs: [Option<Arc<Vec<i16>>>; 9] = Default::default();
        for (i, slot) in refs.iter_mut().enumerate() {
            let (nx, ny) = (
                i64::from(x) + (i as i64 % 3) - 1,
                i64::from(y) + (i as i64 / 3) - 1,
            );
            if nx >= 0 && ny >= 0 && nx < grid && ny < grid {
                *slot = self.tile(rz, nx as u32, ny as u32);
            }
        }
        refs[4].as_ref()?;
        let borrowed: [Option<&[i16]>; 9] =
            core::array::from_fn(|i| refs[i].as_deref().map(|v| &v[..]));
        let dem = assemble(&borrowed);
        // Ground metres per sample at the tile's centre row.
        let lat = tile_centre_lat(rz, y);
        let res = (156_543.03 * lat.to_radians().cos() / f64::from(1u32 << rz)) as f32;
        let plane = Arc::new(relief::compute(&dem, res, sky as f32));
        if self.planes.len() >= PLANE_CACHE {
            evict(&mut self.planes);
        }
        self.planes.insert((rz, x, y, sky), (plane.clone(), stamp));
        Some(plane)
    }

    /// The heights of tile (x, y) at `zoom`, decoded once and cached;
    /// `None` where no pack covers it.
    fn tile(&mut self, zoom: u8, x: u32, y: u32) -> Option<Arc<Vec<i16>>> {
        self.stamp += 1;
        let stamp = self.stamp;
        if let Some((t, used)) = self.tiles.get_mut(&(zoom, x, y)) {
            *used = stamp;
            return Some(t.clone());
        }
        let decoded = self.packs.iter().find_map(|p| {
            if !(p.block_zoom..=p.tile_zoom).contains(&zoom) {
                return None;
            }
            let bytes = p.block_bytes(zoom, x, y)?;
            let block = Block::parse(&bytes).ok()?;
            let mut out = vec![0i16; TILE_CELLS];
            let mut scratch = vec![0i16; TILE_CELLS];
            let mut dec = TileDecoder::new();
            block
                .heights(&mut dec, zoom, x, y, &mut out, &mut scratch)
                .ok()?;
            Some(Arc::new(out))
        })?;
        if self.tiles.len() >= TILE_CACHE {
            evict(&mut self.tiles);
        }
        self.tiles.insert((zoom, x, y), (decoded.clone(), stamp));
        Some(decoded)
    }

    /// The sampler of `view` for the painter's ground pass, or `None`
    /// where no pack could serve this view at all.
    pub fn sampler(&mut self, view: &Viewport) -> Option<Sampler<'_>> {
        let rz = self.relief_zoom(view.zoom())?;
        if rz > view.zoom() {
            // The view stands above the pack's coarsest grid: a
            // world-scale view wears no relief.
            return None;
        }
        Some(Sampler {
            store: self,
            view: *view,
            rz,
        })
    }
}

/// Oldest-used entry out.
fn evict<K: Clone + core::hash::Hash + Eq, V>(map: &mut HashMap<K, (V, u64)>) {
    if let Some(k) = map
        .iter()
        .min_by_key(|(_, (_, used))| *used)
        .map(|(k, _)| k.clone())
    {
        map.remove(&k);
    }
}

/// The latitude of the centre row of tile row `y` at `zoom`, degrees.
fn tile_centre_lat(zoom: u8, y: u32) -> f64 {
    let n = f64::from(1u32 << zoom);
    let v = core::f64::consts::PI * (1.0 - 2.0 * (f64::from(y) + 0.5) / n);
    v.sinh().atan().to_degrees()
}

/// One view's relief rows, in the view's own pixels.
pub struct Sampler<'a> {
    store: &'a mut ReliefStore,
    view: Viewport,
    rz: u8,
}

impl relief::Shade for Sampler<'_> {
    fn row(&mut self, x: i32, y: i32, t: &mut [f32], ao: &mut [f32]) -> bool {
        let zoom = self.view.zoom();
        // World units per display pixel and per relief sample.
        let px_units = 1u64 << (24 - u32::from(zoom));
        let sample_units = 1u64 << (24 - u32::from(self.rz));
        let tile_units = sample_units << 8;
        let w0 = self.view.to_world(x, y);
        // Pixel centres.
        let wy = u64::from(w0.y) + px_units / 2;
        let mut any = false;
        type Held = ((u32, u32), Option<Arc<ShadePlane>>);
        let mut held: Option<Held> = None;
        for (i, (tv, av)) in t.iter_mut().zip(ao.iter_mut()).enumerate() {
            let wx = (u64::from(w0.x) + px_units / 2 + i as u64 * px_units) & 0xFFFF_FFFF;
            let (tx, ty) = (
                (wx / tile_units) as u32,
                (wy / tile_units).min(u64::from(u32::MAX)) as u32,
            );
            if held.as_ref().map(|(k, _)| *k) != Some((tx, ty)) {
                let plane = self.store.plane(self.rz, tx, ty, zoom);
                held = Some(((tx, ty), plane));
            }
            let Some((_, Some(plane))) = held.as_ref() else {
                *tv = 0.0;
                *av = 1.0;
                continue;
            };
            let u = (wx % tile_units) as f32 / sample_units as f32;
            let v = (wy % tile_units) as f32 / sample_units as f32;
            let (ts, aos) = relief::sample(plane, u, v);
            *tv = ts;
            *av = aos;
            any = true;
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::relief::Shade;

    const CHEBBI: &[u8] = include_bytes!("../../leviculum-map/fixtures/erg-chebbi.lmh");

    fn store_with_chebbi(dir: &Path) -> ReliefStore {
        std::fs::write(dir.join("erg-chebbi.lmh"), CHEBBI).expect("write the pack");
        let mut store = ReliefStore::default();
        store.refresh(Some(dir));
        store
    }

    /// The store finds the pack, serves the erg's tiles and leaves the
    /// uncovered world alone.
    #[test]
    fn the_erg_shades_and_the_sea_does_not() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut store = store_with_chebbi(dir.path());
        assert!(store.any());
        let at = project(LatLon::from_degrees(31.16, -3.98));
        let view = Viewport::new(at, 12, 64, 64);
        let mut s = store.sampler(&view).expect("a sampler over the erg");
        let (mut t, mut ao) = (vec![0.0f32; 64], vec![1.0f32; 64]);
        assert!(s.row(0, 32, &mut t, &mut ao), "the erg's row shades");
        assert!(
            t.iter().any(|&v| v.abs() > 0.02),
            "the dunes wear tone, got flat zeros"
        );
        // An ocean view far from any pack.
        let sea = Viewport::new(project(LatLon::from_degrees(0.0, -30.0)), 12, 64, 64);
        let mut s = store.sampler(&sea).expect("a sampler, data or not");
        assert!(!s.row(0, 32, &mut t, &mut ao), "the open sea stays bare");
    }

    /// The caches stay bounded however many tiles a walk touches.
    #[test]
    fn the_caches_stay_bounded() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut store = store_with_chebbi(dir.path());
        let at = project(LatLon::from_degrees(31.16, -3.98));
        for z in [11u8, 12, 13, 14] {
            let view = Viewport::new(at, z, 256, 256);
            if let Some(mut s) = store.sampler(&view) {
                let (mut t, mut ao) = (vec![0.0f32; 256], vec![1.0f32; 256]);
                for y in (0..256).step_by(32) {
                    s.row(0, y, &mut t, &mut ao);
                }
            }
        }
        assert!(store.tiles.len() <= TILE_CACHE);
        assert!(store.planes.len() <= PLANE_CACHE);
    }
}
