//! A label-less citation renumbered onto another item is refused (audit of
//! order 1955, the reviewer's decision of 2026-11-16 21:05).
//!
//! A citation with no label makes no claim the census can check against a
//! name, but the census still resolves the item at its line. The guard let
//! a label-less citation be renumbered in place of a replaced label-less
//! one onto any line; paired with its base self, it passes only when the
//! item at its line stays the item the base resolved.
//!
//! The audit's own text of this test was not on this host (the order named
//! `/audit/in/requested`, which only the reviewer's host holds); this file
//! carries the order's statement of the case under the name it gives.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn audit_1955_a_label_less_citation_renumbered_onto_another_item_is_refused() {
    // Line 6 is `Thing`, line 9 is `Thing::new`.
    let base = page(&[format!("Somewhere (`{}`).", line_cite(LIB, 6))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "the base passes:\n{out}");
    let moved = base.replace(&line_cite(LIB, 6), &line_cite(LIB, 9));
    repo.write(PAGE, moved.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a label-less citation renumbered from `Thing` onto `Thing::new` is refused:\n{out}"
    );
}
