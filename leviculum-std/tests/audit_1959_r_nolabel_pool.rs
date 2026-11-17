// The audit's text follows, verbatim but for the fn-name prefix (order
// 1965). It was green at 1dc1d43c4 (audit build of 2026-11-17 01:27).

//! Second audit of order 1955, text verbatim (only the fn name is
//! prefixed): a change that removes one label-less citation of a file must
//! not be able to add a new label-less citation of another line of that
//! file elsewhere in the text (reviewer 2026-11-16 21:05: an added
//! citation must be `agree`).

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn audit_1959_r_a_removed_label_less_citation_must_not_excuse_one_added_elsewhere() {
    let base = page(&[
        format!("Somewhere (`{}`).", line_cite(LIB, 6)),
        "Unrelated prose.".into(),
    ]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the untouched base passes\n{out}");

    // The citing paragraph is deleted; a new label-less citation of a
    // DIFFERENT line of the same file appears in another paragraph.
    let head = page(&[
        "Unrelated prose.".into(),
        format!("Quite another place (`{}`).", line_cite(LIB, 9)),
    ]);
    repo.write(PAGE, head.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a label-less citation added in one place must not be excused by one \
         removed in another:\n{out}"
    );
}
