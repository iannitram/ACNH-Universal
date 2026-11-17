// Locked tests for version (issue 1.02: the four versions we play and the gate, SPEC.md M1).
use acnh_core::version::{from_build_id, Version, ALL, RECOMMENDED};
use acnh_core::Error;

const IDS: [(&str, &str); 4] = [
    ("3.0.2", "FCD2BB238ABE99E925B4E452DF1F41F1"),
    ("3.0.3", "FF1D1C05670DB6021C85B624A710B963"),
    ("2.0.7", "0948E48778171EE6D5D7A65813ED5FCC"),
    ("1.0.0", "7FC1BAFF976AECA414520CB89F4616CB"),
];

#[test]
fn names_and_ids_in_order() {
    let got: Vec<(&str, &str)> = ALL.iter().map(|v| (v.name(), v.build_id())).collect();
    assert_eq!(got, IDS);
    assert_eq!(RECOMMENDED, Version::V3_0_2);
    assert_eq!(ALL[0], RECOMMENDED);
}

#[test]
fn native60_only_on_3_0_x() {
    let got: Vec<bool> = ALL.iter().map(|v| v.native60()).collect();
    assert_eq!(got, [true, true, false, false]);
}

#[test]
fn from_name_round_trips() {
    for v in ALL {
        assert_eq!(Version::from_name(v.name()), Some(v));
    }
    for n in ["3.0.1", "3.0", "v3.0.2", " 3.0.2", ""] {
        assert_eq!(Version::from_name(n), None, "{n:?}");
    }
}

#[test]
fn the_gate_takes_each_known_id() {
    for v in ALL {
        assert_eq!(from_build_id(v.build_id()), Ok(v));
    }
}

#[test]
fn the_gate_ignores_case_and_outer_spaces() {
    let lower = "fcd2bb238abe99e925b4e452df1f41f1";
    assert_eq!(from_build_id(lower), Ok(Version::V3_0_2));
    assert_eq!(
        from_build_id(&format!("  {lower}\t\n")),
        Ok(Version::V3_0_2)
    );
}

#[test]
fn the_gate_refuses_anything_else_with_the_trimmed_upper_input() {
    for (input, found) in [
        (
            "00112233445566778899aabbccddeeff",
            "00112233445566778899AABBCCDDEEFF",
        ),
        (
            " FCD2BB238ABE99E925B4E452DF1F41F ",
            "FCD2BB238ABE99E925B4E452DF1F41F",
        ),
        (
            "FCD2BB238ABE99E925B4E452DF1F41F1FF",
            "FCD2BB238ABE99E925B4E452DF1F41F1FF",
        ),
        (
            "FCD2 BB238ABE99E925B4E452DF1F41F1",
            "FCD2 BB238ABE99E925B4E452DF1F41F1",
        ),
        ("not hex", "NOT HEX"),
        ("", ""),
    ] {
        assert_eq!(
            from_build_id(input),
            Err(Error::WrongVersion {
                found: found.to_string()
            }),
            "{input:?}"
        );
    }
}

#[test]
fn the_refusal_names_the_versions_we_play() {
    let msg = from_build_id("00").unwrap_err().to_string();
    assert!(msg.contains("(00)"), "{msg}");
    assert!(
        msg.contains("3.0.2 (recommended), 3.0.3, 2.0.7 and 1.0.0"),
        "{msg}"
    );
}
