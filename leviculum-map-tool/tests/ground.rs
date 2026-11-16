//! The ground cut end to end on synthetic inputs laid out as the buckets
//! are (order 1743): a height pack, a Kellndorfer cell, a GeoMAD tile and
//! its STAC answer in a local directory. The cut is deterministic whatever
//! its thread count, writes zero outside the box and classes inside it,
//! and the pack reads back through the map's own reader.

use leviculum_map::elevation::{
    heights_to_planes, write_block, write_directory, DirectoryInfo, PackIndex, PackKind,
    TileDecoder, TILE_BYTES, TILE_CELLS, TILE_EDGE,
};
use leviculum_map::ground::Class;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map_tool::ground::rules::Rules;
use leviculum_map_tool::ground::sources::{
    ease2, geomad_items, DirFetch, Fetch, Geomad, Heights, Kell,
};
use leviculum_map_tool::ground::tiff::write_test_tiff;
use leviculum_map_tool::ground::{blocks_of, cut_ground, GroundOptions};

const BBOX: [f64; 4] = [-3.95, 31.05, -3.80, 31.20];

/// One z12 height tile under the box's middle: a plain falling east with
/// a ridge across it.
fn height_pack(dir: &std::path::Path) -> std::path::PathBuf {
    let at = project(LatLon::from_degrees(31.12, -3.88));
    let (tx, ty) = (at.x >> 20, at.y >> 20);
    let mut h = vec![0i16; TILE_CELLS];
    for r in 0..TILE_EDGE {
        for c in 0..TILE_EDGE {
            let ridge = if (120..136).contains(&r) { 60 } else { 0 };
            h[r * TILE_EDGE + c] = (800 - c as i32 / 4 + ridge) as i16;
        }
    }
    let mut planes = vec![0u8; TILE_BYTES];
    heights_to_planes(&h, &mut planes);
    let frame =
        ruzstd::encoding::compress_to_vec(&planes[..], ruzstd::encoding::CompressionLevel::Fastest);
    let mut pack = write_directory(
        &DirectoryInfo {
            tile_zoom: 12,
            block_zoom: 12,
            bounds: [0, 0, 1, 1],
            attribution: "synthetic",
            notice: "synthetic",
        },
        &[(tx, ty)],
    )
    .expect("directory");
    pack.extend(write_block(12, 12, tx, ty, &[frame]).expect("block"));
    let path = dir.join("h.lmh");
    std::fs::write(&path, pack).expect("write");
    path
}

/// Kellndorfer's cell N32W004 (31..32 N, 4..3 W), 120 pixels a side:
/// coherent ground in the north half, decorrelated sand in the south.
fn kellndorfer(dir: &std::path::Path) {
    let n = 120usize;
    let cell = dir.join("kell/N32W004");
    std::fs::create_dir_all(&cell).expect("dir");
    for layer in [
        "summer_vv_COH12",
        "winter_vv_COH12",
        "spring_vv_COH12",
        "fall_vv_COH12",
    ] {
        let v: Vec<u16> = (0..n * n)
            .map(|i| if i / n < 100 { 80 } else { 20 })
            .collect();
        std::fs::write(
            cell.join(format!("N32W004_{layer}.tif")),
            write_test_tiff(n, n, &v, 8, 5, [1.0 / 120.0, 1.0 / 120.0], [-4.0, 32.0]),
        )
        .expect("write");
    }
    // VV -12 dB: DN = 10^((-12 + 83) / 20).
    let dn = 10f64.powf(71.0 / 20.0).round() as u16;
    std::fs::write(
        cell.join("N32W004_summer_vv_AMP.tif"),
        write_test_tiff(
            n,
            n,
            &vec![dn; n * n],
            16,
            5,
            [1.0 / 120.0, 1.0 / 120.0],
            [-4.0, 32.0],
        ),
    )
    .expect("write");
}

/// One GeoMAD tile of 40 m pixels over the box and its STAC answer: dark
/// bare ground (albedo 0.15) with a green strip (an oasis).
fn geomad(dir: &std::path::Path) {
    let (x0, y0) = ease2(31.3, -4.0);
    let (x1, y1) = ease2(30.95, -3.7);
    let (x0, y0) = ((x0 / 40.0).floor() * 40.0, (y0 / 40.0).ceil() * 40.0);
    let (w, h) = (
        ((x1 - x0) / 40.0).ceil() as usize,
        ((y0 - y1) / 40.0).ceil() as usize,
    );
    let path = dir.join("geomad/gm");
    std::fs::create_dir_all(&path).expect("dir");
    let mut assets = String::new();
    for (band, plain, green) in [
        ("B02", 1400, 300),
        ("B03", 1500, 600),
        ("B04", 1600, 300),
        ("B08", 1800, 4000),
    ] {
        let v: Vec<u16> = (0..w * h)
            .map(|i| if (i % w) * 10 / w == 5 { green } else { plain })
            .collect();
        std::fs::write(
            path.join(format!("t_{band}.tif")),
            write_test_tiff(w, h, &v, 16, 8, [40.0, 40.0], [x0, y0]),
        )
        .expect("write");
        if !assets.is_empty() {
            assets.push(',');
        }
        assets.push_str(&format!(
            "\"{band}\":{{\"href\":\"s3://deafrica-services/gm/t_{band}.tif\"}}"
        ));
    }
    let stac = format!(
        "{{\"features\":[{{\"bbox\":[-4.0,30.95,-3.7,31.3],\"properties\":{{\"proj:transform\":[40.0,0.0,{x0},0.0,-40.0,{y0},0.0,0.0,1.0]}},\"assets\":{{{assets}}}}}]}}"
    );
    std::fs::write(dir.join("geomad/stac.json"), stac).expect("write");
}

fn cut(dir: &std::path::Path, heights: &std::path::Path, threads: usize) -> Vec<u8> {
    let fetch = DirFetch(dir.to_path_buf());
    let items =
        geomad_items(&fetch.stac(BBOX).expect("stac").expect("a catalogue")).expect("items");
    let kell = Kell::new(&fetch);
    let geomad = Geomad::new(&fetch, items, 49.0);
    let heights = Heights::open(heights).expect("heights");
    let mut out = Vec::new();
    cut_ground(
        &heights,
        &kell,
        &geomad,
        &GroundOptions {
            bbox: BBOX,
            rules: Rules::STAGE_A,
            halo: 8,
            level: 3,
            threads,
        },
        &mut out,
    )
    .expect("the cut");
    out
}

#[test]
fn the_cut_is_deterministic_and_classifies_inside_its_box() {
    let dir = tempfile::tempdir().expect("tmp");
    let heights = height_pack(dir.path());
    kellndorfer(dir.path());
    geomad(dir.path());
    let one = cut(dir.path(), &heights, 1);
    let three = cut(dir.path(), &heights, 3);
    assert_eq!(one, three, "the output depends on the thread count");
    let index = PackIndex::of_kind(PackKind::Ground, &one).expect("a ground pack");
    assert_eq!(index.zooms(), 9..=11);
    let blocks = blocks_of(BBOX);
    assert_eq!(blocks.len(), 1);
    let (_, ranges) = leviculum_map::elevation::split_of(PackKind::Ground, &one).expect("split");
    let piece = &one[ranges[1].clone()];
    let mut seen = [0u64; 10];
    let mut dec = TileDecoder::new();
    let (mut out, mut scratch) = (vec![0u8; TILE_CELLS], vec![0u8; TILE_CELLS]);
    let inside = |x: u32, y: u32| {
        // The z11 pixel centre in degrees.
        let u = 1u64 << 13;
        let w = leviculum_map::mercator::World {
            x: (u64::from(x) * u + u / 2) as u32,
            y: (u64::from(y) * u + u / 2) as u32,
        };
        let p = leviculum_map::mercator::unproject(w);
        let (lat, lon) = (f64::from(p.lat_e7) / 1e7, f64::from(p.lon_e7) / 1e7);
        lat > BBOX[1] + 0.001
            && lat < BBOX[3] - 0.001
            && lon > BBOX[0] + 0.001
            && lon < BBOX[2] - 0.001
    };
    let outside = |x: u32, y: u32| {
        let u = 1u64 << 13;
        let w = leviculum_map::mercator::World {
            x: (u64::from(x) * u + u / 2) as u32,
            y: (u64::from(y) * u + u / 2) as u32,
        };
        let p = leviculum_map::mercator::unproject(w);
        let (lat, lon) = (f64::from(p.lat_e7) / 1e7, f64::from(p.lon_e7) / 1e7);
        !(BBOX[1] - 0.001..=BBOX[3] + 0.001).contains(&lat)
            || !(BBOX[0] - 0.001..=BBOX[2] + 0.001).contains(&lon)
    };
    for (tx, ty) in index.tiles(11) {
        index
            .decode_classes(piece, (11, tx, ty), &mut dec, &mut out, &mut scratch)
            .expect("a tile");
        for r in 0..TILE_EDGE as u32 {
            for c in 0..TILE_EDGE as u32 {
                let (x, y) = (tx * 256 + c, ty * 256 + r);
                let b = out[(r * 256 + c) as usize];
                if outside(x, y) {
                    assert_eq!(b, 0, "class {b} outside the box at {x},{y}");
                } else if inside(x, y) {
                    assert_ne!(b, 0, "nothing inside the box at {x},{y}");
                    seen[usize::from(b)] += 1;
                }
            }
        }
    }
    // Coherent dark ground is reg, decorrelated ground dune sand, the
    // green strip vegetation.
    for c in [Class::Reg, Class::Dune, Class::Vegetation] {
        assert!(seen[c as usize] > 100, "{c:?}: {seen:?}");
    }
}
