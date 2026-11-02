//! The island card on the Continue screen (SPEC.md M10): when the island was saved, on which
//! device, for how long it has been played, and a picture. Shape from
//! iannitram/totk-universal's `savecard` (MIT). The island's name is after 1.0: it needs a
//! save reader of our own (NHSE's format notes, ideas only, since NHSE is GPL-3.0).

use crate::error::{not_built, Result};
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
/// ignored; a later line wins. No sidecar leaves `device` and `version` "" and `play_seconds`
/// 0. `saved_unix` is `<save_dir>/main.dat`'s modified time; with no `main.dat` the card is
/// `Error::Saves("no island at <save_dir>")`. `picture` is copied into the card as given.
pub fn read_card(save_dir: &Path, picture: &Path) -> Result<SaveCard> {
    not_built("savecard::read_card")
}

/// The newer of two cards by `saved_unix`; `a` on a tie.
pub fn latest<'a>(a: &'a SaveCard, b: &'a SaveCard) -> &'a SaveCard {
    not_built!("savecard::latest")
}

/// The two lines the conflict screen shows under each card, in this order: "<device>, <date>"
/// (date `YYYY-MM-DD HH:MM` UTC; "unknown device" when `device` is empty) and
/// "<h> h <mm> min played" (`play_seconds`; "play time unknown" when 0).
pub fn side_by_side(card: &SaveCard) -> [String; 2] {
    not_built!("savecard::side_by_side")
}
