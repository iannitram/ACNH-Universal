// Locked tests for prepare::redact (issue 1.16: no key value or home folder reaches a log,
// SPEC.md M1). Hex values are fake.
use acnh_core::prepare::redact;
use std::path::Path;

const K32: &str = "00112233445566778899aabbccddeeff";
// The 3.0.2 build id, which the gate knows, so it is not a key.
const V302: &str = "FCD2BB238ABE99E925B4E452DF1F41F1";

fn r(line: &str) -> String {
    redact(line, Path::new("/home/u"))
}

#[test]
fn a_32_digit_run_is_hex() {
    assert_eq!(r(&format!("header_key = {K32}")), "header_key = <hex>");
}

#[test]
fn longer_runs_and_upper_case() {
    let k64 = format!("{K32}{}", K32.to_uppercase());
    assert_eq!(r(&format!("[{k64}]")), "[<hex>]");
}

#[test]
fn several_runs_in_one_line() {
    assert_eq!(r(&format!("{K32} and {K32}.")), "<hex> and <hex>.");
}

#[test]
fn short_and_split_runs_stay() {
    let line = format!("step 3 of 7: {} 0x7fff deadbeef", &K32[..31]);
    assert_eq!(r(&line), line);
    let split = format!("{}-{}", &K32[..16], &K32[16..]);
    assert_eq!(r(&split), split);
}

#[test]
fn known_build_ids_stay() {
    let line = format!("build id {V302} is 3.0.2");
    assert_eq!(r(&line), line);
    let lower = format!("build id {}", V302.to_lowercase());
    assert_eq!(r(&lower), lower);
}

#[test]
fn home_becomes_tilde() {
    assert_eq!(
        r("reading /home/u/Games/acnh/exefs/main"),
        "reading ~/Games/acnh/exefs/main"
    );
    assert_eq!(r("/home/u and /home/u"), "~ and ~");
    assert_eq!(r("/home/other/x"), "/home/other/x");
}

#[test]
fn home_and_keys_together() {
    assert_eq!(
        r(&format!("/home/u/prod.keys: {K32}")),
        "~/prod.keys: <hex>"
    );
}

#[test]
fn non_ascii_text_is_kept() {
    assert_eq!(r(&format!("pronto {K32} ✓ ç")), "pronto <hex> ✓ ç");
}
