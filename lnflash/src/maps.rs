//! `lnflash --region <file.lmr> --port <tty>` and `lnflash --regions
//! --port <tty>` (1477): put map regions onto a ThinkNode M9, and say which
//! it carries.
//!
//! The M9 keeps the regions in a partition of their own, `regions` at
//! 0xC12000, 4,116,480 B (`leviculum-esp/partitions-m9.csv`), as a
//! `leviculum_map::area`: a directory in the first sector, the regions
//! after it at sector boundaries. The board reads it at boot into its PSRAM
//! and names what it found in its `[MAP] regions=` line; where a region
//! lies under the view it wins over the world overview the image carries.
//!
//! The area is built here, every file checked to be a region first, and
//! written whole through the ESP32's ROM loader by espflash, the program
//! the M9's image is flashed with (`just flash-m9`); lnflash has no ROM
//! loader of its own since b737a11b. The write resets the board into the
//! loader and back, so the board boots with its new regions. Several
//! `--region` files go in one area; an area written replaces the one
//! before.
//!
//! Region versions (1714): lnflash says the version of every region it
//! writes, and before writing reads the image's app descriptor, whose
//! version names the newest region version the image reads (`+lmr5`).
//! A region newer than that is refused, since the board would leave it
//! off the map; an image from before 1714 says nothing, and lnflash
//! refuses a version 5 region for it as for no image at all (1919): flash
//! a current image first, or write with `--no-pois`. `--no-pois` writes every
//! region without its points of interest, as version 4: the M9 draws no
//! pois yet, and they cost its flash.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use leviculum_map::area::{self, Entry};
use leviculum_map::region::{self as lmr, Region};

/// Where the area starts in the M9's flash.
pub const AREA_OFFSET: u32 = 0xC1_2000;

/// Its size: up to the client record's second slot at 0xFFF000.
pub const AREA_CAPACITY: usize = 0xFF_F000 - 0xC1_2000;

/// The rate the M9's CH340K holds for the ROM loader (`just flash-m9`).
pub const LOADER_BAUD: u32 = 230_400;

/// Where the M9's image starts: the `factory` partition
/// (`leviculum-esp/partitions-m9.csv`).
pub const APP_OFFSET: u32 = 0x1_0000;

/// What lnflash reads of the image: its first sector, the app descriptor
/// in it.
pub const APP_HEAD_LEN: usize = 0x1000;

/// Where an ESP-IDF app image keeps its app descriptor: after the image
/// header and the first segment's header.
pub const APP_DESC_AT: usize = 0x20;

/// The app descriptor's magic word.
const APP_DESC_MAGIC: u32 = 0xABCD_5432;

/// The tag in the image's version that names the newest region version
/// it reads: `0.1.7+lmr5`.
pub const READS_TAG: &str = "+lmr";

/// One region as lnflash writes it: its name on the board, its bytes and
/// the versions they say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub name: String,
    pub bytes: Vec<u8>,
    /// The version the file says.
    pub version: u16,
    /// The oldest reader version that reads it.
    pub min_reader: u16,
    /// The file's length before `--no-pois` took the pois out, if it did.
    pub stripped_from: Option<usize>,
}

/// The region files `files` as lnflash writes them, each named by its
/// file stem cut to the directory's 20 bytes, with `no_pois` without
/// their points of interest. Refused before any board is touched: a file
/// that is no region, two of one name.
pub fn prepare(files: &[PathBuf], no_pois: bool) -> Result<Vec<Prepared>, String> {
    let mut read: Vec<Prepared> = Vec::new();
    for file in files {
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let not = |e: lmr::RegionError| format!("{}: not a map region ({e:?})", file.display());
        let (bytes, stripped_from) = match no_pois {
            true => {
                let stripped = lmr::without_pois(&bytes).map_err(not)?;
                let from = (stripped.len() != bytes.len()).then_some(bytes.len());
                (stripped, from)
            }
            false => (bytes, None),
        };
        let region = Region::from_bytes(&bytes).map_err(not)?;
        let (version, min_reader) = (region.version(), region.min_reader());
        let name = name_of(file);
        if read.iter().any(|r| r.name == name) {
            return Err(format!("two regions named {name}"));
        }
        read.push(Prepared {
            name,
            bytes,
            version,
            min_reader,
            stripped_from,
        });
    }
    Ok(read)
}

/// The area of the region files `files`, as they are.
pub fn area_of(files: &[PathBuf]) -> Result<Vec<u8>, String> {
    area_from(&prepare(files, false)?)
}

/// The area of `prepared`. Refused before any board is touched: regions
/// past the area.
pub fn area_from(prepared: &[Prepared]) -> Result<Vec<u8>, String> {
    let regions: Vec<(&str, &[u8])> = prepared
        .iter()
        .map(|r| (r.name.as_str(), r.bytes.as_slice()))
        .collect();
    area::build(&regions, AREA_CAPACITY).map_err(|e| match e {
        area::AreaError::Full { need, capacity } => {
            format!("the regions need {need} bytes, the M9's map area holds {capacity}")
        }
        other => format!("the regions do not make an area: {other}"),
    })
}

/// A region's name on the board: its file's stem, at most the
/// directory's 20 bytes, cut at a character boundary.
pub fn name_of(file: &Path) -> String {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("region"));
    let mut end = stem.len().min(area::NAME);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    match end {
        0 => String::from("region"),
        _ => String::from(&stem[..end]),
    }
}

/// What the M9's image says of the region versions it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageReads {
    /// The image names the newest region version it reads (from 1714).
    UpTo { image: String, version: u16 },
    /// An image from before 1714: its version, no word on regions.
    Unsaid { image: String },
    /// No app descriptor where the image starts: an erased partition, or
    /// no ESP-IDF image.
    Unknown,
}

/// What the first bytes of the M9's application partition say of the
/// region versions the image reads.
pub fn image_reads(head: &[u8]) -> ImageReads {
    let Some(desc) = head.get(APP_DESC_AT..APP_DESC_AT + 48) else {
        return ImageReads::Unknown;
    };
    let magic = u32::from_le_bytes([desc[0], desc[1], desc[2], desc[3]]);
    if magic != APP_DESC_MAGIC {
        return ImageReads::Unknown;
    }
    let field = &desc[16..48];
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    let image = String::from_utf8_lossy(&field[..end]).into_owned();
    let tagged = image
        .rfind(READS_TAG)
        .and_then(|at| image[at + READS_TAG.len()..].parse::<u16>().ok());
    match tagged {
        Some(version) => ImageReads::UpTo { image, version },
        None => ImageReads::Unsaid { image },
    }
}

/// Whether `regions` may go to a board whose image says `image`: an
/// error naming the region the image would leave off the map, and the
/// ways out.
///
/// An image that does not say which region version it reads, or no
/// image lnflash knows, takes only regions every reader reads (version
/// 4). An untagged image may be one from 1685 to 1714 that reads version
/// 5, or one from before 1685 that reads only version 4 and would show
/// nothing of the region; lnflash cannot tell them apart, so it refuses
/// rather than write a region the board may silently leave off (1919,
/// the audit of 1714). Writing the image first is the safe order anyway.
pub fn judge(image: &ImageReads, regions: &[Prepared]) -> Result<(), String> {
    let Some(newest) = regions.iter().max_by_key(|r| r.min_reader) else {
        return Ok(());
    };
    let ways_out = "flash a current image first, or write the regions with --no-pois \
                    (region version 4)";
    match image {
        ImageReads::UpTo { image, version } if newest.min_reader > *version => Err(format!(
            "{} is region version {}, which a reader of version {} or newer reads; the M9's \
             image {image} reads up to version {version} and would leave it off the map: \
             {ways_out}",
            newest.name, newest.version, newest.min_reader
        )),
        ImageReads::UpTo { .. } => Ok(()),
        _ if newest.min_reader <= lmr::VERSION_4 => Ok(()),
        ImageReads::Unsaid { image } => Err(format!(
            "the M9's image {image} does not say which region version it reads, and an \
             image older than the version 5 reader would leave {} (region version {}) off \
             the map: {ways_out}",
            newest.name, newest.version
        )),
        ImageReads::Unknown => Err(format!(
            "the M9's application partition holds no image lnflash knows, so nothing says \
             it reads {} (region version {}): {ways_out}",
            newest.name, newest.version
        )),
    }
}

/// What `--region` says once `regions` are written as an area of
/// `area_len` bytes.
pub fn said_written(port: &Path, regions: &[Prepared], area_len: usize) -> String {
    let each: Vec<String> = regions
        .iter()
        .map(|r| {
            let stripped = match r.stripped_from {
                Some(from) => format!(", without its pois, from {from}"),
                None => String::new(),
            };
            format!(
                "{} (region version {}, {} bytes{stripped})",
                r.name,
                r.version,
                r.bytes.len()
            )
        })
        .collect();
    format!(
        "{}: wrote {} into the M9's map area ({area_len} bytes); the board boots with them",
        port.display(),
        each.join(", ")
    )
}

/// The regions the area's first sector names, in the words of a list.
pub fn listed(sector: &[u8]) -> Result<Vec<Entry>, String> {
    match area::directory(sector, AREA_CAPACITY) {
        Ok(entries) => Ok(entries),
        Err(area::AreaError::Empty) => Ok(Vec::new()),
        Err(e) => Err(format!("the M9's map area is not readable: {e}")),
    }
}

/// The lines `--regions` prints for `entries`.
pub fn said(port: &Path, entries: &[Entry]) -> String {
    let mut out = format!(
        "{}: the M9 carries the world overview and Florence in its image",
        port.display()
    );
    match entries.is_empty() {
        true => out.push_str(", and no region in its map area"),
        false => {
            out.push_str(", and in its map area:");
            for e in entries {
                out.push_str(&format!("\n  {} ({} bytes)", e.name, e.len));
            }
        }
    }
    out
}

/// espflash: on the PATH, else in cargo's bin, where `cargo install`
/// puts it.
pub fn espflash() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("espflash"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let home = std::env::var_os("HOME")?;
        let p = PathBuf::from(home).join(".cargo/bin/espflash");
        p.is_file().then_some(p)
    })
}

/// The arguments that write `image` at the area's start on `port`.
pub fn write_args(port: &Path, image: &Path) -> Vec<OsString> {
    write_at_args(port, AREA_OFFSET, image)
}

/// The arguments that write `image` at `offset` in the M9's flash on
/// `port`, through the ROM loader at its rate.
pub fn write_at_args(port: &Path, offset: u32, image: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "write-bin",
        "--chip",
        "esp32s3",
        "--non-interactive",
        "--baud",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(LOADER_BAUD.to_string().into());
    args.push("--port".into());
    args.push(port.into());
    args.push(format!("{offset:#x}").into());
    args.push(image.into());
    args
}

/// The arguments that read the area's directory sector from `port` into
/// `out`.
pub fn read_args(port: &Path, out: &Path) -> Vec<OsString> {
    read_at_args(port, AREA_OFFSET, area::SECTOR, out)
}

/// The arguments that read `len` bytes at `offset` of the M9's flash on
/// `port` into `out`.
pub fn read_at_args(port: &Path, offset: u32, len: usize, out: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "read-flash",
        "--chip",
        "esp32s3",
        "--non-interactive",
        "--baud",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(LOADER_BAUD.to_string().into());
    args.push("--port".into());
    args.push(port.into());
    args.push(format!("{offset:#x}").into());
    args.push(format!("{len:#x}").into());
    args.push(out.into());
    args
}

/// Run espflash with `args`; an error in words when it is missing or
/// fails.
pub(crate) fn run(args: &[OsString]) -> Result<(), String> {
    let program = espflash().ok_or_else(|| {
        String::from("espflash is not installed: `cargo install espflash` puts it in ~/.cargo/bin")
    })?;
    let status = std::process::Command::new(&program)
        .args(args)
        .status()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!("espflash failed ({status})")),
    }
}

/// `--region`: build the area of `files`, with `no_pois` without their
/// points of interest, read which region versions the M9's image on
/// `port` reads, and write the area unless the image would leave a
/// region off the map. What to say.
pub fn write(port: &Path, files: &[PathBuf], no_pois: bool) -> Result<String, String> {
    let regions = prepare(files, no_pois)?;
    let image = area_from(&regions)?;
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    let head = dir.path().join("image-head.bin");
    run(&read_at_args(port, APP_OFFSET, APP_HEAD_LEN, &head))?;
    let head = std::fs::read(&head).map_err(|e| format!("{}: {e}", head.display()))?;
    judge(&image_reads(&head), &regions)?;
    let path = dir.path().join("regions.bin");
    std::fs::write(&path, &image).map_err(|e| format!("{}: {e}", path.display()))?;
    run(&write_args(port, &path))?;
    Ok(said_written(port, &regions, image.len()))
}

/// `--regions`: read the directory from the M9 on `port`. What to say.
pub fn list(port: &Path) -> Result<String, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    let path = dir.path().join("directory.bin");
    run(&read_args(port, &path))?;
    let sector = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(said(port, &listed(&sector)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(rel)
    }

    /// The area of two real region files, read back as the board reads
    /// it: the directory names both, each slice at its entry is the file's
    /// bytes, passes its sum and opens as a region; the image fits the
    /// area. The control: a file that is no region is refused before
    /// anything is built, as is a name given twice.
    #[test]
    fn the_flash_image_holds_the_regions_the_board_reads_back() {
        let firenze = fixture("leviculum-map/fixtures/firenze.lmr");
        let roemer = fixture("leviculum-map/fixtures/frankfurt/roemer.lmr");
        let image = area_of(&[firenze.clone(), roemer.clone()]).expect("an area");
        assert!(image.len() <= AREA_CAPACITY);
        let entries = listed(&image[..area::SECTOR]).expect("a directory");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["firenze", "roemer"]);
        for (entry, file) in entries.iter().zip([&firenze, &roemer]) {
            let at = entry.offset as usize;
            let bytes = &image[at..at + entry.len as usize];
            assert_eq!(bytes, std::fs::read(file).expect("read"));
            assert_eq!(area::check(entry, bytes), Ok(()));
            assert!(Region::from_bytes(bytes).is_ok());
        }
        let said = said(Path::new("/dev/ttyUSB0"), &entries);
        assert!(said.contains("firenze (210647 bytes)"), "{said}");

        let readme = fixture("leviculum-map/fixtures/README.md");
        let refused = area_of(&[readme]).expect_err("no region");
        assert!(refused.contains("not a map region"), "{refused}");
        let twice = area_of(&[firenze.clone(), firenze]).expect_err("twice");
        assert!(twice.contains("two regions named firenze"), "{twice}");
        // An erased area lists nothing.
        assert!(listed(&[0xFF; area::SECTOR]).expect("erased").is_empty());
    }

    /// An app image's first bytes with `version` in its descriptor.
    fn head(version: &str) -> Vec<u8> {
        let mut b = vec![0u8; APP_HEAD_LEN];
        b[0] = 0xE9;
        b[APP_DESC_AT..APP_DESC_AT + 4].copy_from_slice(&APP_DESC_MAGIC.to_le_bytes());
        let at = APP_DESC_AT + 16;
        b[at..at + version.len()].copy_from_slice(version.as_bytes());
        b
    }

    /// A version 5 region with points of interest, cut from the cut's
    /// fixture archive, written to `dir` as `name.lmr`.
    fn v5_region(dir: &Path, name: &str) -> PathBuf {
        struct InMemory(&'static [u8]);
        impl leviculum_mapcut::pmtiles::RangeRead for InMemory {
            fn read_range(&self, offset: u64, len: u64) -> leviculum_mapcut::Res<Vec<u8>> {
                let at = (offset as usize).min(self.0.len());
                let end = at.saturating_add(len as usize).min(self.0.len());
                Ok(self.0[at..end].to_vec())
            }
        }
        let archive = leviculum_mapcut::pmtiles::Archive::from_source(InMemory(include_bytes!(
            "../../leviculum-mapcut/fixtures/oltrarno.pmtiles"
        )))
        .expect("the fixture archive");
        let opts = leviculum_mapcut::cut::CutOptions {
            bounds: leviculum_map::region::Bounds {
                west: 112_615_000,
                south: 437_405_000,
                east: 112_825_000,
                north: 437_545_000,
            },
            zooms: 14..=15,
            profile: 1,
            attribution: "© OpenStreetMap contributors".to_owned(),
            notice: "test notice, ODbL".to_owned(),
            threads: 1,
            locale: None,
            buildings: true,
            pois: true,
            round: leviculum_mapcut::cut::ROUND,
        };
        let mut out = std::io::Cursor::new(Vec::new());
        leviculum_mapcut::cut::cut(&archive, &opts, &mut out).expect("cut");
        let path = dir.join(format!("{name}.lmr"));
        std::fs::write(&path, out.into_inner()).expect("write");
        path
    }

    /// 1714: what the image's descriptor says. A tagged version names the
    /// newest region version the image reads; an untagged one is an image
    /// from before the tag; no descriptor is no image lnflash knows.
    #[test]
    fn the_image_descriptor_says_which_region_version_it_reads() {
        assert_eq!(
            image_reads(&head("0.1.7+lmr5")),
            ImageReads::UpTo {
                image: "0.1.7+lmr5".to_owned(),
                version: 5
            }
        );
        assert_eq!(
            image_reads(&head("0.2.0+lmr12")),
            ImageReads::UpTo {
                image: "0.2.0+lmr12".to_owned(),
                version: 12
            }
        );
        assert_eq!(
            image_reads(&head("0.1.7")),
            ImageReads::Unsaid {
                image: "0.1.7".to_owned()
            }
        );
        assert_eq!(image_reads(&[0xFF; APP_HEAD_LEN]), ImageReads::Unknown);
        assert_eq!(image_reads(&[0u8; 16]), ImageReads::Unknown);
    }

    /// 1714: the M9 image's tag is the number its region reader reads, so
    /// the refusal below compares the right two numbers.
    #[test]
    fn the_m9_image_names_the_region_version_its_reader_reads() {
        let read = |p: &str| std::fs::read_to_string(fixture(p)).expect("the M9 image's source");
        let image = read("leviculum-esp/src/bin/thinknode_m9.rs");
        assert!(
            image.contains("\nleviculum_esp::m9_app_desc!();\n"),
            "the M9 image does not use the descriptor that names its region reader"
        );
        let lib = read("leviculum-esp/src/lib.rs");
        let tag = format!("\"{READS_TAG}{}\"", leviculum_map::region::VERSION);
        assert!(
            lib.contains(&format!("concat!(env!(\"CARGO_PKG_VERSION\"), {tag})")),
            "the M9 image's app descriptor does not carry {tag}"
        );
    }

    /// 1714: a version 5 region goes to an image that reads it; to an
    /// image that says it reads less it is refused by name, and so it is
    /// to an image that says nothing or to no image (1919). With
    /// --no-pois it is version 4, the cut without pois, and goes to any
    /// image without a word; the area says each region's version.
    #[test]
    fn a_region_the_image_cannot_read_is_refused_and_no_pois_writes_version_4() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let v5 = v5_region(dir.path(), "oltrarno");
        let regions = prepare(std::slice::from_ref(&v5), false).expect("prepared");
        assert_eq!((regions[0].version, regions[0].min_reader), (5, 5));
        assert_eq!(regions[0].stripped_from, None);

        let reads = |v: &str| image_reads(&head(v));
        assert_eq!(judge(&reads("0.1.8+lmr5"), &regions), Ok(()));
        let refused = judge(&reads("0.1.8+lmr4"), &regions).expect_err("refused");
        assert!(
            refused.contains("oltrarno is region version 5")
                && refused.contains("reads up to version 4"),
            "{refused}"
        );
        assert_no_double_space(&refused);
        let unsaid = judge(&reads("0.1.7"), &regions).expect_err("refused");
        assert!(unsaid.contains("image 0.1.7 does not say"), "{unsaid}");
        let unknown = judge(&ImageReads::Unknown, &regions).expect_err("refused");
        assert!(unknown.contains("no image lnflash knows"), "{unknown}");

        let stripped = prepare(std::slice::from_ref(&v5), true).expect("stripped");
        let s = &stripped[0];
        assert_eq!((s.version, s.min_reader), (4, 4));
        let original = std::fs::read(&v5).expect("read").len();
        assert_eq!(s.stripped_from, Some(original));
        assert!(s.bytes.len() < original);
        for image in [reads("0.1.7"), reads("0.1.8+lmr4"), ImageReads::Unknown] {
            assert_eq!(judge(&image, &stripped), Ok(()), "{image:?}");
        }
        let area = area_from(&stripped).expect("an area");
        let entries = listed(&area[..area::SECTOR]).expect("a directory");
        let at = entries[0].offset as usize;
        assert!(area[at..at + entries[0].len as usize] == s.bytes[..]);

        let said = said_written(
            Path::new("/dev/ttyUSB0"),
            &[regions[0].clone(), s.clone()],
            area.len(),
        );
        assert!(
            said.contains(&format!("oltrarno (region version 5, {original} bytes)")),
            "{said}"
        );
        assert!(
            said.contains(&format!(
                "oltrarno (region version 4, {} bytes, without its pois, from {original})",
                s.bytes.len()
            )),
            "{said}"
        );
        assert_no_double_space(&said);
        // A version 4 file passes --no-pois as it is.
        let firenze = fixture("leviculum-map/fixtures/firenze.lmr");
        let kept = prepare(std::slice::from_ref(&firenze), true).expect("kept");
        assert_eq!(kept[0].stripped_from, None);
        assert!(kept[0].bytes == std::fs::read(&firenze).expect("read"));
        assert_eq!(judge(&reads("0.1.7"), &kept), Ok(()));
    }

    /// A message lnflash prints has no run of spaces mid-sentence: a
    /// string literal broken over lines without a `\` continuation
    /// carries the next line's indentation into the text (1919).
    fn assert_no_double_space(message: &str) {
        assert!(!message.contains("  "), "two spaces in: {message}");
    }

    /// 1919 (the audit of 1714): an image that does not say which region
    /// version it reads is an image from before the version 5 reader for
    /// all lnflash can tell, and one from before 1685 leaves a version 5
    /// region off the map with no word on the board. So a version 5
    /// region is refused there, and where no image is known at all, by
    /// name and with the two ways out. The control: the same image takes
    /// a version 4 region, and a tagged image that reads version 5 takes
    /// the version 5 one.
    #[test]
    fn an_untagged_image_or_none_refuses_a_version_5_region() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let v5 = v5_region(dir.path(), "oltrarno");
        let regions = prepare(std::slice::from_ref(&v5), false).expect("prepared");
        assert_eq!(regions[0].min_reader, 5);
        for image in [image_reads(&head("0.1.7")), ImageReads::Unknown] {
            let refused = judge(&image, &regions).expect_err("refused");
            assert!(
                refused.contains("oltrarno (region version 5)")
                    && refused.contains("--no-pois")
                    && refused.contains("flash a current image"),
                "{image:?}: {refused}"
            );
            assert_no_double_space(&refused);
        }
        assert!(judge(&image_reads(&head("0.1.7+lmr5")), &regions).is_ok());
        let firenze = fixture("leviculum-map/fixtures/firenze.lmr");
        let v4 = prepare(std::slice::from_ref(&firenze), false).expect("prepared");
        assert_eq!(v4[0].min_reader, 4);
        assert!(judge(&image_reads(&head("0.1.7")), &v4).is_ok());
        assert!(judge(&ImageReads::Unknown, &v4).is_ok());
    }

    /// The commands write and read where the partition table puts the
    /// area, at the loader's rate the M9 is flashed at.
    #[test]
    fn espflash_writes_and_reads_the_area_where_the_table_puts_it() {
        let port = Path::new("/dev/ttyUSB0");
        let write: Vec<String> = write_args(port, Path::new("/x/regions.bin"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            write.join(" "),
            "write-bin --chip esp32s3 --non-interactive --baud 230400 --port /dev/ttyUSB0 \
             0xc12000 /x/regions.bin"
        );
        let read: Vec<String> = read_args(port, Path::new("/x/dir.bin"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            read.join(" "),
            "read-flash --chip esp32s3 --non-interactive --baud 230400 --port /dev/ttyUSB0 \
             0xc12000 0x1000 /x/dir.bin"
        );
        // The image's head, where the table puts the application (1714).
        let head: Vec<String> = read_at_args(port, APP_OFFSET, APP_HEAD_LEN, Path::new("/x/h"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            head.join(" "),
            "read-flash --chip esp32s3 --non-interactive --baud 230400 --port /dev/ttyUSB0 \
             0x10000 0x1000 /x/h"
        );
        // The partition table says the same.
        let table =
            std::fs::read_to_string(fixture("leviculum-esp/partitions-m9.csv")).expect("the table");
        let row = table
            .lines()
            .find(|l| l.starts_with("regions,"))
            .expect("a regions row");
        let cells: Vec<&str> = row.split(',').map(str::trim).collect();
        assert_eq!(cells[3], format!("{AREA_OFFSET:#x}"));
        let app = table
            .lines()
            .find(|l| l.starts_with("factory,"))
            .expect("a factory row");
        let cells_app: Vec<&str> = app.split(',').map(str::trim).collect();
        assert_eq!(cells_app[3], format!("{APP_OFFSET:#x}"));
        assert_eq!(
            usize::from_str_radix(cells[4].trim_start_matches("0x"), 16).expect("hex"),
            AREA_CAPACITY
        );
    }
}
