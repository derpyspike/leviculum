// Pins behaviour that held before the change: order 1959 ordered this
// test green at bbb14bfd8, and it was green at 1dc1d43c4 (audit build of
// 2026-11-17 01:27). It holds a guard change to what was already refused;
// it is not the red test of one. The audit's text follows, verbatim but
// for the fn-name prefix (order 1965); the allow below keeps its
// `drift.clone()` as the audit wrote it.
#![allow(clippy::cloned_ref_to_slice_refs)]

//! Audit of order 1940, second auditor round 1, verbatim (only the fn name
//! is prefixed): a change must not ADD a non-agreeing line citation while
//! a base citation of its label still stands.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn audit_1959_r_verbatim_added_citation_beside_a_standing_drift() {
    // The base drifts on purpose: `beta` cited at line 2 of the lib,
    // which is `alpha`'s line.
    let drift = format!("Drifting: `beta` (`{}`).", line_cite(LIB, 2));
    let base = page(&[drift.clone()]);
    let repo = Scratch::new(&base, &[]);

    // Control: the unchanged tree passes, so a red below can only come
    // from the added citation.
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the unchanged tree must pass\n{out}");

    // The attack: the old drifting citation stays word for word, and the
    // change adds a second citation of the same cited file and label on
    // another item's line (line 6 is `Thing`, not `beta`).
    let added = page(&[
        drift,
        format!("Also drifting: `beta` (`{}`).", line_cite(LIB, 6)),
    ]);
    repo.write(PAGE, added.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "an added citation must agree; the base's still-standing drift must \
         not excuse a second one:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}
