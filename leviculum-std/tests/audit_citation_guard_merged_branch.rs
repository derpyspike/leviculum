//! A merged pre-guard branch does not excuse a stale line citation (audit
//! of order 1935, carried out by order 1940).
//!
//! The audit's condition, as order 1940 carries it: in
//! `scripts/check-citations-by-name.py`, a line that a merged pre-guard
//! parent holds unchanged may excuse a line citation only when every cited
//! number still points at what the text says — the guard had excused any
//! citation such a parent held, so a branch written before the rule could
//! merge a number that had gone stale while the cited file grew on master,
//! and the guard stayed green. The reviewer's decision of 2026-11-16 13:55
//! replaced the excuse wholesale: the census is the judge, there is no
//! "unchanged since a merged parent" exemption and no line map to trust; a
//! citation a change adds or moves must be `agree`.
//!
//! The scratch history rebuilds the failure: a branch forked before the
//! guard existed cites `alpha` at its then-correct line; master gains the
//! guard, grows the lib above `alpha` and renumbers its own citation; the
//! merge carries the branch's stale number in. The guard under test is the
//! scratch repository's OWN committed copy of the tools, because the
//! merged-parent question only exists where the guard's commit is in the
//! history. The old guard excused this citation and exited 0; the audit
//! demands it refused.

#[path = "support/citation_scratch.rs"]
mod support;

use support::{line_cite, page, Scratch, LIB, LIB_RS, PAGE};

#[test]
fn citation_guard_merged_branch_does_not_excuse_a_stale_number() {
    let base = page(&[
        format!("The first is `alpha` (`{}`).", line_cite(LIB, 2)),
        "Prose between the citation and the branch's addition,".into(),
        "wide enough that git can merge the two edits.".into(),
    ]);
    let repo = Scratch::new(&base, &[]);

    // The branch, written while the rule did not exist: its citation is
    // right about the lib it sees.
    repo.git(&["checkout", "-q", "-b", "pre"]);
    let appended = format!(
        "{base}\nAlso the first: `alpha` (`{}`).\n",
        line_cite(LIB, 2)
    );
    repo.write(PAGE, appended.as_bytes());
    repo.git(&["commit", "-qam", "a citation written before the rule"]);
    repo.git(&["checkout", "-q", "-"]);

    // Master: the guard arrives, the lib grows above `alpha`, and master's
    // own citation follows the item to line 5.
    repo.copy_tools();
    repo.write(
        LIB,
        format!("// one\n// two\n// three\n{LIB_RS}").as_bytes(),
    );
    let renumbered = base.replace(&line_cite(LIB, 2), &line_cite(LIB, 5));
    repo.write(PAGE, renumbered.as_bytes());
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-qm", "the guard arrives and the lib grows"]);

    // The merge brings the branch's number in, now stale: line 2 is a
    // comment, `alpha` stands at line 5.
    repo.git(&["merge", "-q", "--no-edit", "pre"]);

    let (rc, out) = repo.guard_own();
    assert_eq!(
        rc, 1,
        "the branch's stale number must be refused, not excused by the merged parent:\n{out}"
    );
    assert!(out.contains("a new line citation"), "{out}");
}
