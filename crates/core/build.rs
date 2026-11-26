//! Fingerprint of this crate's own source: the build version in the `romfs.pak`, `spirv.pak`
//! and shaders keys, which the workspace version (fixed at 0.0.0) cannot be.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        // Line ends dropped so a checkout with CRLF fingerprints the same.
        let bytes = std::fs::read(&path).unwrap_or_default();
        let text: Vec<u8> = bytes.into_iter().filter(|b| *b != b'\r').collect();
        hasher.update(rel.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update(b"\0");
        hasher.update(&text);
        hasher.update(b"\0");
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("cargo:rustc-env=ACNH_CORE_FINGERPRINT={hex}");
}
