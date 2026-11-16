//! A member is cited with its owner (audit of order 1928).
//!
//! docs/src/concepts/checks-and-citations.md section 9 says a method, field
//! or variant is written `Type::member`. The resolver matched a bare `new`
//! as long as only one item carried that name, so the rule was not enforced
//! and the next `new` in the file would have turned the citation ambiguous.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{name_cite, page, Scratch, LIB, PAGE};

#[test]
fn citation_guard_refuses_a_member_without_its_owner() {
    let repo = Scratch::new(&page(&["Nothing cited.".into()]), &[]);

    let bare = page(&[format!("Made by `{}`.", name_cite(LIB, "new"))]);
    repo.write(PAGE, bare.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(
        rc, 1,
        "a method cited without its type must be refused:\n{out}"
    );
    assert!(
        out.contains("Thing::new"),
        "the refusal names the owner:\n{out}"
    );

    let owned = page(&[format!("Made by `{}`.", name_cite(LIB, "Thing::new"))]);
    repo.write(PAGE, owned.as_bytes());
    let (rc, out) = repo.guard();
    assert_eq!(rc, 0, "with its owner the method resolves:\n{out}");
}
