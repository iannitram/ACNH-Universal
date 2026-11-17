// Locked tests for recipe (issue 1.07: build hubs, prepare stages, outputs; ADR 0004).
use acnh_core::error::Error;
use acnh_core::platform::{Os, ALL_OS};
use acnh_core::recipe::{hub_by_key, hub_for, output, serves, Hub, Stage, STAGES};

#[test]
fn hub_for_each_target() {
    assert_eq!(hub_for(Os::Linux), Hub::StrixHalo);
    assert_eq!(hub_for(Os::Android), Hub::StrixHalo);
    assert_eq!(hub_for(Os::Windows), Hub::Desktop);
    assert_eq!(hub_for(Os::Mac), Hub::Mac);
    assert_eq!(hub_for(Os::Ios), Hub::Mac);
}

#[test]
fn serves_lists_in_order() {
    assert_eq!(serves(Hub::StrixHalo), &[Os::Linux, Os::Android]);
    assert_eq!(serves(Hub::Desktop), &[Os::Windows]);
    assert_eq!(serves(Hub::Mac), &[Os::Mac, Os::Ios]);
}

#[test]
fn serves_round_trips_hub_for() {
    for t in ALL_OS {
        assert!(serves(hub_for(t)).contains(&t), "{}", t.key());
    }
}

#[test]
fn hub_keys() {
    assert_eq!(Hub::StrixHalo.key(), "strix-halo");
    assert_eq!(Hub::Desktop.key(), "desktop");
    assert_eq!(Hub::Mac.key(), "mac");
    for h in [Hub::StrixHalo, Hub::Desktop, Hub::Mac] {
        assert_eq!(hub_by_key(h.key()).unwrap(), h);
    }
}

#[test]
fn unknown_or_inexact_key_is_a_hub_error() {
    for k in ["tablet", "Mac", "DESKTOP", "strix_halo", " mac", "mac ", ""] {
        assert_eq!(
            hub_by_key(k).unwrap_err(),
            Error::Hub(format!("unknown hub {k}: strix-halo, desktop or mac")),
            "{k:?}"
        );
    }
    assert_eq!(
        hub_by_key("tablet").unwrap_err().to_string(),
        "Build hub: unknown hub tablet: strix-halo, desktop or mac"
    );
}

#[test]
fn stage_keys_in_upstream_order() {
    let keys: Vec<&str> = STAGES.iter().map(|s| s.key()).collect();
    assert_eq!(
        keys,
        [
            "tools",
            "game",
            "version",
            "recompile",
            "shaders",
            "fonts",
            "build"
        ]
    );
    let mut sorted = STAGES;
    sorted.sort();
    assert_eq!(sorted, STAGES);
}

#[test]
fn stage_from_progress_line() {
    for s in STAGES {
        assert_eq!(Stage::from_line(&format!("== {}", s.key())), Some(s));
    }
    assert_eq!(
        Stage::from_line("== Recompile (8 jobs)"),
        Some(Stage::Recompile)
    );
    assert_eq!(Stage::from_line("== SHADERS"), Some(Stage::Shaders));
    assert_eq!(Stage::from_line("==recompile"), None);
    assert_eq!(Stage::from_line("  == build"), None);
    assert_eq!(Stage::from_line("== "), None);
    assert_eq!(Stage::from_line("== link"), None);
    assert_eq!(Stage::from_line("recompile"), None);
}

#[test]
fn outputs_per_target() {
    let got: Vec<&str> = ALL_OS.iter().map(|&t| output(t)).collect();
    assert_eq!(
        got,
        ["acnh", "acnh.exe", "ACNH.app", "ACNH.ipa", "acnh.apk"]
    );
}
