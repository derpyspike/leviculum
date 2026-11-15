//! Ground packs and their drawing (order 1743): the container with the
//! ground kind's magics, the class tiles and their derived zooms, the
//! parameter table, the legend, and tiles that join.

use leviculum_map::elevation::{
    self, Block, Directory, DirectoryInfo, ElevationError, PackIndex, PackKind, TileDecoder,
    TILE_CELLS, TILE_EDGE,
};
use leviculum_map::ground::{self, paint_row, Class, Classes, GroundStyle, NONE};
use leviculum_map::ground_style::{self, CLASSES};
use ruzstd::encoding::{compress_to_vec, CompressionLevel};

fn frame(cells: &[u8]) -> Vec<u8> {
    compress_to_vec(cells, CompressionLevel::Fastest)
}

/// One z9 block of z11 tiles at block (x, y), each cell's class from
/// `class(col, row)` in z11 pixels of the block.
fn block(x: u32, y: u32, class: impl Fn(usize, usize) -> u8) -> Vec<u8> {
    let mut frames = Vec::new();
    for ty in 0..4 {
        for tx in 0..4 {
            let mut t = vec![0u8; TILE_CELLS];
            for r in 0..TILE_EDGE {
                for c in 0..TILE_EDGE {
                    t[r * TILE_EDGE + c] = class(tx * TILE_EDGE + c, ty * TILE_EDGE + r);
                }
            }
            frames.push(frame(&t));
        }
    }
    elevation::write_block_of(PackKind::Ground, 11, 9, x, y, &frames).expect("block")
}

fn pack(blocks: &[((u32, u32), Vec<u8>)]) -> Vec<u8> {
    let info = DirectoryInfo {
        tile_zoom: 11,
        block_zoom: 9,
        bounds: [-76_000_000, 293_000_000, -37_000_000, 315_000_000],
        attribution: "test attribution",
        notice: "test notice",
    };
    let at: Vec<(u32, u32)> = blocks.iter().map(|(p, _)| *p).collect();
    let mut out = elevation::write_directory_of(PackKind::Ground, &info, &at).expect("directory");
    for (_, b) in blocks {
        out.extend_from_slice(b);
    }
    out
}

/// Stripes of 4 classes, 16 z11 pixels wide.
fn stripes(c: usize, _r: usize) -> u8 {
    [1, 3, 4, 9][(c / 16) % 4]
}

#[test]
fn every_class_has_one_row_in_the_parameter_table() {
    for c in Class::ALL {
        let rows = CLASSES.iter().filter(|r| r.class == c).count();
        assert_eq!(rows, 1, "{c:?} has {rows} rows in ground_style::CLASSES");
        assert_eq!(
            ground_style::of(c).class,
            c,
            "{c:?}: the table is in class order"
        );
    }
    assert_eq!(CLASSES.len(), Class::ALL.len());
}

#[test]
fn the_final_colours_of_the_trial_are_in_the_table() {
    // README_R2.md, "Colour table (R2 and R')", the values Lew decided.
    let dune = ground_style::of(Class::Dune);
    assert_eq!(dune.strong, Some([240, 200, 112]));
    assert_eq!(dune.ink_strong, [198, 138, 56]);
    let hamada = ground_style::of(Class::Hamada);
    assert_eq!(hamada.strong, Some([200, 204, 206]));
    assert_eq!(hamada.ink_strong, [150, 157, 164]);
    assert_eq!(ground_style::of(Class::Reg).strong, Some([228, 219, 200]));
    // Hamada stays a cool grey in both styles: blue over red.
    for c in [hamada.strong, hamada.pastel].into_iter().flatten() {
        assert!(c[2] > c[0], "hamada {c:?} is not cool");
    }
    // Uncertain is untinted in both.
    let unc = ground_style::of(Class::Uncertain);
    assert_eq!((unc.strong, unc.pastel), (None, None));
    assert_eq!(ground_style::SHADOW_FLOOR, 0.62);
}

#[test]
fn the_legend_names_ground_kinds_and_never_promises_a_way_through() {
    let words = [
        "passable",
        "impassable",
        "drivable",
        "driveable",
        "trafficable",
        "navigable",
        "safe",
        "route",
    ];
    let mut texts: Vec<String> = Class::ALL.iter().map(|c| c.name().to_lowercase()).collect();
    texts.push(ground::SOURCE_LINE.to_lowercase());
    for t in &texts {
        for w in words {
            assert!(!t.contains(w), "legend text {t:?} says {w:?}");
        }
    }
    assert!(ground::SOURCE_LINE.contains("30 m"));
    assert!(ground::SOURCE_LINE.contains("zoom 14"));
}

#[test]
fn a_ground_pack_round_trips_and_derives_its_coarser_zooms() {
    let b = block(250, 210, stripes);
    let file = pack(&[((250, 210), b)]);
    let (dir, ranges) = elevation::split_of(PackKind::Ground, &file).expect("split");
    assert_eq!(dir.kind(), PackKind::Ground);
    assert_eq!((dir.tile_zoom(), dir.block_zoom()), (11, 9));
    let index = PackIndex::of_kind(PackKind::Ground, &file).expect("index");
    assert_eq!(index.zooms(), 9..=11);
    let piece = &file[ranges[1].clone()];
    let mut dec = TileDecoder::new();
    let (mut out, mut scratch) = (vec![0u8; TILE_CELLS], vec![0u8; TILE_CELLS]);
    // z11, the stored tile (1, 0) of the block.
    index
        .decode_classes(piece, (11, 1001, 840), &mut dec, &mut out, &mut scratch)
        .expect("z11");
    for (c, &got) in out.iter().enumerate().take(TILE_EDGE) {
        assert_eq!(got, stripes(TILE_EDGE + c, 0));
    }
    // z10: each cell the mode of 2 x 2; stripes 16 wide stay 8 wide.
    index
        .decode_classes(piece, (10, 500, 420), &mut dec, &mut out, &mut scratch)
        .expect("z10");
    for (c, &got) in out.iter().enumerate().take(TILE_EDGE) {
        assert_eq!(got, stripes(2 * c, 0), "z10 column {c}");
    }
    // z9: 4 x 4 stripes 16 wide become 4 wide.
    index
        .decode_classes(piece, (9, 250, 210), &mut dec, &mut out, &mut scratch)
        .expect("z9");
    for (c, &got) in out.iter().enumerate().take(TILE_EDGE) {
        assert_eq!(got, stripes(4 * c, 0), "z9 column {c}");
    }
}

#[test]
fn a_height_reader_refuses_a_ground_pack_and_the_reverse() {
    let file = pack(&[((250, 210), block(250, 210, stripes))]);
    assert_eq!(
        Directory::parse_prefix(&file).map(|_| ()),
        Err(ElevationError::Magic)
    );
    assert!(matches!(
        elevation::split(&file),
        Err(ElevationError::Magic)
    ));
    assert!(matches!(PackIndex::of(&file), Err(ElevationError::Magic)));
    let (_, ranges) = elevation::split_of(PackKind::Ground, &file).expect("split");
    let ground_block = Block::parse_of(PackKind::Ground, &file[ranges[1].clone()]).expect("block");
    let mut dec = TileDecoder::new();
    let (mut h, mut s) = (vec![0i16; TILE_CELLS], vec![0i16; TILE_CELLS]);
    assert_eq!(
        ground_block.heights(&mut dec, 11, 1000, 840, &mut h, &mut s),
        Err(ElevationError::Kind)
    );
    // And a height pack is no ground pack.
    let chebbi = include_bytes!("../fixtures/erg-chebbi.lmh");
    assert!(matches!(
        elevation::split_of(PackKind::Ground, chebbi),
        Err(ElevationError::Magic)
    ));
    let (_, hr) = elevation::split(chebbi).expect("heights");
    let hb = Block::parse(&chebbi[hr[1].clone()]).expect("height block");
    let (mut o, mut sc) = (vec![0u8; TILE_CELLS], vec![0u8; TILE_CELLS]);
    let (x, y) = hb.position();
    let z = hb.tile_zoom();
    let shift = z - hb.block_zoom();
    assert_eq!(
        hb.classes(&mut dec, z, x << shift, y << shift, &mut o, &mut sc),
        Err(ElevationError::Kind)
    );
}

#[test]
fn a_ground_tile_of_the_wrong_length_is_refused() {
    // A frame of a height tile's length in a ground block.
    let long = frame(&vec![1u8; TILE_CELLS * 2]);
    let mut frames = vec![frame(&vec![1u8; TILE_CELLS]); 16];
    frames[0] = long;
    let b = elevation::write_block_of(PackKind::Ground, 11, 9, 250, 210, &frames).expect("block");
    let file = pack(&[((250, 210), b)]);
    let index = PackIndex::of_kind(PackKind::Ground, &file).expect("index");
    let (_, ranges) = elevation::split_of(PackKind::Ground, &file).expect("split");
    let mut dec = TileDecoder::new();
    let (mut out, mut scratch) = (vec![0u8; TILE_CELLS], vec![0u8; TILE_CELLS]);
    let got = index.decode_classes(
        &file[ranges[1].clone()],
        (11, 1000, 840),
        &mut dec,
        &mut out,
        &mut scratch,
    );
    assert!(matches!(got, Err(ElevationError::Tile { index: 0, .. })));
}

/// Classes from a function of the global sample position.
struct Field<F: Fn(u8, u32, u32) -> u8>(F);

impl<F: Fn(u8, u32, u32) -> u8> Classes for Field<F> {
    fn class(&mut self, zoom: u8, x: u32, y: u32) -> u8 {
        (self.0)(zoom, x, y)
    }
}

/// Hamada with dune patches, at z11 samples.
fn desert(_z: u8, x: u32, y: u32) -> u8 {
    if (x / 5 + y / 7).is_multiple_of(3) {
        Class::Dune as u8
    } else if (x / 9) % 4 == 1 {
        Class::Uncertain as u8
    } else {
        Class::Hamada as u8
    }
}

fn paint(style: GroundStyle, zoom: u8, x0: u64, y: u64, w: usize) -> Vec<[u8; 3]> {
    let mut px = vec![[245, 243, 238]; w];
    paint_row(style, zoom, x0, y, &mut Field(desert), &mut px);
    px
}

#[test]
fn tiles_join_without_a_seam() {
    // Two 256 px tiles side by side painted apart equal the 512 px row
    // painted at once, at the stored zoom and above it (the stones and
    // dots sit on the global pixel grid).
    for zoom in [11u8, 13, 14] {
        for style in [GroundStyle::Pastel, GroundStyle::Strong] {
            for y in [512_000u64, 512_001, 512_006] {
                let x0 = 1_001 * 256;
                let whole = paint(style, zoom, x0, y, 512);
                let mut halves = paint(style, zoom, x0, y, 256);
                halves.extend(paint(style, zoom, x0 + 256, y, 256));
                assert_eq!(whole, halves, "zoom {zoom} {style:?} row {y}");
            }
        }
    }
}

#[test]
fn hamada_carries_stones_in_both_styles_and_the_styles_differ() {
    // A hamada-only field: some pixels of a row carry a stone, most
    // carry the class colour itself.
    let mut f = Field(|_, _, _| Class::Hamada as u8);
    for style in [GroundStyle::Pastel, GroundStyle::Strong] {
        let base = match style {
            GroundStyle::Pastel => ground_style::of(Class::Hamada).pastel,
            GroundStyle::Strong => ground_style::of(Class::Hamada).strong,
        }
        .expect("hamada is tinted");
        let mut marks = 0;
        let mut plain = 0;
        for y in 0..28u64 {
            let mut px = vec![[255, 255, 255]; 280];
            assert!(paint_row(
                style,
                13,
                1_096_000,
                1_000_000 + y,
                &mut f,
                &mut px
            ));
            for p in &px {
                if *p == base {
                    plain += 1;
                } else {
                    marks += 1;
                }
            }
        }
        // About 0.72 x 2 pixels per 49: 3 % to 6 %.
        let share = marks as f32 / (marks + plain) as f32;
        assert!((0.02..0.07).contains(&share), "{style:?} stones {share}");
    }
    let p = paint(GroundStyle::Pastel, 12, 2_000_000, 1_000_000, 64);
    let s = paint(GroundStyle::Strong, 12, 2_000_000, 1_000_000, 64);
    assert_ne!(p, s);
}

#[test]
fn uncertain_and_outside_leave_the_paper() {
    let paper = [245, 243, 238];
    let mut px = vec![paper; 64];
    let mut f = Field(|_, _, _| Class::Uncertain as u8);
    paint_row(GroundStyle::Strong, 12, 0, 1_000_000, &mut f, &mut px);
    assert!(px.iter().all(|p| *p == paper));
    let mut none = Field(|_, _, _| NONE);
    assert!(!paint_row(
        GroundStyle::Strong,
        12,
        0,
        1_000_000,
        &mut none,
        &mut px
    ));
    assert!(px.iter().all(|p| *p == paper));
    // Above the block zoom a view spans more than a block: no ground.
    let mut h = Field(|_, _, _| Class::Hamada as u8);
    assert!(!paint_row(
        GroundStyle::Strong,
        8,
        0,
        1_000,
        &mut h,
        &mut px
    ));
}

#[test]
fn class_borders_are_soft() {
    // A vertical border between dune (west) and reg (east) at z13: the
    // pixels next to it are blends, not one colour or the other.
    let mut f = Field(|_, x: u32, _| {
        if x < 1_000 {
            Class::Dune as u8
        } else {
            Class::Reg as u8
        }
    });
    let mut px = vec![[0, 0, 0]; 64];
    // z13 is four display pixels per z11 sample; sample 1000 starts at
    // pixel 4000.
    paint_row(GroundStyle::Pastel, 13, 4_000 - 32, 1_001, &mut f, &mut px);
    let dune = ground_style::of(Class::Dune).pastel.expect("dune");
    let reg = ground_style::of(Class::Reg).pastel.expect("reg");
    let blends = px.iter().filter(|p| **p != dune && **p != reg).count();
    assert!(blends >= 3, "{blends} blended pixels at the border");
}

/// The committed fixtures (one z9 block each, cut by `leviculum-map-tool
/// ground` from the Morocco pilot box with the C zstd library at level
/// 19) read whole through the pure-Rust decoder: every tile at every zoom
/// the block serves, and every byte a class or the outside.
#[test]
fn the_cut_fixtures_read_at_every_zoom() {
    for (name, bytes) in [
        (
            "chebbi",
            &include_bytes!("../fixtures/ground-chebbi-z9.lmg")[..],
        ),
        (
            "chegaga",
            &include_bytes!("../fixtures/ground-chegaga-z9.lmg")[..],
        ),
        (
            "bani",
            &include_bytes!("../fixtures/ground-bani-z9.lmg")[..],
        ),
    ] {
        let (dir, ranges) = elevation::split_of(PackKind::Ground, bytes).expect(name);
        assert_eq!(dir.len(), 1, "{name}");
        assert!(dir.attribution().contains("Kellndorfer"), "{name}");
        assert!(
            dir.notice().contains("never whether it can be crossed"),
            "{name}"
        );
        let index = PackIndex::of_kind(PackKind::Ground, bytes).expect(name);
        let piece = &bytes[ranges[1].clone()];
        let mut dec = TileDecoder::new();
        let (mut out, mut scratch) = (vec![0u8; TILE_CELLS], vec![0u8; TILE_CELLS]);
        let mut seen = [0u64; 10];
        for zoom in index.zooms() {
            for (x, y) in index.tiles(zoom) {
                index
                    .decode_classes(piece, (zoom, x, y), &mut dec, &mut out, &mut scratch)
                    .expect(name);
                for &b in &out {
                    assert!(b == NONE || Class::of(b).is_some(), "{name}: byte {b}");
                    seen[usize::from(b)] += 1;
                }
            }
        }
        // A desert block holds several classes.
        assert!(
            seen[1..].iter().filter(|&&n| n > 0).count() >= 4,
            "{name}: {seen:?}"
        );
    }
}

/// Inside a class the row painter takes its short way (the class colour
/// and signature, no blend); it gives what the general blend gives for
/// four equal samples, to within one step of rounding, for every class
/// in both styles.
#[test]
fn the_short_way_inside_a_class_is_the_blend() {
    for class in Class::ALL {
        for style in [GroundStyle::Pastel, GroundStyle::Strong] {
            let b = class as u8;
            let mut f = Field(move |_, _, _| b);
            let under = [245, 243, 238];
            for y in 1_000_000..1_000_014u64 {
                let x0 = 2_000_000u64;
                let mut px = vec![under; 96];
                paint_row(style, 13, x0, y, &mut f, &mut px);
                for (i, p) in px.iter().enumerate() {
                    let want = ground::pixel(
                        style,
                        under,
                        [(b, 0.25), (b, 0.25), (b, 0.25), (b, 0.25)],
                        b,
                        (x0 + i as u64, y),
                    );
                    for k in 0..3 {
                        assert!(
                            (i16::from(p[k]) - i16::from(want[k])).abs() <= 1,
                            "{class:?} {style:?} at {i},{y}: {p:?} against {want:?}"
                        );
                    }
                }
            }
        }
    }
}
