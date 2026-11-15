//! The ground cut (order 1743, stage A of the Sahara ground map): the
//! inputs order 1742 cleared ([`sources`]) into the ground classes of
//! [`leviculum_map::ground`], z11 tiles in z9 blocks, the height packs'
//! block grid.
//!
//! Streaming: the cut walks the blocks north to south, a block at a time.
//! Each block is computed over a window reaching [`GroundOptions::halo`]
//! z11 pixels past it on every side, so the filters, the flow
//! accumulation, the dry lakes' size and the generalisation see the
//! ground around the block; only the block itself is written. The inputs
//! are cached by source cell and dropped once the walk is south of them,
//! so the memory is a few block windows and the radar and imagery cells
//! of about two block rows, whatever the box. Catchments are counted
//! inside the window only: a wadi whose catchment reaches further than
//! the halo upstream is found from the part inside it.
//!
//! The output depends only on the input bytes, the box, the rules and
//! the halo: blocks run on several threads, but each block's window is
//! computed alone and written in block order.

pub mod grid;
pub mod rules;
pub mod sources;
pub mod tiff;
pub mod validate;

use std::io::Write;

use leviculum_map::elevation::{
    write_block_of, write_directory_of, DirectoryInfo, PackKind, TILE_EDGE,
};
use leviculum_map::ground::{BLOCK_ZOOM, TILE_ZOOM};
use leviculum_mapcut::Res;

use crate::heights::{lat_at, lon_at, tile_units, tile_x, tile_y};
use grid::{box_mean, box_std, flow_accumulation, majority, mmu, nearest_fill, slope_deg, Grid};
use rules::{classify, Px, Rules, WADI};
use sources::{Geomad, Heights, Kell};

/// What to cut.
#[derive(Clone, Debug)]
pub struct GroundOptions {
    /// west, south, east, north in degrees.
    pub bbox: [f64; 4],
    /// The rule set.
    pub rules: Rules,
    /// Window reach past a block, z11 pixels.
    pub halo: usize,
    /// zstd level of the tile frames.
    pub level: i32,
    /// Worker threads; the output does not depend on it.
    pub threads: usize,
}

/// The pilot's generalisation: the mode filter's window and the minimum
/// mapping unit in z11 pixels (about 5 ha).
pub const MAJORITY: usize = 5;
pub const MMU_PX: u32 = 12;

/// What the cut did.
#[derive(Clone, Debug, Default)]
pub struct GroundStats {
    pub blocks: usize,
    pub tiles: usize,
    pub bytes: u64,
    /// Pixels inside the box per class byte (0..=9), before and after the
    /// generalisation.
    pub raw: [u64; 10],
    pub generalised: [u64; 10],
    /// Pixels inside the box without radar, without imagery.
    pub no_radar: u64,
    pub no_imagery: u64,
    /// Input bytes fetched: radar, imagery.
    pub radar_bytes: u64,
    pub imagery_bytes: u64,
}

/// World units per z11 pixel.
fn px_units() -> u64 {
    tile_units(TILE_ZOOM) >> 8
}

/// Latitude and longitude of the centre of z11 pixel (gx, gy) (global
/// pixel coordinates), and of its 3 x 3 sub-centres.
fn centre(gx: i64, gy: i64) -> (f64, f64) {
    let u = px_units() as i64;
    let wrap = |v: i64| v.rem_euclid(1i64 << 32) as u64;
    (lat_at(wrap(gy * u + u / 2)), lon_at(wrap(gx * u + u / 2)))
}

fn sub_centres(gx: i64, gy: i64) -> [(f64, f64); 9] {
    let u = px_units() as i64;
    let wrap = |v: i64| v.rem_euclid(1i64 << 32) as u64;
    let mut out = [(0.0, 0.0); 9];
    for j in 0..3 {
        for i in 0..3 {
            let x = gx * u + (2 * i + 1) * u / 6;
            let y = gy * u + (2 * j + 1) * u / 6;
            out[(j * 3 + i) as usize] = (lat_at(wrap(y)), lon_at(wrap(x)));
        }
    }
    out
}

/// One block's window: its classes (generalised) and the per-pixel
/// statistics of its core.
pub struct BlockOut {
    /// The core's classes, `1024 x 1024` for a z9 block of z11 pixels.
    pub core: Vec<u8>,
    pub stats: GroundStats,
}

/// Compute block (bx, by) at the block zoom.
pub fn block_classes(
    bx: u32,
    by: u32,
    heights: &Heights,
    kell: &Kell,
    geomad: &Geomad,
    options: &GroundOptions,
) -> Res<BlockOut> {
    let side = (TILE_EDGE as u32) << (TILE_ZOOM - BLOCK_ZOOM);
    let halo = options.halo as i64;
    let n = side as usize + 2 * options.halo;
    let gx0 = i64::from(bx * side) - halo;
    let gy0 = i64::from(by * side) - halo;
    // Heights at z12 over the window: twice the z11 window.
    let n12 = 2 * n;
    let mut d12 = Grid::new(n12, n12, f32::NAN);
    let edge = Heights::EDGE as i64;
    let world12 = 1i64 << (12 + 8);
    for ty in (2 * gy0).div_euclid(edge)..=(2 * gy0 + n12 as i64 - 1).div_euclid(edge) {
        if ty < 0 || ty * edge >= world12 {
            continue;
        }
        for tx in (2 * gx0).div_euclid(edge)..=(2 * gx0 + n12 as i64 - 1).div_euclid(edge) {
            let wx = tx.rem_euclid(world12 / edge);
            let Some(tile) = heights.tile(wx as u32, ty as u32)? else {
                continue;
            };
            for r in 0..edge {
                let y = ty * edge + r - 2 * gy0;
                if y < 0 || y >= n12 as i64 {
                    continue;
                }
                for c in 0..edge {
                    let x = tx * edge + c - 2 * gx0;
                    if x < 0 || x >= n12 as i64 {
                        continue;
                    }
                    d12.v[y as usize * n12 + x as usize] = f32::from(tile[(r * edge + c) as usize]);
                }
            }
        }
    }
    let hole: Vec<bool> = d12.v.iter().map(|v| v.is_nan()).collect();
    if hole.iter().all(|&h| h) {
        d12.v.fill(0.0);
    } else {
        nearest_fill(&mut d12, &hole);
    }
    // Ground metres per z12 pixel, row by row.
    let circ = 40_075_016.686f64;
    let res12: Vec<f32> = (0..n12)
        .map(|r| {
            let (lat, _) = centre(0, gy0 + r as i64 / 2);
            (circ * lat.to_radians().cos() / (256.0 * 4096.0)) as f32
        })
        .collect();
    let s12 = slope_deg(&d12, &res12);
    let mut slope_max = Grid::new(n, n, 0.0f32);
    let mut d11 = Grid::new(n, n, 0.0f32);
    for y in 0..n {
        for x in 0..n {
            let i = (2 * y) * n12 + 2 * x;
            let s = [s12.v[i], s12.v[i + 1], s12.v[i + n12], s12.v[i + n12 + 1]];
            slope_max.v[y * n + x] = s.iter().copied().fold(0.0, f32::max);
            d11.v[y * n + x] =
                (d12.v[i] + d12.v[i + 1] + d12.v[i + n12] + d12.v[i + n12 + 1]) / 4.0;
        }
    }
    drop(s12);
    drop(d12);
    // About 1 km and 5 km at 66 m pixels: 15 and 75 pixels.
    let relief = box_std(&d11, 15);
    let mean5 = box_mean(&d11, 75);
    let acc = flow_accumulation(&d11);
    let [west, south, east, north] = options.bbox;
    let mut px = Grid::new(n, n, Px::default());
    let mut inside = vec![false; n * n];
    let mut held = None;
    for y in 0..n {
        let gy = gy0 + y as i64;
        for x in 0..n {
            let gx = gx0 + x as i64;
            let i = y * n + x;
            let (lat, lon) = centre(gx, gy);
            let (coh, vv) = kell.at_held(lat, lon, &mut held)?;
            let (albedo, ndvi) = geomad.at(sub_centres(gx, gy))?;
            px.v[i] = Px {
                coh,
                vv,
                albedo,
                ndvi,
                slope_max: slope_max.v[i],
                relief_1k: relief.v[i],
                tpi_5k: d11.v[i] - mean5.v[i],
                acc: acc.v[i],
            };
            inside[i] = lat >= south && lat < north && lon >= west && lon < east;
        }
    }
    let raw = classify(&px, &inside, options.rules);
    let general = mmu(&majority(&raw, MAJORITY), MMU_PX, &[WADI]);
    let mut stats = GroundStats::default();
    let mut core = vec![0u8; (side * side) as usize];
    let h = options.halo;
    for y in 0..side as usize {
        for x in 0..side as usize {
            let i = (y + h) * n + x + h;
            let g = general.v[i];
            core[y * side as usize + x] = g;
            if inside[i] {
                stats.raw[usize::from(raw.v[i])] += 1;
                stats.generalised[usize::from(g)] += 1;
                stats.no_radar += u64::from(px.v[i].coh.is_nan());
                stats.no_imagery += u64::from(px.v[i].albedo.is_nan());
            }
        }
    }
    Ok(BlockOut { core, stats })
}

/// Compress one class tile into its zstd frame, with the checksum the
/// reader requires.
pub fn compress_classes(cells: &[u8], level: i32) -> Res<Vec<u8>> {
    let mut c = zstd::bulk::Compressor::new(level)?;
    c.include_checksum(true)?;
    c.include_contentsize(true)?;
    Ok(c.compress(cells)?)
}

/// The z9 blocks meeting the box, rows north to south.
pub fn blocks_of(bbox: [f64; 4]) -> Vec<(u32, u32)> {
    let [west, south, east, north] = bbox;
    let (x0, x1) = (tile_x(west, BLOCK_ZOOM), tile_x(east, BLOCK_ZOOM));
    let (y0, y1) = (tile_y(north, BLOCK_ZOOM), tile_y(south, BLOCK_ZOOM));
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            out.push((x, y));
        }
    }
    out
}

/// Cut the ground pack of `options.bbox` into `out`.
pub fn cut_ground(
    heights: &Heights,
    kell: &Kell,
    geomad: &Geomad,
    options: &GroundOptions,
    out: &mut dyn Write,
) -> Res<GroundStats> {
    let [west, south, east, north] = options.bbox;
    let blocks = blocks_of(options.bbox);
    let e7 = |v: f64| (v * 1e7).round() as i32;
    let directory = write_directory_of(
        PackKind::Ground,
        &DirectoryInfo {
            tile_zoom: TILE_ZOOM,
            block_zoom: BLOCK_ZOOM,
            bounds: [e7(west), e7(south), e7(east), e7(north)],
            attribution: sources::ATTRIBUTION,
            notice: sources::NOTICE,
        },
        &blocks,
    )
    .map_err(|e| format!("directory: {e:?}"))?;
    out.write_all(&directory)?;
    let mut stats = GroundStats {
        bytes: directory.len() as u64,
        ..GroundStats::default()
    };
    let threads = options.threads.max(1);
    let per = 1usize << (TILE_ZOOM - BLOCK_ZOOM);
    let side = TILE_EDGE * per;
    for chunk in blocks.chunks(threads) {
        let results: Vec<Res<BlockOut>> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|&(bx, by)| {
                    s.spawn(move || block_classes(bx, by, heights, kell, geomad, options))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("a block worker died".into()))
                })
                .collect()
        });
        for (&(bx, by), r) in chunk.iter().zip(results) {
            let b = r.map_err(|e| format!("block {bx},{by}: {e}"))?;
            eprintln!(
                "ground: block {bx},{by} done ({} of {})",
                stats.blocks + 1,
                blocks.len()
            );
            let mut frames = Vec::with_capacity(per * per);
            for ty in 0..per {
                for tx in 0..per {
                    let mut t = vec![0u8; TILE_EDGE * TILE_EDGE];
                    for r in 0..TILE_EDGE {
                        let src = (ty * TILE_EDGE + r) * side + tx * TILE_EDGE;
                        t[r * TILE_EDGE..(r + 1) * TILE_EDGE]
                            .copy_from_slice(&b.core[src..src + TILE_EDGE]);
                    }
                    frames.push(compress_classes(&t, options.level)?);
                }
            }
            let piece = write_block_of(PackKind::Ground, TILE_ZOOM, BLOCK_ZOOM, bx, by, &frames)
                .map_err(|e| format!("block: {e:?}"))?;
            out.write_all(&piece)?;
            stats.blocks += 1;
            stats.tiles += frames.len();
            stats.bytes += piece.len() as u64;
            for k in 0..10 {
                stats.raw[k] += b.stats.raw[k];
                stats.generalised[k] += b.stats.generalised[k];
            }
            stats.no_radar += b.stats.no_radar;
            stats.no_imagery += b.stats.no_imagery;
        }
        // The walk is south of every block row above the chunk's first.
        if let Some(&(_, by)) = chunk.first() {
            let top = lat_at(u64::from(by) * tile_units(BLOCK_ZOOM));
            // Keep a block's height of reach for the next windows' halo.
            kell.drop_north_of(top + 1.0);
            geomad.drop_north_of(top + 1.0);
            heights.drop_north_of((by * 8).saturating_sub(4));
        }
    }
    stats.radar_bytes = *kell.bytes.lock().map_err(|_| "lock")?;
    stats.imagery_bytes = *geomad.bytes.lock().map_err(|_| "lock")?;
    Ok(stats)
}
