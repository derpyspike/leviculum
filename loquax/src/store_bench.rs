//! A measuring bench over a real maps directory (order 1943): where the
//! seconds of a mid-zoom frame go. Lew's store of 2026-11-16 — 96 files,
//! 548 MB, 94 heavily overlapping `auto-*.lmr` regions — froze the window
//! for 11 to 20 s at zoom 10 while the small walk fixtures stayed green,
//! so the bench runs against any store a directory holds, not a fixture.
//!
//! Gated on `LOQUAX_STORE_BENCH=<maps dir>`: without it the test returns
//! at once, so no gate pays for it. Run it by hand:
//!
//! ```text
//! LOQUAX_STORE_BENCH=/path/to/maps cargo test --release -p loquax \
//!     --features picture store_bench -- --nocapture
//! ```
//!
//! One `STORE_BENCH` line per screen size and zoom: the blend's `t` at
//! that size (`t < 1` is the curved path, `tiles.rs` `compose_into`,
//! which draws the frame whole and caches nothing), the regions
//! [`leviculum_map::choice::drawn`] returns for the view, the cold
//! compose, the same frame again warm, one pan frame of half a screen
//! with the window's `moving` budget, and the cache's bytes after. A
//! `STORE_BENCH_FLAT` line draws the same view flat (Mercator, blend
//! off) whole: the warp's own share against the feature volume's.

use std::time::Instant;

use leviculum_map::blend::Blend;
use leviculum_map::choice::drawn;
use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::region::{Header, Region};
use leviculum_map::viewport::{ExactView, Viewport, MILLI};
use leviculum_mapfill::style::Style;

use crate::picture::{Maps, Painter};
use crate::tiles::{Frame, TileCache};

/// The freeze's view (gui.log 2026-11-16 13:23 UTC): north Germany.
const AT: (f64, f64) = (53.32745, 9.07579);

/// The screen sizes measured: the load walk's, a 1080p window's content,
/// a maximized window on a 1440p display, and one on a 4K display — the
/// size classes where zoom 10 moves into the blend window.
const SIZES: [(u32, u32); 4] = [(1120, 700), (1920, 1057), (2560, 1377), (3840, 2097)];

/// Every readable region in `dir`, by file name, as [`drawn`] asks.
fn headers_in(dir: &std::path::Path) -> Vec<(String, Header)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".lmr") {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        if let Ok(region) = Region::open(file) {
            found.push((name, *region.header()));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// `centre` shifted `px` screen pixels east at zoom `z`.
fn east_of(centre: World, px: u32, z: u8) -> World {
    let units = u64::from(px) << (24 - u32::from(z.min(24)));
    World {
        x: centre.x.wrapping_add(units as u32),
        y: centre.y,
    }
}

#[test]
fn store_bench_names_where_the_seconds_go() {
    let Some(dir) = std::env::var_os("LOQUAX_STORE_BENCH") else {
        return;
    };
    // Which sections run: "macro", "flat", "regions", or any list of
    // them; unset runs them all.
    let parts = std::env::var("LOQUAX_STORE_BENCH_PART").unwrap_or_default();
    let wants = |p: &str| parts.is_empty() || parts.contains(p);
    let dir = std::path::PathBuf::from(dir);
    let regions = headers_in(&dir);
    println!(
        "STORE_BENCH_DIR regions={} dir={}",
        regions.len(),
        dir.display()
    );
    let centre = project(LatLon::from_degrees(AT.0, AT.1));
    if wants("flat") {
        flat_variants(&dir, centre);
    }
    if wants("relief") {
        relief_rows(&dir, centre);
    }
    if wants("labels") {
        labels_cost(&dir, centre);
    }
    if wants("outline") {
        outline_only(centre);
    }
    if wants("shot") {
        shot(&dir, centre);
    }
    if wants("regions") {
        per_region(&dir, &regions, centre);
    }
    if !wants("macro") {
        return;
    }
    for (sw, sh) in SIZES {
        // The window draws with margins of an eighth of the screen on
        // each side (`gui::MARGIN_PART`), the blend's `t` at the
        // screen's own size (`mapframe`, 1796).
        let (fw, fh) = (sw + 2 * (sw / 8), sh + 2 * (sh / 8));
        for z in 8u8..=12 {
            let t = Blend::new(centre, f64::from(z), sw, sh).t();
            let view = Viewport::new(centre, z, fw, fh);
            let order = drawn(regions.iter().map(|(n, h)| (n.as_str(), h)), &view);
            let maps = Maps::new(Some(dir.clone()));
            let mut cache = TileCache::new(maps, 96 << 20);
            cache.set_world_blend(true);
            cache.set_screen(Some((sw, sh)));
            let exact = ExactView::new(centre, u32::from(z) * MILLI, fw, fh);
            let rest = |view| Frame {
                view,
                zoom: z,
                style: Style::SCREEN,
                scale: 1.0,
                render_limit: None,
                fallback: None,
                moving: false,
                asked: None,
                smooth: true,
                resmooth: false,
            };
            let cold = Instant::now();
            let first = cache.compose(&rest(exact));
            let cold = cold.elapsed();
            let warm = Instant::now();
            let again = cache.compose(&rest(exact));
            let warm = warm.elapsed();
            // Half a screen east, as a pan's frame asks: moving, the
            // machine budget of 16 missing tiles, the rest stood in.
            let shifted = ExactView::new(east_of(centre, sw / 2, z), u32::from(z) * MILLI, fw, fh);
            let pan_frame = Frame {
                render_limit: Some(16),
                fallback: Some(z),
                moving: true,
                ..rest(shifted)
            };
            let pan = Instant::now();
            let moved = cache.compose(&pan_frame);
            let pan = pan.elapsed();
            println!(
                "STORE_BENCH size={sw}x{sh} zoom={z} t={t:.3} regions={} \
                 cold_ms={:.0} warm_ms={:.0} pan_ms={:.0} missing={}/{}/{} \
                 cache_bytes={} rendered={}",
                order.len(),
                cold.as_secs_f64() * 1e3,
                warm.as_secs_f64() * 1e3,
                pan.as_secs_f64() * 1e3,
                first.missing,
                again.missing,
                moved.missing,
                cache.bytes(),
                cache.stats.rendered,
            );
        }
    }
}

/// The flat whole-view draw at zoom 9 and 10 with the screen style's
/// parts turned off one by one: which part carries the 300 s the first
/// run measured at zoom 9.
fn flat_variants(dir: &std::path::Path, centre: World) {
    let variants: [(&str, Style); 4] = [
        ("screen", Style::SCREEN),
        (
            "no_relief",
            Style {
                relief: false,
                ..Style::SCREEN
            },
        ),
        (
            "no_ground",
            Style {
                ground: None,
                ..Style::SCREEN
            },
        ),
        (
            "plain",
            Style {
                relief: false,
                ground: None,
                ..Style::SCREEN
            },
        ),
    ];
    let maps = Maps::new(Some(dir.to_path_buf()));
    let mut painter = Painter::new(maps);
    // The walk's size by default: a quarter of the big frame's pixels,
    // so a slow variant shows itself in tens of seconds, not minutes;
    // `LOQUAX_STORE_BENCH_BIG=1` measures the big frame, and
    // `LOQUAX_STORE_BENCH_VARIANT=<name>` one variant alone.
    let (sw, sh) = match std::env::var_os("LOQUAX_STORE_BENCH_BIG").is_some() {
        true => SIZES[2],
        false => SIZES[0],
    };
    let only = std::env::var("LOQUAX_STORE_BENCH_VARIANT").unwrap_or_default();
    let (fw, fh) = (sw + 2 * (sw / 8), sh + 2 * (sh / 8));
    for z in [9u8, 10] {
        for (name, style) in variants {
            if !only.is_empty() && name != only {
                continue;
            }
            let view = Viewport::new(centre, z, fw, fh);
            let flat = Instant::now();
            let out = painter.draw_rgba(&view, Ink::Colour, style, 100, true);
            let flat = flat.elapsed();
            println!(
                "STORE_BENCH_FLAT zoom={z} style={name} whole_ms={:.0} pending={} unreadable={}",
                flat.as_secs_f64() * 1e3,
                out.pending,
                out.unreadable,
            );
        }
    }
}

/// One still of the freeze's view per zoom, written as PPM beside the
/// store (`shot-z<zoom>.ppm`): what Lew's zoom 10 actually draws, for
/// the eyes the render walk turns on every visual change.
fn shot(dir: &std::path::Path, centre: World) {
    let maps = Maps::new(Some(dir.to_path_buf()));
    let mut cache = TileCache::new(maps, 96 << 20);
    cache.set_world_blend(true);
    let (sw, sh) = SIZES[2];
    cache.set_screen(Some((sw, sh)));
    for z in [9u8, 10, 11] {
        let exact = ExactView::new(centre, u32::from(z) * MILLI, sw, sh);
        let frame = Frame {
            view: exact,
            zoom: z,
            style: Style::SCREEN,
            scale: 1.0,
            render_limit: None,
            fallback: None,
            moving: false,
            asked: None,
            smooth: true,
            resmooth: false,
        };
        let composed = cache.compose(&frame);
        let mut out = alloc::format!(
            "P6
{sw} {sh}
255
"
        )
        .into_bytes();
        for px in composed.rgba.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
        let path = dir.join(alloc::format!("shot-z{z}.ppm"));
        std::fs::write(&path, out).expect("the shot");
        println!("STORE_BENCH_SHOT zoom={z} file={}", path.display());
    }
}

/// The label walk a frame at rest runs ([`Painter::labels`]) over the
/// big frame: what the names pass adds on top of the ground.
fn labels_cost(dir: &std::path::Path, centre: World) {
    let maps = Maps::new(Some(dir.to_path_buf()));
    let mut painter = Painter::new(maps);
    let (sw, sh) = SIZES[2];
    let (fw, fh) = (sw + 2 * (sw / 8), sh + 2 * (sh / 8));
    for z in [9u8, 10, 11] {
        let view = Viewport::new(centre, z, fw, fh);
        let start = Instant::now();
        let found = painter.labels(&view, true);
        println!(
            "STORE_BENCH_LABELS zoom={z} ms={:.0} labels={}",
            start.elapsed().as_secs_f64() * 1e3,
            found.len(),
        );
    }
}

/// The curved frame over an empty store: the outline's own share of a
/// blended compose, no region drawn.
fn outline_only(centre: World) {
    let scratch = std::env::temp_dir().join(format!("store-bench-empty-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch");
    for (sw, sh) in [SIZES[2], SIZES[3]] {
        let (fw, fh) = (sw + 2 * (sw / 8), sh + 2 * (sh / 8));
        for z in [9u8, 10] {
            let maps = Maps::new(Some(scratch.clone()));
            let mut cache = TileCache::new(maps, 96 << 20);
            cache.set_world_blend(true);
            cache.set_screen(Some((sw, sh)));
            let exact = ExactView::new(centre, u32::from(z) * MILLI, fw, fh);
            let frame = Frame {
                view: exact,
                zoom: z,
                style: Style::SCREEN,
                scale: 1.0,
                render_limit: None,
                fallback: None,
                moving: false,
                asked: None,
                smooth: true,
                resmooth: false,
            };
            let start = Instant::now();
            let _ = cache.compose(&frame);
            println!(
                "STORE_BENCH_OUTLINE size={sw}x{sh} zoom={z} ms={:.0}",
                start.elapsed().as_secs_f64() * 1e3,
            );
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

/// The relief sampler alone, row by row over the big frame at zoom 9
/// and 10, as every region's pass drives it: the planes computed, the
/// pack bytes read, and the time — with a running line every 256 rows,
/// so a thrashing cache shows its shape.
fn relief_rows(dir: &std::path::Path, centre: World) {
    use leviculum_map::relief::Shade;
    let (sw, sh) = SIZES[2];
    let (fw, fh) = (sw + 2 * (sw / 8), sh + 2 * (sh / 8));
    for z in [9u8, 10] {
        let store = crate::relief::ReliefStore::default();
        store.refresh(Some(dir));
        let view = Viewport::new(centre, z, fw, fh);
        let Some(mut sampler) = store.sampler(&view) else {
            println!("STORE_BENCH_RELIEF zoom={z} no_sampler");
            continue;
        };
        let mut t = vec![0.0f32; fw as usize];
        let mut ao = vec![1.0f32; fw as usize];
        let start = Instant::now();
        for y in 0..fh as i32 {
            sampler.row(0, y, &mut t, &mut ao);
            if y % 256 == 255 {
                println!(
                    "STORE_BENCH_RELIEF zoom={z} rows={} ms={:.0} planes={} bytes={}",
                    y + 1,
                    start.elapsed().as_secs_f64() * 1e3,
                    store.planes_computed(),
                    store.bytes_read(),
                );
            }
        }
        println!(
            "STORE_BENCH_RELIEF zoom={z} rows={fh} ms={:.0} planes={} bytes={} done",
            start.elapsed().as_secs_f64() * 1e3,
            store.planes_computed(),
            store.bytes_read(),
        );
    }
}

/// Each region of the zoom 9 view drawn alone, flat, slowest style:
/// which file carries the seconds. A temp directory with one hard link
/// a run, so the painter sees a store of one region.
fn per_region(dir: &std::path::Path, regions: &[(String, Header)], centre: World) {
    let (sw, sh) = SIZES[2];
    let (fw, fh) = (sw + 2 * (sw / 8), sh + 2 * (sh / 8));
    let z = 9u8;
    let view = Viewport::new(centre, z, fw, fh);
    let order = drawn(regions.iter().map(|(n, h)| (n.as_str(), h)), &view);
    let scratch = std::env::temp_dir().join(format!("store-bench-one-{}", std::process::id()));
    for &i in &order {
        let (name, _) = &regions[i];
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch");
        std::fs::hard_link(dir.join(name), scratch.join(name))
            .or_else(|_| std::fs::copy(dir.join(name), scratch.join(name)).map(|_| ()))
            .expect("link");
        let maps = Maps::new(Some(scratch.clone()));
        let mut painter = Painter::new(maps);
        let flat = Instant::now();
        let out = painter.draw_rgba(&view, Ink::Colour, Style::SCREEN, 100, true);
        let flat = flat.elapsed();
        println!(
            "STORE_BENCH_REGION zoom={z} name={name} whole_ms={:.0} pending={}",
            flat.as_secs_f64() * 1e3,
            out.pending,
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
