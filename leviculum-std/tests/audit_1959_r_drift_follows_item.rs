// The audit's text follows, verbatim but for the fn-name prefix (order
// 1965). It was green at 1dc1d43c4 (audit build of 2026-11-17 01:27).

//! Audit of order 1955, text verbatim (only the fn name is prefixed): the
//! reviewer's rule (2026-11-16 21:05, point 2) lets a drifting citation
//! stay when it follows its item; it refuses one moved onto a third item.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, LIB_RS, PAGE};

fn drifting(n: usize) -> String {
    page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, n))])
}

#[test]
fn audit_1959_r_a_drift_that_follows_its_item_passes() {
    // Line 9 is `Thing::new`: a drifting `beta` citation.
    let repo = Scratch::new(&drifting(9), &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the unchanged tree passes\n{out}");

    // Three lines above everything: `Thing::new` moves to line 12,
    // `Other::make` to line 20.
    let shifted = format!("pub fn gamma() {{}}\n\npub fn delta() {{}}\n{}", LIB_RS);
    repo.write(LIB, shifted.as_bytes());

    // Negative twin: renumbered onto `Other::make`, a third item.
    repo.write(PAGE, drifting(20).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a drift renumbered onto a third item must be refused\n{out}"
    );

    // The citation follows its item, `Thing::new`, from 9 to 12.
    repo.write(PAGE, drifting(12).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "a drifting citation renumbered with its own item must pass \
         (item(head) == item(base))\n{out}"
    );
}
