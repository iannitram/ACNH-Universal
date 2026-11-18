// Tests for the packed data files (issue 1.27): made-up files, expected bytes written here.
use acnh_core::pack::{romfs, romfs_range, romfs_size, spirv};
use std::path::{Path, PathBuf};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-pack-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn le16(value: u16) -> [u8; 2] {
    value.to_le_bytes()
}

fn le32(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn le64(value: u64) -> [u8; 8] {
    value.to_le_bytes()
}

fn expected_romfs_pack() -> Vec<u8> {
    let mut expected = Vec::new();
    expected.extend_from_slice(b"NXRP");
    expected.extend_from_slice(&le32(1));
    expected.extend_from_slice(&le32(3));
    expected.extend_from_slice(&le16(5));
    expected.extend_from_slice(b"a.txt");
    expected.extend_from_slice(&le64(96));
    expected.extend_from_slice(&le64(5));
    expected.extend_from_slice(&le16(9));
    expected.extend_from_slice(b"dir/b.bin");
    expected.extend_from_slice(&le64(112));
    expected.extend_from_slice(&le64(16));
    expected.extend_from_slice(&le16(5));
    expected.extend_from_slice(b"empty");
    expected.extend_from_slice(&le64(128));
    expected.extend_from_slice(&le64(0));
    assert_eq!(expected.len(), 85);
    expected.resize(96, 0);
    expected.extend_from_slice(b"alpha");
    expected.resize(112, 0);
    expected.extend_from_slice(b"0123456789ABCDEF");
    expected
}

#[test]
fn romfs_pack_matches_the_expected_bytes() {
    let root = temp("romfs");
    write(&root, "dir/b.bin", b"0123456789ABCDEF");
    write(&root, "empty", b"");
    write(&root, "a.txt", b"alpha");
    let out = root.join("romfs.pak");

    assert_eq!(romfs_size(&root).unwrap(), 128);
    romfs(&root, &out).unwrap();

    assert_eq!(std::fs::read(&out).unwrap(), expected_romfs_pack());
}

#[test]
fn romfs_range_writes_only_its_bytes() {
    let root = temp("range");
    write(&root, "dir/b.bin", b"0123456789ABCDEF");
    write(&root, "empty", b"");
    write(&root, "a.txt", b"alpha");
    let out = root.join("part");

    romfs_range(&root, &out, 96, 112).unwrap();

    assert_eq!(std::fs::read(&out).unwrap(), b"alpha\0\0\0\0\0\0\0\0\0\0\0");
}

#[test]
fn empty_romfs_is_only_the_header() {
    let root = temp("empty");
    let out = root.join("romfs.pak");

    assert_eq!(romfs_size(&root).unwrap(), 12);
    romfs(&root, &out).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(b"NXRP");
    expected.extend_from_slice(&le32(1));
    expected.extend_from_slice(&le32(0));
    assert_eq!(std::fs::read(&out).unwrap(), expected);
}

fn expected_spirv_pack() -> Vec<u8> {
    let mut expected = Vec::new();
    expected.extend_from_slice(b"SPK1");
    expected.extend_from_slice(&le32(3));
    let mut a = [0u8; 16];
    a[0] = b'a';
    expected.extend_from_slice(&a);
    expected.extend_from_slice(&le64(104));
    expected.extend_from_slice(&le64(2));
    expected.extend_from_slice(b"shader0123456789");
    expected.extend_from_slice(&le64(106));
    expected.extend_from_slice(&le64(1));
    let mut results = [0u8; 16];
    results[..11].copy_from_slice(b"results.tsv");
    expected.extend_from_slice(&results);
    expected.extend_from_slice(&le64(107));
    expected.extend_from_slice(&le64(4));
    assert_eq!(expected.len(), 104);
    expected.extend_from_slice(b"AB");
    expected.extend_from_slice(b"C");
    expected.extend_from_slice(b"x\ty\n");
    expected
}

#[test]
fn spirv_pack_matches_the_expected_bytes() {
    let root = temp("spirv");
    write(&root, "a.spv", b"AB");
    write(&root, "shader0123456789.spv", b"C");
    write(
        &root,
        "0123456789abcdefg.spv",
        b"left out: its name does not fit",
    );
    write(&root, "results.tsv", b"x\ty\n");
    let out = root.join("spirv.pak");

    spirv(&root, &out).unwrap();

    assert_eq!(std::fs::read(&out).unwrap(), expected_spirv_pack());
}

#[test]
fn spirv_pack_without_results_tsv_has_only_the_shaders() {
    let root = temp("spirv-no-results");
    write(&root, "a.spv", b"AB");
    let out = root.join("spirv.pak");

    spirv(&root, &out).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(b"SPK1");
    expected.extend_from_slice(&le32(1));
    let mut a = [0u8; 16];
    a[0] = b'a';
    expected.extend_from_slice(&a);
    expected.extend_from_slice(&le64(40));
    expected.extend_from_slice(&le64(2));
    expected.extend_from_slice(b"AB");
    assert_eq!(std::fs::read(&out).unwrap(), expected);
    assert!(!root.join("spirv.pak.tmp").exists());
}
