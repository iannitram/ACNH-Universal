// SPDX-License-Identifier: MIT
// Copyright (c) 2026 iannitram
//! Library code never prints: a GUI shows no console, so a message printed there is lost.
//! A warning is returned to the caller, which shows it where its user looks.

use std::fs;
use std::path::Path;

fn rust_files(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn the_core_library_has_no_println_or_eprintln() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty());
    let mut found = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            let printing = ["println!", "eprintln!", "print!", "eprint!", "dbg!"]
                .iter()
                .any(|mac| code.contains(mac));
            if printing && !code.starts_with("//") {
                found.push(format!(
                    "{}:{}",
                    file.strip_prefix(&src).unwrap().display(),
                    number + 1
                ));
            }
        }
    }
    assert!(found.is_empty(), "printing in crates/core/src: {found:?}");
}
