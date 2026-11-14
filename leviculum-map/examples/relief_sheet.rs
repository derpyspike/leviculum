//! The order's relief sheets (1741): a window of a height pack shaded
//! with style R′ beside the plain damped hillshade CoMaps-style light
//! the pilot rejected, over the light table's paper, as one PNG — and
//! the luminance percentiles and per-tile compute times on stdout.
//!
//!    relief_sheet <pack.lmh> <lon> <lat> <zoom> <width> <height> <out.png>
//!
//! The left panel is R′ ([`leviculum_map::relief`]), the right the
//! comparator: one north-west light, a fixed gain tuned for mountain
//! slopes, a soft damp, no local normalisation — the "plain CoMaps
//! damping" of the order's header, which must and does erase the 20 to
//! 80 m dune chains the left panel keeps.

use std::collections::HashMap;
use std::fs;
use std::time::Instant;

use leviculum_map::elevation::{split, Block, TileDecoder, TILE_CELLS};
use leviculum_map::relief::{self, assemble, sample, shade_rgb, sky_radius_m, Plane, ShadePlane};

/// The light table's paper (leviculum-mapfill's LIGHT.background).
const PAPER: [u8; 3] = [245, 241, 233];

struct Pack<'a> {
    bytes: &'a [u8],
    ranges: Vec<core::ops::Range<usize>>,
    blocks: Vec<(u32, u32)>,
    tile_zoom: u8,
    block_zoom: u8,
    tiles: HashMap<(u8, u32, u32), Option<Vec<i16>>>,
}

impl<'a> Pack<'a> {
    fn open(bytes: &'a [u8]) -> Self {
        let (dir, ranges) = split(bytes).expect("a height pack");
        let blocks = (0..dir.len()).filter_map(|n| dir.block(n)).collect();
        Self {
            bytes,
            ranges,
            blocks,
            tile_zoom: dir.tile_zoom(),
            block_zoom: dir.block_zoom(),
            tiles: HashMap::new(),
        }
    }

    /// The heights of tile (x, y) at `zoom`, decoded once.
    // Not an `entry`: the decode reads the pack while an entry would
    // hold the map's borrow across it.
    #[allow(clippy::map_entry)]
    fn tile(&mut self, zoom: u8, x: u32, y: u32) -> Option<&[i16]> {
        if !self.tiles.contains_key(&(zoom, x, y)) {
            let got = self.decode(zoom, x, y);
            self.tiles.insert((zoom, x, y), got);
        }
        self.tiles.get(&(zoom, x, y)).unwrap().as_deref()
    }

    fn decode(&self, zoom: u8, x: u32, y: u32) -> Option<Vec<i16>> {
        let (bx, by) = (x >> (zoom - self.block_zoom), y >> (zoom - self.block_zoom));
        let n = self.blocks.iter().position(|&b| b == (bx, by))?;
        let block = Block::parse(&self.bytes[self.ranges[n + 1].clone()]).ok()?;
        let mut out = vec![0i16; TILE_CELLS];
        let mut scratch = vec![0i16; TILE_CELLS];
        let mut dec = TileDecoder::new();
        block
            .heights(&mut dec, zoom, x, y, &mut out, &mut scratch)
            .ok()?;
        Some(out)
    }

    /// The aproned plane of relief tile (x, y) at `zoom`, each
    /// neighbour decoded once across tiles.
    fn plane(&mut self, zoom: u8, x: u32, y: u32) -> Plane {
        let grid = i64::from(1u32 << zoom);
        let at = |i: usize| {
            let (dx, dy) = ((i % 3) as i64 - 1, (i / 3) as i64 - 1);
            (i64::from(x) + dx, i64::from(y) + dy)
        };
        for i in 0..9 {
            let (nx, ny) = at(i);
            if nx >= 0 && ny >= 0 && nx < grid && ny < grid {
                self.tile(zoom, nx as u32, ny as u32);
            }
        }
        let refs: [Option<&[i16]>; 9] = core::array::from_fn(|i| {
            let (nx, ny) = at(i);
            self.tiles
                .get(&(zoom, nx as u32, ny as u32))
                .and_then(|o| o.as_deref())
        });
        assemble(&refs)
    }
}

/// The plain comparator: one NW light at a mountain gain, damped.
fn plain_tone(dem: &Plane, res: f32) -> Vec<f32> {
    let (gx, gy) = relief::sobel(dem, res, 1.0);
    gx.v.iter()
        .zip(&gy.v)
        .map(|(&x, &y)| {
            let s = core::f32::consts::FRAC_1_SQRT_2 * (x + y);
            // Full shade near a one-in-two slope, then damped by half:
            // the plateau look that drowns a two-degree dune face.
            0.5 * (2.2 * s).tanh()
        })
        .collect()
}

fn lum(c: [u8; 3]) -> f32 {
    0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
}

fn percentiles(px: &[[u8; 3]]) -> (f32, f32, f32) {
    let mut l: Vec<f32> = px.iter().map(|&c| lum(c)).collect();
    l.sort_by(f32::total_cmp);
    (
        l[(l.len() - 1) * 5 / 100],
        l[(l.len() - 1) / 2],
        l[(l.len() - 1) * 95 / 100],
    )
}

#[allow(clippy::too_many_arguments)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 8 {
        eprintln!("relief_sheet <pack.lmh> <lon> <lat> <zoom> <width> <height> <out.png>");
        std::process::exit(2);
    }
    let bytes = fs::read(&args[1]).expect("read the pack");
    let (lon, lat): (f64, f64) = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let zoom: u8 = args[4].parse().unwrap();
    let (w, h): (usize, usize) = (args[5].parse().unwrap(), args[6].parse().unwrap());
    let mut pack = Pack::open(&bytes);
    let rz = zoom.min(pack.tile_zoom).max(pack.block_zoom);
    assert!(zoom >= rz, "zoom under the pack's block zoom");
    let res = (156_543.03 * lat.to_radians().cos() / f64::from(1u32 << rz) as f64) as f32;
    // The view's top-left in world pixels at the display zoom.
    let n = f64::from(1u32 << zoom) * 256.0;
    let cx = (lon + 180.0) / 360.0 * n;
    let merc = (lat.to_radians().tan() + 1.0 / lat.to_radians().cos()).ln();
    let cy = (1.0 - merc / core::f64::consts::PI) / 2.0 * n;
    let (x0, y0) = (cx - w as f64 / 2.0, cy - h as f64 / 2.0);
    let step = f64::from(1u32 << (zoom - rz)); // display px per relief sample
                                               // Every relief tile the view touches, computed once.
    let mut shades: HashMap<(u32, u32), (ShadePlane, Vec<f32>)> = HashMap::new();
    let mut out = vec![[0u8; 3]; w * h * 2];
    let mut tile_ms: Vec<f64> = Vec::new();
    for py in 0..h {
        for px in 0..w {
            let (wx, wy) = (x0 + px as f64 + 0.5, y0 + py as f64 + 0.5);
            // The relief sample coordinate and its tile.
            let (sx, sy) = (wx / step, wy / step);
            let (tx, ty) = ((sx / 256.0) as u32, (sy / 256.0) as u32);
            let (sp, plain) = shades.entry((tx, ty)).or_insert_with(|| {
                let t0 = Instant::now();
                let dem = pack.plane(rz, tx, ty);
                let sp = relief::compute(&dem, res, sky_radius_m(zoom));
                let plain = plain_tone(&dem, res);
                let ms = t0.elapsed().as_secs_f64() * 1e3;
                println!("RELIEF tile={rz}/{tx}/{ty} ms={ms:.1}");
                tile_ms.push(ms);
                (sp, plain)
            });
            let (u, v) = (
                (sx - f64::from(tx) * 256.0) as f32,
                (sy - f64::from(ty) * 256.0) as f32,
            );
            let (t, ao) = sample(sp, u, v);
            out[py * 2 * w + px] = shade_rgb(PAPER, t, ao);
            // The comparator panel: nearest relief sample, no sky term.
            let (iu, iv) = (
                (u as usize + relief::APRON).min(relief::SIDE - 1),
                (v as usize + relief::APRON).min(relief::SIDE - 1),
            );
            let tp = plain[iv * relief::SIDE + iu];
            let shaded = |c: u8| ((f32::from(c) * (1.0 + tp)).clamp(0.0, 255.0)) as u8;
            out[py * 2 * w + w + px] = [shaded(PAPER[0]), shaded(PAPER[1]), shaded(PAPER[2])];
        }
    }
    let (p5, med, p95) = percentiles(
        &out[..]
            .iter()
            .enumerate()
            .filter(|(i, _)| i % (2 * w) < w)
            .map(|(_, &c)| c)
            .collect::<Vec<_>>(),
    );
    println!("RPRIME p5={p5:.0} median={med:.0} p95={p95:.0}");
    let plain_px: Vec<[u8; 3]> = out
        .iter()
        .enumerate()
        .filter(|(i, _)| i % (2 * w) >= w)
        .map(|(_, &c)| c)
        .collect();
    let (p5, med, p95) = percentiles(&plain_px);
    println!("PLAIN p5={p5:.0} median={med:.0} p95={p95:.0}");
    // The sky exaggeration and its measured inputs on the window's
    // centre tile (the README's open ZF point, now data-driven).
    let (csx, csy) = (cx / step / 256.0, cy / step / 256.0);
    let dem = pack.plane(rz, csx as u32, csy as u32);
    let (fine, form, floor, zf) = relief::sky_zf(&dem, res);
    println!("ZF fine_rms={fine:.4} form_rms={form:.4} floor={floor:.4} zf={zf:.2}");
    tile_ms.sort_by(f64::total_cmp);
    if !tile_ms.is_empty() {
        println!(
            "TILES n={} median_ms={:.1} max_ms={:.1}",
            tile_ms.len(),
            tile_ms[(tile_ms.len() - 1) / 2],
            tile_ms[tile_ms.len() - 1]
        );
    }
    let file = fs::File::create(&args[7]).expect("create the sheet");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 2 * w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    let mut writer = enc.write_header().expect("png header");
    let flat: Vec<u8> = out.iter().flatten().copied().collect();
    writer.write_image_data(&flat).expect("png data");
}
