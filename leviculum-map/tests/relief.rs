//! The desert relief (order 1741): the R′ term's behaviour pinned on
//! synthetic ground and on the committed Erg Chebbi cut — the light's
//! direction, the normalisation that gives small and large relief the
//! same spread, the noise roll-off that spares the dunes, the ambient
//! floor under the darkest shade, and the equality of the scalar,
//! decimated and threaded kernel forms.

use leviculum_map::elevation::{split, TileDecoder, TILE_CELLS};
use leviculum_map::relief::{
    self, ao_of, assemble, fill_ground, gblur, gblur_exact, measure, sample, shade_rgb,
    sky_exaggeration, sky_radius_m, Ground, Grounds, Plane, ShadePlane, APRON, PLANE_SIDE, SIDE,
    TILE,
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
        p95_small > 0.5 * p95_large && p95_large < 2.0 * p95_small,
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
/// `par` is the `std` half of the module, so the test is too.
#[cfg(feature = "std")]
#[test]
fn threads_change_nothing() {
    let p = dunes(40.0);
    let a = gblur_exact(&p, 2.5);
    let b = relief::par::gblur_exact_mt(&p, 2.5, 4);
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
    // Half-way up the fine-share ramp (0.55 of the slope energy in the
    // fine band; it starts at 0.5 since the stone-desert measure).
    assert!(
        sky_exaggeration(0.012, 0.01, 0.002) > alp_like,
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
            sp.t[y * PLANE_SIDE + x] = (((x * 7 + y * 3) % 200) as i32 - 100) as i8;
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

/// The fixture's four z12 tiles, north-west, north-east, south-west,
/// south-east.
fn chebbi_tiles() -> Vec<Vec<i16>> {
    let (_, ranges) = split(CHEBBI).expect("split the fixture");
    let block = leviculum_map::elevation::Block::parse(&CHEBBI[ranges[1].clone()])
        .expect("parse the block");
    let mut dec = TileDecoder::new();
    let mut scratch = vec![0i16; TILE_CELLS];
    [(2002, 1674), (2003, 1674), (2002, 1675), (2003, 1675)]
        .iter()
        .map(|&(x, y)| {
            let mut out = vec![0i16; TILE_CELLS];
            block
                .heights(&mut dec, 12, x, y, &mut out, &mut scratch)
                .expect("decode the tile");
            out
        })
        .collect()
}

/// The plane and the neighbourhood's grounds of the tile whose 3 x 3
/// neighbourhood is `slots` (row-major, centre 4), as the product's
/// store builds them: heights by [`assemble`], each present tile's
/// ground measured on the tile alone, the missing ones filled.
fn plane_in(slots: [Option<&[i16]>; 9], res: f32) -> (Plane, Grounds) {
    let grounds: [Option<Ground>; 9] = core::array::from_fn(|i| slots[i].map(|t| measure(t, res)));
    (
        assemble(&slots),
        fill_ground(&grounds).expect("the centre is present"),
    )
}

/// The largest tone step across the shared columns of two horizontally
/// adjacent planes, rows 40..TILE-40.
fn seam_step(a: &ShadePlane, b: &ShadePlane) -> i32 {
    let mut worst = 0i32;
    for y in 40..TILE - 40 {
        for k in 0..2usize {
            let la = i32::from(a.t[(y + 2) * PLANE_SIDE + 2 + TILE + k]);
            let rb = i32::from(b.t[(y + 2) * PLANE_SIDE + 2 + k]);
            worst = worst.max((la - rb).abs());
        }
    }
    worst
}

/// The seam between two neighbouring tiles is quiet on real ground,
/// not only on a synthetic field (the audit of order 1741): the planes
/// of (2002, 1674) and (2003, 1674) of the committed Erg Chebbi cut,
/// each assembled with what the pack has of its neighbourhood, wear
/// the same tone on the same samples within 3 of 254. Before the
/// per-tile ground and the shared clamp it stepped by 6: each plane
/// measured its own noise floor and sky exaggeration, and each clamped
/// a missing corner to its own centre.
#[test]
fn audit_real_ground_seams_are_quiet() {
    let t = chebbi_tiles();
    let (nw, ne, sw, se) = (&t[0][..], &t[1][..], &t[2][..], &t[3][..]);
    let (left, lg) = plane_in(
        [
            None,
            None,
            None,
            None,
            Some(nw),
            Some(ne),
            None,
            Some(sw),
            Some(se),
        ],
        32.7,
    );
    let (right, rg) = plane_in(
        [
            None,
            None,
            None,
            Some(nw),
            Some(ne),
            None,
            Some(sw),
            Some(se),
            None,
        ],
        32.7,
    );
    let a = relief::compute_in(&left, 32.7, sky_radius_m(14), &lg);
    let b = relief::compute_in(&right, 32.7, sky_radius_m(14), &rg);
    let worst = seam_step(&a, &b);
    assert!(worst <= 3, "seam step on real ground: {worst} of 254");
}

// ---- real ground with full neighbourhoods, and the relief goldens ----

const CHEBBI_Z10: &[u8] = include_bytes!("../fixtures/erg-chebbi-z10.lmh");
const CHEGAGA_Z10: &[u8] = include_bytes!("../fixtures/erg-chegaga-z10.lmh");
const ALPS: &[u8] = include_bytes!("../fixtures/alps-z11.lmh");
const UCKERMARK: &[u8] = include_bytes!("../fixtures/uckermark-z11.lmh");
const BANI: &[u8] = include_bytes!("../fixtures/bani-z11.lmh");

/// A height pack read straight from its bytes: its one block's
/// position at the block zoom, its zooms, and tiles decoded once.
struct TestPack {
    bytes: &'static [u8],
    block: core::ops::Range<usize>,
    tile_zoom: u8,
    block_zoom: u8,
    pos: (u32, u32),
    tiles: std::collections::HashMap<(u8, u32, u32), Option<Vec<i16>>>,
    grounds: std::collections::HashMap<(u8, u32, u32), Ground>,
}

impl TestPack {
    fn open(bytes: &'static [u8]) -> Self {
        let (dir, ranges) = split(bytes).expect("a height pack");
        assert_eq!(dir.len(), 1, "the fixtures are one block each");
        Self {
            bytes,
            block: ranges[1].clone(),
            tile_zoom: dir.tile_zoom(),
            block_zoom: dir.block_zoom(),
            pos: dir.block(0).expect("the block"),
            tiles: Default::default(),
            grounds: Default::default(),
        }
    }

    fn tile(&mut self, z: u8, x: u32, y: u32) -> Option<&[i16]> {
        if !self.tiles.contains_key(&(z, x, y)) {
            let got = (|| {
                if z < self.block_zoom || z > self.tile_zoom {
                    return None;
                }
                let shift = z - self.block_zoom;
                if (x >> shift, y >> shift) != self.pos {
                    return None;
                }
                let block = leviculum_map::elevation::Block::parse(&self.bytes[self.block.clone()])
                    .expect("the block parses");
                let mut out = vec![0i16; TILE_CELLS];
                let mut scratch = vec![0i16; TILE_CELLS];
                block
                    .heights(&mut TileDecoder::new(), z, x, y, &mut out, &mut scratch)
                    .expect("the tile decodes");
                Some(out)
            })();
            self.tiles.insert((z, x, y), got);
        }
        self.tiles.get(&(z, x, y)).and_then(|t| t.as_deref())
    }

    /// The plane of tile (x, y) at `z` with what the pack has of its
    /// neighbourhood and every present tile's ground, each measured at
    /// its own row's resolution ([`relief::tile_res`]) as the product's
    /// store measures it; `None` where the pack lacks the tile itself.
    fn plane(&mut self, z: u8, x: u32, y: u32) -> Option<(Plane, Grounds)> {
        let at = |i: usize| (x as i64 + (i % 3) as i64 - 1, y as i64 + (i / 3) as i64 - 1);
        let mut grounds: [Option<Ground>; 9] = [None; 9];
        for (i, g) in grounds.iter_mut().enumerate() {
            let (nx, ny) = at(i);
            if nx < 0 || ny < 0 {
                continue;
            }
            let key = (z, nx as u32, ny as u32);
            if let Some(&known) = self.grounds.get(&key) {
                *g = Some(known);
            } else if let Some(t) = self.tile(z, nx as u32, ny as u32) {
                let m = measure(t, relief::tile_res(z, ny as u32));
                self.grounds.insert(key, m);
                *g = Some(m);
            }
        }
        let ground = fill_ground(&grounds)?;
        let refs: [Option<&[i16]>; 9] = core::array::from_fn(|i| {
            let (nx, ny) = at(i);
            if nx < 0 || ny < 0 {
                return None;
            }
            self.tiles
                .get(&(z, nx as u32, ny as u32))
                .and_then(|t| t.as_deref())
        });
        Some((assemble(&refs), ground))
    }
}

/// Ground metres per sample at the centre row of tile row `y` at `z`:
/// the product's one formula.
fn res_at(z: u8, y: u32) -> f32 {
    relief::tile_res(z, y)
}

/// Two horizontally adjacent tiles of a bigger real cut, each with its
/// full 3 x 3 neighbourhood present: no clamping anywhere, so the seam
/// measures the per-tile ground and the apron's truncation alone.
#[test]
fn real_ground_seams_are_quiet_with_full_neighbourhoods() {
    for (name, bytes) in [("chegaga", CHEGAGA_Z10), ("chebbi", CHEBBI_Z10)] {
        let mut pack = TestPack::open(bytes);
        assert_eq!((pack.block_zoom, pack.tile_zoom), (10, 12));
        // The block is 4 x 4 z12 tiles: (1, 1) and (2, 1) have every
        // neighbour inside it.
        let (x0, y0) = (pack.pos.0 * 4 + 1, pack.pos.1 * 4 + 1);
        let res = res_at(12, y0);
        let (left, lg) = pack.plane(12, x0, y0).expect("left tile");
        let (right, rg) = pack.plane(12, x0 + 1, y0).expect("right tile");
        let a = relief::compute_in(&left, res, sky_radius_m(14), &lg);
        let b = relief::compute_in(&right, res, sky_radius_m(14), &rg);
        let worst = seam_step(&a, &b);
        assert!(worst <= 3, "{name}: seam step {worst} of 254");
    }
}

/// The light table's paper (leviculum-mapfill's LIGHT background).
const PAPER: [u8; 3] = [245, 241, 233];

/// `w` x `h` pixels of a view at `zoom` centred on the pack's block,
/// the relief over paper, as RGB: the frontends' sampling (the relief
/// grid is the pack's own, clamped between its block and tile zooms,
/// sampled bicubically into the view's pixels).
fn relief_picture(bytes: &'static [u8], zoom: u8, w: usize, h: usize) -> Vec<u8> {
    let mut pack = TestPack::open(bytes);
    let rz = zoom.clamp(pack.block_zoom, pack.tile_zoom);
    assert!(rz <= zoom, "the view stands below the pack's grid");
    // The block's centre in world pixels at the view zoom.
    let span = f64::from(1u32 << (zoom - pack.block_zoom)) * 256.0;
    let cx = (f64::from(pack.pos.0) + 0.5) * span;
    let cy = (f64::from(pack.pos.1) + 0.5) * span;
    let step = f64::from(1u32 << (zoom - rz));
    let mut planes: std::collections::HashMap<(u32, u32), Option<ShadePlane>> = Default::default();
    let mut out = Vec::with_capacity(w * h * 3);
    for py in 0..h {
        for px in 0..w {
            let wx = cx - w as f64 / 2.0 + px as f64 + 0.5;
            let wy = cy - h as f64 / 2.0 + py as f64 + 0.5;
            let (sx, sy) = (wx / step, wy / step);
            let (tx, ty) = ((sx / 256.0) as u32, (sy / 256.0) as u32);
            let plane = planes.entry((tx, ty)).or_insert_with(|| {
                let res = res_at(rz, ty);
                pack.plane(rz, tx, ty)
                    .map(|(dem, g)| relief::compute_in(&dem, res, sky_radius_m(zoom), &g))
            });
            let c = match plane {
                Some(p) => {
                    let u = (sx - f64::from(tx) * 256.0) as f32;
                    let v = (sy - f64::from(ty) * 256.0) as f32;
                    let (t, ao) = sample(p, u, v);
                    shade_rgb(PAPER, t, ao)
                }
                None => PAPER,
            };
            out.extend_from_slice(&c);
        }
    }
    out
}

/// Compare `rgb` against `tests/golden/relief/<name>.png`: every channel
/// within 1 of the golden (the kernels are deterministic `f32` without
/// fused operations, so a host gives the same bytes; the step of one
/// leaves room for a libm that rounds a last bit otherwise), else the
/// picture is left in `$TMPDIR/leviculum-golden/relief-<name>.png` to
/// look at and copy over. `LEVICULUM_GOLDEN_BLESS` writes the golden.
fn golden(name: &str, rgb: &[u8], w: usize, h: usize) {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/relief");
    let path = dir.join(format!("{name}.png"));
    let write = |p: &std::path::Path| {
        let f = std::fs::File::create(p).expect("create the picture");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.write_header()
            .expect("png header")
            .write_image_data(rgb)
            .expect("png data");
    };
    if std::env::var_os("LEVICULUM_GOLDEN_BLESS").is_some() {
        std::fs::create_dir_all(&dir).expect("golden dir");
        write(&path);
        return;
    }
    let decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    ));
    let mut reader = decoder.read_info().expect("png info");
    let mut want = vec![0u8; reader.output_buffer_size().expect("png size")];
    let info = reader.next_frame(&mut want).expect("png frame");
    want.truncate(info.buffer_size());
    let worst = want
        .iter()
        .zip(rgb)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(255);
    if want.len() != rgb.len() || worst > 1 {
        let tmp = std::env::temp_dir().join("leviculum-golden");
        std::fs::create_dir_all(&tmp).expect("tmp dir");
        let moved = tmp.join(format!("relief-{name}.png"));
        write(&moved);
        panic!(
            "relief golden {name} moved (worst channel step {worst}); this run's picture: {}",
            moved.display()
        );
    }
}

/// Luminance percentiles (p5, median) of an RGB picture.
fn lum_p5_median(rgb: &[u8]) -> (f32, f32) {
    let mut l: Vec<f32> = rgb
        .chunks_exact(3)
        .map(|c| 0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2]))
        .collect();
    l.sort_by(f32::total_cmp);
    (l[(l.len() - 1) * 5 / 100], l[(l.len() - 1) / 2])
}

/// The relief pinned on real ground: Erg Chebbi and Erg Chegaga at z10,
/// z12 and z14, Jbel Bani's stone desert (hamada, wadis, the escarpment)
/// at z12 and z14, central Switzerland and the Uckermark lowland at z12
/// — each 256 x 256 over paper. Every picture also keeps the study's
/// floor: nothing below luminance 100.
#[test]
fn relief_goldens() {
    let cases: [(&str, &'static [u8], u8); 10] = [
        ("chebbi-z10", CHEBBI_Z10, 10),
        ("chebbi-z12", CHEBBI_Z10, 12),
        ("chebbi-z14", CHEBBI_Z10, 14),
        ("chegaga-z10", CHEGAGA_Z10, 10),
        ("chegaga-z12", CHEGAGA_Z10, 12),
        ("chegaga-z14", CHEGAGA_Z10, 14),
        ("bani-z12", BANI, 12),
        ("bani-z14", BANI, 14),
        ("alps-z12", ALPS, 12),
        ("uckermark-z12", UCKERMARK, 12),
    ];
    for (name, bytes, zoom) in cases {
        let rgb = relief_picture(bytes, zoom, 256, 256);
        let (p5, median) = lum_p5_median(&rgb);
        println!("RELIEF_GOLDEN {name} p5={p5:.0} median={median:.0}");
        let darkest = rgb
            .chunks_exact(3)
            .map(|c| 0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2]))
            .fold(255.0f32, f32::min);
        assert!(darkest >= 100.0, "{name}: darkest luminance {darkest}");
        golden(name, &rgb, 256, 256);
    }
}

/// Vertically adjacent planes at a concave pack corner: the pack lacks
/// (0, 2) of the block but holds (0, 1). Both planes see (0, 2) in their
/// neighbourhood and must fill it the same way, so their shared rows agree
/// within 3 of 254. First the same pair with nothing missing, as control.
#[test]
fn audit_vertical_seams_agree_at_a_pack_corner() {
    let rim = (PLANE_SIDE - TILE) / 2;
    for (name, bytes) in [("chebbi", CHEBBI_Z10), ("chegaga", CHEGAGA_Z10)] {
        let mut pack = TestPack::open(bytes);
        let (bx, by) = (pack.pos.0 * 4, pack.pos.1 * 4);
        let res = res_at(12, by + 1);
        let mut t: std::collections::HashMap<(u32, u32), Vec<i16>> = Default::default();
        for y in 0..4u32 {
            for x in 0..4u32 {
                let tile = pack.tile(12, bx + x, by + y).expect("held").to_vec();
                t.insert((x, y), tile);
            }
        }
        for missing in [None, Some((0u32, 2u32))] {
            let slots = |cx: u32, cy: u32| -> [Option<&[i16]>; 9] {
                core::array::from_fn(|i| {
                    let x = cx as i64 + (i % 3) as i64 - 1;
                    let y = cy as i64 + (i / 3) as i64 - 1;
                    if x < 0 || y < 0 {
                        return None;
                    }
                    let k = (x as u32, y as u32);
                    if Some(k) == missing {
                        return None;
                    }
                    t.get(&k).map(|v| &v[..])
                })
            };
            let (upper, ug) = plane_in(slots(1, 1), res);
            let (lower, lg) = plane_in(slots(1, 2), res);
            let a = relief::compute_in(&upper, res, sky_radius_m(14), &ug);
            let b = relief::compute_in(&lower, res, sky_radius_m(14), &lg);
            let mut worst = 0i32;
            for k in 0..rim {
                for x in 0..TILE {
                    let la = i32::from(a.t[(rim + TILE + k) * PLANE_SIDE + rim + x]);
                    let lb = i32::from(b.t[(rim + k) * PLANE_SIDE + rim + x]);
                    worst = worst.max((la - lb).abs());
                }
            }
            assert!(
                worst <= 3,
                "{name}, missing {missing:?}: vertical seam step {worst} of 254"
            );
        }
    }
}

/// The planes of every held tile of a fixture's 4 x 4 block, with the
/// tiles of `missing` (block coordinates) taken out of the pack, each
/// plane at its own row's resolution and every tile's ground measured
/// at its own row's, as the product's store builds them.
fn block_planes(
    bytes: &'static [u8],
    missing: &[(u32, u32)],
) -> std::collections::HashMap<(u32, u32), ShadePlane> {
    let mut pack = TestPack::open(bytes);
    let (bx, by) = (pack.pos.0 * 4, pack.pos.1 * 4);
    let mut tiles: std::collections::HashMap<(u32, u32), Vec<i16>> = Default::default();
    for y in 0..4u32 {
        for x in 0..4u32 {
            if !missing.contains(&(x, y)) {
                tiles.insert(
                    (x, y),
                    pack.tile(12, bx + x, by + y).expect("held").to_vec(),
                );
            }
        }
    }
    let grounds: std::collections::HashMap<(u32, u32), Ground> = tiles
        .iter()
        .map(|(&(x, y), t)| ((x, y), measure(t, res_at(12, by + y))))
        .collect();
    let held: Vec<(u32, u32)> = tiles.keys().copied().collect();
    let planes = std::thread::scope(|s| {
        let jobs: Vec<_> = held
            .iter()
            .map(|&(cx, cy)| {
                let (tiles, grounds) = (&tiles, &grounds);
                s.spawn(move || {
                    let at = |i: usize| {
                        let x = cx as i64 + (i % 3) as i64 - 1;
                        let y = cy as i64 + (i / 3) as i64 - 1;
                        (x >= 0 && y >= 0).then_some((x as u32, y as u32))
                    };
                    let slots: [Option<&[i16]>; 9] =
                        core::array::from_fn(|i| at(i).and_then(|k| tiles.get(&k)).map(|v| &v[..]));
                    let g: [Option<Ground>; 9] =
                        core::array::from_fn(|i| at(i).and_then(|k| grounds.get(&k)).copied());
                    let res = res_at(12, by + cy);
                    let ground = fill_ground(&g).expect("the centre is held");
                    let dem = assemble(&slots);
                    (
                        (cx, cy),
                        relief::compute_in(&dem, res, sky_radius_m(14), &ground),
                    )
                })
            })
            .collect();
        jobs.into_iter()
            .map(|j| j.join().expect("plane worker"))
            .collect()
    });
    planes
}

/// Every pair of neighbouring held tiles (horizontal, vertical and both
/// diagonals) around missing tiles, on both ergs: the whole block as
/// control, a hole in the block at (1, 1) and at (2, 2) (the pack's
/// concave corners around a void, its straight edges on all four
/// sides), a notch at the block's edge (the audit's corner), an L of
/// three missing tiles (the pack's convex corner pointing into the void)
/// and a missing corner square. Every sample the two planes share,
/// their rims included, wears the same tone within 3 of 254: the
/// stand-in of a missing tile depends on where it is and what the pack
/// holds, never on which plane asks (order 1853). On the code of
/// 8a2a3ddad370 it failed at 36 of 254 (Chebbi, the hole at (1, 1),
/// planes (0, 0) and (0, 1)).
#[test]
fn every_corner_of_a_missing_tile_agrees() {
    let cases: [&[(u32, u32)]; 6] = [
        &[],
        &[(1, 1)],
        &[(2, 2)],
        &[(0, 2)],
        &[(1, 1), (2, 1), (1, 2)],
        &[(2, 2), (3, 2), (2, 3), (3, 3)],
    ];
    let side = PLANE_SIDE as i64;
    for (name, bytes) in [("chebbi", CHEBBI_Z10), ("chegaga", CHEGAGA_Z10)] {
        for missing in cases {
            let planes = block_planes(bytes, missing);
            let mut keys: Vec<_> = planes.keys().copied().collect();
            keys.sort_unstable();
            for &(ax, ay) in &keys {
                for (dx, dy) in [(1i64, 0i64), (0, 1), (1, 1), (-1, 1)] {
                    let (bx, by) = (ax as i64 + dx, ay as i64 + dy);
                    if bx < 0 || by < 0 {
                        continue;
                    }
                    let Some(b) = planes.get(&(bx as u32, by as u32)) else {
                        continue;
                    };
                    let a = &planes[&(ax, ay)];
                    let mut worst = 0i32;
                    for py in 0..side {
                        for px in 0..side {
                            // The same ground sample in b's plane.
                            let (qx, qy) = (px - dx * TILE as i64, py - dy * TILE as i64);
                            if !(0..side).contains(&qx) || !(0..side).contains(&qy) {
                                continue;
                            }
                            let la = i32::from(a.t[(py * side + px) as usize]);
                            let lb = i32::from(b.t[(qy * side + qx) as usize]);
                            worst = worst.max((la - lb).abs());
                        }
                    }
                    assert!(
                        worst <= 3,
                        "{name}, missing {missing:?}: planes {:?} and {:?} differ by {worst} of 254",
                        (ax, ay),
                        (bx, by)
                    );
                }
            }
        }
    }
}

/// Jbel Bani's stone desert (hamada, gullied escarpment, the plain
/// below) does not wear the dune sky: no tile of the fixture asks for
/// more than 1.2 of sky exaggeration, while the ergs keep theirs (95 %
/// of the trip is stone desert, Lew 2026-11-14).
#[test]
fn stone_desert_wears_no_dune_sky() {
    let zf_of = |bytes: &'static [u8]| -> Vec<((u32, u32), Ground, f32)> {
        let mut pack = TestPack::open(bytes);
        let tz = pack.tile_zoom;
        let n = 1u32 << (tz - pack.block_zoom);
        let (bx, by) = (pack.pos.0 * n, pack.pos.1 * n);
        let mut out = Vec::new();
        for y in by..by + n {
            for x in bx..bx + n {
                let res = res_at(tz, y);
                if let Some(t) = pack.tile(tz, x, y) {
                    let g = measure(t, res);
                    out.push(((x, y), g, g.zf()));
                }
            }
        }
        out
    };
    let bani = zf_of(BANI);
    assert!(!bani.is_empty());
    for (at, g, zf) in &bani {
        println!("BANI {at:?} {g:?} zf={zf:.3}");
    }
    for (name, bytes) in [("chebbi", CHEBBI_Z10), ("chegaga", CHEGAGA_Z10)] {
        let erg = zf_of(bytes);
        for (at, g, zf) in &erg {
            println!("ERG {name} {at:?} {g:?} zf={zf:.3}");
        }
        let max = erg.iter().map(|e| e.2).fold(0.0f32, f32::max);
        assert!(max > 1.5, "{name}: the dunes keep their sky, max zf {max}");
    }
    let worst = bani.iter().map(|e| e.2).fold(0.0f32, f32::max);
    assert!(
        worst <= 1.2,
        "stone desert: a Bani tile asks for sky exaggeration {worst}"
    );
}
