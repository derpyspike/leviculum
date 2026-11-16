//! A drifting citation renumbered with its item passes (audit of order
//! 1955, the reviewer's decision of 2026-11-16 21:05).
//!
//! The base cites `beta` at a line `alpha` holds: it drifts, and the item
//! the census resolves there is `alpha`. Lines go in above `alpha` and the
//! number follows it. Paired with its base self through the page's own
//! diff, the citation still resolves to `alpha`, so it passes: the guard
//! asks that a drift not wander onto another item, not that it be written
//! the same.
//!
//! The audit's own text of this test was not on this host (the order named
//! `/audit/in/requested`, which only the reviewer's host holds); this file
//! carries the order's statement of the case under the name it gives.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, LIB_RS, PAGE};

#[test]
fn audit_1955_a_drift_that_follows_its_item_passes() {
    // Line 2 is `alpha`; `beta` is at line 22.
    let base = page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, 2))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "the base passes:\n{out}");
    // Three lines above `alpha` move it to line 5; the citation follows.
    repo.write(
        LIB,
        format!("// one\n// two\n// three\n{LIB_RS}").as_bytes(),
    );
    let followed = base.replace(&line_cite(LIB, 2), &line_cite(LIB, 5));
    repo.write(PAGE, followed.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "a drift renumbered with its item (`alpha` at 2, now at 5) passes:\n{out}"
    );
}
