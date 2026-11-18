// Tests for SARC archives and the shaders stage (issue 1.29): made-up archives, the same
// bytes upstream's `sarc_endian.py` writes, and the grouping/cleanup of `extract_shaders.py`.
use acnh_core::sarc::{big_endian, check, parse, read, rewrite, Endian};
use acnh_core::shaders::{cleanup, plan};
use std::path::{Path, PathBuf};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-sarc-{name}-{}", std::process::id()));
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

fn be16(value: u16) -> [u8; 2] {
    value.to_be_bytes()
}

fn be32(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

fn names() -> &'static [u8] {
    b"a.txt\0\0\0bb.bin\0\0"
}

fn sample_le() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"SARC");
    out.extend_from_slice(&le16(0x14));
    out.extend_from_slice(&[0xff, 0xfe]);
    out.extend_from_slice(&le32(0x60));
    out.extend_from_slice(&le32(0x58));
    out.extend_from_slice(&le16(0x0100));
    out.extend_from_slice(&le16(0));
    out.extend_from_slice(b"SFAT");
    out.extend_from_slice(&le16(0x0C));
    out.extend_from_slice(&le16(2));
    out.extend_from_slice(&le32(0xAABB_CCDD));
    for node in [
        (0x1122_3344u32, 0x0100_0000u32, 0u32, 4u32),
        (0x5566_7788, 0x0100_0002, 4, 8),
    ] {
        out.extend_from_slice(&le32(node.0));
        out.extend_from_slice(&le32(node.1));
        out.extend_from_slice(&le32(node.2));
        out.extend_from_slice(&le32(node.3));
    }
    out.extend_from_slice(b"SFNT");
    out.extend_from_slice(&le16(0x08));
    out.extend_from_slice(&le16(0));
    out.extend_from_slice(names());
    assert_eq!(out.len(), 0x58);
    out.extend_from_slice(b"ABCDEFGH");
    assert_eq!(out.len(), 0x60);
    out
}

fn expected_big_endian() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"SARC");
    out.extend_from_slice(&be16(0x14));
    out.extend_from_slice(&[0xfe, 0xff]);
    out.extend_from_slice(&be32(0x60));
    out.extend_from_slice(&be32(0x58));
    out.extend_from_slice(&be16(0x0100));
    out.extend_from_slice(&be16(0));
    out.extend_from_slice(b"SFAT");
    out.extend_from_slice(&be16(0x0C));
    out.extend_from_slice(&be16(2));
    out.extend_from_slice(&be32(0xAABB_CCDD));
    for node in [
        (0x1122_3344u32, 0x0100_0000u32, 0u32, 4u32),
        (0x5566_7788, 0x0100_0002, 4, 8),
    ] {
        out.extend_from_slice(&be32(node.0));
        out.extend_from_slice(&be32(node.1));
        out.extend_from_slice(&be32(node.2));
        out.extend_from_slice(&be32(node.3));
    }
    out.extend_from_slice(b"SFNT");
    out.extend_from_slice(&be16(0x08));
    out.extend_from_slice(&be16(0));
    out.extend_from_slice(names());
    assert_eq!(out.len(), 0x58);
    out.extend_from_slice(b"ABCDEFGH");
    out
}

#[test]
fn big_endian_rewrites_the_headers_like_the_python_script() {
    assert_eq!(big_endian(&sample_le()).unwrap(), expected_big_endian());
}

#[test]
fn a_big_endian_archive_is_returned_unchanged() {
    let big = expected_big_endian();
    assert_eq!(big_endian(&big).unwrap(), big);
}

#[test]
fn parse_reads_the_fields_and_the_members() {
    let data = sample_le();
    let archive = parse(&data).unwrap();
    assert_eq!(archive.endian, Endian::Little);
    assert_eq!(archive.version, 0x0100);
    assert_eq!(archive.hash_key, 0xAABB_CCDD);
    assert_eq!(archive.data_offset, 0x58);
    assert_eq!(archive.names, names());
    assert_eq!(archive.nodes.len(), 2);
    assert_eq!(archive.nodes[1].attr, 0x0100_0002);

    let members = archive.members().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].name, "a.txt");
    assert_eq!(members[0].data, b"ABCD");
    assert_eq!(members[1].name, "bb.bin");
    assert_eq!(members[1].data, b"EFGH");
}

#[test]
fn read_and_rewrite_go_through_a_file() {
    let dir = temp("rewrite");
    let src = dir.join("in.sarc");
    let dst = dir.join("out.sarc");
    std::fs::write(&src, sample_le()).unwrap();

    assert_eq!(read(&src).unwrap(), sample_le());
    assert_eq!(check(&src).unwrap(), Endian::Little);
    rewrite(&src, &dst).unwrap();
    assert_eq!(std::fs::read(dst).unwrap(), expected_big_endian());
}

#[test]
fn malformed_archives_are_refused_with_a_plain_message() {
    let error = parse(b"SARC").unwrap_err();
    assert!(error.to_string().contains("SARC"), "{error}");

    let mut not_sarc = sample_le();
    not_sarc[..4].copy_from_slice(b"NOPE");
    let error = big_endian(&not_sarc).unwrap_err();
    assert!(error.to_string().contains("not a SARC"), "{error}");

    let mut too_many_nodes = sample_le();
    too_many_nodes[0x1A..0x1C].copy_from_slice(&le16(100));
    let error = parse(&too_many_nodes).unwrap_err();
    assert!(error.to_string().contains("SARC"), "{error}");

    let mut data_past_end = sample_le();
    data_past_end[0x0C..0x10].copy_from_slice(&le32(0x100));
    let error = parse(&data_past_end).unwrap_err();
    assert!(error.to_string().contains("SARC"), "{error}");

    let mut names_overlap = sample_le();
    names_overlap[0x0C..0x10].copy_from_slice(&le32(0x10));
    let error = parse(&names_overlap).unwrap_err();
    assert!(error.to_string().contains("SARC"), "{error}");
}

#[test]
fn plan_names_the_groups_like_extract_shaders() {
    let root = temp("plan");
    let romfs = root.join("romfs");
    for rel in [
        "Shader/Park_UBER_All.Nin_NX_NVN.szs",
        "Shader/Park_Proc.Nin_NX_NVN.release.szs",
        "Effect/Park_Idr.Nin_NX_NVN.szs",
        "System/GSys/gsys_resource.Nin_NX_NVN.release.szs",
        "System/Font/nvn_font_shader_1.bin",
        "Swkbd/shader/kbd_Nx.arc.szs",
        "Swkbd/shader/other.arc.szs",
        "Layout/a.Nin_NX_NVN.zs",
        "Model/m.Nin_NX_NVN.zs",
    ] {
        write(&romfs, rel, b"made up");
    }

    let groups = plan(&romfs, false).unwrap();
    let names: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "shader_Park_Proc",
            "shader_Park_UBER_All",
            "effect_Park_Idr",
            "system_Font_nvn_font_shader_1",
            "system_GSys_gsys_resource",
            "swkbd_kbd_Nx",
            "layout_a",
        ]
    );
    assert_eq!(
        groups[0].path,
        romfs.join("Shader/Park_Proc.Nin_NX_NVN.release.szs")
    );

    let full = plan(&romfs, true).unwrap();
    assert_eq!(full.last().unwrap().name, "model_m");
}

#[test]
fn cleanup_drops_groups_without_shaders_and_their_logs() {
    let out = temp("cleanup");
    write(&out, "empty/manifest.tsv", b"id\tstage\n");
    write(&out, "full/manifest.tsv", b"id\tstage\n");
    write(&out, "full/code_abcd.bin", b"code");
    write(&out, "keep/manifest.tsv", b"id\tstage\n");
    write(&out, "keep/code_efgh.bin", b"code");
    write(&out, "empty.log", b"no shader container recognised\n");
    write(&out, "full.log", b"extracted\n");
    write(&out, "orphan.log", b"no manifest\n");

    cleanup(&out).unwrap();

    assert!(!out.join("empty").exists());
    assert!(out.join("full").join("code_abcd.bin").is_file());
    assert!(out.join("keep").join("code_efgh.bin").is_file());
    assert!(!out.join("empty.log").exists());
    assert!(out.join("full.log").is_file());
    assert!(!out.join("orphan.log").exists());
}
