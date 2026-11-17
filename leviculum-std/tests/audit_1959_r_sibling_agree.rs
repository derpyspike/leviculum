// Pins behaviour that held before the change: order 1959 ordered this
// test green at bbb14bfd8, and it was green at 1dc1d43c4 (audit build of
// 2026-11-17 01:27). It holds a guard change to what was already refused;
// it is not the red test of one. The audit's text follows, verbatim but
// for the fn-name prefix (order 1965).

//! Audit of order 1940, round 1, verbatim (only the fn name is prefixed):
//! the guard must not let an agreeing citation move onto another item
//! because a same-label drifting citation stands elsewhere.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

fn cite(n: usize) -> String {
    format!("(`{}`)", line_cite(LIB, n))
}

#[test]
fn audit_1959_r_verbatim_agree_moved_beside_a_drifting_sibling_is_refused() {
    // Positive control: without a sibling, the move is refused.
    let alone = page(&[format!("The first is `alpha` {}.", cite(2))]);
    let repo = Scratch::new(&alone, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "control: the untouched base passes\n{out}");
    repo.write(PAGE, alone.replace(&cite(2), &cite(6)).as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "control: alpha moved onto Thing (line 6) is refused\n{out}"
    );

    // Line 17 is `Other::make`: a drifting `alpha` citation beside it.
    let base = page(&[
        format!("The first is `alpha` {}.", cite(2)),
        format!("Elsewhere `alpha` {}.", cite(17)),
    ]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "control: the untouched base with a drifting sibling passes\n{out}"
    );
    let moved = base.replacen(&cite(2), &cite(6), 1);
    assert_ne!(moved, base);
    repo.write(PAGE, moved.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "an agreeing citation moved onto another item must be refused even when \
         a drifting citation of the same label stands untouched in the text\n{out}"
    );
}
