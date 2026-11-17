// Pins behaviour that held before the change: order 1959 ordered this
// test green at bbb14bfd8, and it was green at 1dc1d43c4 (audit build of
// 2026-11-17 01:27). It holds a guard change to what was already refused;
// it is not the red test of one. The audit's text follows, verbatim but
// for the fn-name prefix (order 1965).

//! Audit of order 1940, round 1, verbatim (only the fn name is prefixed).

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

fn cite(n: usize) -> String {
    format!("(`{}`)", line_cite(LIB, n))
}

#[test]
fn audit_1959_r_verbatim_new_drift_beside_an_untouched_drift_is_refused() {
    let base = page(&[format!("Elsewhere `alpha` {}.", cite(17))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the untouched drifting base passes\n{out}");

    // Positive control: a new agreeing citation of the same label passes.
    repo.write(
        PAGE,
        format!("{base}\nAgain `alpha` {}.\n", cite(2)).as_bytes(),
    );
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: a new agreeing citation passes\n{out}");

    // The attack: a new drifting citation beside the untouched one.
    repo.write(
        PAGE,
        format!("{base}\nAgain `alpha` {}.\n", cite(17)).as_bytes(),
    );
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a citation the change adds must agree; an untouched drifting sibling \
         must not excuse it\n{out}"
    );
}
