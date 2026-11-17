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

use crate::error::{Error, Result};

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
        if text.is_empty() {
            return Ok(Config::default());
        }

        let mut lines = Vec::new();
        let text = text.strip_suffix('\n').unwrap_or(text);

        for (index, raw_line) in text.split('\n').enumerate() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let trimmed = line.trim();

            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with(';')
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
            {
                lines.push(Line::Other(line.to_owned()));
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                return Err(Error::Config(format!(
                    "line {} has no '=': {line}",
                    index + 1
                )));
            };
            let key = key.trim();
            if key.is_empty() {
                return Err(Error::Config(format!("line {} has no key", index + 1)));
            }
            lines.push(Line::Entry {
                key: key.to_owned(),
                value: value.trim().to_owned(),
            });
        }

        Ok(Config { lines })
    }

    /// The value of the last `key` entry, or `None`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().rev().find_map(|line| match line {
            Line::Entry {
                key: entry_key,
                value,
            } if entry_key == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Set `key` to `value`: every entry with that key gets the value; with none, one entry
    /// is added at the end.
    pub fn set(&mut self, key: &str, value: &str) {
        let mut found = false;
        for line in &mut self.lines {
            if let Line::Entry {
                key: entry_key,
                value: entry_value,
            } = line
            {
                if entry_key == key {
                    entry_value.clear();
                    entry_value.push_str(value);
                    found = true;
                }
            }
        }
        if !found {
            self.lines.push(Line::Entry {
                key: key.to_owned(),
                value: value.to_owned(),
            });
        }
    }

    /// The file text: each `Entry` as `key = value`, each `Other` as it was, every line ending
    /// in `\n`. `parse(&c.to_text())` equals `c`.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            match line {
                Line::Entry { key, value } => {
                    text.push_str(key);
                    text.push_str(" = ");
                    text.push_str(value);
                }
                Line::Other(line) => text.push_str(line),
            }
            text.push('\n');
        }
        text
    }
}
