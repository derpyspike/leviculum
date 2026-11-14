//! Reads the index of `mi_option_allow_thp` out of the mimalloc header the
//! build links (1805).
//!
//! libmimalloc-sys 0.1.49 declares no Rust constant for that option, and
//! `mi_option_t` is a plain C enum whose values move whenever upstream
//! inserts an option. Counting the header's enum here, rather than typing
//! the number, keeps `huge_pages_allowed()` reading the right option after a
//! mimalloc update; a header without the option fails the build.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        return;
    }
    let Ok(include) = env::var("DEP_MIMALLOC_INCLUDE_DIR") else {
        panic!("libmimalloc-sys did not export DEP_MIMALLOC_INCLUDE_DIR");
    };
    let header = Path::new(&include).join("mimalloc.h");
    println!("cargo:rerun-if-changed={}", header.display());
    let text = match fs::read_to_string(&header) {
        Ok(text) => text,
        Err(e) => panic!("cannot read {}: {e}", header.display()),
    };
    let Some(start) = text.find("typedef enum mi_option_e {") else {
        panic!("{}: no mi_option_e enum", header.display());
    };
    let rest = &text[start..];
    let Some(end) = rest.find("} mi_option_t;") else {
        panic!("{}: mi_option_e is not closed", header.display());
    };
    let body = &rest["typedef enum mi_option_e {".len()..end];
    let mut index = 0usize;
    let mut found = None;
    for line in body.lines() {
        let code = line.split("//").next().unwrap_or("");
        for name in code.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            // Aliases (`a = b`) come after `_mi_option_last` and take no new value.
            if name.contains('=') {
                continue;
            }
            if name == "mi_option_allow_thp" {
                found = Some(index);
            }
            index += 1;
        }
    }
    let Some(found) = found else {
        panic!("{}: no mi_option_allow_thp", header.display());
    };
    println!("cargo:rustc-env=LEVICULUM_ALLOC_MI_OPTION_ALLOW_THP={found}");
}
