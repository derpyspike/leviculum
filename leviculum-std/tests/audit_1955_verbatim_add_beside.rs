//! A citation added beside a drift the base keeps word for word is judged
//! as new (audit of order 1955).
//!
//! The audit's own text of this test was not on this host (the order named
//! the round-1 Tests to run, which only the reviewer's host holds); this
//! file carries the order's statement of the case under the name it gives,
//! with the rc=0 baseline and an agreeing twin as positive controls.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn audit_1955_verbatim_added_citation_beside_a_standing_drift() {
    // Line 2 is `alpha`; `beta` is at line 22; line 9 is `Thing::new`.
    let base = page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, 2))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "the base passes:\n{out}");
    let agreeing = format!("{base}\nAnd `beta` (`{}`).\n", line_cite(LIB, 22));
    repo.write(PAGE, agreeing.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "an added citation that agrees passes:\n{out}");
    let added = format!("{base}\nAnd `beta` (`{}`).\n", line_cite(LIB, 9));
    repo.write(PAGE, added.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "the standing drift must not excuse the added one:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}
