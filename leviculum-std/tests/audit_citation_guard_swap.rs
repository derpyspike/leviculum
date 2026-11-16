//! A new line citation that takes the place of an old one into the same
//! file is still new (audit of order 1928).
//!
//! The guard counted line citations per text and per cited path, so a
//! citation whose number was changed by hand, or a sentence swapped for
//! another that cites the same file, kept the count and passed. A changed
//! line keeps a line citation only where the base text held one into the
//! same path whose number git's line map of the cited file carries to the
//! new number: the map the doc_citations fixer follows.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn citation_guard_refuses_a_new_line_citation_that_replaces_an_old_one() {
    let base = page(&[format!("The first is `alpha` (`{}`).", line_cite(LIB, 2))]);
    let repo = Scratch::new(&base, &[]);

    // The sentence goes, and another one cites `beta` in the same file,
    // which did not change: one citation into the path before, one after.
    let swapped = page(&[format!("The last is `beta` (`{}`).", line_cite(LIB, 20))]);
    repo.write(PAGE, swapped.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(rc, 1, "a swapped-in line citation must be refused:\n{out}");
    assert!(out.contains("a new line citation"), "{out}");

    // The same number edited in place, the cited file unchanged.
    let edited = base.replace(&line_cite(LIB, 2), &line_cite(LIB, 3));
    repo.write(PAGE, edited.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(rc, 1, "a hand-edited number must be refused:\n{out}");
}
