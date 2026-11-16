//! A new drifting citation beside an untouched one of its label is refused
//! (audit of order 1955).
//!
//! The audit's own text of this test was not on this host (the order named
//! the round-1 Tests to run, which only the reviewer's host holds); this
//! file carries the order's statement of the case under the name it gives,
//! with the rc=0 baseline and an agreeing twin as positive controls.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn audit_1955_verbatim_new_drift_beside_an_untouched_drift_is_refused() {
    // Line 2 is `alpha`; `beta` is at line 22.
    let base = page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, 2))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "the base passes:\n{out}");
    let agreeing = format!("{base}\nOn its item: `beta` (`{}`).\n", line_cite(LIB, 22));
    repo.write(PAGE, agreeing.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "a new agreeing citation beside the drift passes:\n{out}"
    );
    let added = format!("{base}\nAlso drifting: `beta` (`{}`).\n", line_cite(LIB, 2));
    repo.write(PAGE, added.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a new drift beside an untouched one must be refused:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}
