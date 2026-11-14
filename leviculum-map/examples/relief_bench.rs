//! The relief kernels timed (order 1741, the vectorisation addendum):
//! one line per kernel and thread count, median of the runs, on the
//! committed Erg Chebbi cut's aproned plane — real ground, so the
//! branches in the gain loop see real amplitudes. The workspace
//! measures with release examples, not a bench harness
//! (`leviculum-mapfill/measure`, `map_measure.rs`); this follows.
//!
//!    relief_bench [runs]
//!
//! Platforms are the caller's: run once as built (x86-64 baseline),
//! once with RUSTFLAGS=-C target-cpu=x86-64-v3, and on an aarch64
//! host when one is at hand.

use std::time::Instant;

use leviculum_map::elevation::{split, Block, TileDecoder, TILE_CELLS};
use leviculum_map::relief::{self, assemble, gblur, gblur_exact, par, sobel, Plane};

const CHEBBI: &[u8] = include_bytes!("../fixtures/erg-chebbi.lmh");

fn plane() -> Plane {
    let (_, ranges) = split(CHEBBI).expect("split");
    let block = Block::parse(&CHEBBI[ranges[1].clone()]).expect("block");
    let mut dec = TileDecoder::new();
    let mut scratch = vec![0i16; TILE_CELLS];
    let mut tiles: Vec<Vec<i16>> = Vec::new();
    for (x, y) in [(2002, 1674), (2003, 1674), (2002, 1675), (2003, 1675)] {
        let mut out = vec![0i16; TILE_CELLS];
        block
            .heights(&mut dec, 12, x, y, &mut out, &mut scratch)
            .expect("decode");
        tiles.push(out);
    }
    assemble(&[
        None,
        None,
        None,
        None,
        Some(&tiles[0]),
        Some(&tiles[1]),
        None,
        Some(&tiles[2]),
        Some(&tiles[3]),
    ])
}

fn bench(name: &str, threads: usize, runs: usize, mut f: impl FnMut()) {
    let mut ms: Vec<f64> = (0..runs)
        .map(|_| {
            let t0 = Instant::now();
            f();
            t0.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    println!(
        "BENCH kernel={name} threads={threads} ms={:.2}",
        ms[(ms.len() - 1) / 2]
    );
}

fn main() {
    let runs: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(9);
    let p = plane();
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    bench("gblur_s2.5", 1, runs, || {
        std::hint::black_box(gblur(&p, 2.5));
    });
    bench("gblur_s2.5", cores, runs, || {
        std::hint::black_box(par::gblur_exact_mt(&p, 2.5, cores));
    });
    bench("gblur_s20_decimated", 1, runs, || {
        std::hint::black_box(gblur(&p, 20.0));
    });
    bench("gblur_s20_scalar", 1, runs, || {
        std::hint::black_box(gblur_exact(&p, 20.0));
    });
    bench("gblur_s20_scalar", cores, runs, || {
        std::hint::black_box(par::gblur_exact_mt(&p, 20.0, cores));
    });
    bench("sobel", 1, runs, || {
        std::hint::black_box(sobel(&p, 32.7, 1.3));
    });
    let (gx, gy) = sobel(&p, 32.7, 1.3);
    bench("multilight", 1, runs, || {
        std::hint::black_box(relief::multilight(&gx, &gy));
    });
    let b = relief::multilight(&gx, &gy);
    bench("local_rms_s6", 1, runs, || {
        std::hint::black_box(relief::local_rms(&b, 6.0));
    });
    bench("local_rms_s60", 1, runs, || {
        std::hint::black_box(relief::local_rms(&b, 60.0));
    });
    // The per-tile ground (order 1851): measured once per height tile
    // and cached by the store, so a plane costs `compute_in` and about
    // one `measure` on a cold screen.
    let centre: Vec<i16> = (0..relief::TILE)
        .flat_map(|y| {
            let row = (y + relief::APRON) * relief::SIDE + relief::APRON;
            p.v[row..row + relief::TILE]
                .iter()
                .map(|&v| v as i16)
                .collect::<Vec<_>>()
        })
        .collect();
    bench("measure_tile", 1, runs, || {
        std::hint::black_box(relief::measure(&centre, 32.7));
    });
    let ground = [relief::measure(&centre, 32.7); 9];
    bench("compute_in_tile", 1, runs, || {
        std::hint::black_box(relief::compute_in(&p, 32.7, 3000.0, &ground));
    });
    bench("compute_tile", 1, runs, || {
        std::hint::black_box(relief::compute(&p, 32.7, 3000.0));
    });
    // The product's multicore grain: independent tiles on scoped
    // threads, as a view draws them.
    bench("compute_in_tile", cores, runs, || {
        std::thread::scope(|s| {
            for _ in 0..cores {
                s.spawn(|| std::hint::black_box(relief::compute_in(&p, 32.7, 3000.0, &ground)));
            }
        });
    });
    println!("BENCH note=compute_in_tile at {cores} threads runs {cores} tiles at once; divide by the thread count for per-tile throughput");
}
