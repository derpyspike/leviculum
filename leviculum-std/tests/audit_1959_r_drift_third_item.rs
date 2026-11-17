// The audit's text follows, verbatim but for the fn-name prefix (order
// 1965). It was green at 1dc1d43c4 (audit build of 2026-11-17 01:27).

//! Audit of order 1955, text verbatim (only the fn name is prefixed): a
//! drifting citation whose line the change leaves alone, but whose cited
//! file moves another item under it, now points at a third item and must
//! be refused (reviewer 2026-11-16 21:05, point 2).

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, LIB_RS, PAGE};

/// `Thing` and `Other` swapped, same line count: line 9 is `Other::make`,
/// line 17 is `Thing::new`, line 22 is still `beta`.
const SWAPPED: &str = "\
/// Alpha's doc.
pub fn alpha() -> u32 {
    1
}

pub struct Other;

impl Other {
    pub fn make() -> Self {
        Other
    }
}

pub struct Thing;

impl Thing {
    pub fn new() -> Self {
        Thing
    }
}

pub fn beta() {}
";

#[test]
fn audit_1959_r_a_drift_whose_file_moves_a_third_item_under_it_is_refused() {
    // Line 9 is `Thing::new`: a drifting `beta` citation.
    let base = page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, 9))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the unchanged tree passes\n{out}");

    // Positive control: the cited file changes below the cited line;
    // line 9 is still `Thing::new`.
    repo.write(LIB, format!("{}\npub fn omega() {{}}\n", LIB_RS).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "control: a drift whose item stays under it passes\n{out}"
    );

    // The attack: the page stays word for word, the file swaps another
    // item under the cited line.
    repo.write(LIB, SWAPPED.as_bytes());
    let page_now = std::fs::read_to_string(repo.path().join(PAGE)).expect("read page");
    assert_eq!(page_now, base);
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a drifting citation that now points at a third item (Other::make, \
         base Thing::new) must be refused\n{out}"
    );
}
