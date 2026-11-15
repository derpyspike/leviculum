//! The ground classes from their features: the Morocco pilot's rules
//! (`pilot-morocco/scripts/classify.py`, `classify()`), each with the
//! reason it is there, and the two places stage A departs from them.
//!
//! Departures from the pilot, each one measured (see the order's report
//! and `docs/src/concepts/map-server.md`, "Ground packs"):
//!
//! 1. No water inputs. The pilot also called dry lake wherever Water
//!    Observations from Space saw water in more than 2 % of the scenes,
//!    and open water above 30 %. Lew's rule (order 1742) leaves water to
//!    OpenStreetMap, so open water is not a class and a dry lake is found
//!    from the ground's own flatness and paleness alone.
//! 2. The erg guard ([`Rules::erg_guard`]). The pilot's mountain rule
//!    (steep or rough, coherent) also fires on the slip faces of the big
//!    star dunes of Erg Chebbi, where steep sand keeps enough coherence
//!    between two passes, and paints small grey specks inside the erg.
//!    With the guard, steep ground inside an erg (dune sand holding more
//!    than 35 % of the 1.5 km around it, the pilot's own erg measure of
//!    its corridors) stays dune sand.
//!
//! Known weaknesses the rules keep, left visible on purpose: hamada
//! against reg was relabelled in the pilot using the very tracks the
//! validation counts (rule [`HAMADA_ROUGH`] and the pale-high branch),
//! so that split is partly circular; and the dry-lake rule spreads past
//! the lake beds it finds.

use super::grid::{box_mean, components, Grid};

/// Dune sand.
pub const DUNE: u8 = 1;
/// Sand sheet.
pub const SHEET: u8 = 2;
/// Rock plateau, hamada.
pub const HAMADA: u8 = 3;
/// Gravel plain, reg.
pub const REG: u8 = 4;
/// Mountain, steep rock.
pub const MOUNT: u8 = 5;
/// Dry lake, sebkha.
pub const LAKE: u8 = 6;
/// Wadi bed.
pub const WADI: u8 = 7;
/// Vegetation, oasis.
pub const VEG: u8 = 8;
/// No rule was sure.
pub const UNC: u8 = 9;

/// The radar backscatter above which coherent flat ground is bedrock:
/// a very rough return (dB, summer VV).
pub const HAMADA_ROUGH: f32 = -8.0;

/// The features of one pixel of the z11 grid. NaN where an input has no
/// value there.
#[derive(Clone, Copy, Debug, Default)]
pub struct Px {
    /// Sentinel-1 12-day coherence, median of the four seasons, 0..1.
    pub coh: f32,
    /// Summer VV backscatter, dB.
    pub vv: f32,
    /// Visible albedo: mean of GeoMAD's blue, green and red reflectance.
    pub albedo: f32,
    /// NDVI from GeoMAD's near-infrared and red.
    pub ndvi: f32,
    /// The steepest z12 slope of the four under this pixel, degrees.
    pub slope_max: f32,
    /// Height standard deviation within about 1 km, metres.
    pub relief_1k: f32,
    /// Height above the 5 km mean, metres.
    pub tpi_5k: f32,
    /// Upstream cells draining through this pixel (itself included).
    pub acc: f32,
}

/// Which rule set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    /// Steep ground inside an erg stays dune sand (departure 2).
    pub erg_guard: bool,
}

impl Rules {
    /// Stage A: the pilot's rules with the erg guard.
    pub const STAGE_A: Rules = Rules { erg_guard: true };
    /// The pilot's rules as they stood (without its water inputs).
    pub const PILOT: Rules = Rules { erg_guard: false };
}

/// The minimum size of a dry lake, in z11 pixels: 460 x 66 m x 66 m,
/// 2 km² (the pilot's).
pub const LAKE_MIN_PX: u32 = 460;

/// One pixel's class before the rules that look at its neighbours (dry
/// lakes, wadis, the erg guard). Later rules overwrite earlier ones, as
/// in the pilot.
pub fn classify_px(p: &Px) -> u8 {
    let mut c = UNC;
    let flat = p.slope_max < 12.0;
    // Coherent, stable ground: the radar sees the same surface twelve days
    // apart, so nothing moves on it. Dark-to-mid, moderately rough stable
    // surfaces are the stony plains the pistes use (reg).
    let stable = p.coh >= 0.65 && flat;
    if stable && p.albedo < 0.21 && p.vv > -17.0 {
        c = REG;
    }
    // A very rough return off stable ground is bare bedrock (hamada). In
    // the pilot the first rule (dark is hamada) was backwards and was
    // relabelled from the tracks: partly circular, kept visible.
    if stable && p.vv > HAMADA_ROUGH {
        c = HAMADA;
    }
    // Pale stable ground that sits high is a pale rock plateau.
    if stable && p.albedo >= 0.21 && p.vv <= HAMADA_ROUGH && p.tpi_5k >= 0.0 {
        c = HAMADA;
    }
    // Pale, smooth stable ground (radar-dark, bright in the visible) is a
    // sandy plain; pale low coherent ground that is not smooth stays
    // uncertain, nothing says what it is.
    if stable && p.vv <= -17.0 && p.albedo >= 0.17 {
        c = SHEET;
    }
    // Partly decorrelating, smooth: sand sheets, whose surface moves a
    // little between passes.
    if (0.30..0.65).contains(&p.coh) && p.slope_max < 15.0 && p.vv < -12.0 {
        c = SHEET;
    }
    // Decorrelated: mobile sand. VV may be bright on slip faces (Erg
    // Chegaga), so backscatter does not enter.
    if p.coh < 0.30 && p.slope_max < 25.0 {
        c = DUNE;
    }
    // Mountain, steep rock: coherent ground with relief.
    if (p.relief_1k > 25.0 || p.slope_max > 18.0) && p.coh >= 0.45 {
        c = MOUNT;
    }
    c
}

/// Whether a pixel could be a dry-lake bed: extremely flat, smooth over a
/// kilometre, pale, bare. Only a connected bed of [`LAKE_MIN_PX`] is one.
fn lake_bed(p: &Px) -> bool {
    p.slope_max < 0.45 && p.relief_1k < 0.3 && p.albedo > 0.215 && p.ndvi < 0.12
}

/// The classes of a grid of features: [`classify_px`], then dry lakes,
/// wadis and vegetation over it, then the erg guard. `inside` marks the
/// pixels the cut classifies; the rest are 0.
pub fn classify(px: &Grid<Px>, inside: &[bool], rules: Rules) -> Grid<u8> {
    let (w, h) = (px.w, px.h);
    let mut cls = Grid {
        w,
        h,
        v: px.v.iter().map(classify_px).collect(),
    };
    // Dry lakes: big flat pale beds.
    let bed: Vec<bool> = px.v.iter().map(lake_bed).collect();
    let (label, sizes) = components(&bed, w, h);
    for i in 0..w * h {
        if label[i] != 0 && sizes[label[i] as usize] >= LAKE_MIN_PX {
            cls.v[i] = LAKE;
        }
    }
    // Wadi beds: DEM flow lines with a catchment over about 10 km² (2,300
    // pixels) where the surface differs from the plain around (less
    // coherent, paler or greener); the largest (over 220 km²) as a wider
    // band.
    let big: Vec<bool> = px.v.iter().map(|p| p.acc >= 50_000.0).collect();
    for i in 0..w * h {
        let p = &px.v[i];
        if p.slope_max >= 15.0 || cls.v[i] == LAKE {
            continue;
        }
        let differs = p.coh < 0.85 || p.albedo > 0.205 || p.ndvi > 0.10;
        let (x, y) = (i % w, i / w);
        let near_big = big[i]
            || (x > 0 && big[i - 1])
            || (x + 1 < w && big[i + 1])
            || (y > 0 && big[i - w])
            || (y + 1 < h && big[i + w]);
        if (p.acc >= 2_300.0 && differs) || near_big {
            cls.v[i] = WADI;
        }
    }
    // Vegetation last: oases and palm groves.
    for i in 0..w * h {
        if px.v[i].ndvi > 0.13 {
            cls.v[i] = VEG;
        }
    }
    if rules.erg_guard {
        // The pilot's erg: dune sand on more than 35 % of the 23 x 23
        // pixels (1.5 km) around.
        let dune = Grid {
            w,
            h,
            v: cls
                .v
                .iter()
                .map(|&c| f32::from(u8::from(c == DUNE)))
                .collect(),
        };
        let frac = box_mean(&dune, 23);
        for i in 0..w * h {
            if cls.v[i] == MOUNT && frac.v[i] > 0.35 {
                cls.v[i] = DUNE;
            }
        }
    }
    for (c, &ins) in cls.v.iter_mut().zip(inside) {
        if !ins {
            *c = 0;
        }
    }
    cls
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px() -> Px {
        Px {
            coh: 0.8,
            vv: -12.0,
            albedo: 0.18,
            ndvi: 0.05,
            slope_max: 2.0,
            relief_1k: 3.0,
            tpi_5k: -2.0,
            acc: 1.0,
        }
    }

    #[test]
    fn each_rule_names_its_class() {
        assert_eq!(classify_px(&px()), REG);
        assert_eq!(classify_px(&Px { vv: -6.0, ..px() }), HAMADA);
        assert_eq!(
            classify_px(&Px {
                albedo: 0.25,
                vv: -10.0,
                tpi_5k: 5.0,
                ..px()
            }),
            HAMADA
        );
        assert_eq!(
            classify_px(&Px {
                albedo: 0.19,
                vv: -18.0,
                ..px()
            }),
            SHEET
        );
        assert_eq!(
            classify_px(&Px {
                coh: 0.5,
                vv: -14.0,
                ..px()
            }),
            SHEET
        );
        assert_eq!(classify_px(&Px { coh: 0.2, ..px() }), DUNE);
        assert_eq!(
            classify_px(&Px {
                slope_max: 22.0,
                ..px()
            }),
            MOUNT
        );
        assert_eq!(
            classify_px(&Px {
                albedo: 0.25,
                vv: -10.0,
                tpi_5k: -5.0,
                ..px()
            }),
            UNC
        );
    }

    /// A star-dune field: decorrelated sand with steep, coherent slip
    /// faces in it. The pilot's rules paint the faces as mountain; the
    /// erg guard keeps them dune sand. Outside an erg a steep coherent
    /// face stays mountain either way.
    #[test]
    fn the_erg_guard_keeps_slip_faces_sand() {
        let (w, h) = (40usize, 40usize);
        let mut g = Grid::new(
            w,
            h,
            Px {
                coh: 0.2,
                slope_max: 10.0,
                ..px()
            },
        );
        for y in 0..h {
            for x in (5..w).step_by(7) {
                g.v[y * w + x] = Px {
                    coh: 0.5,
                    slope_max: 28.0,
                    ..px()
                };
            }
        }
        let inside = vec![true; w * h];
        let pilot = classify(&g, &inside, Rules::PILOT);
        let stage_a = classify(&g, &inside, Rules::STAGE_A);
        let at = 20 * w + 12;
        assert_eq!(pilot.v[at], MOUNT);
        assert_eq!(stage_a.v[at], DUNE);
        // A mountain on a reg plain is not touched.
        let mut m = Grid::new(w, h, px());
        m.v[at] = Px {
            slope_max: 28.0,
            ..px()
        };
        assert_eq!(classify(&m, &inside, Rules::STAGE_A).v[at], MOUNT);
    }

    #[test]
    fn a_dry_lake_needs_its_size() {
        let (w, h) = (40usize, 40usize);
        let bed = Px {
            slope_max: 0.1,
            relief_1k: 0.1,
            albedo: 0.3,
            ..px()
        };
        let mut g = Grid::new(w, h, px());
        for y in 0..h {
            for x in 0..w {
                // A 30 x 20 bed (600 px) and a 10 x 10 one (100 px).
                if (2..32).contains(&x) && (2..22).contains(&y)
                    || (2..12).contains(&x) && (28..38).contains(&y)
                {
                    g.v[y * w + x] = bed;
                }
            }
        }
        let cls = classify(&g, &vec![true; w * h], Rules::STAGE_A);
        assert_eq!(cls.v[10 * w + 10], LAKE);
        assert_ne!(cls.v[30 * w + 5], LAKE);
    }

    #[test]
    fn outside_the_box_is_zero_and_vegetation_wins() {
        let g = Grid::new(4, 1, Px { ndvi: 0.3, ..px() });
        let cls = classify(&g, &[true, true, false, true], Rules::STAGE_A);
        assert_eq!(cls.v, vec![VEG, VEG, 0, VEG]);
    }

    #[test]
    fn the_class_bytes_are_the_maps() {
        use leviculum_map::ground::Class;
        for (b, c) in [
            (DUNE, Class::Dune),
            (SHEET, Class::Sheet),
            (HAMADA, Class::Hamada),
            (REG, Class::Reg),
            (MOUNT, Class::Mountain),
            (LAKE, Class::DryLake),
            (WADI, Class::Wadi),
            (VEG, Class::Vegetation),
            (UNC, Class::Uncertain),
        ] {
            assert_eq!(Class::of(b), Some(c));
        }
    }
}
