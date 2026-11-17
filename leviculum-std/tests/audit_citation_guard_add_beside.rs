//! A citation added beside one the base keeps word for word is new (second
//! audit of order 1940, carried out by order 1955).
//!
//! The base holds a drifting citation; the change leaves it as it is and
//! adds a second citation of the same cited file and label on another
//! item's line. The guard drew the excuse for the new one from the pool
//! entry of the untouched one, and passed. Order 1955: a base citation
//! excuses only when it no longer stands at head, so the untouched one
//! keeps its own entry and the added one is judged as new.
//!
//! The audit's own text (round 1) was not delivered to the pass that wrote
//! this file (the bridge held only instructions.md); it carries order
//! 1955's statement of the case.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn citation_guard_refuses_a_drift_added_beside_one_the_base_keeps() {
    // `beta` is at line 22; line 2 is `alpha`.
    let base = page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, 2))]);
    let repo = Scratch::new(&base, &[]);
    // Line 9 is `Thing::new`, another item again.
    let added = format!("{base}\nAnd `beta` (`{}`).\n", line_cite(LIB, 9));
    repo.write(PAGE, added.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "the untouched drift must not excuse the added one:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}
