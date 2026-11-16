//! An untouched sibling citation does not excuse a touched one (audit of
//! order 1940, carried out by order 1955).
//!
//! The guard paired a touched citation that is not `agree` with any base
//! citation of the same cited file and label, so a citation that stood
//! untouched in the working tree could excuse a second one the change
//! added or moved beside it. Order 1955's rule: every base citation that
//! still stands untouched uses up its own pool entry first; a touched
//! citation is excused only by a base citation the change replaced or
//! moved (a line the diff removed or changed), written the same, with a
//! verdict no better.
//!
//! The audit's own text of these three tests was not delivered to the
//! pass that wrote them (the bridge held only instructions.md); they carry
//! order 1955's statement of the three cases, under the names it gives.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

/// `alpha` agrees at line 2 and drifts, untouched, at line 20 (the end of
/// `impl Other`; `beta` is at line 22).
fn agree_and_drift() -> String {
    page(&[
        format!("The first is `alpha` (`{}`).", line_cite(LIB, 2)),
        "Prose between the two.".into(),
        format!("Again `alpha` (`{}`).", line_cite(LIB, 20)),
    ])
}

#[test]
fn audit_1940_agree_moved_beside_a_drifting_sibling_is_refused() {
    let base = agree_and_drift();
    let repo = Scratch::new(&base, &[]);
    // The agreeing citation moves onto `Thing` (line 6); the drifting one
    // of the same label into the same file stands untouched.
    let moved = base.replacen(&line_cite(LIB, 2), &line_cite(LIB, 6), 1);
    repo.write(PAGE, moved.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "the untouched drifting sibling must not excuse the moved agree:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}

#[test]
fn audit_1940_new_drift_beside_an_untouched_drift_is_refused() {
    let base = page(&[format!("Drifting: `beta` (`{}`).", line_cite(LIB, 2))]);
    let repo = Scratch::new(&base, &[]);
    let added = format!("{base}\nAlso drifting: `beta` (`{}`).\n", line_cite(LIB, 2));
    repo.write(PAGE, added.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a new drift beside an untouched one must be refused:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}

#[test]
fn audit_1940_new_unlabelled_beside_an_untouched_unlabelled_is_refused() {
    let base = page(&[format!("Somewhere (`{}`).", line_cite(LIB, 6))]);
    let repo = Scratch::new(&base, &[]);
    let added = format!("{base}\nSomewhere else (`{}`).\n", line_cite(LIB, 9));
    repo.write(PAGE, added.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a new label-less citation beside an untouched one must be refused:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}
