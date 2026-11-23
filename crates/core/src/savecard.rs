//! The island card on the Continue screen (SPEC.md M10): when the island was saved, on which
//! device, for how long it has been played, and a picture. Shape from
//! iannitram/totk-universal's `savecard` (MIT). The island's name is after 1.0: it needs a
//! save reader of our own (NHSE's format notes, ideas only, since NHSE is GPL-3.0).

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

/// One island, as the Continue screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveCard {
    /// Seconds since the Unix epoch of `main.dat`'s last write.
    pub saved_unix: u64,
    /// The device that made it ("AMD handheld"); "" when not known.
    pub device: String,
    /// Play time in seconds as the launcher counted it; 0 when not known.
    pub play_seconds: u64,
    /// The game version that made it ("3.0.2"); "" when not known.
    pub version: String,
    /// The picture: the game folder's icon when the hub sent one, else the project's own
    /// `assets/acnhrecomp.png`.
    pub picture: PathBuf,
}

/// The sidecar the launcher writes beside the island after each play,
/// `<save_dir>/../launcher-card.txt` (outside `device_save`, so it does not sync as game data
/// but travels with the save-relay metadata): lines `device <name>`, `play <seconds>`,
/// `version <name>`, in any order; unknown lines, and a `play` value that is not a number, are
/// ignored; a later line wins. Relay-written cards also carry `saved` and `file_modified_ns`:
/// when the file still matches that stamp, `saved` is the relay card time; when it changed,
/// device and play time are unknown. No sidecar leaves `device` and `version` "" and
/// `play_seconds` 0. Otherwise `saved_unix` is `<save_dir>/main.dat`'s modified time; with no `main.dat` the card is
/// `Error::Saves("no island at <save_dir>")`. `picture` is copied into the card as given.
pub fn read_card(save_dir: &Path, picture: &Path) -> Result<SaveCard> {
    let main_dat = save_dir.join("main.dat");
    let meta = match std::fs::metadata(&main_dat) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Err(Error::Saves(format!("no island at {}", save_dir.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Saves(format!("no island at {}", save_dir.display())));
        }
        Err(e) => return Err(Error::from(e)),
    };
    let modified = meta.modified()?;
    let mut saved_unix = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => 0,
    };

    let mut device = String::new();
    let mut play_seconds = 0u64;
    let mut version = String::new();
    let mut card_saved = None;
    let mut file_stamp = None;

    let sidecar = save_dir.join("..").join("launcher-card.txt");
    if let Ok(content) = std::fs::read_to_string(&sidecar) {
        for line in content.lines() {
            if let Some((key, val)) = line.split_once(' ') {
                match key {
                    "device" => device = val.to_string(),
                    "saved" => card_saved = val.parse::<u64>().ok(),
                    "file_modified_ns" => file_stamp = val.parse::<u128>().ok(),
                    "play" => {
                        if let Ok(secs) = val.parse::<u64>() {
                            play_seconds = secs;
                        }
                    }
                    "version" => version = val.to_string(),
                    _ => {}
                }
            }
        }
    }

    if let Some(stamp) = file_stamp {
        let actual = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_nanos());
        if actual == Some(stamp) {
            if let Some(time) = card_saved {
                saved_unix = time;
            }
        } else {
            device.clear();
            play_seconds = 0;
        }
    }

    Ok(SaveCard {
        saved_unix,
        device,
        play_seconds,
        version,
        picture: picture.to_path_buf(),
    })
}

/// The newer of two cards by `saved_unix`; `a` on a tie.
pub fn latest<'a>(a: &'a SaveCard, b: &'a SaveCard) -> &'a SaveCard {
    if b.saved_unix > a.saved_unix {
        b
    } else {
        a
    }
}

/// The two lines the conflict screen shows under each card, in this order: "<device>, <date>"
/// (date `YYYY-MM-DD HH:MM` UTC; "unknown device" when `device` is empty) and
/// "<h> h <mm> min played" (`play_seconds`; "play time unknown" when 0).
pub fn side_by_side(card: &SaveCard) -> [String; 2] {
    let device_part = if card.device.is_empty() {
        "unknown device"
    } else {
        card.device.as_str()
    };
    let date_part = format_utc_datetime(card.saved_unix);
    let line1 = format!("{device_part}, {date_part}");

    let line2 = if card.play_seconds == 0 {
        "play time unknown".to_string()
    } else {
        let hours = card.play_seconds / 3600;
        let minutes = (card.play_seconds % 3600) / 60;
        format!("{hours} h {minutes:02} min played")
    };

    [line1, line2]
}

fn format_utc_datetime(unix: u64) -> String {
    let day_seconds = unix % 86400;
    let hour = day_seconds / 3600;
    let minute = (day_seconds % 3600) / 60;

    let z = (unix / 86400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };

    format!("{year:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}
