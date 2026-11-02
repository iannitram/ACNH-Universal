//! The runtime's settings file, `<data root>/config.ini`, and the launcher's `launcher.ini`
//! (SPEC.md M2: device profiles write their settings here). Same format as upstream's
//! `runtime/src/config.cpp`: one `section.key = value` per line; the runtime turns each
//! setting it maps into its `NX_*` environment variable unless that is already set.
//!
//! `launcher.ini` lives in the launcher's settings folder (`platform::AppDirs::config`).
//!
//! Rules: a line splits at its first `=`; spaces around the key and the value are trimmed;
//! an empty line, or a line whose first non-space character is `#` or `;`, is a comment; a
//! line `[name]` is a section header and is kept but means nothing (upstream writes keys with
//! their section). Unknown keys are kept. Writing keeps every line in place and changes only
//! the value of a key that is set.

use crate::error::{not_built, Result};

/// One line of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// `key = value`, trimmed.
    Entry { key: String, value: String },
    /// A comment, a blank line or a `[section]` header, kept exactly as it was.
    Other(String),
}

/// A parsed settings file, lines in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
    pub lines: Vec<Line>,
}

impl Config {
    /// Parse `text` (lines split on `\n`, a trailing `\r` dropped; the empty piece after a
    /// final `\n` is not a line, so "" and "a=1\n" have 0 and 1 lines). A line that is not a
    /// comment, blank or `[section]` and has no `=` is
    /// `Error::Config("line <n> has no '=': <line>")` (n from 1); an empty key is
    /// `Error::Config("line <n> has no key")`.
    pub fn parse(text: &str) -> Result<Config> {
        not_built("config::Config::parse")
    }

    /// The value of the last `key` entry, or `None`.
    pub fn get(&self, key: &str) -> Option<&str> {
        not_built!("config::Config::get")
    }

    /// Set `key` to `value`: every entry with that key gets the value; with none, one entry
    /// is added at the end.
    pub fn set(&mut self, key: &str, value: &str) {
        not_built!("config::Config::set")
    }

    /// The file text: each `Entry` as `key = value`, each `Other` as it was, every line ending
    /// in `\n`. `parse(&c.to_text())` equals `c`.
    pub fn to_text(&self) -> String {
        not_built!("config::Config::to_text")
    }
}
