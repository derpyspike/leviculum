//! The ground map on the device (order 1743): the `*.lmg` ground packs
//! beside the regions in the maps directory, a bounded cache of decoded
//! class tiles, and the row painter the map lays under a view's areas
//! through `leviculum_map::draw::draw_in_relief`.
//!
//! As with the height packs ([`crate::relief`]), a pack is indexed with
//! bounded reads and kept on disk; a block is read back as a tile needs
//! it, and a decoded class tile (64 KiB) is cached, least recently used
//! first out. The class grid is the packs' own: z11 where they store it,
//! z10 and z9 derived, a view deeper than z11 sampling z11 with soft
//! borders (`leviculum_map::ground::paint_row`). A view above z9 draws no
//! ground.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use leviculum_map::elevation::{PackIndex, PackKind, TileDecoder, TILE_CELLS, TILE_EDGE};
use leviculum_map::ground::{self, Classes, GroundStyle, NONE};
use leviculum_map::viewport::Viewport;

/// How many decoded class tiles stay around: 64 x 64 KiB = 4 MiB.
const TILE_CACHE: usize = 64;

/// A pack file as the listing saw it: path, length, modification time.
type Listed = (PathBuf, u64, Option<SystemTime>);

/// A decoded class tile, or `None` where no pack holds it.
type ClassTile = Option<Arc<Vec<u8>>>;

/// A tile of the class grid: (zoom, x, y).
type TileId = (u8, u32, u32);

struct Pack {
    path: PathBuf,
    index: PackIndex,
}

impl Pack {
    fn open(path: &Path) -> Option<Self> {
        let file = fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        let index = PackIndex::read_of(PackKind::Ground, len, |at, buf| {
            file.read_exact_at(buf, at).is_ok()
        })
        .ok()?;
        Some(Self {
            path: path.to_path_buf(),
            index,
        })
    }

    /// The classes of tile (x, y) at `zoom`; `None` where the pack does
    /// not hold it or its block does not read.
    fn classes(&self, zoom: u8, x: u32, y: u32) -> Option<Vec<u8>> {
        let range = self.index.block_for(zoom, x, y)?;
        let file = fs::File::open(&self.path).ok()?;
        let mut piece = vec![0u8; range.len()];
        file.read_exact_at(&mut piece, range.start as u64).ok()?;
        let mut out = vec![0u8; TILE_CELLS];
        let mut scratch = vec![0u8; TILE_CELLS];
        self.index
            .decode_classes(
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

struct Shared {
    packs: Vec<Pack>,
    listing: Vec<Listed>,
    tiles: HashMap<TileId, (ClassTile, u64)>,
    stamp: u64,
}

/// The painter's ground side: the ground packs of the maps directory and
/// the class tile cache, shared by every clone.
#[derive(Clone)]
pub struct GroundStore {
    shared: Arc<Mutex<Shared>>,
}

impl Default for GroundStore {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                packs: Vec::new(),
                listing: Vec::new(),
                tiles: HashMap::new(),
                stamp: 0,
            })),
        }
    }
}

impl GroundStore {
    /// The shared state; every write is one insert or remove, so a
    /// poisoned lock is taken as it is.
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Look at the `*.lmg` packs of `dir` again; a pack set whose names,
    /// lengths and times did not move is kept, any change indexes the
    /// packs again and empties the cache.
    pub fn refresh(&self, dir: Option<&Path>) {
        let listing = dir.map(list_packs).unwrap_or_default();
        let mut s = self.lock();
        if listing == s.listing {
            return;
        }
        s.packs = listing
            .iter()
            .filter_map(|(p, _, _)| Pack::open(p))
            .collect();
        s.listing = listing;
        s.tiles.clear();
    }

    /// Whether any ground pack is installed.
    pub fn any(&self) -> bool {
        !self.lock().packs.is_empty()
    }

    /// The classes of tile `id`, decoded once and cached; `None` where no
    /// pack covers it (remembered too).
    fn tile(&self, id: TileId) -> Option<Arc<Vec<u8>>> {
        let mut s = self.lock();
        s.stamp += 1;
        let stamp = s.stamp;
        if let Some((t, used)) = s.tiles.get_mut(&id) {
            *used = stamp;
            return t.clone();
        }
        let (zoom, x, y) = id;
        let decoded = s
            .packs
            .iter()
            .find_map(|p| p.classes(zoom, x, y))
            .map(Arc::new);
        if s.tiles.len() >= TILE_CACHE {
            if let Some(k) = s.tiles.iter().min_by_key(|(_, (_, u))| *u).map(|(k, _)| *k) {
                s.tiles.remove(&k);
            }
        }
        s.tiles.insert(id, (decoded.clone(), stamp));
        decoded
    }

    /// The row painter of `view` in `style`, or `None` where the view is
    /// too far out for any ground or no pack is installed.
    pub fn sampler(&self, view: &Viewport, style: GroundStyle) -> Option<Sampler> {
        ground::grid_zoom(view.zoom())?;
        self.any().then(|| Sampler {
            store: self.clone(),
            view: *view,
            style,
            held: None,
        })
    }
}

fn list_packs(dir: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Listed> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if !path.extension().is_some_and(|x| x == "lmg") {
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

/// One view's ground rows, in the view's own pixels.
pub struct Sampler {
    store: GroundStore,
    view: Viewport,
    style: GroundStyle,
    held: Option<(TileId, Option<Arc<Vec<u8>>>)>,
}

impl Classes for Sampler {
    fn class(&mut self, zoom: u8, x: u32, y: u32) -> u8 {
        let id = (zoom, x >> 8, y >> 8);
        if self.held.as_ref().map(|h| h.0) != Some(id) {
            self.held = Some((id, self.store.tile(id)));
        }
        match self.held.as_ref().and_then(|h| h.1.as_ref()) {
            Some(t) => t[(y as usize & 255) * TILE_EDGE + (x as usize & 255)],
            None => NONE,
        }
    }
}

impl ground::Ground for Sampler {
    fn row(&mut self, x: i32, y: i32, px: &mut [[u8; 3]]) -> bool {
        let zoom = self.view.zoom();
        // World units per display pixel: the world is 2^32 around.
        let px_units = 1u64 << (24 - u32::from(zoom));
        let w = self.view.to_world(x, y);
        let (gx, gy) = (u64::from(w.x) / px_units, u64::from(w.y) / px_units);
        let style = self.style;
        ground::paint_row(style, zoom, gx, gy, self, px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::ground::{Class, Ground};
    use leviculum_map::mercator::{project, LatLon};

    /// A one-block pack of hamada with a dune square, written as the cut
    /// writes it.
    fn pack(bx: u32, by: u32) -> Vec<u8> {
        use leviculum_map::elevation::{write_block_of, write_directory_of, DirectoryInfo};
        let mut frames = Vec::new();
        for _ in 0..16 {
            let mut t = vec![Class::Hamada as u8; TILE_CELLS];
            for r in 100..156 {
                for c in 100..156 {
                    t[r * TILE_EDGE + c] = Class::Dune as u8;
                }
            }
            frames.push(ruzstd::encoding::compress_to_vec(
                &t[..],
                ruzstd::encoding::CompressionLevel::Fastest,
            ));
        }
        let info = DirectoryInfo {
            tile_zoom: 11,
            block_zoom: 9,
            bounds: [0; 4],
            attribution: "a",
            notice: "n",
        };
        let mut out = write_directory_of(PackKind::Ground, &info, &[(bx, by)]).expect("directory");
        out.extend(write_block_of(PackKind::Ground, 11, 9, bx, by, &frames).expect("block"));
        out
    }

    #[test]
    fn a_pack_in_the_maps_directory_paints_the_ground_of_its_view() {
        let dir = tempfile::tempdir().expect("dir");
        let at = project(LatLon::from_degrees(31.1, -4.0));
        let (bx, by) = (at.x >> 23, at.y >> 23);
        std::fs::write(dir.path().join("auto-g9.lmg"), pack(bx, by)).expect("write");
        // A height pack beside it is not a ground pack.
        std::fs::write(
            dir.path().join("h.lmh"),
            include_bytes!("../../leviculum-map/fixtures/erg-chebbi.lmh"),
        )
        .expect("write");
        let store = GroundStore::default();
        assert!(!store.any());
        store.refresh(Some(dir.path()));
        assert!(store.any());
        let view = Viewport::new(at, 12, 64, 64);
        let mut s = store
            .sampler(&view, GroundStyle::Strong)
            .expect("a sampler");
        let paper = [245, 243, 238];
        let mut px = vec![paper; 64];
        assert!(s.row(0, 32, &mut px));
        assert!(
            px.iter().all(|p| *p != paper),
            "every pixel lies on a class"
        );
        // Far out, no ground.
        assert!(store
            .sampler(&Viewport::new(at, 8, 64, 64), GroundStyle::Strong)
            .is_none());
        // Off the pack, the paper stays.
        let away = Viewport::new(project(LatLon::from_degrees(48.0, 11.0)), 12, 64, 64);
        let mut s = store
            .sampler(&away, GroundStyle::Pastel)
            .expect("a sampler");
        let mut px = vec![paper; 64];
        assert!(!s.row(0, 32, &mut px));
        assert!(px.iter().all(|p| *p == paper));
        // A pack removed is gone after the next refresh.
        std::fs::remove_file(dir.path().join("auto-g9.lmg")).expect("rm");
        store.refresh(Some(dir.path()));
        assert!(!store.any());
    }
}
