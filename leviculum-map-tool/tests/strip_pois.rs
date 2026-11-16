//! `leviculum-map-tool strip-pois` (1714): a region cut with its points
//! of interest, stripped of them, is the region cut without them, byte
//! for byte, and says the version it wrote.

use std::path::PathBuf;
use std::process::Command;

fn tool(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_leviculum-map-tool"))
        .args(args)
        .output()
        .expect("spawn leviculum-map-tool");
    assert!(
        out.status.success(),
        "{args:?}: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_region_stripped_of_its_pois_is_the_cut_without_them() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../leviculum-mapcut/fixtures/oltrarno.pmtiles");
    let fixture = fixture.to_str().expect("utf-8 path");
    let path = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
    let (with, without, stripped) = (path("with.lmr"), path("without.lmr"), path("stripped.lmr"));
    for (out, pois) in [(&with, "true"), (&without, "false")] {
        tool(&[
            "cut",
            "-o",
            out,
            fixture,
            "--bbox",
            "11.2615,43.7405,11.2825,43.7545",
            "--min-zoom",
            "14",
            "--max-zoom",
            "15",
            "--pois",
            pois,
        ]);
    }
    let said = tool(&["strip-pois", &with, "-o", &stripped]);
    let read = |p: &str| std::fs::read(p).expect("read");
    assert_eq!(u16::from_le_bytes([read(&with)[4], read(&with)[5]]), 5);
    assert!(
        read(&stripped) == read(&without),
        "the strip is the cut without pois"
    );
    assert!(said.contains("region version 4"), "{said}");
    assert!(said.contains("from version 5"), "{said}");
}
