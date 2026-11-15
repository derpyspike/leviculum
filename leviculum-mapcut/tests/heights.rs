//! The height catalogue and the one-block pack a device writes (1863),
//! over the committed GLO-30 fixtures: two real blocks joined into one
//! pack file as the server holds them.

use leviculum_map::elevation::{self, split, Directory, DirectoryInfo, PackIndex, TileDecoder};
use leviculum_map::region::Bounds;
use leviculum_mapcut::heights::{
    block_bounds, local_pack, Catalogue, BLOCK_MAX, CATALOGUE_MAGIC, DIRECTORY_MAX, PACK_MAX,
};

const CHEBBI: &[u8] = include_bytes!("../../leviculum-map/fixtures/erg-chebbi.lmh");
const BANI: &[u8] = include_bytes!("../../leviculum-map/fixtures/bani-z11.lmh");

/// One pack file of the two z11 blocks, Bani first: what a server cuts
/// over an area wider than a block.
fn two_block_pack() -> Vec<u8> {
    let (chebbi, chebbi_r) = split(CHEBBI).expect("chebbi");
    let (bani, bani_r) = split(BANI).expect("bani");
    let blocks = [bani.block(0).expect("b"), chebbi.block(0).expect("c")];
    let mut out = elevation::write_directory(
        &DirectoryInfo {
            tile_zoom: chebbi.tile_zoom(),
            block_zoom: chebbi.block_zoom(),
            bounds: [-70_000_000, 290_000_000, -39_000_000, 315_000_000],
            attribution: chebbi.attribution(),
            notice: chebbi.notice(),
        },
        &blocks,
    )
    .expect("directory");
    out.extend_from_slice(&BANI[bani_r[1].clone()]);
    out.extend_from_slice(&CHEBBI[chebbi_r[1].clone()]);
    out
}

/// Every tile's heights of a pack, in its order.
fn heights_of(pack: &[u8]) -> Vec<Vec<i16>> {
    let index = PackIndex::of(pack).expect("index");
    let mut decoder = TileDecoder::new();
    let mut scratch = vec![0i16; elevation::TILE_CELLS];
    index
        .tiles(index.tile_zoom())
        .into_iter()
        .map(|(x, y)| {
            let range = index.block_for(index.tile_zoom(), x, y).expect("block");
            let mut out = vec![0i16; elevation::TILE_CELLS];
            index
                .decode(
                    &pack[range],
                    (index.tile_zoom(), x, y),
                    &mut decoder,
                    &mut out,
                    &mut scratch,
                )
                .expect("decode");
            out
        })
        .collect()
}

#[test]
fn the_catalogue_names_every_block_at_its_range_and_reads_back() {
    let pack = two_block_pack();
    let mut catalogue = Catalogue::default();
    catalogue.add_pack("morocco.lmh", &pack).expect("add");
    assert_eq!(catalogue.packs.len(), 1);
    assert_eq!(catalogue.packs[0].bytes, pack.len() as u64);
    assert_eq!(catalogue.blocks.len(), 2);
    let (_, ranges) = split(&pack).expect("split");
    assert_eq!(catalogue.packs[0].directory, ranges[0].end as u64);
    for (b, r) in catalogue.blocks.iter().zip(&ranges[1..]) {
        assert_eq!((b.offset, b.len), (r.start as u64, r.len() as u64));
        assert_eq!(b.zoom, 11);
    }
    let text = catalogue.write();
    assert!(text.starts_with(&format!("{CATALOGUE_MAGIC}\t1\n")));
    // Bounds are written for readers that do not project.
    let chebbi = catalogue.blocks[1].bounds();
    assert!(text.contains(&format!(
        "\t{}\t{}\t{}\t{}\n",
        chebbi.west, chebbi.south, chebbi.east, chebbi.north
    )));
    assert_eq!(Catalogue::parse(&text).expect("parse"), catalogue);
}

#[test]
fn a_block_fetched_by_its_range_becomes_a_pack_with_the_same_heights() {
    let pack = two_block_pack();
    let mut catalogue = Catalogue::default();
    catalogue.add_pack("morocco.lmh", &pack).expect("add");
    let entry = catalogue.blocks[1];
    let dir = &pack[..catalogue.packs[0].directory as usize];
    let block = &pack[entry.offset as usize..(entry.offset + entry.len) as usize];
    let local = local_pack(dir, block, &entry).expect("local pack");
    // The Erg Chebbi block alone, as the fixture holds it: same heights.
    assert_eq!(heights_of(&local), heights_of(CHEBBI));
    let (directory, _) = Directory::parse_prefix(&local).expect("directory");
    let b = entry.bounds();
    assert_eq!(directory.bounds(), [b.west, b.south, b.east, b.north]);
    assert_eq!(
        directory.attribution(),
        Directory::parse_prefix(CHEBBI).expect("c").0.attribution()
    );
    assert_eq!(directory.len(), 1);
    assert_eq!(entry.local_name(), "h11-1001-837.lmh");
}

#[test]
fn a_block_that_is_not_the_one_asked_or_is_broken_is_refused() {
    let pack = two_block_pack();
    let mut catalogue = Catalogue::default();
    catalogue.add_pack("morocco.lmh", &pack).expect("add");
    let dir = &pack[..catalogue.packs[0].directory as usize];
    let (bani, chebbi) = (catalogue.blocks[0], catalogue.blocks[1]);
    let chebbi_bytes = &pack[chebbi.offset as usize..(chebbi.offset + chebbi.len) as usize];
    // The bytes of one block under the other's name.
    assert!(local_pack(dir, chebbi_bytes, &bani).is_err());
    // Short by a byte: a range the server cut short.
    assert!(local_pack(dir, &chebbi_bytes[..chebbi_bytes.len() - 1], &chebbi).is_err());
    // A flipped byte inside a tile's frame: its checksum disagrees.
    let mut bad = chebbi_bytes.to_vec();
    let at = bad.len() - 40;
    bad[at] ^= 0x55;
    assert!(local_pack(dir, &bad, &chebbi).is_err());
}

#[test]
fn the_blocks_under_a_box_are_those_it_touches() {
    let pack = two_block_pack();
    let mut catalogue = Catalogue::default();
    catalogue.add_pack("morocco.lmh", &pack).expect("add");
    let chebbi = catalogue.blocks[1];
    let b = chebbi.bounds();
    // The block's own box, and a box inside it.
    assert_eq!(catalogue.under(&b), vec![chebbi]);
    let inner = Bounds {
        west: -39_750_000,
        south: 311_200_000,
        east: -39_700_000,
        north: 311_250_000,
    };
    assert_eq!(catalogue.under(&inner), vec![chebbi]);
    // A box ending one step (a centimetre) short of the block's west
    // edge does not reach into it; the edge itself rounds through the
    // projection within a world unit, so the step is the margin.
    let beside = Bounds {
        west: b.west - 1_000_000,
        south: b.south,
        east: b.west - 1,
        north: b.north,
    };
    assert!(catalogue.under(&beside).is_empty());
    // A box over both deserts holds both blocks.
    let both = Bounds {
        west: -70_000_000,
        south: 290_000_000,
        east: -39_000_000,
        north: 315_000_000,
    };
    assert_eq!(catalogue.under(&both).len(), 2);
    // Berlin holds none.
    let berlin = Bounds {
        west: 133_000_000,
        south: 524_000_000,
        east: 135_000_000,
        north: 526_000_000,
    };
    assert!(catalogue.under(&berlin).is_empty());
}

#[test]
fn a_block_listed_twice_is_fetched_once() {
    let pack = two_block_pack();
    let mut catalogue = Catalogue::default();
    catalogue.add_pack("a.lmh", &pack).expect("a");
    catalogue.add_pack("b.lmh", &pack).expect("b");
    let b = catalogue.blocks[1].bounds();
    let under = catalogue.under(&b);
    assert_eq!(under.len(), 1);
    assert_eq!(under[0].pack, 0, "the first listed wins");
}

#[test]
fn block_bounds_tile_the_world() {
    let a = block_bounds(9, 250, 206);
    let east = block_bounds(9, 251, 206);
    let south = block_bounds(9, 250, 207);
    assert_eq!(a.east, east.west);
    assert_eq!(a.south, south.north);
    assert!(a.west < a.east && a.south < a.north);
    let last = block_bounds(1, 1, 1);
    assert_eq!(last.east, 1_800_000_000);
}

#[test]
fn a_catalogue_that_lies_is_refused() {
    let ok = "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\n";
    assert!(Catalogue::parse(ok).is_ok());
    for (bad, why) in [
        ("lmh-catalogue\t2\n", "a later version"),
        ("hello\n", "no catalogue"),
        ("", "empty"),
        ("lmh-catalogue\t1\npack\t../m.lmh\t1000\t100\n", "a path"),
        ("lmh-catalogue\t1\npack\t.m\t1000\t100\n", "a dot file"),
        (
            "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\npack\tm.lmh\t1\t1\n",
            "twice",
        ),
        (
            "lmh-catalogue\t1\npack\tm.lmh\t100\t1000\n",
            "directory past the end",
        ),
        (
            "lmh-catalogue\t1\nblock\tm.lmh\t9\t1\t1\t100\t10\n",
            "pack not listed",
        ),
        (
            "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\nblock\tm.lmh\t9\t1\t1\t990\t20\n",
            "past the pack's end",
        ),
        (
            "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\nblock\tm.lmh\t9\t1\t1\t50\t20\n",
            "inside the directory",
        ),
        (
            "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\nblock\tm.lmh\t9\t512\t1\t100\t20\n",
            "outside the grid",
        ),
        (
            "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\nblock\tm.lmh\t9\t1\t1\t100\t0\n",
            "empty block",
        ),
        (
            "lmh-catalogue\t1\npack\tm.lmh\tmany\t100\n",
            "a word for a number",
        ),
    ] {
        assert!(Catalogue::parse(bad).is_err(), "{why}");
    }
    // A kind of line a later version adds is skipped.
    let later = format!("{ok}shade\tm.lmh\t1\n");
    assert_eq!(Catalogue::parse(&later).expect("later").packs.len(), 1);
    // Adding a file that is no pack, or a name twice, is refused too.
    let mut c = Catalogue::default();
    assert!(c.add_pack("x.lmh", b"not a pack").is_err());
    assert!(c.add_pack("../x.lmh", CHEBBI).is_err());
    c.add_pack("x.lmh", CHEBBI).expect("x");
    assert!(c.add_pack("x.lmh", CHEBBI).is_err());
}

/// Audit 1863, round 2: a catalogue may not make a device read a block of
/// any size it names into memory. An 8 GiB pack with one 4 GiB block is
/// refused when the catalogue is read, before any fetch.
#[test]
fn a_catalogue_naming_a_huge_block_is_refused() {
    let hostile =
        "lmh-catalogue\t1\npack\tm.lmh\t8589934592\t100\nblock\tm.lmh\t9\t1\t1\t100\t4294967296\n";
    assert!(
        Catalogue::parse(hostile).is_err(),
        "a 4 GiB block was accepted"
    );
}

#[test]
fn a_catalogue_with_another_magic_is_refused() {
    assert!(Catalogue::parse("other\t1\n").is_err());
    assert!(Catalogue::parse("other\t1\npack\tm.lmh\t1000\t100\n").is_err());
}

/// The boundaries of the catalogue's own checks, each on its edge: a pack
/// that is all directory, a block at the deepest zoom a pack may have, and
/// one zoom past it.
#[test]
fn the_catalogue_checks_hold_on_their_edges() {
    let all_directory = "lmh-catalogue\t1\npack\tm.lmh\t100\t100\n";
    assert!(Catalogue::parse(all_directory).is_ok(), "directory = pack");
    let deepest = format!(
        "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\nblock\tm.lmh\t{}\t1\t1\t100\t20\n",
        elevation::MAX_TILE_ZOOM
    );
    assert_eq!(Catalogue::parse(&deepest).expect("deepest").blocks.len(), 1);
    let past = format!(
        "lmh-catalogue\t1\npack\tm.lmh\t1000\t100\nblock\tm.lmh\t{}\t1\t1\t100\t20\n",
        elevation::MAX_TILE_ZOOM + 1
    );
    assert!(Catalogue::parse(&past).is_err(), "a zoom past the deepest");
}

/// A box across the antimeridian (west east of its east) and a box of no
/// width or no height hold no block, even where the block lies under it.
#[test]
fn a_box_across_the_antimeridian_or_without_area_holds_no_block() {
    let pack = two_block_pack();
    let mut catalogue = Catalogue::default();
    catalogue.add_pack("morocco.lmh", &pack).expect("add");
    let b = catalogue.blocks[1].bounds();
    let mid_x = (b.west + b.east) / 2;
    let mid_y = (b.south + b.north) / 2;
    let across = Bounds {
        west: 1_790_000_000,
        south: b.south,
        east: -1_790_000_000,
        north: b.north,
    };
    assert!(
        catalogue.under(&across).is_empty(),
        "across the antimeridian"
    );
    let no_width = Bounds {
        west: mid_x,
        east: mid_x,
        ..b
    };
    assert!(catalogue.under(&no_width).is_empty(), "no width");
    let no_height = Bounds {
        south: mid_y,
        north: mid_y,
        ..b
    };
    assert!(catalogue.under(&no_height).is_empty(), "no height");
}

/// The twin of the huge block: a block of exactly [`BLOCK_MAX`], in a pack
/// of exactly [`PACK_MAX`] with a directory of exactly [`DIRECTORY_MAX`],
/// is read; a byte past any of them is not.
#[test]
fn a_catalogue_at_the_limits_is_read_and_a_byte_past_is_not() {
    let at = |bytes: u64, directory: u64, len: u64| {
        format!(
            "lmh-catalogue\t1\npack\tm.lmh\t{bytes}\t{directory}\nblock\tm.lmh\t9\t1\t1\t{directory}\t{len}\n"
        )
    };
    let edge = Catalogue::parse(&at(PACK_MAX, DIRECTORY_MAX, BLOCK_MAX)).expect("at the limits");
    assert_eq!(edge.blocks[0].len, BLOCK_MAX);
    assert!(Catalogue::parse(&at(PACK_MAX, DIRECTORY_MAX, BLOCK_MAX + 1)).is_err());
    assert!(Catalogue::parse(&at(PACK_MAX + 1, DIRECTORY_MAX, BLOCK_MAX)).is_err());
    assert!(Catalogue::parse(&at(PACK_MAX, DIRECTORY_MAX + 1, BLOCK_MAX)).is_err());
    // The pilot's largest block, 1.81 MB, with room to spare.
    const { assert!(BLOCK_MAX >= 8 * 1_810_000) };
}
