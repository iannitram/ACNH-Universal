// Locked tests for profile (issue 1.06: the built-in device profiles, SPEC.md M2).
use acnh_core::profile::{builtin, by_id, detect};

#[test]
fn three_profiles_in_order() {
    let ids: Vec<&str> = builtin().iter().map(|p| p.id).collect();
    assert_eq!(ids, ["amd-handheld", "strix-halo-tablet", "desktop"]);
}

#[test]
fn handheld_values() {
    let p = &builtin()[0];
    assert_eq!(p.name, "AMD handheld");
    assert_eq!(p.hardware, "Ryzen Z1 Extreme, Radeon 780M, 2560x1600 panel");
    assert_eq!(
        p.settings,
        &[
            ("video.fullscreen", "1"),
            ("video.window", "1280x800"),
            ("video.frame_cap", "30"),
        ]
    );
    assert_eq!(p.power, "20 W in Steam's performance menu");
}

#[test]
fn tablet_values() {
    let p = &builtin()[1];
    assert_eq!(p.name, "Strix Halo tablet");
    assert_eq!(
        p.hardware,
        "Ryzen AI Max+ 395, Radeon 8060S, 2560x1600 panel"
    );
    assert_eq!(
        p.settings,
        &[
            ("video.fullscreen", "1"),
            ("video.window", "2560x1600"),
            ("video.frame_cap", "60"),
        ]
    );
    assert_eq!(p.power, "plugged in, or 45 W on battery");
}

#[test]
fn desktop_values() {
    let p = &builtin()[2];
    assert_eq!(
        (p.name, p.hardware, p.power),
        ("Desktop", "any other PC", "")
    );
    assert!(p.settings.is_empty());
}

#[test]
fn by_id_finds_each() {
    for p in builtin() {
        assert_eq!(by_id(p.id), Some(p));
    }
    assert!(by_id("steam-deck").is_none());
    assert!(by_id("Desktop").is_none());
}

#[test]
fn detect_by_cpu_model() {
    let id = |m: &str| detect(m).id;
    assert_eq!(id("AMD Ryzen Z1 Extreme"), "amd-handheld");
    assert_eq!(
        id("AMD RYZEN AI MAX+ 395 w/ Radeon 8060S"),
        "strix-halo-tablet"
    );
    assert_eq!(id("AMD Ryzen AI Max+ 395"), "strix-halo-tablet");
    assert_eq!(id("13th Gen Intel(R) Core(TM) i7-13700K"), "desktop");
    assert_eq!(id("AMD Ryzen Z1"), "desktop");
    assert_eq!(id("amd ryzen z1 extreme"), "desktop");
    assert_eq!(id(""), "desktop");
}
