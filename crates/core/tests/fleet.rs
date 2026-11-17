// Locked tests for fleet (issue 1.20: drift, warning and the one-line device record, M11).
use acnh_core::fleet::{drift, from_line, to_line, warning, DeviceRecord, Drift};
use acnh_core::platform::{Os, ALL_OS};

#[rustfmt::skip]
fn rec(device: &str, version: &str, build: &str) -> DeviceRecord {
    DeviceRecord { device: device.into(), os: Os::Linux, version: version.into(), build_id: build.into(), last_sync_unix: 1_700_000_000, free_bytes: 512_000 }
}

#[test]
fn same_version_and_build_is_no_drift() {
    let a = rec("A", "3.0.2", "b1");
    let mut b = rec("B", "3.0.2", "b1");
    b.os = Os::Android;
    b.free_bytes = 1;
    assert_eq!(drift(&a, &b), Drift::None);
}

#[test]
fn version_drift_takes_theirs_from_saved_by_and_ours_from_here() {
    let a = rec("A", "3.0.2", "b1");
    let want = Drift::Version {
        theirs: "3.0.3".to_string(),
        ours: "3.0.2".to_string(),
    };
    assert_eq!(drift(&a, &rec("B", "3.0.3", "b1")), want);
    // A version difference wins over a build difference.
    assert_eq!(drift(&a, &rec("B", "3.0.3", "b2")), want);
}

#[test]
fn build_drift_at_the_same_version() {
    let a = rec("A", "3.0.2", "b1");
    let want = Drift::Build {
        theirs: "b2".to_string(),
        ours: "b1".to_string(),
    };
    assert_eq!(drift(&a, &rec("B", "3.0.2", "b2")), want);
}

#[test]
fn warning_texts_are_exact() {
    let by = rec("AMD handheld", "3.0.3", "b2");
    assert_eq!(warning(&Drift::None, &by), None);
    let v = Drift::Version {
        theirs: "3.0.3".to_string(),
        ours: "3.0.2".to_string(),
    };
    assert_eq!(
        warning(&v, &by).unwrap(),
        "AMD handheld made this save with version 3.0.3; this device has 3.0.2. \
         Put the same version on both, or the game may refuse the island."
    );
    let b = Drift::Build {
        theirs: "b2".to_string(),
        ours: "b1".to_string(),
    };
    assert_eq!(
        warning(&b, &by).unwrap(),
        "AMD handheld made this save with another build of ACNH Universal. \
         Update this device first, or play on."
    );
}

#[test]
fn line_is_tab_separated_in_field_order() {
    let mut r = rec("AMD handheld", "3.0.2", "deadbeefcafef00d");
    assert_eq!(
        to_line(&r).unwrap(),
        "AMD handheld\tlinux\t3.0.2\tdeadbeefcafef00d\t1700000000\t512000"
    );
    r.os = Os::Android;
    r.last_sync_unix = 0;
    assert_eq!(
        to_line(&r).unwrap(),
        "AMD handheld\tandroid\t3.0.2\tdeadbeefcafef00d\t0\t512000"
    );
}

#[test]
fn a_tab_or_new_line_in_the_device_name_is_refused() {
    for name in ["a\tb", "a\nb", "\t", "x\n"] {
        let err = to_line(&rec(name, "3.0.2", "b")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Build hub: a device name cannot hold a tab or a new line"
        );
    }
}

#[test]
fn every_os_round_trips() {
    for os in ALL_OS {
        let mut r = rec("Desk top", "1.0.0", "0123456789abcdef");
        r.os = os;
        r.free_bytes = u64::MAX;
        assert_eq!(from_line(&to_line(&r).unwrap()).unwrap(), r);
    }
}

#[test]
fn from_line_reads_a_literal_line() {
    let r = from_line("Z13\tmac\t2.0.7\tb\t99\t7").unwrap();
    let mut want = rec("Z13", "2.0.7", "b");
    want.os = Os::Mac;
    want.last_sync_unix = 99;
    want.free_bytes = 7;
    assert_eq!(r, want);
}

#[test]
fn bad_lines_are_refused_with_the_line_in_the_message() {
    for line in [
        "",
        "A\tlinux\t3.0.2\tb\t2",
        "A\tlinux\t3.0.2\tb\t2\t3\t4",
        "A\tbeos\t3.0.2\tb\t2\t3",
        "A\tLinux\t3.0.2\tb\t2\t3",
        "A\tlinux\t3.0.2\tb\tnow\t3",
        "A\tlinux\t3.0.2\tb\t2\t-3",
    ] {
        let err = from_line(line).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("Build hub: bad device record: {line}")
        );
    }
}
