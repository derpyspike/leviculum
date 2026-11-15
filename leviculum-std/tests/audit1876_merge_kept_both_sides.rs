//! Doc guard: the merges of order 1876 keep lane4's side of the map pages.
//!
//! Both merges of master into lane4 (adcd111b4, 82c84a9ab) resolved their
//! conflicted pages by taking master's whole file. That dropped lane4's
//! passages outside the conflict hunks: rows 48 (`map_relief`) and 49
//! (`map_ground`) of the target table with their two "remaining gaps"
//! bullets, the config paragraph on the relief in the map concept, and the
//! Stage A bullet in the expedition map. The audit of 1876 returned the
//! range for it; order 1889 put the passages back. This test holds them on
//! the head, so a later whole-file resolution that drops them again is red.

use std::fs;
use std::path::{Path, PathBuf};

fn concept(name: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/src/concepts")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn targets_table_has_rows_48_and_49() {
    let page = concept("loquax-targets.md");
    assert!(
        page.lines()
            .any(|l| l.starts_with("| 48 |") && l.contains("map_relief")),
        "loquax-targets.md has no row 48 naming `map_relief`"
    );
    assert!(
        page.lines()
            .any(|l| l.starts_with("| 49 |") && l.contains("map_ground")),
        "loquax-targets.md has no row 49 naming `map_ground`"
    );
}

#[test]
fn remaining_gaps_name_rows_48_and_49() {
    let page = concept("loquax-targets.md");
    let at = page
        .find("## The remaining gaps")
        .unwrap_or_else(|| panic!("loquax-targets.md has no section \"## The remaining gaps\""));
    let section = &page[at..];
    assert!(
        section.contains("row 48"),
        "the remaining gaps do not name row 48"
    );
    assert!(
        section.contains("row 49"),
        "the remaining gaps do not name row 49"
    );
}

#[test]
fn map_concept_names_the_relief_key() {
    assert!(
        concept("loquax-map.md").contains("`map_relief`"),
        "loquax-map.md does not name `map_relief`"
    );
}

#[test]
fn expedition_map_keeps_the_stage_a_bullet() {
    assert!(
        concept("loquax-expedition-map.md").contains("Stage A landed with order 1743"),
        "loquax-expedition-map.md lost \"Stage A landed with order 1743\""
    );
}
