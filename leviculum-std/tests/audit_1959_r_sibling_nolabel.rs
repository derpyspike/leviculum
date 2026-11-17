// Pins behaviour that held before the change: order 1959 ordered this
// test green at bbb14bfd8, and it was green at 1dc1d43c4 (audit build of
// 2026-11-17 01:27). It holds a guard change to what was already refused;
// it is not the red test of one. The audit's text follows, verbatim but
// for the fn-name prefix (order 1965).

//! Audit of order 1940, round 1, verbatim (only the fn name is prefixed).

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, PAGE};

#[test]
fn audit_1959_r_verbatim_new_unlabelled_beside_an_untouched_unlabelled_is_refused() {
    let base = page(&[format!("Old: `{}`.", line_cite(LIB, 22))]);
    let repo = Scratch::new(&base, &[]);
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 0,
        "control: the untouched label-less base passes\n{out}"
    );

    repo.write(
        PAGE,
        format!("{base}\nNew: `{}`.\n", line_cite(LIB, 6)).as_bytes(),
    );
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a new label-less line citation must be refused even when an untouched \
         label-less citation into the same file stands in the text\n{out}"
    );
}
