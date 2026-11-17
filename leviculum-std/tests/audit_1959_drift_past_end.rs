//! Audit of order 1959: a drifting citation past the end of its file
//! ("past-end" in the census) must not pass by "item equality" when it is
//! renumbered onto another past-end line.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

fn drifting(n: usize) -> String {
    page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, n))])
}

#[test]
fn audit_1959_drift_renumbered_between_past_end_lines_is_refused() {
    // LIB_RS has 22 lines; line 40 is past its end.
    let repo = Scratch::new(&drifting(40), &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the unchanged tree passes\n{out}");

    // Positive control: repaired onto `beta` (line 22), it agrees and passes.
    repo.write(PAGE, drifting(22).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: a repaired citation passes\n{out}");

    // The attack: renumbered from 40 to 90, both past the end.
    repo.write(PAGE, drifting(90).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a drift renumbered from one past-end line to another must be refused\n{out}"
    );
}
