//! A scratch git repository for the tests that hold
//! `scripts/check-citations-by-name.py` to its refusals.
//!
//! The guard reads this crate's tests as living texts, so no citation-shaped
//! string stands in these files: every citation is put together at run time
//! by [`line_cite`] and [`name_cite`].

// Each test file includes this module and uses a part of it.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The Rust file every scratch repository carries.
pub const LIB: &str = "crate/src/lib.rs";

/// Its text. Line 2 is `alpha`, line 22 is `beta`; `new` is a method of
/// `Thing` and of nothing else.
pub const LIB_RS: &str = "\
/// Alpha's doc.
pub fn alpha() -> u32 {
    1
}

pub struct Thing;

impl Thing {
    pub fn new() -> Self {
        Thing
    }
}

pub struct Other;

impl Other {
    pub fn make() -> Self {
        Other
    }
}

pub fn beta() {}
";

/// The page every scratch repository carries.
pub const PAGE: &str = "docs/src/page.md";

/// A line citation.
pub fn line_cite(path: &str, line: usize) -> String {
    format!("{path}:{line}")
}

/// A name citation.
pub fn name_cite(path: &str, item: &str) -> String {
    format!("{path}#{item}")
}

/// A markdown page of paragraphs.
pub fn page(paras: &[String]) -> String {
    format!("# Page\n\n{}\n", paras.join("\n\n"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("leviculum-std sits one level under the repo root")
        .to_path_buf()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@invalid"])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A committed scratch repository.
pub struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    /// `LIB` and `crate/Cargo.toml`, the page with `page_text`, and `extra`
    /// files, committed as the base.
    pub fn new(page_text: &str, extra: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Scratch { dir };
        git(s.path(), &["init", "-q"]);
        s.write("crate/Cargo.toml", b"[package]\nname = \"c\"\n");
        s.write(LIB, LIB_RS.as_bytes());
        s.write(PAGE, page_text.as_bytes());
        for (rel, bytes) in extra {
            s.write(rel, bytes);
        }
        git(s.path(), &["add", "-A"]);
        git(s.path(), &["commit", "-q", "-m", "base"]);
        s
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Write a file of the working tree.
    pub fn write(&self, rel: &str, bytes: &[u8]) {
        let full = self.path().join(rel);
        fs::create_dir_all(full.parent().expect("a file has a directory")).expect("mkdir");
        fs::write(full, bytes).expect("write");
    }

    /// Run git in the scratch repository.
    pub fn git(&self, args: &[&str]) {
        git(self.path(), args);
    }

    /// Copy the tree's citation tools into the scratch repository's own
    /// `scripts/`, uncommitted: the caller commits them, and that commit
    /// is "the commit that added this guard" inside the scratch history.
    pub fn copy_tools(&self) {
        for name in [
            "citations.py",
            "check-citations-by-name.py",
            "citation-census.py",
            "check-python-test-prerequisites.py",
            "citation-foreign.txt",
        ] {
            let text = fs::read(repo_root().join("scripts").join(name)).expect("read tool");
            self.write(&format!("scripts/{name}"), &text);
        }
    }

    /// The scratch repository's OWN guard copy, with the base it derives
    /// itself (the commit that added the guard there): (exit code, output).
    pub fn guard_own(&self) -> (i32, String) {
        let out = Command::new("python3")
            .arg("-I")
            .arg(self.path().join("scripts/check-citations-by-name.py"))
            .arg("--root")
            .arg(self.path())
            .output()
            .expect("spawn python3");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// The guard against the base commit: (exit code, stdout and stderr).
    pub fn guard(&self) -> (i32, String) {
        let out = Command::new("python3")
            .arg("-I")
            .arg(repo_root().join("scripts/check-citations-by-name.py"))
            .arg("--root")
            .arg(self.path())
            .args(["--base", "HEAD"])
            .output()
            .expect("spawn python3");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}
