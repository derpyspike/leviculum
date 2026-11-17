//! The relief layer's height data on the device (order 1741): the
//! `*.lmh` height packs beside the regions in the maps directory, a
//! bounded cache of decoded tiles, their measured ground and computed
//! shade planes, and the sampler the painter lays under a view's ground
//! through `leviculum_map::draw::draw_in_relief`.
//!
//! A pack is indexed with bounded reads ([`PackIndex::read`]: the
//! directory piece and each block's header and length table) and kept
//! on disk; a block is read back from the file as a tile needs it, so
//! the Morocco pilot's 37 MB never sits in memory. The maps directory
//! is listed again on every refresh, but a pack set whose names, sizes
//! and modification times did not change is not indexed again, so the
//! loader's progress ticks cost a directory listing and nothing more.
//!
//! One store is shared by every painter of a tile cache (a clone is a
//! handle on the same packs and caches): a plane one worker computed is
//! there for the next. The caches hold a few megabytes — decoded height
//! tiles (128 KiB each), shade planes (132 KiB each), and the per-tile
//! ground statistics (12 bytes each) — evicted least recently used
//! first. A plane is computed outside the lock, so workers shade in
//! parallel; two that want the same plane at once may both compute it,
//! and the second insert is a no-op.
//!
//! The relief grid is the packs' own: tiles at the display zoom where a
//! pack stores or derives them (its block zoom up to its tile zoom),
//! the display sampling bicubic through [`relief::sample`]. A view
//! deeper than the tile zoom samples the tile zoom's grid upsampled, as
//! the study does; a view above every pack's block zoom carries no
//! relief.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use leviculum_map::elevation::{PackIndex, TileDecoder, TILE_CELLS};
use leviculum_map::relief::{
    self, assemble, fill_ground, measure, sky_radius_m, tile_res, Ground, ShadePlane,
};
use leviculum_map::viewport::Viewport;

/// How many decoded height tiles stay around: 32 x 128 KiB = 4 MiB.
const TILE_CACHE: usize = 32;

/// How many shade planes stay around: 24 x 132 KiB = 3.1 MiB — a
/// desert screen at the relief zoom needs about a dozen.
const PLANE_CACHE: usize = 24;

/// How many tiles' ground statistics stay around: 12 bytes each.
const GROUND_CACHE: usize = 1024;

/// One indexed height pack: the file stays on disk, only its index is
/// held.
struct Pack {
    path: PathBuf,
    index: PackIndex,
}

impl Pack {
    /// Index the pack at `path` with bounded reads, adding what it read
    /// to `read`.
    fn open(path: &Path, read: &mut u64) -> Option<Self> {
        let mut file = fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        let index = PackIndex::read(len, |at, buf| {
            *read += buf.len() as u64;
            file.seek(SeekFrom::Start(at)).is_ok() && file.read_exact(buf).is_ok()
        })
        .ok()?;
        Some(Self {
            path: path.to_path_buf(),
            index,
        })
    }

    /// The heights of tile (x, y) at `zoom`, its block read from the
    /// file; `None` where the pack does not hold it.
    fn heights(&self, zoom: u8, x: u32, y: u32, read: &mut u64) -> Option<Vec<i16>> {
        let range = self.index.block_for(zoom, x, y)?;
        let mut file = fs::File::open(&self.path).ok()?;
        file.seek(SeekFrom::Start(range.start as u64)).ok()?;
        let mut piece = vec![0u8; range.len()];
        file.read_exact(&mut piece).ok()?;
        *read += piece.len() as u64;
        let mut out = vec![0i16; TILE_CELLS];
        let mut scratch = vec![0i16; TILE_CELLS];
        self.index
            .decode(
                &piece,
                (zoom, x, y),
                &mut TileDecoder::new(),
                &mut out,
                &mut scratch,
            )
            .ok()?;
        Some(out)
    }
}

/// A cache entry with its use stamp, least recently used evicted first.
type Used<V> = (V, u64);

/// A decoded heights tile, shared between the cache and its users.
type Heights = Arc<Vec<i16>>;

/// A tile of the relief grid: (zoom, x, y).
type TileId = (u8, u32, u32);

/// A pack file as the listing saw it: path, length, modification time.
type Listed = (PathBuf, u64, Option<SystemTime>);

/// What the store's handles share.
struct Shared {
    packs: Vec<Pack>,
    /// The pack files the packs were indexed from.
    listing: Vec<Listed>,
    tiles: HashMap<TileId, Used<Option<Heights>>>,
    grounds: HashMap<TileId, Used<Ground>>,
    /// Shade planes by tile and sky radius in metres.
    planes: HashMap<(u8, u32, u32, u32), Used<Arc<ShadePlane>>>,
    stamp: u64,
    tile_cap: usize,
    plane_cap: usize,
    /// Bytes read from pack files since the store was made.
    read: u64,
    /// Planes computed since the store was made.
    computed: u64,
    /// Bumped by every [`ReliefStore::refresh`] that changed the pack
    /// set: a plane or ground computed outside the lock from tiles read
    /// under an older generation is not cached.
    generation: u64,
}

impl Shared {
    fn tick(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    /// The heights of tile `id`, decoded once and cached; `None` where
    /// no pack covers it (remembered too, so open sea costs one look).
    fn tile(&mut self, id: TileId) -> Option<Heights> {
        let stamp = self.tick();
        if let Some((t, used)) = self.tiles.get_mut(&id) {
            *used = stamp;
            return t.clone();
        }
        let (zoom, x, y) = id;
        let mut read = 0;
        let decoded = self
            .packs
            .iter()
            .find_map(|p| p.heights(zoom, x, y, &mut read))
            .map(Arc::new);
        self.read += read;
        if self.tiles.len() >= self.tile_cap {
            evict(&mut self.tiles);
        }
        self.tiles.insert(id, (decoded.clone(), stamp));
        decoded
    }
}

/// The painter's relief side: the packs of the maps directory and the
/// bounded tile, ground and plane caches, shared by every clone.
#[derive(Clone)]
pub struct ReliefStore {
    shared: Arc<Mutex<Shared>>,
}

impl Default for ReliefStore {
    fn default() -> Self {
        Self::with_capacity(TILE_CACHE, PLANE_CACHE)
    }
}

impl ReliefStore {
    /// A store holding at most `tiles` decoded height tiles and `planes`
    /// shade planes.
    pub fn with_capacity(tiles: usize, planes: usize) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                packs: Vec::new(),
                listing: Vec::new(),
                tiles: HashMap::new(),
                grounds: HashMap::new(),
                planes: HashMap::new(),
                stamp: 0,
                tile_cap: tiles.max(1),
                plane_cap: planes.max(1),
                read: 0,
                computed: 0,
                generation: 0,
            })),
        }
    }

    /// The shared state; a worker that panicked holding it left the
    /// caches consistent (every write is one insert or remove), so a
    /// poisoned lock is taken as it is.
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether `other` is a handle on the same store.
    pub fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Look at the `*.lmh` packs of `dir` again: what the painter does
    /// when the maps directory's epoch or progress moved. A pack set
    /// whose names, lengths and modification times are the ones
    /// indexed is kept as it is; any change indexes the packs again
    /// and empties the caches, whose keys do not name a pack.
    pub fn refresh(&self, dir: Option<&Path>) {
        let listing = dir.map(list_packs).unwrap_or_default();
        let mut s = self.lock();
        if listing == s.listing {
            return;
        }
        let mut read = 0;
        s.packs = listing
            .iter()
            .filter_map(|(p, _, _)| Pack::open(p, &mut read))
            .collect();
        s.read += read;
        s.listing = listing;
        s.generation += 1;
        s.tiles.clear();
        s.grounds.clear();
        s.planes.clear();
    }

    /// Whether any pack is installed at all: the painter skips the
    /// whole pass without one.
    pub fn any(&self) -> bool {
        !self.lock().packs.is_empty()
    }

    /// Bytes read from pack files since the store was made.
    pub fn bytes_read(&self) -> u64 {
        self.lock().read
    }

    /// Shade planes computed since the store was made.
    pub fn planes_computed(&self) -> u64 {
        self.lock().computed
    }

    /// The relief zoom a view at `zoom` samples: the deepest zoom at or
    /// above the view's that some pack serves (a pack serves its block
    /// zoom to its tile zoom). `None` where every pack's grid is finer
    /// than the view, or without a pack.
    fn relief_zoom(&self, zoom: u8) -> Option<u8> {
        self.lock()
            .packs
            .iter()
            .filter_map(|p| {
                let z = zoom.min(p.index.tile_zoom());
                (z >= p.index.block_zoom()).then_some(z)
            })
            .max()
    }

    /// The shade plane of relief tile (x, y) at `rz` for a view at
    /// `zoom`, computed once and cached. `None` where no pack covers
    /// the tile.
    fn plane(&self, rz: u8, x: u32, y: u32, zoom: u8) -> Option<Arc<ShadePlane>> {
        self.plane_then(rz, x, y, zoom, || {})
    }

    /// [`Self::plane`], running `before_insert` once the plane is
    /// computed, outside the lock, before it is cached: where another
    /// worker's [`Self::refresh`] can land.
    fn plane_then(
        &self,
        rz: u8,
        x: u32,
        y: u32,
        zoom: u8,
        before_insert: impl FnOnce(),
    ) -> Option<Arc<ShadePlane>> {
        let sky = sky_radius_m(zoom) as u32;
        let key = (rz, x, y, sky);
        // The neighbourhood's heights, under the lock: a cache hit or a
        // block read and decode, and the pack set's generation they
        // came from.
        let (refs, generation): ([Option<Heights>; 9], u64) = {
            let mut s = self.lock();
            let stamp = s.tick();
            if let Some((p, used)) = s.planes.get_mut(&key) {
                *used = stamp;
                return Some(p.clone());
            }
            let grid = 1i64 << rz;
            let mut refs: [Option<Heights>; 9] = Default::default();
            for (i, slot) in refs.iter_mut().enumerate() {
                let (nx, ny) = (
                    i64::from(x) + (i as i64 % 3) - 1,
                    i64::from(y) + (i as i64 / 3) - 1,
                );
                if nx >= 0 && ny >= 0 && nx < grid && ny < grid {
                    *slot = s.tile((rz, nx as u32, ny as u32));
                }
            }
            (refs, s.generation)
        };
        refs[4].as_ref()?;
        // Ground metres per sample at the tile's centre row; each
        // neighbour's ground is measured at its own row's, so a tile's
        // ground is the same whichever plane asks first.
        let res = tile_res(rz, y);
        let grounds: [Option<Ground>; 9] = core::array::from_fn(|i| {
            let tile = refs[i].as_ref()?;
            let (nx, ny) = (x as i64 + (i as i64 % 3) - 1, y as i64 + (i as i64 / 3) - 1);
            let id = (rz, nx as u32, ny as u32);
            Some(self.ground(id, tile, tile_res(rz, id.2), generation))
        });
        let ground = fill_ground(&grounds)?;
        let borrowed: [Option<&[i16]>; 9] =
            core::array::from_fn(|i| refs[i].as_deref().map(|v| &v[..]));
        let dem = assemble(&borrowed);
        let plane = Arc::new(relief::compute_in(&dem, res, sky as f32, &ground));
        before_insert();
        let mut s = self.lock();
        s.computed += 1;
        if s.generation != generation {
            // A refresh replaced the pack set while this plane was
            // computed: hand it to the asking frame, keep it out of the
            // cache.
            return Some(plane);
        }
        let stamp = s.tick();
        if !s.planes.contains_key(&key) && s.planes.len() >= s.plane_cap {
            evict(&mut s.planes);
        }
        s.planes.insert(key, (plane.clone(), stamp));
        Some(plane)
    }

    /// The ground of tile `id`, measured once on its heights `tile` at
    /// `res`, the tile's own row's resolution (outside the lock), and
    /// cached unless the pack set moved past `generation` meanwhile.
    fn ground(&self, id: TileId, tile: &[i16], res: f32, generation: u64) -> Ground {
        {
            let mut s = self.lock();
            let stamp = s.tick();
            if let Some((g, used)) = s.grounds.get_mut(&id) {
                *used = stamp;
                return *g;
            }
        }
        let g = measure(tile, res);
        let mut s = self.lock();
        if s.generation != generation {
            return g;
        }
        let stamp = s.tick();
        if s.grounds.len() >= GROUND_CACHE {
            evict(&mut s.grounds);
        }
        s.grounds.insert(id, (g, stamp));
        g
    }

    /// The sampler of `view` for the painter's ground pass, or `None`
    /// where no pack could serve this view at all.
    pub fn sampler(&self, view: &Viewport) -> Option<Sampler> {
        let rz = self.relief_zoom(view.zoom())?;
        Some(Sampler {
            store: self.clone(),
            view: *view,
            rz,
        })
    }
}

/// The `*.lmh` files of `dir` with their lengths and modification
/// times, sorted by path.
fn list_packs(dir: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Listed> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if !path.extension().is_some_and(|x| x == "lmh") {
                return None;
            }
            let meta = fs::metadata(&path).ok()?;
            meta.is_file()
                .then(|| (path, meta.len(), meta.modified().ok()))
        })
        .collect();
    out.sort();
    out
}

/// Least recently used entry out.
fn evict<K: Clone + core::hash::Hash + Eq, V>(map: &mut HashMap<K, (V, u64)>) {
    if let Some(k) = map
        .iter()
        .min_by_key(|(_, (_, used))| *used)
        .map(|(k, _)| k.clone())
    {
        map.remove(&k);
    }
}

/// One view's relief rows, in the view's own pixels.
pub struct Sampler {
    store: ReliefStore,
    view: Viewport,
    rz: u8,
}

impl relief::Shade for Sampler {
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
    const CHEGAGA: &[u8] = include_bytes!("../../leviculum-map/fixtures/erg-chegaga-z10.lmh");

    fn store_with(dir: &Path, packs: &[(&str, &[u8])], store: ReliefStore) -> ReliefStore {
        for (name, bytes) in packs {
            std::fs::write(dir.join(name), bytes).expect("write the pack");
        }
        store.refresh(Some(dir));
        store
    }

    /// The z12 tile under (lat, lon).
    fn tile_at(lat: f64, lon: f64) -> (u32, u32) {
        let at = project(LatLon::from_degrees(lat, lon));
        (at.x >> 20, at.y >> 20)
    }

    /// The store finds the pack, serves the erg's tiles and leaves the
    /// uncovered world alone.
    #[test]
    fn the_erg_shades_and_the_sea_does_not() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = store_with(
            dir.path(),
            &[("erg-chebbi.lmh", CHEBBI)],
            ReliefStore::default(),
        );
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

    /// Both caches fill, evict the least recently used entry first and
    /// stay at their bound (the audit of order 1741: the old test never
    /// filled either).
    #[test]
    fn the_caches_evict_least_recently_used_and_stay_bounded() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = store_with(
            dir.path(),
            &[("chegaga.lmh", CHEGAGA)],
            ReliefStore::with_capacity(4, 2),
        );
        let (x0, y0) = tile_at(29.8475, -6.2975);
        let (x0, y0) = (x0 & !3, y0 & !3);
        // Heights: four in, the first used again, a fifth in — the
        // second goes, the first stays.
        {
            let mut s = store.lock();
            for dx in 0..4 {
                assert!(s.tile((12, x0 + dx, y0)).is_some(), "tile {dx} in the pack");
            }
            s.tile((12, x0, y0));
            s.tile((12, x0, y0 + 1));
            assert_eq!(s.tiles.len(), 4);
            assert!(s.tiles.contains_key(&(12, x0, y0)), "recently used stays");
            assert!(
                !s.tiles.contains_key(&(12, x0 + 1, y0)),
                "least recently used goes"
            );
            assert!(s.tiles.contains_key(&(12, x0 + 3, y0)));
        }
        // Planes: two in, the first used again, a third in — the second
        // goes.
        let a = store.plane(12, x0 + 1, y0 + 1, 12).expect("plane a");
        let _b = store.plane(12, x0 + 2, y0 + 1, 12).expect("plane b");
        let again = store.plane(12, x0 + 1, y0 + 1, 12).expect("plane a again");
        assert!(Arc::ptr_eq(&a, &again), "a cache hit, not a second compute");
        store.plane(12, x0 + 1, y0 + 2, 12).expect("plane c");
        let sky = sky_radius_m(12) as u32;
        let s = store.lock();
        assert_eq!(s.planes.len(), 2);
        assert!(s.planes.contains_key(&(12, x0 + 1, y0 + 1, sky)));
        assert!(!s.planes.contains_key(&(12, x0 + 2, y0 + 1, sky)));
        assert!(s.planes.contains_key(&(12, x0 + 1, y0 + 2, sky)));
        assert!(s.tiles.len() <= 4);
        assert_eq!(s.computed, 3);
    }

    /// A pack is indexed with bounded reads and only once: the
    /// directory piece and the block headers, not the pack; a hundred
    /// refreshes (the loader's progress ticks) read nothing more; a
    /// changed pack set is indexed again and empties the caches.
    #[test]
    fn packs_are_indexed_once_with_bounded_reads() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = store_with(
            dir.path(),
            &[("chegaga.lmh", CHEGAGA)],
            ReliefStore::default(),
        );
        let first = store.bytes_read();
        assert!(first > 0 && first < 2048, "indexing read {first} bytes");
        for _ in 0..100 {
            store.refresh(Some(dir.path()));
        }
        assert_eq!(store.bytes_read(), first, "progress ticks read no pack");
        let (x0, y0) = tile_at(29.8475, -6.2975);
        assert!(store.plane(12, x0, y0, 12).is_some());
        let after_plane = store.bytes_read();
        assert!(!store.lock().planes.is_empty());
        std::fs::write(dir.path().join("chebbi.lmh"), CHEBBI).expect("a second pack");
        store.refresh(Some(dir.path()));
        assert!(store.bytes_read() > after_plane, "the new set is indexed");
        let s = store.lock();
        assert_eq!(s.packs.len(), 2);
        assert!(s.planes.is_empty() && s.tiles.is_empty(), "caches emptied");
    }

    /// The relief zoom looks at every pack: a z10 view over a set whose
    /// first pack starts at z11 still samples the pack that derives z10.
    #[test]
    fn the_relief_zoom_considers_every_pack() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = store_with(
            dir.path(),
            &[("a-chebbi.lmh", CHEBBI), ("b-chegaga.lmh", CHEGAGA)],
            ReliefStore::default(),
        );
        assert_eq!(store.relief_zoom(10), Some(10));
        assert_eq!(store.relief_zoom(11), Some(11));
        assert_eq!(store.relief_zoom(15), Some(12));
        assert_eq!(store.relief_zoom(9), None);
        let at = project(LatLon::from_degrees(29.8475, -6.2975));
        let view = Viewport::new(at, 10, 64, 64);
        let mut s = store.sampler(&view).expect("the z10 view has relief");
        let (mut t, mut ao) = (vec![0.0f32; 64], vec![1.0f32; 64]);
        assert!(s.row(0, 32, &mut t, &mut ao), "Chegaga shades at z10");
    }

    /// A tile's ground does not depend on which plane asked for it first:
    /// two planes one row apart, computed in either order, give the same
    /// grounds for every tile and the same bytes (the audit of order
    /// 1851: each neighbour was measured at the asking plane's row).
    #[test]
    fn the_draw_order_changes_no_byte() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("chegaga.lmh"), CHEGAGA).expect("write the pack");
        let (x0, y0) = tile_at(29.8475, -6.2975);
        let (x, y) = ((x0 & !3) + 1, (y0 & !3) + 1);
        let run = |first: (u32, u32), second: (u32, u32)| {
            let store = ReliefStore::default();
            store.refresh(Some(dir.path()));
            let a = store.plane(12, first.0, first.1, 12).expect("first plane");
            let b = store
                .plane(12, second.0, second.1, 12)
                .expect("second plane");
            let grounds: Vec<(TileId, Ground)> = {
                let s = store.lock();
                let mut g: Vec<_> = s.grounds.iter().map(|(k, (g, _))| (*k, *g)).collect();
                g.sort_by_key(|(k, _)| *k);
                g
            };
            (a, b, grounds)
        };
        let (upper, lower) = ((x, y), (x, y + 1));
        let (u1, l1, g1) = run(upper, lower);
        let (l2, u2, g2) = run(lower, upper);
        assert_eq!(
            g1, g2,
            "every shared tile's ground, whichever plane asked first"
        );
        assert!(u1.t == u2.t && u1.ao == u2.ao, "the upper plane's bytes");
        assert!(l1.t == l2.t && l1.ao == l2.ao, "the lower plane's bytes");
    }

    /// A plane read from the pack set a refresh then replaced is not
    /// cached: the refresh lands between the compute and the insert,
    /// and afterwards neither the plane nor its grounds are in the
    /// caches (the audit of order 1851: the stale plane stayed until
    /// evicted).
    #[test]
    fn a_refresh_during_a_compute_keeps_the_stale_plane_out() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = store_with(
            dir.path(),
            &[("chegaga.lmh", CHEGAGA)],
            ReliefStore::default(),
        );
        let (x0, y0) = tile_at(29.8475, -6.2975);
        let (x, y) = ((x0 & !3) + 1, (y0 & !3) + 1);
        let plane = store.plane_then(12, x, y, 12, || {
            std::fs::write(dir.path().join("chebbi.lmh"), CHEBBI).expect("a second pack");
            store.refresh(Some(dir.path()));
        });
        assert!(plane.is_some(), "the asking frame still gets its plane");
        let s = store.lock();
        assert_eq!(s.packs.len(), 2, "the refresh indexed the new set");
        assert!(s.planes.is_empty(), "the stale plane is not cached");
        assert!(
            s.grounds.is_empty(),
            "nor are grounds measured before the refresh"
        );
        drop(s);
        // The next ask computes afresh from the new set and caches.
        let before = store.planes_computed();
        store.plane(12, x, y, 12).expect("the plane again");
        assert_eq!(store.planes_computed(), before + 1);
        assert_eq!(store.lock().planes.len(), 1);
    }

    /// Clones share one store: a plane one painter's sampler computed is
    /// a cache hit for another's.
    #[test]
    fn clones_share_their_planes() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = store_with(
            dir.path(),
            &[("chebbi.lmh", CHEBBI)],
            ReliefStore::default(),
        );
        let other = store.clone();
        assert!(store.same_as(&other));
        let view = Viewport::new(project(LatLon::from_degrees(31.16, -3.98)), 12, 64, 64);
        let (mut t, mut ao) = (vec![0.0f32; 64], vec![1.0f32; 64]);
        let mut s = store.sampler(&view).expect("sampler");
        s.row(0, 32, &mut t, &mut ao);
        let computed = store.planes_computed();
        assert!(computed > 0);
        let mut s = other.sampler(&view).expect("sampler");
        s.row(0, 32, &mut t, &mut ao);
        assert_eq!(other.planes_computed(), computed, "no second compute");
        assert!(!store.same_as(&ReliefStore::default()));
    }
}
