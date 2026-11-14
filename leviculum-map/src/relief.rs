//! The ground relief under the map: style R′, one parameter set for the
//! whole world (order 1741, the mapout-r2 study's `README_R2.md`).
//!
//! The height pieces ([`crate::elevation`], 1740) give a tile of ground
//! heights in metres. This module turns such a tile, with an apron of its
//! neighbours, into a [`ShadePlane`]: a per-sample tone `t` (lit over
//! shaded, −1..1) and sky-view factor `ao`, which [`apply`] lays under a
//! drawn view between the area fills and the lines — the relief shades
//! the ground colours, never the line work or the names.
//!
//! The method is the pilot's "desert combination" as the study settled
//! it: four multidirectional-Lambert bands at fixed landform scales,
//! each normalised toward a target amplitude (`REF`) so low relief is
//! amplified and high relief compressed, a noise roll-off with a floor
//! measured from the DEM itself (a surface model's canopy pocks roll
//! off, bare dune ground keeps its 20 to 80 m chains), an unsharp pass,
//! a soft tone curve, and a wide sky-view term. Every constant below is
//! the study's R′ row, named as `README_R2.md` names it. None is keyed
//! to a region: the only data-dependent numbers are the DEM's own noise
//! class (the noise floor of [`Ground`]) and the sky-view exaggeration
//! ([`sky_exaggeration`]), each tied to a measurable property.
//!
//! Both are measured per height tile ([`measure`], on the tile alone)
//! and laid over the plane as a field interpolated between the nine
//! tile centres ([`compute_in`]), so the same ground sample gets the
//! same floor and exaggeration from either side of a tile seam; a
//! value measured once per plane made neighbouring planes disagree
//! along their shared edge (6 of 254 on Erg Chebbi, the audit of order
//! 1741). Where the pack lacks a tile, its heights ([`stand_in`]) and
//! its share of the field are filled by a rule that looks only at where
//! the missing ground lies and which tiles the pack holds, never at
//! which plane asks, so planes meeting at a pack's corner agree too
//! (the audit of order 1851: 30 of 254 at a concave corner).
//!
//! Two pieces of the reference are reconstructions, since only
//! `mo_shade2.py` and the parameter table travelled with the order, not
//! `mo_shade.py` and `stage1.py`: the per-light soft limit is applied as
//! `tanh(1.4 s)` to the directional slope, and the sky-view factor is a
//! normalised openness (height against its wide neighbourhood mean)
//! rather than a horizon walk. Both honour the table's weights, radii
//! and clips; the golden sheets are the check that they behave.
//!
//! The kernels are plain `f32` plane loops, `no_std`; blurs with a
//! sigma of six samples and over run on a decimated grid (the field
//! carries nothing near the fine grid's Nyquist there) and come back
//! bilinearly — gradients are decimated before the upsample, so no
//! block artefacts enter. The product runs one tile per worker thread
//! (the frontends' tile workers), so [`compute`] itself is
//! single-threaded. With the `std` feature, [`par`] holds a threaded
//! blur for the kernel bench (`examples/relief_bench.rs`), equality-tested
//! against the scalar one (`tests/relief.rs`); nothing on the draw path
//! calls it.

use alloc::vec;
use alloc::vec::Vec;
use libm::{expf, powf, sqrtf, tanhf};

/// The relief grid's tile side in samples: the height pieces' own
/// ([`crate::elevation::TILE_EDGE`]).
pub const TILE: usize = 256;

/// The apron of neighbour samples a tile is computed with, each side:
/// half a tile, wide enough for the form band's blur (sigma 20, radius
/// 80) and most of its normalisation window (sigma 60); beyond it the
/// window truncates. At 96 the truncation alone set two vertically
/// adjacent planes of Erg Chebbi apart by 5 of 254 on their shared
/// rows with every neighbour present (the audit of order 1851); at 128
/// the seams of `every_corner_of_a_missing_tile_agrees` stay within 3.
/// It must not exceed half a tile: [`stand_in`] relies on the apron
/// reaching only the near half of a neighbour.
pub const APRON: usize = 128;
const _: () = assert!(
    APRON <= TILE / 2,
    "stand_in needs the apron within half a tile"
);

/// The rim of samples a [`ShadePlane`] keeps past its tile on each side,
/// so a bicubic sample anywhere on the tile, its edges included, has its
/// full 4 x 4 support.
pub const RIM: usize = 2;

/// A plane's side with the apron on both sides: what [`compute`] takes.
pub const SIDE: usize = TILE + 2 * APRON;

/// The ground resolution the band scales are stated at: metres per
/// sample of the study's desert windows' z12 grid (`REF_M`).
pub const REF_METRES: f32 = 33.0;

// ---- the R′ parameter set, `README_R2.md` "Final parameter set R'" ----

/// Band sigmas in reference samples: fine, mid, coarse, form — 23, 83,
/// 231, 660 m of landform.
const SIGMAS: [f32; 4] = [0.7, 2.5, 7.0, 20.0];
/// Normalisation window sigmas in reference samples.
const RMS_SIGMA: [f32; 4] = [6.0, 12.0, 28.0, 60.0];
/// The widest normalisation window a plane can hold, in samples: a
/// third of the apron, so the window's reach (three sigma) stays on the
/// plane. Only the form band's 60 at z12 and finer passes it; wider, it
/// read different ground in two neighbouring planes and set their shared
/// samples apart (4 of 254 at a pack corner on Erg Chegaga, the audit of
/// order 1851).
const RMS_SIGMA_MAX: f32 = APRON as f32 / 3.0;
/// Target band amplitude.
const REF: [f32; 4] = [0.040, 0.040, 0.040, 0.045];
/// Fixed roll-off thresholds.
const NOISE: [f32; 4] = [0.004, 0.0035, 0.003, 0.003];
/// Per-band share of the gain cap.
const GCAP: [f32; 4] = [0.5, 0.75, 1.0, 1.0];
/// Band weights, fine to form.
const WEIGHT: [f32; 4] = [0.8, 0.6, 0.45, 0.42];
/// The gain cap: a band is amplified at most `GMAX * GCAP[i]`-fold.
const GMAX: f32 = 3.5;
/// Normalisation exponent where a band is quieter than `REF`.
const K: f32 = 0.75;
/// ... and where it is louder (full compression of high relief).
const K_HI: f32 = 1.0;
/// Very high relief's amplitude: the gain falls smoothly to this at
/// four times `REF`, so whole Alpine faces keep tones between.
const HI_AMP: f32 = 0.75;
/// The roll-off's floor: a band below its threshold keeps this share.
const FLAT_MIN: f32 = 0.35;
/// The roll-off reaches full gain at this multiple of the threshold.
const ROLL: f32 = 3.0;
/// Vertical factor in the bands.
const ZF: f32 = 1.3;
/// Gaussian on every source DEM, in source samples (`denoise_all`).
const PRE_SIGMA: f32 = 0.7;
/// Unsharp amount and sigma (reference samples).
const US: f32 = 0.6;
const US_SIGMA: f32 = 2.0;
/// The lights: azimuth in degrees and weight; elevation 45 degrees.
const LIGHTS: [(f32, f32); 4] = [(315.0, 0.55), (270.0, 0.20), (0.0, 0.20), (225.0, 0.05)];
/// Each light's soft limit: `tanh(SOFT * slope)`.
const SOFT: f32 = 1.4;
/// Tone: `t = tanh(c V)`, shade and light sides.
const C_NEG: f32 = 0.62;
const C_POS: f32 = 0.48;
/// The darkest multiplier (ambient lift).
const FLOOR: f32 = 0.62;
/// Shadow tint per unit darkening.
const TINT: [f32; 3] = [-0.03, -0.03, 0.06];
/// Skylight drift: shadows move this share toward their own luminance
/// times a cool grey.
const DRIFT: f32 = 0.3;
const COOL: [f32; 3] = [0.97, 0.985, 1.05];
/// Highlight: lit faces move toward warm white by `HL * t`.
const HL: f32 = 0.92;
const WARM: [f32; 3] = [255.0, 252.0, 236.0];
/// Sky-view share of the picture and its factor's clip.
const SVF: f32 = 0.35;
const AO_LO: f32 = 0.6;
const AO_HI: f32 = 1.15;
/// The reconstructed openness gain: with it, two standard deviations of
/// valley floor sit at the clip's bottom at exaggeration 1.
const AO_GAIN: f32 = 0.14;
/// Adaptive noise floor: percentile, per-band multiples, cap.
const ADAPT_Q: f32 = 10.0;
const ADAPT_A: [f32; 4] = [2.0, 1.5, 0.0, 0.0];
const ADAPT_CAP: f32 = 0.015;

/// The sky-view radius in ground metres for a view at `zoom`:
/// `README_R2.md`'s 420 m at z14, 1500 at z13, 3000 at z12, 6000 at z11
/// and coarser.
pub fn sky_radius_m(zoom: u8) -> f32 {
    match zoom {
        0..=11 => 6000.0,
        12 => 3000.0,
        13 => 1500.0,
        _ => 420.0,
    }
}

/// A `side x side` plane of `f32` samples, row-major.
#[derive(Clone)]
pub struct Plane {
    pub side: usize,
    pub v: Vec<f32>,
}

impl Plane {
    /// A zeroed plane.
    pub fn new(side: usize) -> Self {
        Self {
            side,
            v: vec![0.0; side * side],
        }
    }

    /// The plane of a heights tile with apron, metres as `f32`.
    pub fn from_heights(h: &[i16], side: usize) -> Self {
        let mut p = Self::new(side);
        for (o, &m) in p.v.iter_mut().zip(h) {
            *o = f32::from(m);
        }
        p
    }
}

/// The heights plane of one tile with its apron, from the 3 x 3
/// neighbourhood of heights tiles ([`TILE`] squared samples each),
/// row-major with the centre at index 4. A sample of a missing
/// neighbour clamps to the edge of the tile [`stand_in`] names for it,
/// so a pack's edge shades flat toward the void instead of inventing a
/// cliff — and since that tile depends only on where the sample lies
/// and what the pack holds, every plane that sees the sample extends
/// the same edge the same way.
pub fn assemble(tiles: &[Option<&[i16]>; 9]) -> Plane {
    let mut out = Plane::new(SIDE);
    let t = TILE as isize;
    let a = APRON as isize;
    let present = |n: usize| tiles[n].is_some();
    for y in 0..SIDE as isize {
        for x in 0..SIDE as isize {
            let (gx, gy) = (x - a, y - a);
            let (tx, ty) = (gx.div_euclid(t), gy.div_euclid(t));
            let n = ((ty + 1) * 3 + tx + 1) as usize;
            let (lx, ly) = ((gx - tx * t) as usize, (gy - ty * t) as usize);
            let s = stand_in(n, lx, ly, present).unwrap_or(StandIn {
                first: 4,
                second: 4,
                weight: 0.0,
            });
            // The sample's position in a stand-in's frame, clamped to
            // its extent along each axis the stand-in moved.
            let from = |m: usize| -> Option<f32> {
                let h = tiles[m]?;
                let (mx, my) = ((m % 3) as isize - 1, (m / 3) as isize - 1);
                let lx = (gx - mx * t).clamp(0, t - 1) as usize;
                let ly = (gy - my * t).clamp(0, t - 1) as usize;
                Some(f32::from(h[ly * TILE + lx]))
            };
            let Some(a) = from(s.first) else { continue };
            let v = match from(s.second) {
                Some(b) if s.weight > 0.0 => a + s.weight * (b - a),
                _ => a,
            };
            out.v[(y * SIDE as isize + x) as usize] = v;
        }
    }
    out
}

/// Where a sample's heights come from ([`stand_in`]): `first`, mixed
/// with `second` at `weight` (0 is `first` alone).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandIn {
    pub first: usize,
    pub second: usize,
    pub weight: f32,
}

/// The neighbourhood slots (row-major, centre 4) whose heights stand in
/// for sample (`lx`, `ly`) of slot `n` (the sample's place inside that
/// tile, 0..[`TILE`] each way): `n` itself where `present`. Otherwise
/// the sample lies in one quarter of the missing tile, and that quarter
/// touches three tiles: the one beside it across the nearer vertical
/// edge, the one across the nearer horizontal edge, and the one across
/// the corner. Where the pack holds both edge tiles, the sample mixes
/// their clamped edges, each weighted by the sample's distance from the
/// OTHER edge, so the fill meets either tile's edge exactly and turns
/// smoothly between them (a hard switch drew a cliff along the
/// quarter's diagonal); where it holds one, that one; else the corner
/// tile; `None` where the pack holds none of them or they lie outside
/// the 3 x 3 neighbourhood.
///
/// Nothing in the rule names the centre: the choice is a function of
/// the sample's absolute place and of which tiles the pack holds. Every
/// plane whose apron reaches the sample sees all three candidates
/// (the apron, [`APRON`] samples, is under half a tile, so it only ever
/// reaches the quarter of a missing tile that touches the plane's own
/// tile, and that quarter's three neighbours lie around the centre), so
/// every such plane fills the sample from the same ground.
pub fn stand_in(
    n: usize,
    lx: usize,
    ly: usize,
    present: impl Fn(usize) -> bool,
) -> Option<StandIn> {
    let alone = |m: usize| StandIn {
        first: m,
        second: m,
        weight: 0.0,
    };
    if present(n) {
        return Some(alone(n));
    }
    let (col, row) = ((n % 3) as isize, (n / 3) as isize);
    let half = TILE / 2;
    let dx: isize = if lx < half { -1 } else { 1 };
    let dy: isize = if ly < half { -1 } else { 1 };
    let to_vertical_edge = if dx < 0 { lx } else { TILE - 1 - lx };
    let to_horizontal_edge = if dy < 0 { ly } else { TILE - 1 - ly };
    let slot = |c: isize, r: isize| {
        ((0..3).contains(&c) && (0..3).contains(&r)).then_some((r * 3 + c) as usize)
    };
    let held = |m: Option<usize>| m.filter(|&m| present(m));
    let beside = held(slot(col + dx, row));
    let above_or_below = held(slot(col, row + dy));
    match (beside, above_or_below) {
        (Some(b), Some(a)) => {
            // Distances to the two edges, from the sample's centre.
            let (to_b, to_a) = (
                to_vertical_edge as f32 + 0.5,
                to_horizontal_edge as f32 + 0.5,
            );
            Some(StandIn {
                first: b,
                second: a,
                weight: to_b / (to_a + to_b),
            })
        }
        (Some(m), None) | (None, Some(m)) => Some(alone(m)),
        (None, None) => held(slot(col + dx, row + dy)).map(alone),
    }
}

/// `x` clamped to [0, 1], then the smooth step `3x^2 - 2x^3`.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The reflected index for position `i` of `n` (scipy's `reflect`:
/// `(d c b a | a b c d)`), for kernels that reach past an edge.
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    let period = 2 * n;
    let mut i = i.rem_euclid(period);
    if i >= n {
        i = period - 1 - i;
    }
    i as usize
}

/// A normalised gaussian FIR kernel for `sigma`, truncated at four
/// sigma as the reference's `gaussian_filter` truncates.
fn kernel(sigma: f32) -> Vec<f32> {
    let r = (4.0 * sigma + 0.5) as usize;
    let mut k = Vec::with_capacity(2 * r + 1);
    let mut sum = 0.0;
    for i in 0..=2 * r {
        let d = i as f32 - r as f32;
        let w = expf(-0.5 * d * d / (sigma * sigma));
        k.push(w);
        sum += w;
    }
    for w in &mut k {
        *w /= sum;
    }
    k
}

/// One horizontal FIR pass over the rows `rows` of `src` into the same
/// rows of `dst`, reflect boundary: the row-band worker both the
/// single-thread and the threaded blur run.
fn hpass_rows(src: &Plane, k: &[f32], rows: core::ops::Range<usize>, dst: &mut [f32]) {
    let side = src.side;
    let r = k.len() / 2;
    for (band_row, y) in rows.enumerate() {
        let row = &src.v[y * side..(y + 1) * side];
        let out = &mut dst[band_row * side..(band_row + 1) * side];
        // The interior, where the kernel never leaves the row, as one
        // bounds-check-free window walk; the two edges by reflection.
        if side > 2 * r {
            for (x, o) in out.iter_mut().enumerate().take(side - r).skip(r) {
                let mut s = 0.0;
                for (w, &v) in k.iter().zip(&row[x - r..=x + r]) {
                    s += w * v;
                }
                *o = s;
            }
        }
        let edge = |x: usize| {
            let mut s = 0.0;
            for (i, w) in k.iter().enumerate() {
                s += w * row[reflect(x as isize + i as isize - r as isize, side)];
            }
            s
        };
        for (x, o) in out.iter_mut().enumerate().take(r.min(side)) {
            *o = edge(x);
        }
        for (x, o) in out.iter_mut().enumerate().skip(side.saturating_sub(r)) {
            *o = edge(x);
        }
    }
}

/// One vertical FIR pass writing the rows `rows` of `dst`, reading the
/// rows of `src` the kernel spans, reflect boundary.
fn vpass_rows(src: &[f32], side: usize, k: &[f32], rows: core::ops::Range<usize>, dst: &mut [f32]) {
    let r = k.len() / 2;
    let n = src.len() / side;
    for (band_row, y) in rows.enumerate() {
        let out = &mut dst[band_row * side..(band_row + 1) * side];
        out.fill(0.0);
        for (i, w) in k.iter().enumerate() {
            let sy = reflect(y as isize + i as isize - r as isize, n);
            let row = &src[sy * side..(sy + 1) * side];
            for (o, &v) in out.iter_mut().zip(row) {
                *o += w * v;
            }
        }
    }
}

/// `p` blurred by a gaussian of `sigma` samples, separably, reflect
/// boundary — the scalar form, every sigma at full resolution.
pub fn gblur_exact(p: &Plane, sigma: f32) -> Plane {
    let k = kernel(sigma);
    let mut tmp = Plane::new(p.side);
    hpass_rows(p, &k, 0..p.side, &mut tmp.v);
    let mut out = Plane::new(p.side);
    let src = tmp.v.clone();
    vpass_rows(&src, p.side, &k, 0..p.side, &mut out.v);
    out
}

/// The decimation a blur of `sigma` runs at: none below six samples,
/// then the largest power of two that keeps the decimated sigma at
/// three samples or more.
fn decimation(sigma: f32) -> usize {
    if sigma < 6.0 {
        1
    } else if sigma < 12.0 {
        2
    } else if sigma < 24.0 {
        4
    } else {
        8
    }
}

/// `p` box-averaged `f`-fold.
fn decimate(p: &Plane, f: usize) -> Plane {
    let side = p.side / f;
    let mut out = Plane::new(side);
    let inv = 1.0 / (f * f) as f32;
    for y in 0..side {
        for x in 0..side {
            let mut s = 0.0;
            for dy in 0..f {
                let row = &p.v[(y * f + dy) * p.side + x * f..][..f];
                for &v in row {
                    s += v;
                }
            }
            out.v[y * side + x] = s * inv;
        }
    }
    out
}

/// `p` brought back `f`-fold bilinearly to `side`.
fn upsample(p: &Plane, f: usize, side: usize) -> Plane {
    let mut out = Plane::new(side);
    let n = p.side;
    for y in 0..side {
        // Sample centres: fine centre (y + 0.5) lies at coarse
        // coordinate (y + 0.5) / f; its grid offset against coarse
        // centres is that minus 0.5.
        let gy = (y as f32 + 0.5) / f as f32 - 0.5;
        let y0 = (gy as isize).clamp(0, n as isize - 1) as usize;
        let y1 = (y0 + 1).min(n - 1);
        let fy = (gy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..side {
            let gx = (x as f32 + 0.5) / f as f32 - 0.5;
            let x0 = (gx as isize).clamp(0, n as isize - 1) as usize;
            let x1 = (x0 + 1).min(n - 1);
            let fx = (gx - x0 as f32).clamp(0.0, 1.0);
            let a = p.v[y0 * n + x0] * (1.0 - fx) + p.v[y0 * n + x1] * fx;
            let b = p.v[y1 * n + x0] * (1.0 - fx) + p.v[y1 * n + x1] * fx;
            out.v[y * side + x] = a * (1.0 - fy) + b * fy;
        }
    }
    out
}

/// [`gblur_exact`] with the large sigmas on a decimated grid.
pub fn gblur(p: &Plane, sigma: f32) -> Plane {
    let f = decimation(sigma);
    if f == 1 {
        return gblur_exact(p, sigma);
    }
    let small = decimate(p, f);
    let blurred = gblur_exact(&small, sigma / f as f32);
    upsample(&blurred, f, p.side)
}

/// The Sobel gradient of `p`: `dz` per ground metre times the vertical
/// factor `zf`, `res` metres per sample. `(gx, gy)`, x to the right,
/// y downward.
pub fn sobel(p: &Plane, res: f32, zf: f32) -> (Plane, Plane) {
    let side = p.side;
    let mut gx = Plane::new(side);
    let mut gy = Plane::new(side);
    let s = zf / (8.0 * res);
    let at = |x: isize, y: isize| p.v[reflect(y, side) * side + reflect(x, side)];
    for y in 0..side as isize {
        for x in 0..side as isize {
            let (nw, n, ne) = (at(x - 1, y - 1), at(x, y - 1), at(x + 1, y - 1));
            let (w, e) = (at(x - 1, y), at(x + 1, y));
            let (sw, so, se) = (at(x - 1, y + 1), at(x, y + 1), at(x + 1, y + 1));
            let i = y as usize * side + x as usize;
            gx.v[i] = ((ne + 2.0 * e + se) - (nw + 2.0 * w + sw)) * s;
            gy.v[i] = ((sw + 2.0 * so + se) - (nw + 2.0 * n + ne)) * s;
        }
    }
    (gx, gy)
}

/// The multidirectional Lambert of a gradient field: each light's
/// directional slope, soft-limited by `tanh(SOFT s)`, summed by the
/// lights' weights. Positive faces the main light (NW).
pub fn multilight(gx: &Plane, gy: &Plane) -> Plane {
    let mut out = Plane::new(gx.side);
    // A face is lit where the ground rises away from the light: the
    // directional slope away from the light's azimuth (clockwise from
    // north) in image coordinates, y downward, east = sin, north = -y.
    let dirs: [(f32, f32, f32); 4] = {
        let mut d = [(0.0, 0.0, 0.0); 4];
        for (o, &(az, w)) in d.iter_mut().zip(&LIGHTS) {
            let a = az.to_radians();
            *o = (-libm::sinf(a), libm::cosf(a), w);
        }
        d
    };
    for ((o, &x), &y) in out.v.iter_mut().zip(&gx.v).zip(&gy.v) {
        let mut s = 0.0;
        for &(ux, uy, w) in &dirs {
            s += w * tanhf(SOFT * (x * ux + y * uy));
        }
        *o = s;
    }
    out
}

/// The local rms of a band: the square root of its gaussian-smoothed
/// square, window sigma `sigma` samples.
pub fn local_rms(b: &Plane, sigma: f32) -> Plane {
    let mut sq = Plane::new(b.side);
    for (o, &v) in sq.v.iter_mut().zip(&b.v) {
        *o = v * v;
    }
    let mut sm = gblur(&sq, sigma);
    for v in &mut sm.v {
        *v = sqrtf(v.max(0.0));
    }
    sm
}

/// The q-th percentile of `p`, from a sorted copy of every fourth
/// sample in each direction.
fn percentile(p: &Plane, q: f32) -> f32 {
    let mut s: Vec<f32> =
        p.v.chunks(p.side * 4)
            .flat_map(|rows| rows[..p.side].iter().step_by(4))
            .copied()
            .collect();
    if s.is_empty() {
        return 0.0;
    }
    s.sort_by(f32::total_cmp);
    s[((q / 100.0 * (s.len() - 1) as f32) + 0.5) as usize]
}

/// How the sky-view term is exaggerated, from two measurable
/// properties of the window and none of its name: the share of slope
/// energy in the fine band against the form band, and the DEM's own
/// noise floor. Dune fields put their relief into the fine chains (the
/// study's ZF 2.0 Chebbi, 2.5 Chegaga), mountain and lowland windows
/// into the form band (its 1.0 everywhere else) — but on a vegetated
/// surface model the fine band measures canopy, not dunes (Uckermark's
/// fine rms is louder than Chegaga's), so the exaggeration only comes
/// where the floor says the DEM is clean bare ground (0.002 to 0.003,
/// against 0.012 and up on canopy). Both steps are smooth.
///
/// Stone desert must not wear the dune sky (Lew 2026-11-14: 95 % of
/// the trip is hamada and reg). On GLO-30 the Jbel Bani hamada and its
/// gullied escarpment measure a fine share of 0.38 to 0.70 and a floor
/// of 0.0031 to 0.0054, so neither property alone sets it apart from
/// Erg Chebbi's tiles (0.47 to 0.72, 0.0025 to 0.0068); together they
/// do: the dune factor needs the fine band to carry more than half the
/// slope energy (full from 0.6) AND a floor of clean sand (full under
/// 0.004, none from 0.0055). `stone_desert_wears_no_dune_sky` in
/// `tests/relief.rs` holds every Bani tile at or under 1.2 and the
/// ergs above 1.5.
pub fn sky_exaggeration(fine_rms_med: f32, form_rms_med: f32, noise_floor: f32) -> f32 {
    let ff = fine_rms_med / (fine_rms_med + form_rms_med + 1e-6);
    let clean = smoothstep((0.0055 - noise_floor) / 0.0015);
    1.0 + 1.5 * smoothstep((ff - 0.5) / 0.1) * clean
}

/// Ground metres per sample at the centre row of tile row `y` of the
/// relief grid at `zoom` (Web Mercator, 256 samples a tile): the one
/// resolution a tile's [`measure`] and its plane use, whichever plane
/// asks.
pub fn tile_res(zoom: u8, y: u32) -> f32 {
    let n = f64::from(1u32 << zoom);
    let lat = libm::atan(libm::sinh(
        core::f64::consts::PI * (1.0 - 2.0 * (f64::from(y) + 0.5) / n),
    ));
    (156_543.03 * libm::cos(lat) / n) as f32
}

/// What [`compute_in`] measures of one height tile's ground, on the
/// tile alone: the median local rms of the fine and of the form band,
/// and the DEM's noise floor (the 10th percentile of the fine band's
/// local rms, capped where it would be measuring mountains).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ground {
    pub fine: f32,
    pub form: f32,
    pub floor: f32,
}

impl Ground {
    /// The sky-view exaggeration this ground asks for
    /// ([`sky_exaggeration`]).
    pub fn zf(&self) -> f32 {
        sky_exaggeration(self.fine, self.form, self.floor)
    }
}

/// The [`Ground`] of one [`TILE`] by [`TILE`] heights tile, `res`
/// ground metres per sample, measured on the tile alone (reflect
/// boundary): the per-tile statistics every plane that sees the tile
/// shares. The reference measures its floor in a 20 km window; a z12
/// tile is about 8 km, and [`compute_in`] interpolates over the nine
/// tiles around a plane, about 17 km.
pub fn measure(tile: &[i16], res: f32) -> Ground {
    let dem = gblur(&Plane::from_heights(tile, TILE), PRE_SIGMA);
    let scale = REF_METRES / res;
    let field = |s: f32| {
        let b = gblur(&dem, s * scale);
        let (gx, gy) = sobel(&b, res, ZF);
        multilight(&gx, &gy)
    };
    let b0 = diff(&field(SIGMAS[0]), &field(SIGMAS[1]));
    let b3 = field(SIGMAS[3]);
    let rms0 = local_rms(&b0, RMS_SIGMA[0] * scale);
    Ground {
        fine: percentile(&rms0, 50.0),
        form: percentile(&local_rms(&b3, RMS_SIGMA[3] * scale), 50.0),
        floor: percentile(&rms0, ADAPT_Q).min(ADAPT_CAP),
    }
}

/// The [`Ground`]s of a plane's 3 x 3 neighbourhood (row-major, centre
/// 4) as [`compute_in`] takes them: a missing tile's is `None`, the
/// centre's is always there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grounds([Option<Ground>; 9]);

impl Grounds {
    /// The same ground on all nine tiles.
    pub fn uniform(g: Ground) -> Self {
        Self([Some(g); 9])
    }

    /// The grounds the pack has of the neighbourhood.
    pub fn get(&self) -> &[Option<Ground>; 9] {
        &self.0
    }
}

/// The grounds of a plane's neighbourhood as the pack has them; `None`
/// without the centre's. A missing tile needs no stand-in value: the
/// field [`compute_in`] lays between the tile centres leaves it out of
/// every cell it corners (`field_of`).
pub fn fill_ground(g: &[Option<Ground>; 9]) -> Option<Grounds> {
    g[4]?;
    Some(Grounds(*g))
}

/// The centre tile of an aproned [`SIDE`] plane, as heights.
fn centre_tile(dem: &Plane) -> Vec<i16> {
    let mut out = Vec::with_capacity(TILE * TILE);
    for y in 0..TILE {
        let row = &dem.v[(y + APRON) * SIDE + APRON..][..TILE];
        out.extend(row.iter().map(|&v| v.clamp(-32768.0, 32767.0) as i16));
    }
    out
}

/// One axis's interpolation between the three tile centres: for each
/// plane sample along the axis, the lower centre (0 or 1) and the
/// weight of the upper one.
fn centre_weights() -> Vec<(usize, f32)> {
    let half = TILE as f32 / 2.0 - 0.5;
    (0..SIDE)
        .map(|i| {
            let g = i as f32 - APRON as f32;
            if g < half {
                (
                    0,
                    ((g - (half - TILE as f32)) / TILE as f32).clamp(0.0, 1.0),
                )
            } else {
                (1, ((g - half) / TILE as f32).clamp(0.0, 1.0))
            }
        })
        .collect()
}

/// A per-tile value of the 3 x 3 neighbourhood laid over the plane,
/// bilinear between the tile centres. A missing tile (`None`) drops out
/// of each cell it corners and the cell's other corners share its
/// weight: the value at a sample is then a function of the cell (four
/// tile centres, fixed by the sample's absolute place) and of which of
/// them the pack holds, whichever plane asks, and it stays continuous
/// across the cells' edges (on an edge only its two ends weigh). The
/// centre is always there and weighs at least 1/64 anywhere on the
/// plane, so the sum is never empty.
fn field_of(values: &[Option<f32>; 9]) -> Plane {
    let w = centre_weights();
    let mut out = Plane::new(SIDE);
    for (y, &(ry, fy)) in w.iter().enumerate() {
        for (x, &(rx, fx)) in w.iter().enumerate() {
            let corners = [
                (ry, rx, (1.0 - fx) * (1.0 - fy)),
                (ry, rx + 1, fx * (1.0 - fy)),
                (ry + 1, rx, (1.0 - fx) * fy),
                (ry + 1, rx + 1, fx * fy),
            ];
            let (mut sum, mut weight) = (0.0f32, 0.0f32);
            for (r, c, k) in corners {
                if let Some(v) = values[r * 3 + c] {
                    sum += k * v;
                    weight += k;
                }
            }
            out.v[y * SIDE + x] = if weight > 0.0 {
                sum / weight
            } else {
                values[4].unwrap_or(0.0)
            };
        }
    }
    out
}

/// The per-sample tone and sky-view of one relief tile, quantized:
/// `t` as 1/127 steps of −1..1, `ao` as 255 steps of `AO_LO` (0.6)..1.2.
/// The plane covers the tile plus [`RIM`] samples each side.
pub struct ShadePlane {
    pub t: Vec<i8>,
    pub ao: Vec<u8>,
}

/// The planes' side.
pub const PLANE_SIDE: usize = TILE + 2 * RIM;

const AO_SPAN: f32 = 0.6;

fn quant_ao(a: f32) -> u8 {
    (((a - AO_LO) / AO_SPAN) * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// The `ao` a stored byte means.
pub fn ao_of(q: u8) -> f32 {
    AO_LO + f32::from(q) / 255.0 * AO_SPAN
}

/// The whole R′ relief term for one tile on ground that is one
/// [`Ground`] throughout: `dem` is the [`SIDE`] by [`SIDE`] heights
/// plane (tile plus apron), `res` its ground metres per sample, `sky_m`
/// the sky-view radius in metres ([`sky_radius_m`]), the ground
/// measured on the plane's centre tile. For a tile among neighbours,
/// [`compute_in`] with each neighbour's own [`measure`] keeps the seams
/// quiet. Deterministic: the same plane gives the same bytes on every
/// run and host.
pub fn compute(dem: &Plane, res: f32, sky_m: f32) -> ShadePlane {
    let g = measure(&centre_tile(dem), res);
    compute_in(dem, res, sky_m, &Grounds::uniform(g))
}

/// [`compute`] with the [`Ground`] of each of the nine tiles of the
/// plane's neighbourhood ([`Grounds`]; a missing neighbour drops out of
/// the interpolation, `field_of`): the noise floor and the sky-view
/// exaggeration are fields interpolated between the tile centres, so
/// neighbouring planes agree on their shared ground.
pub fn compute_in(dem: &Plane, res: f32, sky_m: f32, ground: &Grounds) -> ShadePlane {
    // `denoise_all`: 0.7 samples on every source DEM.
    let dem = gblur(dem, PRE_SIGMA);
    // `metres`: the band scales are landform sizes, not samples.
    let scale = REF_METRES / res;
    // The four Lambert fields, finest blur to form blur, then bands as
    // their differences.
    let mut fields: Vec<Plane> = Vec::with_capacity(4);
    for &s in &SIGMAS {
        let b = gblur(&dem, s * scale);
        let (gx, gy) = sobel(&b, res, ZF);
        fields.push(multilight(&gx, &gy));
    }
    let bands = [
        diff(&fields[0], &fields[1]),
        diff(&fields[1], &fields[2]),
        diff(&fields[2], &fields[3]),
        fields.pop().unwrap_or_else(|| Plane::new(dem.side)),
    ];
    // Each band's local rms, and the DEM's own noise floor: the 10th
    // percentile of the fine band's rms per tile ([`measure`]), laid
    // over the plane between the tile centres. The cap keeps a floor
    // that measured mountains, not noise, from fogging them.
    let rms: Vec<Plane> = bands
        .iter()
        .zip(&RMS_SIGMA)
        .map(|(b, &s)| local_rms(b, (s * scale).min(RMS_SIGMA_MAX)))
        .collect();
    let floor = field_of(&ground.0.map(|g| g.map(|g| g.floor)));
    // The normalised sum: amplify quiet bands, compress loud ones,
    // roll off under the noise threshold.
    let mut v = Plane::new(dem.side);
    for i in 0..4 {
        let cap = (GMAX * GCAP[i]).max(1.0);
        for (((o, &b), &r), &f) in v.v.iter_mut().zip(&bands[i].v).zip(&rms[i].v).zip(&floor.v) {
            let thr = NOISE[i].max(ADAPT_A[i] * f);
            let ratio = REF[i] / r.max(1e-6);
            let mut g = powf(ratio, if ratio >= 1.0 { K } else { K_HI });
            g = g.min(cap);
            g *= 1.0 - (1.0 - HI_AMP) * smoothstep((1.0 / ratio - 1.0) / 3.0);
            g *= FLAT_MIN + (1.0 - FLAT_MIN) * smoothstep((r - thr) / ((ROLL - 1.0) * thr));
            *o += WEIGHT[i] * g * b / REF[i];
        }
    }
    // Unsharp.
    let blur = gblur(&v, US_SIGMA * scale);
    for (o, &b) in v.v.iter_mut().zip(&blur.v) {
        *o += US * (*o - b);
    }
    // Sky-view: normalised openness over the zoom's radius, its
    // exaggeration from each tile's slope-energy split, between the
    // tile centres like the floor.
    let zf_sky = field_of(&ground.0.map(|g| g.map(|g| g.zf())));
    let sigma = (sky_m / res / 3.0).max(2.0);
    let wide = gblur(&dem, sigma);
    let mut delta = diff(&dem, &wide);
    let norm = local_rms(&delta, sigma);
    for (d, &n) in delta.v.iter_mut().zip(&norm.v) {
        *d /= n.max(1e-3);
    }
    // Quantize the plane's rim-and-tile window.
    let mut out = ShadePlane {
        t: vec![0; PLANE_SIDE * PLANE_SIDE],
        ao: vec![0; PLANE_SIDE * PLANE_SIDE],
    };
    let off = APRON - RIM;
    for y in 0..PLANE_SIDE {
        for x in 0..PLANE_SIDE {
            let i = (y + off) * SIDE + (x + off);
            let t = if v.v[i] < 0.0 {
                tanhf(C_NEG * v.v[i])
            } else {
                tanhf(C_POS * v.v[i])
            };
            let raw = (1.0 + AO_GAIN * zf_sky.v[i] * delta.v[i]).clamp(AO_LO, AO_HI);
            let ao = 1.0 + SVF * (raw - 1.0);
            let o = y * PLANE_SIDE + x;
            out.t[o] = (t * 127.0 + if t >= 0.0 { 0.5 } else { -0.5 }) as i8;
            out.ao[o] = quant_ao(ao);
        }
    }
    out
}

fn diff(a: &Plane, b: &Plane) -> Plane {
    let mut out = Plane::new(a.side);
    for ((o, &x), &y) in out.v.iter_mut().zip(&a.v).zip(&b.v) {
        *o = x - y;
    }
    out
}

/// One ground colour under tone `t` (−1..1) and sky-view `ao`: the
/// study's `shade_rgb`. Shade faces are darkened toward the ambient
/// floor, tinted cool and drifted toward a cool grey of their own
/// luminance; lit faces move toward warm white; the sky-view factor
/// multiplies last.
pub fn shade_rgb(rgb: [u8; 3], t: f32, ao: f32) -> [u8; 3] {
    let mut c = [f32::from(rgb[0]), f32::from(rgb[1]), f32::from(rgb[2])];
    if t < 0.0 {
        let d = -t;
        let m = 1.0 - (1.0 - FLOOR) * d;
        for (ch, &tint) in c.iter_mut().zip(&TINT) {
            *ch *= m + tint * d;
        }
        let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        for (ch, &cool) in c.iter_mut().zip(&COOL) {
            *ch = *ch * (1.0 - DRIFT * d) + l * cool * (DRIFT * d);
        }
    } else {
        for (ch, &w) in c.iter_mut().zip(&WARM) {
            *ch += (w - *ch) * HL * t;
        }
    }
    let mut out = [0u8; 3];
    for (o, ch) in out.iter_mut().zip(&c) {
        *o = (ch * ao + 0.5).clamp(0.0, 255.0) as u8;
    }
    out
}

/// Where a drawn view finds its tone: filled by the frontend's relief
/// cache in the view's own pixel coordinates. `row` writes one `t` and
/// `ao` value per pixel for the row at `(x, y)`; `false` where no
/// height data covers it, and the row is left unshaded.
pub trait Shade {
    fn row(&mut self, x: i32, y: i32, t: &mut [f32], ao: &mut [f32]) -> bool;
}

/// Shade the ground `area` of `target` by `shade`: read each row of
/// pixels back, put it through [`shade_rgb`], write it again. A target
/// that cannot read its pixels (`Coverage::row_rgb` false) is left as
/// it is, as is a row without height data. Called by the painter
/// between the area fills and the lines (`draw::draw_in_relief`).
pub fn apply<D: leviculum_mapfill::coverage::Coverage>(
    target: &mut D,
    area: embedded_graphics::primitives::Rectangle,
    shade: &mut dyn Shade,
) -> Result<(), D::Error> {
    let w = area.size.width as usize;
    if w == 0 {
        return Ok(());
    }
    let mut px = vec![[0u8; 3]; w];
    let mut t = vec![0.0f32; w];
    let mut ao = vec![1.0f32; w];
    for y in area.top_left.y..area.top_left.y + area.size.height as i32 {
        if !shade.row(area.top_left.x, y, &mut t, &mut ao) {
            continue;
        }
        if !target.row_rgb(area.top_left.x, y, &mut px) {
            return Ok(());
        }
        for ((p, &tv), &av) in px.iter_mut().zip(&t).zip(&ao) {
            *p = shade_rgb(*p, tv, av);
        }
        target.set_row_rgb(area.top_left.x, y, &px)?;
    }
    Ok(())
}

/// A bicubic (Catmull-Rom) sample of a [`ShadePlane`] at the tile
/// coordinate `(u, v)` in samples, 0..[`TILE`]; the rim carries the
/// support past the edges. `(t, ao)`.
pub fn sample(p: &ShadePlane, u: f32, v: f32) -> (f32, f32) {
    let gf = |c: f32| c - 0.5 + RIM as f32;
    let (gu, gv) = (gf(u), gf(v));
    let (iu, iv) = (gu as i32, gv as i32);
    let (fu, fv) = (gu - iu as f32, gv - iv as f32);
    let wu = cubic_weights(fu);
    let wv = cubic_weights(fv);
    let side = PLANE_SIDE as i32;
    let mut t = 0.0;
    let mut ao = 0.0;
    for (j, &wy) in wv.iter().enumerate() {
        let y = (iv - 1 + j as i32).clamp(0, side - 1);
        for (i, &wx) in wu.iter().enumerate() {
            let x = (iu - 1 + i as i32).clamp(0, side - 1);
            let at = (y * side + x) as usize;
            let w = wy * wx;
            t += w * f32::from(p.t[at]) / 127.0;
            ao += w * ao_of(p.ao[at]);
        }
    }
    (t.clamp(-1.0, 1.0), ao.clamp(AO_LO, AO_HI))
}

/// Catmull-Rom weights for fractional position `f`.
fn cubic_weights(f: f32) -> [f32; 4] {
    let f2 = f * f;
    let f3 = f2 * f;
    [
        0.5 * (-f3 + 2.0 * f2 - f),
        0.5 * (3.0 * f3 - 5.0 * f2 + 2.0),
        0.5 * (-3.0 * f3 + 4.0 * f2 + f),
        0.5 * (f3 - f2),
    ]
}

/// The blur on scoped threads, for the kernel bench: each pass splits
/// its output rows into one band per thread over the same shared input,
/// so the threaded result is the single-thread result bit for bit
/// (`tests/relief.rs` holds it there). The draw path does not use it:
/// the frontends already run one tile per worker.
#[cfg(feature = "std")]
pub mod par {
    use super::*;

    /// [`gblur_exact`] with both passes split over `threads` row bands.
    pub fn gblur_exact_mt(p: &Plane, sigma: f32, threads: usize) -> Plane {
        let k = kernel(sigma);
        let side = p.side;
        let mut tmp = Plane::new(side);
        run_bands(side, threads, &mut tmp.v, |rows, dst| {
            hpass_rows(p, &k, rows, dst);
        });
        let src = tmp.v;
        let mut out = Plane::new(side);
        run_bands(side, threads, &mut out.v, |rows, dst| {
            vpass_rows(&src, side, &k, rows, dst);
        });
        out
    }

    /// Split the `side * side` output `dst` into about `threads` row
    /// bands and run `work` on each in a scoped thread.
    fn run_bands(
        side: usize,
        threads: usize,
        dst: &mut [f32],
        work: impl Fn(core::ops::Range<usize>, &mut [f32]) + Sync,
    ) {
        let threads = threads.max(1).min(side);
        let band = side.div_ceil(threads);
        std::thread::scope(|s| {
            let mut rest = dst;
            let mut y = 0;
            while y < side {
                let take = band.min(side - y);
                let (mine, tail) = rest.split_at_mut(take * side);
                rest = tail;
                let rows = y..y + take;
                let work = &work;
                s.spawn(move || work(rows, mine));
                y += take;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reflect index walks `d c b a | a b c d`.
    #[test]
    fn reflect_is_scipys() {
        let n = 4;
        let got: Vec<usize> = (-4..8).map(|i| reflect(i, n)).collect();
        assert_eq!(got, [3, 2, 1, 0, 0, 1, 2, 3, 3, 2, 1, 0]);
    }

    /// A constant plane blurs to itself and shades to nothing.
    #[test]
    fn flat_ground_is_left_alone() {
        let mut p = Plane::new(64);
        p.v.fill(500.0);
        let b = gblur_exact(&p, 2.5);
        for &v in &b.v {
            assert!((v - 500.0).abs() < 1e-2);
        }
        let (gx, gy) = sobel(&p, 30.0, ZF);
        assert!(gx.v.iter().chain(&gy.v).all(|&v| v == 0.0));
    }
}
