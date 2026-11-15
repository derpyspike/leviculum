//! The relief's height packs reaching the device (order 1863), measured
//! end to end on the loader the window and the phone share: a place in
//! the desert asked as the map asks it (`want_place`, 12 by 12 km at
//! z10..z15), the region cut from a PMTiles archive and its height pack
//! fetched from the catalogue beside it, both from a local range server
//! held to 200 kB/s; then the place drawn with the relief and without,
//! at two zooms in the phone's and the window's size, and the cost of one
//! 256 px tile with the relief and without.
//!
//! A measuring tool for the reviewer and the report, not a gate.
//!
//! ```sh
//! cargo run --release -p loquax --features mapload --example heights_walk -- SRV OUT
//! ```
//!
//! `SRV` holds `desert.pmtiles` (any PMTiles over the places),
//! `heights.tsv` and the packs it names (`leviculum-map-tool
//! heights-catalogue`); `OUT` gets the pictures and `heights_walk.tsv`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::viewport::Viewport;
use leviculum_mapcut::serve::Serve;
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::style::Style;
use loquax::mapload::{Loader, Source, Step, StepKind};
use loquax::picture::{Maps, Painter};
use loquax::ui::map::want_place;

/// The link the order names.
const RATE: u64 = 200_000;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(srv), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: heights_walk SRV OUT");
        std::process::exit(2);
    };
    let (srv, out) = (PathBuf::from(srv), PathBuf::from(out));
    std::fs::create_dir_all(&out).expect("out");
    let mut files = vec![];
    for entry in std::fs::read_dir(&srv).expect("srv") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        files.push((name, std::fs::read(&path).expect("read")));
    }
    let serve = Serve::start(files).expect("serve");
    serve.throttle(RATE, Duration::from_millis(50));
    let mut tsv = std::fs::File::create(out.join("heights_walk.tsv")).expect("tsv");
    writeln!(tsv, "place\twhat\tvalue").expect("tsv");
    for (place, lat, lon) in [("erg-chebbi", 31.12, -3.975), ("bani", 29.75, -6.85)] {
        let maps_dir = out.join(format!("maps-{place}"));
        let _ = std::fs::remove_dir_all(&maps_dir);
        std::fs::create_dir_all(&maps_dir).expect("maps");
        let maps = Maps::new(Some(maps_dir.clone()));
        let mut loader = Loader::new(
            maps.clone(),
            Source::Url(serve.url("desert.pmtiles")),
            500 << 20,
        )
        .with_heights(serve.url("heights.tsv"));
        let at = project(LatLon::from_degrees(lat, lon));
        let want = want_place(at, &[]).expect("a place to load");
        let started = Instant::now();
        let sent = serve.sent();
        let mut steps = loader.ask(want, 0);
        let deadline = started + Duration::from_secs(600);
        loop {
            for step in &steps {
                match step {
                    Step::Done {
                        landed, elapsed_ms, ..
                    } => {
                        println!(
                            "SMOKE {place} region bytes={} fetched={} elapsed_ms={elapsed_ms}",
                            landed.bytes, landed.fetched
                        );
                        writeln!(tsv, "{place}\tregion_bytes\t{}", landed.bytes).expect("tsv");
                        writeln!(tsv, "{place}\tregion_fetched\t{}", landed.fetched).expect("tsv");
                        writeln!(tsv, "{place}\tregion_ms\t{elapsed_ms}").expect("tsv");
                    }
                    Step::Heights {
                        landed, elapsed_ms, ..
                    } => {
                        let l = landed.as_ref().expect("the pack landed");
                        println!(
                            "SMOKE {place} heights packs={:?} fetched={} elapsed_ms={elapsed_ms}",
                            l.packs, l.fetched
                        );
                        writeln!(tsv, "{place}\tpack_fetched\t{}", l.fetched).expect("tsv");
                        writeln!(tsv, "{place}\tpack_ms\t{elapsed_ms}").expect("tsv");
                        writeln!(tsv, "{place}\tpacks\t{}", l.packs.len()).expect("tsv");
                    }
                    Step::Failed { detail, .. } => panic!("{place}: {detail}"),
                    _ => {}
                }
            }
            if steps.iter().any(|s| s.kind() == StepKind::Heights) {
                break;
            }
            assert!(Instant::now() < deadline, "{place}: nothing landed");
            std::thread::sleep(Duration::from_millis(20));
            steps = loader.poll(started.elapsed().as_millis() as u64);
        }
        println!(
            "SMOKE {place} wire_bytes={} total_ms={}",
            serve.sent() - sent,
            started.elapsed().as_millis()
        );
        draw_place(&maps, place, at, &out, &mut tsv);
    }
}

/// The place in the phone's and the window's size at z12 and z13, with
/// relief and without; then 256 px tiles' draw time both ways.
fn draw_place(
    maps: &Maps,
    place: &str,
    at: leviculum_map::mercator::World,
    out: &Path,
    tsv: &mut std::fs::File,
) {
    let mut painter = Painter::new(maps.clone());
    let off = Style {
        relief: false,
        ..Style::SCREEN
    };
    for (size, w, h) in [("phone", 1080u32, 2400u32), ("window", 1600, 1000)] {
        for zoom in [12u8, 13] {
            for (look, style) in [("relief", Style::SCREEN), ("flat", off)] {
                let view = Viewport::new(at, zoom, w, h);
                let drawn = painter.draw_with(&view, None, Ink::Colour, style, 100);
                let name = format!("{place}-{size}-z{zoom}-{look}.png");
                write_png(&out.join(&name), &drawn.fb, w, h);
                println!("SMOKE picture {name}");
            }
        }
    }
    // One tile of the phone's tile cache, 256 px, at z13 over the place:
    // the median of 15 draws after one warm-up, each way. The relief's
    // planes are cached across draws, so the first draw is the cold one.
    for (look, style) in [("relief", Style::SCREEN), ("flat", off)] {
        let mut fresh = Painter::new(maps.clone());
        let view = Viewport::new(at, 13, 256, 256);
        let t = Instant::now();
        let _ = fresh.draw_rgba(&view, Ink::Colour, style, 100, true);
        let cold = t.elapsed().as_secs_f64() * 1000.0;
        let mut ms: Vec<f64> = (0..15)
            .map(|_| {
                let t = Instant::now();
                let _ = fresh.draw_rgba(&view, Ink::Colour, style, 100, true);
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        println!(
            "SMOKE {place} tile256 {look} cold_ms={cold:.1} warm_median_ms={:.1}",
            ms[ms.len() / 2]
        );
        writeln!(tsv, "{place}\ttile256_{look}_cold_ms\t{cold:.1}").expect("tsv");
        writeln!(
            tsv,
            "{place}\ttile256_{look}_warm_ms\t{:.1}",
            ms[ms.len() / 2]
        )
        .expect("tsv");
    }
}

fn write_png(path: &Path, fb: &[u16], w: u32, h: u32) {
    let mut rgb = Vec::new();
    Frame::new(&mut fb.to_vec(), w, h)
        .expect("frame")
        .rgb888_into(&mut rgb);
    let file = std::fs::File::create(path).expect("png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("header")
        .write_image_data(&rgb)
        .expect("data");
}
