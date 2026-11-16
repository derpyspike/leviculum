//! The poi section format (LMR5, 1685): the writer's bytes read back,
//! and corrupt bytes an error, never a panic.

use leviculum_map::pois::{
    encode_section, Poi, PoiError, PoiRecord, Pois, WayRecord, MIN_ZOOM_UNKNOWN,
};

fn section() -> Vec<u8> {
    encode_section(
        &[
            PoiRecord {
                kind: 3,
                name: Some(7),
                min_zoom: Some(14),
                flags: 0,
                x: 100,
                y: 200,
                attrs: vec![(1, 2), (4, 5)],
            },
            PoiRecord {
                kind: 0,
                name: None,
                min_zoom: None,
                flags: 0,
                x: 4095,
                y: 0,
                attrs: vec![],
            },
        ],
        &[
            WayRecord {
                feature: 9,
                attrs: vec![(6, 2)],
            },
            WayRecord {
                feature: 0,
                attrs: vec![],
            },
        ],
    )
    .expect("encode")
}

#[test]
fn a_section_reads_back_what_was_written() {
    let b = section();
    let s = Pois::parse(&b, 10).expect("parse");
    assert_eq!((s.poi_count(), s.way_count()), (2, 2));
    let pois: Vec<Poi> = s.pois().collect();
    assert_eq!((pois[0].kind, pois[0].name), (3, Some(7)));
    assert_eq!(
        (pois[0].min_zoom, pois[0].x, pois[0].y),
        (Some(14), 100, 200)
    );
    let attrs: Vec<(u32, u32)> = s.attrs_of(&pois[0]).collect();
    assert_eq!(attrs, [(1, 2), (4, 5)]);
    assert_eq!(pois[1].name, None);
    assert_eq!(s.attrs_of(&pois[1]).count(), 0);
    let ways: Vec<_> = s.ways().collect();
    assert_eq!(ways[0].feature, 9);
    let attrs: Vec<(u32, u32)> = s.way_attrs_of(&ways[0]).collect();
    assert_eq!(attrs, [(6, 2)]);
    assert_eq!(s.way_attrs_of(&ways[1]).count(), 0);
}

#[test]
fn corrupt_bytes_are_an_error_not_a_panic() {
    let b = section();
    // Truncated, and one byte long.
    assert_eq!(
        Pois::parse(&b[..b.len() - 1], 10).err(),
        Some(PoiError::Length)
    );
    assert_eq!(Pois::parse(&b[..1], 10).err(), Some(PoiError::Length));
    // A string past the table: the first poi's kind is 3.
    assert!(matches!(
        Pois::parse(&b, 3).err(),
        Some(PoiError::String(_))
    ));
    // An anchor outside the tile.
    let mut bad = section();
    bad[12 + 16..12 + 18].copy_from_slice(&4096i16.to_le_bytes());
    assert_eq!(Pois::parse(&bad, 10).err(), Some(PoiError::Anchor(0)));
    // An attribute run past the attribute array.
    let mut bad = section();
    bad[12 + 12..12 + 14].copy_from_slice(&9u16.to_le_bytes());
    assert_eq!(Pois::parse(&bad, 10).err(), Some(PoiError::Attributes(0)));
}

#[test]
fn an_oversized_record_is_refused_at_the_writer() {
    let too_many = vec![
        PoiRecord {
            kind: 0,
            ..PoiRecord::default()
        };
        usize::from(u16::MAX) + 1
    ];
    assert_eq!(encode_section(&too_many, &[]).err(), Some(PoiError::Count));
}

/// 1714: the section length is computed without overflow. On a 32-bit
/// target `attrs as usize * 8` wrapped for `attrs = 0x2000_0000` (2^32
/// bytes), so an 8-byte head passed the length check and the attribute
/// walk read past the buffer and panicked. Both heads say `Length`: the
/// bare 8 bytes the old check let through, and a 12-byte head whose
/// wrapped sum would come back to its own length on 32 bits.
#[test]
fn an_attribute_count_whose_bytes_wrap_a_32_bit_length_is_a_length_error() {
    let mut eight = vec![0u8; 8];
    eight[4..8].copy_from_slice(&0x2000_0000u32.to_le_bytes());
    assert_eq!(Pois::parse(&eight, 10).err(), Some(PoiError::Length));
    let mut twelve = eight.clone();
    twelve.extend_from_slice(&[12, 20, 8, 8]);
    assert_eq!(Pois::parse(&twelve, 10).err(), Some(PoiError::Length));
}

/// 1714: a missing `min_zoom` is written as "unknown", never as 0, which
/// would rank the point with the most important ones; a known 0 stays 0.
#[test]
fn a_missing_min_zoom_is_written_as_unknown_and_reads_back_as_none() {
    let b = section();
    // The second record's min zoom byte: head 12, record 20, byte 14.
    assert_eq!(b[12 + 20 + 14], MIN_ZOOM_UNKNOWN);
    let s = Pois::parse(&b, 10).expect("parse");
    let ranks: Vec<Option<u8>> = s.pois().map(|p| p.min_zoom).collect();
    assert_eq!(ranks, [Some(14), None]);
    let zero = encode_section(
        &[PoiRecord {
            min_zoom: Some(0),
            ..PoiRecord::default()
        }],
        &[],
    )
    .expect("encode");
    let s = Pois::parse(&zero, 1).expect("parse");
    assert_eq!(s.pois().next().map(|p| p.min_zoom), Some(Some(0)));
}

/// The section a newer writer might write (1714): a longer head, every
/// record longer by `grow` bytes, unknown flag bits, and bytes after the
/// attribute array. Kinds, names, keys and values are string indices,
/// so an unknown kind or key is any index whose string this reader has
/// no meaning for.
fn future_section(grow: usize, tail: &[u8]) -> Vec<u8> {
    let (head, poi, way, attr) = (12 + grow, 20 + grow, 8 + grow, 8 + grow);
    let mut b = Vec::new();
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&3u32.to_le_bytes());
    b.extend_from_slice(&[head as u8, poi as u8, way as u8, attr as u8]);
    b.resize(head, 0xAB);
    // The poi: kind 8 (say `yurt`, a kind nobody knew), name 9, its two
    // attributes from 0, min zoom 13, flags with every bit set.
    let at = b.len();
    b.extend_from_slice(&8u32.to_le_bytes());
    b.extend_from_slice(&9u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&[13, 0xFF]);
    b.extend_from_slice(&7i16.to_le_bytes());
    b.extend_from_slice(&4000i16.to_le_bytes());
    b.resize(at + poi, 0xCD);
    // The way: feature 5, one attribute from 2.
    let at = b.len();
    b.extend_from_slice(&5u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u32.to_le_bytes());
    b.resize(at + way, 0xEF);
    // Three attributes: an unknown key 1 with value 2, then 3=4, 5=6.
    for (k, v) in [(1u32, 2u32), (3, 4), (5, 6)] {
        let at = b.len();
        b.extend_from_slice(&k.to_le_bytes());
        b.extend_from_slice(&v.to_le_bytes());
        b.resize(at + attr, 0x11);
    }
    b.extend_from_slice(tail);
    b
}

#[test]
fn a_newer_writers_longer_records_and_tail_read_back_their_known_part() {
    for (grow, tail) in [
        (0, &[][..]),
        (0, &[1, 2, 3][..]),
        (4, &[][..]),
        (9, &[7; 40][..]),
    ] {
        let b = future_section(grow, tail);
        let s = Pois::parse(&b, 10).unwrap_or_else(|e| panic!("grow {grow}: {e:?}"));
        let pois: Vec<Poi> = s.pois().collect();
        assert_eq!(pois.len(), 1, "grow {grow}");
        let p = pois[0];
        assert_eq!((p.kind, p.name, p.min_zoom), (8, Some(9), Some(13)));
        assert_eq!((p.flags, p.x, p.y), (0xFF, 7, 4000));
        let attrs: Vec<(u32, u32)> = s.attrs_of(&p).collect();
        assert_eq!(attrs, [(1, 2), (3, 4)], "grow {grow}");
        let ways: Vec<_> = s.ways().collect();
        assert_eq!(ways.len(), 1);
        assert_eq!(ways[0].feature, 5);
        let attrs: Vec<(u32, u32)> = s.way_attrs_of(&ways[0]).collect();
        assert_eq!(attrs, [(5, 6)], "grow {grow}");
    }
    // Control: a section cut short of its records is still refused, and
    // so is a head that says a record is shorter than this reader's.
    let b = future_section(4, &[]);
    assert_eq!(
        Pois::parse(&b[..b.len() - 1], 10).err(),
        Some(PoiError::Length)
    );
    let mut b = future_section(0, &[]);
    b[9] = 19;
    assert_eq!(Pois::parse(&b, 10).err(), Some(PoiError::Length));
}

/// 1919 (the audit of 1714): each length in the head, one below this
/// reader's, is a length error; the unchanged section reads.
#[test]
fn each_head_length_below_the_readers_is_a_length_error() {
    let b = section();
    assert_eq!(&b[8..12], &[12, 20, 8, 8]);
    assert!(Pois::parse(&b, 10).is_ok(), "the unchanged section reads");
    for at in 8..12 {
        let mut short = b.clone();
        short[at] -= 1;
        assert_eq!(
            Pois::parse(&short, 10).err(),
            Some(PoiError::Length),
            "byte {at} at {}",
            short[at]
        );
    }
}

/// 1919: the attribute walk steps by the head's attribute length. With
/// 12-byte attributes, a string index past the table in the third one
/// is found there; the section as written reads.
#[test]
fn a_bad_string_in_the_third_of_longer_attributes_is_found() {
    let b = future_section(4, &[]);
    assert_eq!(b[11], 12);
    assert!(Pois::parse(&b, 10).is_ok());
    let third = b.len() - 12;
    assert_eq!(&b[third..third + 4], &5u32.to_le_bytes());
    let mut bad = b.clone();
    bad[third..third + 4].copy_from_slice(&50u32.to_le_bytes());
    assert_eq!(Pois::parse(&bad, 10).err(), Some(PoiError::String(50)));
}
