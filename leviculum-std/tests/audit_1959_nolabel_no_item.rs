//! Audit of order 1959: a label-less citation whose cited line holds no
//! item ("-" in the census) must not pass by "item equality" when it is
//! renumbered onto another line that holds no item either.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

fn somewhere(n: usize) -> String {
    page(&[format!("Somewhere (`{}`).", line_cite(LIB, n))])
}

#[test]
fn audit_1959_label_less_citation_renumbered_between_lines_outside_any_item_is_refused() {
    // Line 5 of LIB_RS is the blank line between `alpha` and `Thing`.
    let repo = Scratch::new(&somewhere(5), &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the unchanged tree passes\n{out}");

    // Control: renumbered onto an item (`Thing`, line 6), it is refused.
    repo.write(PAGE, somewhere(6).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "control: a label-less citation moved onto an item is refused\n{out}"
    );

    // The attack: renumbered onto line 13, another blank line, between
    // `impl Thing` and `Other`. Neither line holds an item; that is not
    // "the same item".
    repo.write(PAGE, somewhere(13).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a label-less citation renumbered from one no-item line to another must be refused\n{out}"
    );
}
