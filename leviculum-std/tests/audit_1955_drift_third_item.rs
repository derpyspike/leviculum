//! A drifting citation whose cited file moves a third item under it is
//! refused, although its citing line stands untouched (audit of order 1955,
//! the reviewer's decision of 2026-11-16 21:05).
//!
//! The base cites `beta` at a line `alpha` holds. The change leaves the page
//! alone and puts a new fn `gamma` above `alpha`, so the cited line now
//! holds `gamma`: neither the item the base resolved (`alpha`) nor the one
//! the label names. The guard compared verdicts only (drift stayed drift)
//! and passed it; paired with its base self, the item changed, so it is
//! refused.
//!
//! The audit's own text of this test was not on this host (the order named
//! `/audit/in/requested`, which only the reviewer's host holds); this file
//! carries the order's statement of the case under the name it gives.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, LIB_RS};

#[test]
fn audit_1955_a_drift_whose_file_moves_a_third_item_under_it_is_refused() {
    // Line 2 is `alpha`; `beta` is at line 22.
    let base = page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, 2))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "the base passes:\n{out}");
    // `gamma` takes lines 1-2; `alpha` moves to 5. The page is untouched.
    repo.write(LIB, format!("pub fn gamma() {{\n}}\n\n{LIB_RS}").as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a drift whose cited line now holds a third item (`gamma`) is refused:\n{out}"
    );
    assert!(out.contains("gamma"), "{out}");
}
