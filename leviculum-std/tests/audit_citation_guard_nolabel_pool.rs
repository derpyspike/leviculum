//! No pool of replaced label-less citations excuses a touched one (second
//! audit of order 1955, the reviewer's decision of 2026-11-16 21:05).
//!
//! The guard kept a pool of the base citations a change replaced and let a
//! label-less citation draw any label-less entry of its file from it,
//! whatever line it now named. Paired by occurrence, each touched
//! citation answers to the one it replaced, and a label-less one passes
//! only on the item that one resolved.
//!
//! The audit's own text of this test (round 2) was not on this host; this
//! file carries the order's statement of the case.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

/// Two label-less citations on one line: `Thing` (6) and `Other` (14).
fn two_unlabelled(a: usize, b: usize) -> String {
    page(&[format!(
        "Here (`{}`) and there (`{}`).",
        line_cite(LIB, a),
        line_cite(LIB, b)
    )])
}

#[test]
fn citation_guard_refuses_label_less_citations_swapped_onto_each_others_items() {
    let repo = Scratch::new(&two_unlabelled(6, 14), &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "the base passes:\n{out}");
    // The same two numbers, each now where the other one was.
    repo.write(PAGE, two_unlabelled(14, 6).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "each label-less citation answers to the one it replaced:\n{out}"
    );
}

#[test]
fn citation_guard_passes_label_less_citations_rewritten_in_place() {
    let repo = Scratch::new(&two_unlabelled(6, 14), &[]);
    // The words change, the numbers and their items do not.
    let reworded = page(&[format!(
        "Here, first (`{}`), and there, second (`{}`).",
        line_cite(LIB, 6),
        line_cite(LIB, 14)
    )]);
    repo.write(PAGE, reworded.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "a reworded line keeps its pairs:\n{out}");
}
