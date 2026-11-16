//! The header checks that refuse a region file before anything else of
//! it is read (1919, the audit of 1714): a header length that does not
//! match the version, and a version newer than the reader.

use leviculum_map::region::{
    Header, Region, RegionError, HEADER_LEN, HEADER_LEN_5, MAGIC, MIN_READER_AT, VERSION, VERSION_4,
};

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");

/// A version 4 file has the version 4 header length exactly: the
/// Florence fixture with its header length at bytes 6..8 raised by 8 is
/// refused as `Header("header length")`, the unchanged file opens.
#[test]
fn a_version_4_header_length_that_is_not_its_versions_is_refused() {
    assert!(Region::from_bytes(FLORENCE).is_ok());
    assert_eq!(u16::from_le_bytes([FLORENCE[4], FLORENCE[5]]), VERSION_4);
    let mut b = FLORENCE.to_vec();
    let len = u16::from_le_bytes([b[6], b[7]]) + 8;
    b[6..8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(
        Region::from_bytes(&b).err(),
        Some(RegionError::Header("header length"))
    );
}

/// The first 64 bytes of a file of `version`.
fn head(version: u16) -> [u8; HEADER_LEN] {
    let mut b = [0u8; HEADER_LEN];
    b[..4].copy_from_slice(&MAGIC);
    b[4..6].copy_from_slice(&version.to_le_bytes());
    b
}

/// A reader of version 4 refuses every newer version by its number,
/// whatever the file's minimum reader version: it cannot read that
/// field. A reader of version 5 lets a newer version through to the
/// minimum reader check, and still refuses one it does not know below
/// its own.
#[test]
fn a_version_4_reader_refuses_every_newer_version() {
    for v in [5, 6, 200] {
        assert_eq!(
            Header::version_up_to(&head(v), VERSION_4),
            Err(RegionError::Version(v)),
            "version {v}"
        );
    }
    assert_eq!(Header::version_up_to(&head(4), VERSION_4), Ok(4));
    assert_eq!(Header::version_up_to(&head(6), VERSION), Ok(6));
    assert_eq!(Header::version_up_to(&head(200), VERSION), Ok(200));
    assert_eq!(Header::version_up_to(&head(5), VERSION), Ok(5));
    assert_eq!(
        Header::version_up_to(&head(0), VERSION),
        Err(RegionError::Version(0))
    );
}

/// A version 5 header is at least 128 bytes long: one that declares
/// fewer is refused as `Header("header length")`, at the first check and
/// when the file is opened; declared 128 it passes that check.
#[test]
fn a_version_5_header_shorter_than_128_bytes_is_refused() {
    let file = |declared: u16| {
        let mut b = vec![0u8; HEADER_LEN_5];
        b[..HEADER_LEN].copy_from_slice(&head(VERSION));
        b[6..8].copy_from_slice(&declared.to_le_bytes());
        b[MIN_READER_AT..MIN_READER_AT + 2].copy_from_slice(&VERSION.to_le_bytes());
        b
    };
    for declared in [0u16, 112, 127] {
        let b = file(declared);
        assert_eq!(
            Header::reads_as(&b),
            Err(RegionError::Header("header length")),
            "declared {declared}"
        );
        assert_eq!(
            Region::from_bytes(&b).err(),
            Some(RegionError::Header("header length")),
            "declared {declared}"
        );
    }
    assert_eq!(Header::reads_as(&file(128)), Ok((VERSION, HEADER_LEN_5)));
}
