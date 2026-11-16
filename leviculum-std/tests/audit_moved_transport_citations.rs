//! The transport citations commit 4d29e575 moved land on their items
//! (audit of order 1935, carried out by order 1940).
//!
//! The audit's condition, as order 1940 carries it: in every doc file the
//! commit changed, each line citation into `leviculum-core/src/transport.rs`
//! must land on a line of the item its label names. The commit had moved
//! citations onto the lines git's line map gave them, and nobody checked
//! that the new line holds the item the text names: `handle_announce`,
//! `interface_announce_caps` (twice), `own_tunnel_ids`,
//! `drop_paths_via_peer`, `announce_emission_secs` (twice) and
//! `time_source` (twice) were among the listed casualties, several of them
//! resting on a `//` comment that names a different identifier
//! (`// announce_rate_table: migrated to Storage trait` above
//! `interface_announce_caps`), which the census of that day read as the
//! item below it.
//!
//! The reviewer's decision of 2026-11-16 13:55 makes the census the judge
//! and keeps the moves that did not get worse, so this test asserts what
//! that rule repairs: every citation under these labels whose verdict was
//! `agree` at the commit's parent is `agree` in the working tree, and an
//! `agree` under `interface_announce_caps` really rests on the field, not
//! on the stray comment (the cited line holds the label's token). The
//! labels that already drifted at the guard's base (`time_source`,
//! `interface_names`, the hop-counting pair) stay as they are: the rule
//! refuses getting worse, it does not order a conversion wave.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("leviculum-std sits one level under the repo root")
        .to_path_buf()
}

struct Cite {
    file: String,
    at_path: String,
    span: (usize, usize),
    label: String,
    verdict: String,
}

fn census(files: &[&str]) -> Vec<Cite> {
    let root = repo_root();
    let out = Command::new("python3")
        .current_dir(&root)
        .arg("-I")
        .arg("scripts/citation-census.py")
        .arg("--only")
        .args(files)
        .output()
        .expect("spawn python3");
    assert!(
        out.status.success(),
        "census: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut cites = Vec::new();
    for line in text.lines() {
        if !line.starts_with("CITE ") {
            continue;
        }
        let mut file = None;
        let mut at = None;
        let mut label = None;
        let mut verdict = None;
        for tok in line.split_whitespace() {
            if let Some((k, v)) = tok.split_once('=') {
                match k {
                    "file" => file = Some(v.to_string()),
                    "at" => at = Some(v.to_string()),
                    "label" => label = Some(v.to_string()),
                    "verdict" => verdict = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        let (file, at, label, verdict) = match (file, at, label, verdict) {
            (Some(f), Some(a), Some(l), Some(v)) => (f, a, l, v),
            _ => continue,
        };
        let (at_path, spans) = match at.rsplit_once(':') {
            Some((p, s)) => (p.to_string(), s.to_string()),
            None => continue,
        };
        let first = spans.split(',').next().unwrap_or("");
        let (a, b) = match first.split_once('-') {
            Some((a, b)) => (a.parse().unwrap_or(0), b.parse().unwrap_or(0)),
            None => {
                let n = first.parse().unwrap_or(0);
                (n, n)
            }
        };
        let file = match file.rsplit_once(':') {
            Some((f, _)) => f.to_string(),
            None => file,
        };
        cites.push(Cite {
            file,
            at_path,
            span: (a, b),
            label,
            verdict,
        });
    }
    cites
}

const TRANSPORT: &str = "leviculum-core/src/transport.rs";

#[test]
fn moved_transport_citations_land_on_their_item() {
    let files = [
        "docs/src/concepts/core-lock-budget.md",
        "docs/src/concepts/multi-peer-interfaces.md",
        "docs/src/concepts/time-and-clocks.md",
    ];
    let cites = census(&files);
    assert!(!cites.is_empty(), "the census returned no citations");

    // Labels whose parent verdict was agree: every citation of them into
    // transport.rs must agree in the working tree.
    let must_agree: &[(&str, &str)] = &[
        ("docs/src/concepts/core-lock-budget.md", "handle_announce"),
        (
            "docs/src/concepts/multi-peer-interfaces.md",
            "split_horizon",
        ),
        (
            "docs/src/concepts/multi-peer-interfaces.md",
            "drop_paths_via_peer",
        ),
        (
            "docs/src/concepts/multi-peer-interfaces.md",
            "own_tunnel_ids",
        ),
        (
            "docs/src/concepts/multi-peer-interfaces.md",
            "interface_held_announces",
        ),
        (
            "docs/src/concepts/multi-peer-interfaces.md",
            "interface_next_slot_ms",
        ),
        (
            "docs/src/concepts/multi-peer-interfaces.md",
            "interface_max_airtime_ms",
        ),
        (
            "docs/src/concepts/time-and-clocks.md",
            "announce_emission_secs",
        ),
    ];
    for (file, label) in must_agree {
        let found: Vec<&Cite> = cites
            .iter()
            .filter(|c| {
                c.file == *file
                    && c.at_path.ends_with(TRANSPORT)
                    && (c.label == *label || c.label.ends_with(&format!("::{label}")))
            })
            .collect();
        assert!(
            !found.is_empty(),
            "{file}: no citation labelled `{label}` into {TRANSPORT}"
        );
        for c in &found {
            assert_eq!(
                c.verdict, "agree",
                "{file}: `{label}` cites {}:{}-{} with verdict {}",
                c.at_path, c.span.0, c.span.1, c.verdict
            );
        }
    }

    // `interface_announce_caps` is the audit's own example: at least one
    // citation agrees, and an agree really rests on the field - the cited
    // span holds the label's token, so the stray `// announce_rate_table`
    // comment above the field can never carry an agree again.
    let transport = std::fs::read_to_string(repo_root().join(TRANSPORT)).expect("read transport");
    let lines: Vec<&str> = transport.lines().collect();
    let caps: Vec<&Cite> = cites
        .iter()
        .filter(|c| {
            c.file == "docs/src/concepts/multi-peer-interfaces.md"
                && c.at_path.ends_with(TRANSPORT)
                && c.label == "interface_announce_caps"
        })
        .collect();
    assert!(
        caps.iter().any(|c| c.verdict == "agree"),
        "no agreeing `interface_announce_caps` citation is left"
    );
    for c in &caps {
        if c.verdict != "agree" {
            continue;
        }
        let (a, b) = c.span;
        let holds = (a..=b.max(a))
            .filter_map(|n| lines.get(n - 1))
            .any(|l| l.contains("interface_announce_caps"));
        assert!(
            holds,
            "`interface_announce_caps` claims agree at {}:{a}-{b}, but no cited line holds \
             the token: the citation rests on something else's line",
            c.at_path
        );
    }
}
