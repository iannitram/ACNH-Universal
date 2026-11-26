// Locked tests for savecard (issue 1.09: the island card on the Continue screen, SPEC.md M10).
use acnh_core::savecard::{latest, read_card, side_by_side, SaveCard};
use acnh_core::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

fn card(saved_unix: u64, device: &str, play_seconds: u64) -> SaveCard {
    SaveCard {
        saved_unix,
        device: device.into(),
        play_seconds,
        version: String::new(),
        picture: PathBuf::from("p.png"),
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acnh-card-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("device_save")).unwrap();
    d
}

fn island(d: &Path, unix: u64) {
    let main = d.join("device_save/main.dat");
    fs::write(&main, b"").unwrap();
    let f = fs::File::options().write(true).open(&main).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(unix))
        .unwrap();
}

#[test]
fn reads_the_sidecar_and_the_save_time() {
    let d = tmp("read");
    island(&d, 1_793_620_799);
    fs::write(
        d.join("launcher-card.txt"),
        "version 3.0.2\nplay x\ndevice AMD handheld\nplay 3720\nnote ignored\ndevice Strix Halo tablet\n",
    )
    .unwrap();
    let c = read_card(&d.join("device_save"), Path::new("/a/icon.png"));
    fs::remove_dir_all(&d).unwrap();
    let c = c.unwrap();
    assert_eq!(c.saved_unix, 1_793_620_799);
    assert_eq!(c.device, "Strix Halo tablet");
    assert_eq!(c.play_seconds, 3720);
    assert_eq!(c.version, "3.0.2");
    assert_eq!(c.picture, PathBuf::from("/a/icon.png"));
}

#[test]
fn no_sidecar_leaves_blanks() {
    let d = tmp("bare");
    island(&d, 100);
    let c = read_card(&d.join("device_save"), Path::new("p.png"));
    fs::remove_dir_all(&d).unwrap();
    let c = c.unwrap();
    assert_eq!(
        (
            c.saved_unix,
            c.device.as_str(),
            c.play_seconds,
            c.version.as_str()
        ),
        (100, "", 0, "")
    );
}

#[test]
fn no_island_is_a_saves_error() {
    let d = tmp("none");
    let save = d.join("device_save");
    let r = read_card(&save, Path::new("p.png"));
    fs::remove_dir_all(&d).unwrap();
    assert_eq!(
        r,
        Err(Error::Saves(format!("no island at {}", save.display())))
    );
}

#[test]
fn latest_by_time_and_a_on_a_tie() {
    let (a, b) = (card(10, "A", 0), card(20, "B", 0));
    assert_eq!(latest(&a, &b).device, "B");
    assert_eq!(latest(&b, &a).device, "B");
    let c = card(10, "C", 0);
    assert_eq!(latest(&a, &c).device, "A");
    assert_eq!(latest(&c, &a).device, "C");
}

#[test]
fn side_by_side_lines() {
    assert_eq!(
        side_by_side(&card(1_793_620_799, "AMD handheld", 3720)),
        ["AMD handheld, 2026-11-02 11:59 UTC", "1 h 02 min played"]
    );
    assert_eq!(
        side_by_side(&card(0, "", 0)),
        ["unknown device, 1970-01-01 00:00 UTC", "play time unknown"]
    );
    assert_eq!(
        side_by_side(&card(951_782_400, "D", 59))[1],
        "0 h 00 min played"
    );
    assert_eq!(
        side_by_side(&card(951_782_400, "D", 360_000))[0],
        "D, 2000-02-29 00:00 UTC"
    );
    assert_eq!(
        side_by_side(&card(951_782_400, "D", 360_000))[1],
        "100 h 00 min played"
    );
}

#[test]
fn a_relay_stamp_reads_like_a_card_time() {
    use acnh_core::savecard::readable_stamp;
    assert_eq!(
        readable_stamp("2026-11-24T17:39:43Z"),
        "2026-11-24 17:39 UTC"
    );
    assert_eq!(readable_stamp("2026-11-24T17:39Z"), "2026-11-24 17:39 UTC");
    assert_eq!(readable_stamp("yesterday"), "yesterday");
    assert_eq!(
        readable_stamp("2026-11-24T17:39:43+02:00"),
        "2026-11-24T17:39:43+02:00"
    );
}
