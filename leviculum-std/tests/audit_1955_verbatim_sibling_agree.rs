//! An agreeing citation moved onto another item is refused beside an
//! untouched drifting sibling of its label (audit of order 1955).
//!
//! The audit's own text of this test was not on this host (the order named
//! the round-1 Tests to run, which only the reviewer's host holds); this
//! file carries the order's statement of the case under the name it gives,
//! with the rc=0 baseline and an agreeing twin as positive controls.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

/// `alpha` agrees at line 2 and drifts, untouched, at line 20 (the end of
/// `impl Other`; `beta` is at line 22).
fn agree_and_drift(first: usize) -> String {
    page(&[
        format!("The first is `alpha` (`{}`).", line_cite(LIB, first)),
        "Prose between the two.".into(),
        format!("Again `alpha` (`{}`).", line_cite(LIB, 20)),
    ])
}

#[test]
fn audit_1955_verbatim_agree_moved_beside_a_drifting_sibling_is_refused() {
    let repo = Scratch::new(&agree_and_drift(2), &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "the base passes:\n{out}");
    // Line 3 is still `alpha`: a number that stays on its item passes.
    repo.write(PAGE, agree_and_drift(3).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "an agreeing citation kept on its item passes:\n{out}"
    );
    // Line 6 is `Thing`.
    repo.write(PAGE, agree_and_drift(6).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "the untouched drifting sibling must not excuse the moved agree:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}
