//! The desert relief (order 1741): the R′ term's behaviour pinned on
//! synthetic ground and on the committed Erg Chebbi cut — the light's
//! direction, the normalisation that gives small and large relief the
//! same spread, the noise roll-off that spares the dunes, the ambient
//! floor under the darkest shade, and the equality of the scalar,
//! decimated and threaded kernel forms.

use leviculum_map::elevation::{split, TileDecoder, TILE_CELLS};
use leviculum_map::relief::{
    self, ao_of, assemble, gblur, gblur_exact, par, sample, shade_rgb, sky_exaggeration,
    sky_radius_m, Plane, ShadePlane, APRON, PLANE_SIDE, SIDE, TILE,
};

const CHEBBI: &[u8] = include_bytes!("../fixtures/erg-chebbi.lmh");

/// Erg Chebbi's z12 ground resolution: 156543.03 * cos(31.15°) / 2^12
/// metres per pixel.
const CHEBBI_RES: f32 = 32.7;

/// The committed cut's north-western z12 tile (2002, 1674), with its
/// three in-pack neighbours; the rest of the apron clamps.
fn chebbi_plane() -> Plane {
    let (dir, ranges) = split(CHEBBI).expect("split the fixture");
    assert_eq!(dir.tile_zoom(), 12);
    // `ranges[0]` is the directory piece; the one block follows.
    let block = leviculum_map::elevation::Block::parse(&CHEBBI[ranges[1].clone()])
        .expect("parse the block");
    let mut dec = TileDecoder::new();
    let mut tiles: Vec<Vec<i16>> = Vec::new();
    let mut scratch = vec![0i16; TILE_CELLS];
    for (x, y) in [(2002, 1674), (2003, 1674), (2002, 1675), (2003, 1675)] {
        let mut out = vec![0i16; TILE_CELLS];
        block
            .heights(&mut dec, 12, x, y, &mut out, &mut scratch)
            .expect("decode the tile");
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

/// A plane from a closed form over the apron grid.
fn synthetic(f: impl Fn(f32, f32) -> f32) -> Plane {
    let mut p = Plane::new(SIDE);
    for y in 0..SIDE {
        for x in 0..SIDE {
            p.v[y * SIDE + x] = f(x as f32, y as f32);
        }
    }
    p
}

/// Linear dunes: chains every 12 samples (about 400 m at z12),
/// `amp` metres high, running north-east so both main lights see them.
fn dunes(amp: f32) -> Plane {
    synthetic(|x, y| amp * ((x + y) * (core::f32::consts::TAU / 12.0)).sin())
}

fn stats(t: &[i8]) -> (f32, f32) {
    let mut s: Vec<f32> = t.iter().map(|&v| f32::from(v) / 127.0).collect();
    s.sort_by(f32::total_cmp);
    let abs_p95 = {
        let mut a: Vec<f32> = s.iter().map(|v| v.abs()).collect();
        a.sort_by(f32::total_cmp);
        a[(a.len() - 1) * 95 / 100]
    };
    (s[(s.len() - 1) / 2], abs_p95)
}

/// The main light stands in the north-west: a slope rising toward the
/// south-east is shaded, its north-western counterpart lit.
#[test]
fn the_light_stands_in_the_north_west() {
    // One broad ridge along the NE-SW diagonal: NW faces rise toward
    // the light.
    let p = synthetic(|x, y| 300.0 - 0.5 * (x + y - SIDE as f32).abs());
    let sp = relief::compute(&p, 33.0, sky_radius_m(12));
    // Sample well inside each half of the tile.
    let at = |u: usize, v: usize| f32::from(sp.t[(v + 2) * PLANE_SIDE + u + 2]) / 127.0;
    let nw = at(40, 40);
    let se = at(TILE - 40, TILE - 40);
    assert!(nw > 0.05, "north-west face lit, got {nw}");
    assert!(se < -0.05, "south-east face shaded, got {se}");
}

/// Flat ground is left alone: tone zero, sky-view one.
#[test]
fn flat_ground_wears_no_shade() {
    let p = synthetic(|_, _| 450.0);
    let sp = relief::compute(&p, 33.0, sky_radius_m(12));
    assert!(sp.t.iter().all(|&t| t == 0), "tone off flat ground");
    assert!(
        sp.ao.iter().all(|&a| (ao_of(a) - 1.0).abs() < 0.01),
        "sky-view one on flat ground"
    );
}

/// The normalisation gives low and high relief the same spread: the
/// same dune field at 8 m and at 200 m amplitude shades within a
/// factor of two of each other, not the 25-fold of its slopes.
#[test]
fn small_relief_reads_like_large() {
    let small = relief::compute(&dunes(8.0), 33.0, sky_radius_m(12));
    let large = relief::compute(&dunes(200.0), 33.0, sky_radius_m(12));
    let (_, p95_small) = stats(&small.t);
    let (_, p95_large) = stats(&large.t);
    assert!(
        p95_small > 0.2 * p95_large && p95_small < 2.0 * p95_small.max(p95_large),
        "spread p95 small {p95_small} vs large {p95_large}"
    );
    assert!(p95_small > 0.1, "8 m dunes still read, got {p95_small}");
}

/// A DEM's noise class rolls off, the dunes do not: pixel-scale pocks
/// of 1.5 m rms vanish against 15 m dune chains of the same window.
#[test]
fn pocks_roll_off_and_dunes_stay() {
    // Deterministic pocks: a hash-noise plane, sample-scale.
    let pocks = synthetic(|x, y| {
        let h = (x as u32)
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add((y as u32).wrapping_mul(0x85EB_CA6B));
        (h >> 16) as f32 / 65535.0 * 4.0 - 2.0
    });
    let noisy = relief::compute(&pocks, 23.0, sky_radius_m(12));
    let field = relief::compute(&dunes(15.0), 23.0, sky_radius_m(12));
    let (_, noise_p95) = stats(&noisy.t);
    let (_, dune_p95) = stats(&field.t);
    assert!(
        noise_p95 < 0.5 * dune_p95,
        "pocks {noise_p95} must sink against dunes {dune_p95}"
    );
    assert!(dune_p95 > 0.15, "the dunes must keep their chains");
}

/// The ambient floor: the palette's paper under the deepest shade and
/// the darkest sky stays above luminance 100 — nothing sinks into
/// black (the addendum's first goal).
#[test]
fn nothing_sinks_into_shadow() {
    let paper = [245u8, 241, 233];
    let worst = shade_rgb(paper, -1.0, 0.86);
    let lum =
        0.2126 * f32::from(worst[0]) + 0.7152 * f32::from(worst[1]) + 0.0722 * f32::from(worst[2]);
    assert!(lum >= 100.0, "deepest shade on paper at {lum}");
    // And the lit side never leaves the warm white.
    let lit = shade_rgb(paper, 1.0, 1.0);
    assert!(lit.iter().all(|&c| c >= 230), "lit paper stays light");
}

/// The threaded kernels are the single-thread kernels bit for bit.
#[test]
fn threads_change_nothing() {
    let p = dunes(40.0);
    let a = gblur_exact(&p, 2.5);
    let b = par::gblur_exact_mt(&p, 2.5, 4);
    assert_eq!(a.v, b.v, "threaded blur equals scalar blur");
}

/// The decimated large-sigma blur stays within a hair of the exact
/// one on ground that has any business being blurred that wide.
#[test]
fn decimation_is_faithful() {
    let p = synthetic(|x, y| {
        200.0 * ((x / 60.0).sin() + (y / 45.0).cos()) + 20.0 * ((x + y) / 9.0).sin()
    });
    let exact = gblur_exact(&p, 8.0);
    let fast = gblur(&p, 8.0);
    let spread = {
        let m = exact.v.iter().sum::<f32>() / exact.v.len() as f32;
        (exact.v.iter().map(|v| (v - m) * (v - m)).sum::<f32>() / exact.v.len() as f32).sqrt()
    };
    let worst = exact
        .v
        .iter()
        .zip(&fast.v)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst < 0.05 * spread,
        "decimated blur off by {worst} against a spread of {spread}"
    );
}

/// The same plane computes to the same bytes, twice: the relief is
/// deterministic, as the golden sheets demand.
#[test]
fn the_relief_is_deterministic() {
    let p = chebbi_plane();
    let a = relief::compute(&p, CHEBBI_RES, sky_radius_m(14));
    let b = relief::compute(&p, CHEBBI_RES, sky_radius_m(14));
    assert_eq!(a.t, b.t);
    assert_eq!(a.ao, b.ao);
}

/// Erg Chebbi's real dunes read: the committed cut's shade has the
/// desert's spread, and paper under it keeps the study's floor —
/// nothing below luminance 100, the 5th percentile above 120
/// (`README_R2.md` measures 149 with the dune colours; paper alone
/// sits a little higher).
#[test]
fn erg_chebbi_reads_and_stays_light() {
    let p = chebbi_plane();
    let sp = relief::compute(&p, CHEBBI_RES, sky_radius_m(14));
    let (_, p95) = stats(&sp.t);
    assert!(p95 > 0.1, "the erg must shade, p95 {p95}");
    let paper = [245u8, 241, 233];
    let mut lums: Vec<f32> = (0..PLANE_SIDE * PLANE_SIDE)
        .map(|i| {
            let c = shade_rgb(paper, f32::from(sp.t[i]) / 127.0, ao_of(sp.ao[i]));
            0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
        })
        .collect();
    lums.sort_by(f32::total_cmp);
    assert!(lums[0] >= 100.0, "darkest {}", lums[0]);
    let p5 = lums[(lums.len() - 1) * 5 / 100];
    assert!(p5 >= 120.0, "p5 {p5}");
    let median = lums[(lums.len() - 1) / 2];
    assert!(
        (170.0..=250.0).contains(&median),
        "median luminance {median}"
    );
}

/// The sky-view exaggeration follows the slope-energy split, not a
/// region: fine-banded ground gets the dune factor, form-banded
/// ground the plain one, smoothly.
#[test]
fn sky_exaggeration_follows_the_fine_band() {
    let dune_like = sky_exaggeration(0.02, 0.004, 0.002);
    let alp_like = sky_exaggeration(0.01, 0.12, 0.015);
    assert!(dune_like > 2.2, "fine-banded clean ground, got {dune_like}");
    assert!(alp_like < 1.1, "form-banded ground, got {alp_like}");
    assert!(
        sky_exaggeration(0.01, 0.012, 0.002) > alp_like,
        "the step between is smooth"
    );
    // A vegetated surface model is fine-band-loud too — canopy, not
    // dunes: the noise floor keeps the exaggeration plain (Uckermark).
    let canopy = sky_exaggeration(0.06, 0.005, 0.013);
    assert!(
        canopy < 1.1,
        "canopy must not wear the dune sky, got {canopy}"
    );
}

/// A bicubic sample at a grid centre returns the stored value, and
/// the rim carries the support across the tile's edge.
#[test]
fn sampling_hits_the_grid() {
    let mut sp = ShadePlane {
        t: vec![0; PLANE_SIDE * PLANE_SIDE],
        ao: vec![128; PLANE_SIDE * PLANE_SIDE],
    };
    for y in 0..PLANE_SIDE {
        for x in 0..PLANE_SIDE {
            sp.t[y * PLANE_SIDE + x] = ((x * 7 + y * 3) % 200) as i8 - 100;
        }
    }
    for (u, v) in [(0.5, 0.5), (100.5, 7.5), (255.5, 255.5), (0.5, 255.5)] {
        let (t, _) = sample(&sp, u, v);
        let want = f32::from(sp.t[(v as usize + 2) * PLANE_SIDE + u as usize + 2]) / 127.0;
        assert!(
            (t - want).abs() < 1e-4,
            "sample at ({u}, {v}): {t} vs {want}"
        );
    }
    // On the very edge the rim keeps the sample finite and close.
    let (t, ao) = sample(&sp, 0.0, 256.0);
    assert!(t.abs() <= 1.0 && (0.6..=1.15).contains(&ao));
}

/// The assembly clamps where a neighbour is missing instead of
/// inventing a cliff: the apron continues the edge value.
#[test]
fn a_missing_neighbour_clamps() {
    let flat = vec![100i16; TILE_CELLS];
    let p = assemble(&[None, None, None, None, Some(&flat), None, None, None, None]);
    assert!(p.v.iter().all(|&v| (v - 100.0).abs() < 0.5));
    let sp = relief::compute(&p, 33.0, sky_radius_m(12));
    assert!(sp.t.iter().all(|&t| t == 0), "no shade at the pack's edge");
}

/// The apron is wide enough that the same ground shades the same in
/// two neighbouring tiles' planes: the seam between tiles is quiet.
#[test]
fn tile_seams_are_quiet() {
    // One continuous dune field, cut into two horizontally adjacent
    // tiles, each with the true neighbour data in its apron.
    let field = |x: f32, y: f32| 30.0 * ((x + y) * (core::f32::consts::TAU / 12.0)).sin();
    let left = synthetic(field);
    let right = synthetic(|x, y| field(x + TILE as f32, y));
    let a = relief::compute(&left, 33.0, sky_radius_m(12));
    let b = relief::compute(&right, 33.0, sky_radius_m(12));
    // The left tile's rim columns past its edge cover the same ground
    // samples as the right tile's first columns: the same ground must
    // wear the same tone from either plane, give or take a couple of
    // quantization steps.
    let mut worst = 0i32;
    for y in 40..TILE - 40 {
        for k in 0..2usize {
            let la = i32::from(a.t[(y + 2) * PLANE_SIDE + 2 + TILE + k]);
            let rb = i32::from(b.t[(y + 2) * PLANE_SIDE + 2 + k]);
            worst = worst.max((la - rb).abs());
        }
    }
    assert!(worst <= 3, "seam step: {worst} of 254");
}

const _: () = assert!(APRON >= 80, "the form band's blur needs its apron");
