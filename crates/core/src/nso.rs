//! The NSO header of `exefs/main`: just enough to read its build id (SPEC.md M1). Replaces
//! the header read in upstream's `tools/acnh_version.py` and `launcher/files.cpp`
//! (`build_id`). Reads the first 0x100 bytes only; never reads, copies or logs the code.
//!
//! An NSO starts with the magic `NSO0` (bytes 0x4E 0x53 0x4F 0x30) at 0x00; its build id (the
//! module id) is the 32 bytes at 0x40, of which the game's version table uses the first 16.

use crate::error::{not_built, Result};
use std::path::Path;

/// The header bytes `build_id` needs: 0x60.
pub const HEADER_LEN: usize = 0x60;

/// The build id from an NSO header: the 16 bytes at 0x40..0x50 as 32 uppercase hex digits.
/// Fewer than `HEADER_LEN` bytes is `Error::NotAcnh("exefs/main is too short to be a Switch
/// executable")`; a first four bytes other than `NSO0` is `Error::NotAcnh("exefs/main is not
/// a Switch executable (NSO)")`.
pub fn build_id(header: &[u8]) -> Result<String> {
    not_built("nso::build_id")
}

/// `build_id` of the file at `path`, reading at most `HEADER_LEN` bytes. A file that cannot be
/// opened is `Error::Io`.
pub fn read_build_id(path: &Path) -> Result<String> {
    not_built("nso::read_build_id")
}
