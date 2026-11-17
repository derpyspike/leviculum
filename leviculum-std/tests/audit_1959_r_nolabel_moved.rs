// The audit's text follows, verbatim but for the fn-name prefix (order
// 1965). It was green at 1dc1d43c4 (audit build of 2026-11-17 01:27).

//! Audit of order 1955, text verbatim (only the fn name is prefixed): a
//! label-less citation may follow its item but not be renumbered onto
//! another one (reviewer 2026-11-16 21:05, point 2).

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, LIB_RS, PAGE};

fn somewhere(n: usize) -> String {
    page(&[format!("Somewhere (`{}`).", line_cite(LIB, n))])
}

#[test]
fn audit_1959_r_a_label_less_citation_renumbered_onto_another_item_is_refused() {
    // Line 9 is `Thing::new`.
    let repo = Scratch::new(&somewhere(9), &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the unchanged tree passes\n{out}");

    // Positive control: the code moves three lines down, the citation
    // follows `Thing::new` from 9 to 12.
    let shifted = format!("pub fn gamma() {{}}\n\npub fn delta() {{}}\n{}", LIB_RS);
    repo.write(LIB, shifted.as_bytes());
    repo.write(PAGE, somewhere(12).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "control: a label-less citation that follows its item passes\n{out}"
    );

    // The attack: the code stays, the citation moves from `Thing::new`
    // (line 9) to `Other::make` (line 17).
    repo.write(LIB, LIB_RS.as_bytes());
    repo.write(PAGE, somewhere(17).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a label-less citation renumbered onto another item must be refused\n{out}"
    );
}
