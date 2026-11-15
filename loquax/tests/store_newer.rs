//! A store a later Loquax wrote, on the desktop (1760): `loquax tui` (and
//! `loquax gui`, which starts through the same `shell::start`) refuses it
//! with the one plain sentence the phone shows, in the locale's language,
//! no path and no schema numbers; `--verbose` prints those, the desktop's
//! Details. The store is left byte for byte as it was. Through the real
//! binary over a temporary home: the store is read before the daemon, so
//! none is needed. The sentences are pinned as
//! `golden/store-newer.txt` and `golden/store-newer-de.txt`; `LOQUAX_WRITE_GOLDEN=1` writes them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const LOQUAX: &str = env!("CARGO_BIN_EXE_loquax");

/// A home whose store is stamped `ahead` schemas past this build's: the
/// header's `user_version`, four bytes big end first at offset 60, as a
/// later Loquax's migration leaves it.
fn home_with_store_ahead(ahead: i32) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("loquax");
    std::fs::create_dir_all(&home).expect("home");
    drop(loquax::store::Store::open_in(&home, [0x42; 16]).expect("a store"));
    let path = home.join(loquax::store::STORE_FILE);
    let mut bytes = std::fs::read(&path).expect("the store");
    let version = loquax::store::SCHEMA_VERSION + ahead;
    bytes[60..64].copy_from_slice(&version.to_be_bytes());
    std::fs::write(&path, bytes).expect("stamp the store");
    (dir, path)
}

fn run(dir: &Path, locale: &str, verbose: bool) -> Output {
    let mut command = Command::new(LOQUAX);
    if verbose {
        command.arg("--verbose");
    }
    command
        .arg("tui")
        .env("LNMSG_HOME", dir.join("loquax"))
        .env("HOME", dir)
        .env("LC_ALL", locale)
        .env_remove("LC_MESSAGES")
        .env_remove("LANG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("loquax runs")
}

fn golden(name: &str, got: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{name} moved; the run said:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

#[test]
fn a_store_from_a_later_loquax_is_said_in_plain_words_and_left_alone() {
    let (dir, path) = home_with_store_ahead(1);
    let before = std::fs::read(&path).expect("the store");
    for (locale, name, said) in [
        (
            "de_DE.UTF-8",
            "store-newer-de.txt",
            "Deine Chats wurden mit einer neueren Loquax-Version gespeichert.",
        ),
        (
            "C",
            "store-newer.txt",
            "Your chats were saved by a newer version of Loquax.",
        ),
    ] {
        let output = run(dir.path(), locale, false);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(!output.status.success(), "{locale}: {stderr}");
        assert!(stderr.contains(said), "{locale}: {stderr}");
        assert!(
            !stderr.contains("sqlite3") && !stderr.contains("schema"),
            "{locale}: no path, no schema: {stderr}"
        );
        golden(name, &stderr);
    }
    // The detail behind `--verbose`: the file and both schemas.
    let output = run(dir.path(), "C", true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&path.display().to_string())
            && stderr.contains(&format!("schema {}", loquax::store::SCHEMA_VERSION + 1)),
        "{stderr}"
    );
    assert!(
        std::fs::read(&path).expect("the store") == before,
        "the store was touched"
    );
}
