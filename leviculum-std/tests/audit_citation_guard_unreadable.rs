//! A name citation into a file the guard cannot read is refused, not
//! counted as checked (audit of order 1928).
//!
//! `resolve_in` answers `missing-file` for a tracked file that is not UTF-8,
//! and the guard dropped that answer: the citation counted as checked and
//! nothing was refused, although no item of the file had been looked at.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{name_cite, page, Scratch, PAGE};

const BAD: &str = "crate/src/bad.rs";

#[test]
fn citation_guard_refuses_a_name_into_a_file_it_cannot_read() {
    let bytes: &[u8] = b"pub fn x() {}\n// \xff\xfe not UTF-8\n";
    let repo = Scratch::new(&page(&["Nothing cited.".into()]), &[(BAD, bytes)]);

    let cites = page(&[format!("See `{}`.", name_cite(BAD, "x"))]);
    repo.write(PAGE, cites.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a citation into an unreadable file must be refused:\n{out}"
    );
    assert!(out.contains(&format!("cannot read {BAD}")), "{out}");
    assert!(
        out.contains("0 name citation(s) checked"),
        "an unreadable file checks nothing:\n{out}"
    );
}
