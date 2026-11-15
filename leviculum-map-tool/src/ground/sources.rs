//! The ground cut's inputs, the ones order 1742 cleared and nothing
//! else: Kellndorfer et al.'s seasonal Sentinel-1 coherence and
//! backscatter (CC BY 4.0), Digital Earth Africa's GeoMAD (CC BY 4.0) and
//! Copernicus GLO-30, the last read from a height pack the height cut
//! already wrote (`leviculum-map-tool heights`), so the ground and the
//! relief see the same heights.
//!
//! Every input is read through [`Fetch`]: from the public buckets over
//! HTTP range requests, or from a local directory with the same layout
//! (`--source`), which is how the tests run and how a cut is repeated
//! offline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use leviculum_map::elevation::{PackIndex, TileDecoder, TILE_CELLS, TILE_EDGE};
use leviculum_mapcut::http::HttpRange;
use leviculum_mapcut::pmtiles::RangeRead;
use leviculum_mapcut::Res;

use super::tiff::Tiff;

/// Kellndorfer's tiles in the AWS Open Data bucket.
pub const KELL_HTTP: &str =
    "https://sentinel-1-global-coherence-earthbigdata.s3.us-west-2.amazonaws.com/data/tiles";
/// GeoMAD's COGs (Sentinel-2 annual, 2024) in Digital Earth Africa's bucket.
pub const GEOMAD_HTTP: &str = "https://deafrica-services.s3.af-south-1.amazonaws.com";
/// Digital Earth Africa's STAC search, to find GeoMAD's tiles.
pub const GEOMAD_STAC: &str = "https://explorer.digitalearth.africa/stac/search";
/// The GeoMAD year read.
pub const GEOMAD_YEAR: u32 = 2024;

/// The attribution line of a ground pack: every input's credit, in the
/// words order 1742 settled (`docs/drafts/africa-ground-map-licences.md`).
pub const ATTRIBUTION: &str = "Ground classes: Global seasonal Sentinel-1 coherence and \
     backscatter, Kellndorfer et al. 2022, CC BY 4.0, modified; GeoMAD © Digital Earth Africa, \
     CC BY 4.0, modified; contains modified Copernicus Sentinel data 2019-2020 and 2024; \
     produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus Defence and Space \
     GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all rights reserved";

/// The notice: the licences with their addresses, and what was done.
pub const NOTICE: &str = "Ground classes derived (resampled to Web Mercator, classified and \
     generalised by leviculum-map-tool ground) from: Kellndorfer, J. et al. (2022), Global \
     seasonal Sentinel-1 interferometric coherence and backscatter data set, Scientific Data 9, \
     73, https://registry.opendata.aws/ebd-sentinel-1-global-coherence-backscatter/ , \
     CC BY 4.0 https://creativecommons.org/licenses/by/4.0/ ; Digital Earth Africa GeoMAD \
     (gm_s2_annual 2024), https://docs.digitalearthafrica.org/en/latest/data_specs/GeoMAD_specs.html , \
     CC BY 4.0, algorithms by Dr Dale Roberts, Australian National University; Copernicus \
     Sentinel data (Regulation (EU) 377/2014, Delegated Regulation (EU) 1159/2013); Copernicus \
     DEM GLO-30 under the Licence for the use of the Copernicus WorldDEM-30, whose providers \
     incur no liability for any use of it. No endorsement by any provider is implied. The \
     classes describe ground, never whether it can be crossed.";

/// Where input bytes come from.
pub trait Fetch: Sync {
    /// A reader over the object at `path` (relative to the source's
    /// base: `kell/...`, `geomad/...`), `None` where the source has no
    /// such object.
    fn open(&self, path: &str) -> Res<Option<Box<dyn RangeRead>>>;
    /// The STAC search answer for GeoMAD over a box, `None` where the
    /// source holds no catalogue.
    fn stac(&self, bbox: [f64; 4]) -> Res<Option<String>>;
}

/// A local directory: `kell/<tile>/<tile>_<layer>.tif`,
/// `geomad/<object path>`, `geomad/stac.json`.
pub struct DirFetch(pub PathBuf);

impl Fetch for DirFetch {
    fn open(&self, path: &str) -> Res<Option<Box<dyn RangeRead>>> {
        match std::fs::File::open(self.0.join(path)) {
            Ok(f) => Ok(Some(Box::new(f))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{path}: {e}").into()),
        }
    }

    fn stac(&self, _bbox: [f64; 4]) -> Res<Option<String>> {
        match std::fs::read_to_string(self.0.join("geomad/stac.json")) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("geomad/stac.json: {e}").into()),
        }
    }
}

/// A local directory first, the public buckets for what it lacks: the
/// small radar cells mirrored once, the imagery read by range requests.
pub struct LocalFirst(pub DirFetch, pub HttpFetch);

impl Fetch for LocalFirst {
    fn open(&self, path: &str) -> Res<Option<Box<dyn RangeRead>>> {
        match self.0.open(path)? {
            Some(r) => Ok(Some(r)),
            None => self.1.open(path),
        }
    }

    fn stac(&self, bbox: [f64; 4]) -> Res<Option<String>> {
        match self.0.stac(bbox)? {
            Some(s) => Ok(Some(s)),
            None => self.1.stac(bbox),
        }
    }
}

/// The public buckets.
pub struct HttpFetch {
    agent: ureq::Agent,
}

impl Default for HttpFetch {
    fn default() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(600)))
            .user_agent(concat!("leviculum-map-tool/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        HttpFetch { agent }
    }
}

impl Fetch for HttpFetch {
    fn open(&self, path: &str) -> Res<Option<Box<dyn RangeRead>>> {
        let url = if let Some(rest) = path.strip_prefix("kell/") {
            format!("{KELL_HTTP}/{rest}")
        } else if let Some(rest) = path.strip_prefix("geomad/") {
            format!("{GEOMAD_HTTP}/{rest}")
        } else {
            return Err(format!("no source for {path}").into());
        };
        match self.agent.head(&url).call() {
            Ok(_) => Ok(Some(Box::new(HttpRange::new(&url)))),
            Err(ureq::Error::StatusCode(403 | 404)) => Ok(None),
            Err(e) => Err(format!("{url}: {e}").into()),
        }
    }

    fn stac(&self, b: [f64; 4]) -> Res<Option<String>> {
        let url = format!(
            "{GEOMAD_STAC}?collections=gm_s2_annual&bbox={},{},{},{}&datetime={y}-01-01/{y}-12-31&limit=500",
            b[0],
            b[1],
            b[2],
            b[3],
            y = GEOMAD_YEAR
        );
        let mut r = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("{url}: {e}"))?;
        Ok(Some(
            r.body_mut()
                .with_config()
                .limit(64 << 20)
                .read_to_string()
                .map_err(|e| format!("{url}: {e}"))?,
        ))
    }
}

/// Kellndorfer's tile name of the 1° cell holding (lat, lon): named by
/// its north-west corner.
pub fn kell_tile(lat: f64, lon: f64) -> String {
    let n = lat.floor() as i32 + 1;
    let w = lon.floor() as i32;
    let ns = if n < 0 { 'S' } else { 'N' };
    let ew = if w < 0 { 'W' } else { 'E' };
    format!("{ns}{:02}{ew}{:03}", n.unsigned_abs(), w.unsigned_abs())
}

/// The coherence layers whose median is the coherence feature.
pub const KELL_COH: [&str; 4] = [
    "summer_vv_COH12",
    "winter_vv_COH12",
    "spring_vv_COH12",
    "fall_vv_COH12",
];
/// The backscatter layer.
pub const KELL_VV: &str = "summer_vv_AMP";

/// One Kellndorfer cell decoded: coherence median and VV in dB, on the
/// cell's own grid.
pub struct KellCell {
    w: usize,
    h: usize,
    lon0: f64,
    lat0: f64,
    dx: f64,
    dy: f64,
    coh: Vec<f32>,
    vv: Vec<f32>,
}

impl KellCell {
    fn sample(&self, lat: f64, lon: f64) -> (f32, f32) {
        let fx = (lon - self.lon0) / self.dx - 0.5;
        let fy = (self.lat0 - lat) / self.dy - 0.5;
        (
            bilinear(&self.coh, self.w, self.h, fx, fy),
            bilinear(&self.vv, self.w, self.h, fx, fy),
        )
    }
}

/// Bilinear sample of a grid at fractional pixel (fx, fy) of pixel
/// centres, clamped to the grid; NaN where a neighbour is NaN.
fn bilinear(v: &[f32], w: usize, h: usize, fx: f64, fy: f64) -> f32 {
    let x0 = fx.floor().clamp(0.0, (w - 1) as f64);
    let y0 = fy.floor().clamp(0.0, (h - 1) as f64);
    let tx = (fx - x0).clamp(0.0, 1.0) as f32;
    let ty = (fy - y0).clamp(0.0, 1.0) as f32;
    let (x0, y0) = (x0 as usize, y0 as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let a = v[y0 * w + x0];
    let b = v[y0 * w + x1];
    let c = v[y1 * w + x0];
    let d = v[y1 * w + x1];
    (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty
}

fn median4(mut v: [f32; 4]) -> f32 {
    let n = v.iter().filter(|x| x.is_finite()).count();
    if n == 0 {
        return f32::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    // NaNs sort last under total_cmp for positive NaN.
    let f: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if f.len() % 2 == 1 {
        f[f.len() / 2]
    } else {
        (f[f.len() / 2 - 1] + f[f.len() / 2]) / 2.0
    }
}

/// One decoded layer of a cell: samples, width, height, and the
/// north-west corner and pixel size (lon, lat, dx, dy).
type Layer = (Vec<f32>, usize, usize, [f64; 4]);

/// A cell held across a row of pixels: its 1° square and the cell.
pub type HeldCell = Option<((i32, i32), Option<std::sync::Arc<KellCell>>)>;

/// The radar inputs, cell by cell, cached.
pub struct Kell<'a> {
    fetch: &'a dyn Fetch,
    cells: Mutex<HashMap<String, Option<std::sync::Arc<KellCell>>>>,
    /// Bytes fetched.
    pub bytes: Mutex<u64>,
}

impl<'a> Kell<'a> {
    pub fn new(fetch: &'a dyn Fetch) -> Self {
        Kell {
            fetch,
            cells: Mutex::new(HashMap::new()),
            bytes: Mutex::new(0),
        }
    }

    fn layer(&self, tile: &str, layer: &str) -> Res<Option<Layer>> {
        let Some(src) = self
            .fetch
            .open(&format!("kell/{tile}/{tile}_{layer}.tif"))?
        else {
            return Ok(None);
        };
        let t = Tiff::open(src.as_ref())?;
        let im = &t.images[0];
        let (scale, tie) = match (im.scale, im.tie) {
            (Some(s), Some(t)) => (s, t),
            _ => return Err(format!("{tile}_{layer}: no georeference").into()),
        };
        let (w, h) = (im.width, im.height);
        let (_, _, _, _, v) = t.window(0, (0, 0, w, h))?;
        let fetched: u64 = im.chunk_bytes();
        *self.bytes.lock().map_err(|_| "lock")? += fetched;
        // The tie point names the north-west corner of pixel (0, 0).
        Ok(Some((v, w, h, [tie[2], tie[3], scale[0], scale[1]])))
    }

    fn cell(&self, tile: &str) -> Res<Option<std::sync::Arc<KellCell>>> {
        // Held over the fetch: a cell the next worker wants too is fetched
        // once, not once per worker.
        let mut cells = self.cells.lock().map_err(|_| "lock")?;
        if let Some(c) = cells.get(tile) {
            return Ok(c.clone());
        }
        let mut cohs = Vec::new();
        let mut geo = None;
        for layer in KELL_COH {
            if let Some((v, w, h, g)) = self.layer(tile, layer)? {
                geo = Some((w, h, g));
                cohs.push(v);
            }
        }
        let vv = self.layer(tile, KELL_VV)?;
        let cell = match (geo, vv) {
            (Some((w, h, g)), Some((vv, vw, vh, _))) if vw == w && vh == h => {
                let n = w * h;
                let coh = (0..n)
                    .map(|i| {
                        let mut s = [f32::NAN; 4];
                        for (k, c) in cohs.iter().enumerate() {
                            // Coherence is stored as DN = 100 x coherence.
                            s[k] = c[i] / 100.0;
                        }
                        median4(s)
                    })
                    .collect();
                // Backscatter: gamma0 dB = 20 log10(DN) - 83.
                let vv = vv
                    .iter()
                    .map(|&d| {
                        if d > 0.0 {
                            20.0 * d.log10() - 83.0
                        } else {
                            f32::NAN
                        }
                    })
                    .collect();
                Some(std::sync::Arc::new(KellCell {
                    w,
                    h,
                    lon0: g[0],
                    lat0: g[1],
                    dx: g[2],
                    dy: g[3],
                    coh,
                    vv,
                }))
            }
            _ => None,
        };
        cells.insert(tile.to_owned(), cell.clone());
        Ok(cell)
    }

    /// Coherence and VV at (lat, lon); NaN where no cell covers it.
    pub fn at(&self, lat: f64, lon: f64) -> Res<(f32, f32)> {
        Ok(match self.cell(&kell_tile(lat, lon))? {
            Some(c) => c.sample(lat, lon),
            None => (f32::NAN, f32::NAN),
        })
    }

    /// [`Kell::at`] through a held cell: `held` keeps the last cell and its
    /// 1° square, so a row of pixels looks the cache up once a cell.
    pub fn at_held(&self, lat: f64, lon: f64, held: &mut HeldCell) -> Res<(f32, f32)> {
        let key = (lat.floor() as i32, lon.floor() as i32);
        if held.as_ref().map(|h| h.0) != Some(key) {
            *held = Some((key, self.cell(&kell_tile(lat, lon))?));
        }
        Ok(match held.as_ref().and_then(|h| h.1.as_ref()) {
            Some(c) => c.sample(lat, lon),
            None => (f32::NAN, f32::NAN),
        })
    }

    /// Drop the cells no longer needed: those whose south edge lies at
    /// or north of `lat`.
    pub fn drop_north_of(&self, lat: f64) {
        if let Ok(mut cells) = self.cells.lock() {
            cells.retain(|k, _| {
                let n: i32 = k.get(1..3).and_then(|d| d.parse().ok()).unwrap_or(i32::MIN);
                let n = if k.starts_with('S') { -n } else { n };
                f64::from(n) - 1.0 < lat
            });
        }
    }
}

/// WGS 84 / NSIDC EASE-Grid 2.0 Global (EPSG:6933), the CRS of Digital
/// Earth Africa's products: a Lambert cylindrical equal-area projection
/// of the ellipsoid with standard parallel 30°. Returns (x, y) metres.
pub fn ease2(lat: f64, lon: f64) -> (f64, f64) {
    const A: f64 = 6_378_137.0;
    const F: f64 = 1.0 / 298.257_223_563;
    let e2 = F * (2.0 - F);
    let e = e2.sqrt();
    let s1 = 30f64.to_radians().sin();
    let k0 = 30f64.to_radians().cos() / (1.0 - e2 * s1 * s1).sqrt();
    let s = lat.to_radians().sin();
    let q = (1.0 - e2)
        * (s / (1.0 - e2 * s * s) - (1.0 / (2.0 * e)) * ((1.0 - e * s) / (1.0 + e * s)).ln());
    (A * k0 * lon.to_radians(), A * q / (2.0 * k0))
}

/// One GeoMAD tile from the STAC answer: its box and band objects.
#[derive(Clone, Debug)]
pub struct GeomadItem {
    pub bbox: [f64; 4],
    /// Object paths under the bucket, by band.
    pub bands: HashMap<String, String>,
    /// The full image's origin (x, y of its north-west corner) and pixel
    /// size, metres in EPSG:6933.
    pub origin: (f64, f64),
    pub pixel: f64,
}

/// The bands read.
pub const GEOMAD_BANDS: [&str; 4] = ["B02", "B03", "B04", "B08"];

/// Parse the STAC search answer.
pub fn geomad_items(json: &str) -> Res<Vec<GeomadItem>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let mut out = Vec::new();
    for f in v["features"].as_array().ok_or("STAC: no features")? {
        let bbox: Vec<f64> = f["bbox"]
            .as_array()
            .ok_or("STAC: no bbox")?
            .iter()
            .filter_map(|x| x.as_f64())
            .collect();
        let t: Vec<f64> = f["properties"]["proj:transform"]
            .as_array()
            .ok_or("STAC: no transform")?
            .iter()
            .filter_map(|x| x.as_f64())
            .collect();
        if bbox.len() != 4 || t.len() < 6 {
            return Err("STAC: bad bbox or transform".into());
        }
        let mut bands = HashMap::new();
        for b in GEOMAD_BANDS {
            let href = f["assets"][b]["href"].as_str().ok_or("STAC: no band")?;
            let path = href
                .strip_prefix("s3://deafrica-services/")
                .ok_or("STAC: band outside deafrica-services")?;
            bands.insert(b.to_owned(), path.to_owned());
        }
        out.push(GeomadItem {
            bbox: [bbox[0], bbox[1], bbox[2], bbox[3]],
            bands,
            origin: (t[2], t[5]),
            pixel: t[0],
        });
    }
    // A fixed order: by box, so the first valid item that covers a
    // pixel is the same on every run.
    out.sort_by(|a, b| {
        a.bbox[0]
            .total_cmp(&b.bbox[0])
            .then(a.bbox[1].total_cmp(&b.bbox[1]))
    });
    Ok(out)
}

/// One GeoMAD tile's overview, albedo and NDVI on its own grid.
struct GeomadTile {
    w: usize,
    h: usize,
    origin: (f64, f64),
    pixel: f64,
    albedo: Vec<f32>,
    ndvi: Vec<f32>,
}

/// GeoMAD, tile by tile, read at the coarsest overview whose pixel is
/// still at most `want` metres (the pilot's: three quarters of a z11
/// pixel).
pub struct Geomad<'a> {
    fetch: &'a dyn Fetch,
    items: Vec<GeomadItem>,
    want: f64,
    tiles: Mutex<HashMap<usize, Option<std::sync::Arc<GeomadTile>>>>,
    pub bytes: Mutex<u64>,
}

impl<'a> Geomad<'a> {
    pub fn new(fetch: &'a dyn Fetch, items: Vec<GeomadItem>, want: f64) -> Self {
        Geomad {
            fetch,
            items,
            want,
            tiles: Mutex::new(HashMap::new()),
            bytes: Mutex::new(0),
        }
    }

    fn tile(&self, n: usize) -> Res<Option<std::sync::Arc<GeomadTile>>> {
        // Held over the fetch, as for the radar cells.
        let mut tiles = self.tiles.lock().map_err(|_| "lock")?;
        if let Some(t) = tiles.get(&n) {
            return Ok(t.clone());
        }
        let item = &self.items[n];
        let mut bands = Vec::new();
        let mut grid = None;
        for b in GEOMAD_BANDS {
            let Some(src) = self.fetch.open(&format!("geomad/{}", item.bands[b]))? else {
                bands.clear();
                break;
            };
            let t = Tiff::open(src.as_ref())?;
            let full = t.images[0].width as f64;
            // The coarsest image whose pixel is still fine enough.
            let mut k = 0;
            for (i, im) in t.images.iter().enumerate() {
                if item.pixel * full / im.width as f64 <= self.want {
                    k = i;
                }
            }
            let im = &t.images[k];
            let (_, _, w, h, v) = t.window(k, (0, 0, im.width, im.height))?;
            *self.bytes.lock().map_err(|_| "lock")? += im.chunk_bytes();
            grid = Some((w, h, item.pixel * full / im.width as f64));
            bands.push(v);
        }
        let tile = match (grid, bands.len()) {
            (Some((w, h, pixel)), 4) => {
                let n = w * h;
                let mut albedo = vec![f32::NAN; n];
                let mut ndvi = vec![f32::NAN; n];
                for i in 0..n {
                    let (b2, b3, b4, b8) = (
                        bands[0][i] / 1e4,
                        bands[1][i] / 1e4,
                        bands[2][i] / 1e4,
                        bands[3][i] / 1e4,
                    );
                    albedo[i] = (b2 + b3 + b4) / 3.0;
                    ndvi[i] = (b8 - b4) / (b8 + b4).max(1e-4);
                }
                Some(std::sync::Arc::new(GeomadTile {
                    w,
                    h,
                    origin: item.origin,
                    pixel,
                    albedo,
                    ndvi,
                }))
            }
            _ => None,
        };
        tiles.insert(n, tile.clone());
        Ok(tile)
    }

    /// Albedo and NDVI averaged over a z11 pixel: 3 x 3 points at the
    /// pixel's sub-centres, each the nearest source pixel; NaN where no
    /// tile covers a point.
    pub fn at(&self, corners: [(f64, f64); 9]) -> Res<(f32, f32)> {
        let (mut a, mut v, mut n) = (0.0f32, 0.0f32, 0u32);
        for (lat, lon) in corners {
            let (x, y) = ease2(lat, lon);
            for (k, item) in self.items.iter().enumerate() {
                let [w, s, e, nn] = item.bbox;
                if lon < w || lon >= e || lat < s || lat >= nn {
                    continue;
                }
                let Some(t) = self.tile(k)? else {
                    continue;
                };
                let px = ((x - t.origin.0) / t.pixel).floor();
                let py = ((t.origin.1 - y) / t.pixel).floor();
                if px < 0.0 || py < 0.0 || px >= t.w as f64 || py >= t.h as f64 {
                    continue;
                }
                let i = py as usize * t.w + px as usize;
                if t.albedo[i].is_finite() && t.ndvi[i].is_finite() {
                    a += t.albedo[i];
                    v += t.ndvi[i];
                    n += 1;
                    break;
                }
            }
        }
        Ok(if n == 0 {
            (f32::NAN, f32::NAN)
        } else {
            (a / n as f32, v / n as f32)
        })
    }

    /// Drop the tiles whose south edge lies at or north of `lat`.
    pub fn drop_north_of(&self, lat: f64) {
        if let Ok(mut tiles) = self.tiles.lock() {
            let items = &self.items;
            tiles.retain(|&k, _| items[k].bbox[1] < lat);
        }
    }
}

/// A decoded z12 height tile, or `None` where the pack has none.
type HeightTile = Option<std::sync::Arc<Vec<i16>>>;

/// The heights of a height pack, z12 tile by tile, cached.
pub struct Heights {
    file: std::fs::File,
    index: PackIndex,
    tiles: Mutex<HashMap<(u32, u32), HeightTile>>,
}

impl Heights {
    pub fn open(path: &std::path::Path) -> Res<Self> {
        use std::os::unix::fs::FileExt;
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = file.metadata()?.len();
        let index = PackIndex::read(len, |at, buf| file.read_exact_at(buf, at).is_ok())
            .map_err(|e| format!("{}: {e:?}", path.display()))?;
        if index.tile_zoom() != 12 {
            return Err(format!(
                "{}: heights at z{}, the ground cut reads z12",
                path.display(),
                index.tile_zoom()
            )
            .into());
        }
        Ok(Heights {
            file,
            index,
            tiles: Mutex::new(HashMap::new()),
        })
    }

    /// The z12 tile (x, y); `None` where the pack does not hold it.
    pub fn tile(&self, x: u32, y: u32) -> Res<Option<std::sync::Arc<Vec<i16>>>> {
        use std::os::unix::fs::FileExt;
        let mut tiles = self.tiles.lock().map_err(|_| "lock")?;
        if let Some(t) = tiles.get(&(x, y)) {
            return Ok(t.clone());
        }
        let t = match self.index.block_for(12, x, y) {
            Some(range) => {
                let mut piece = vec![0u8; range.len()];
                self.file.read_exact_at(&mut piece, range.start as u64)?;
                let mut out = vec![0i16; TILE_CELLS];
                let mut scratch = vec![0i16; TILE_CELLS];
                self.index
                    .decode(
                        &piece,
                        (12, x, y),
                        &mut TileDecoder::new(),
                        &mut out,
                        &mut scratch,
                    )
                    .map_err(|e| format!("height tile {x},{y}: {e:?}"))?;
                Some(std::sync::Arc::new(out))
            }
            None => None,
        };
        tiles.insert((x, y), t.clone());
        Ok(t)
    }

    /// Drop the tiles north of z12 tile row `y`.
    pub fn drop_north_of(&self, y: u32) {
        if let Ok(mut t) = self.tiles.lock() {
            t.retain(|k, _| k.1 >= y);
        }
    }

    /// The z12 edge in samples, for the cut's arithmetic.
    pub const EDGE: usize = TILE_EDGE;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kellndorfer_names_cells_by_their_north_west_corner() {
        assert_eq!(kell_tile(30.5, -4.5), "N31W005");
        assert_eq!(kell_tile(29.3, -7.6), "N30W008");
        assert_eq!(kell_tile(-0.5, 10.2), "N00E010");
        assert_eq!(kell_tile(-1.5, 10.2), "S01E010");
    }

    /// The projection against a GeoMAD tile's own numbers: tile x176y116
    /// starts at x = -480,000 m, which the STAC answer places at
    /// 4.97480054938215 W, and its top edge y = 3,840,000 m lies at
    /// 31.64834214907708 N.
    #[test]
    fn ease2_matches_the_stac_answer() {
        let (x, _) = ease2(31.0, -4.974_800_549_382_15);
        assert!((x + 480_000.0).abs() < 0.01, "x {x}");
        let (_, y) = ease2(31.648_342_149_077_08, -4.5);
        assert!((y - 3_840_000.0).abs() < 1.0, "y {y}");
    }

    #[test]
    fn the_coherence_median_skips_missing_seasons() {
        assert_eq!(median4([0.1, 0.5, 0.3, 0.9]), 0.4);
        assert_eq!(median4([0.1, f32::NAN, 0.3, 0.9]), 0.3);
        assert!(median4([f32::NAN; 4]).is_nan());
    }
}
