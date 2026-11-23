//! The one error type. Every variant has a stable `code()` (the launcher's UI and the CLI's
//! `--json` match on it) and a plain message (`Display`) a player can read. Built in the
//! skeleton: every module uses it. Shape from iannitram/totk-universal (MIT).
//!
//! No message ever holds game data: errors name files, never their contents (SPEC.md M14).

use std::fmt;

/// What a stub returns, so tests and callers can tell "not built yet" apart.
/// The string names the function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotBuilt(pub &'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A stub (skeleton only).
    NotBuilt(NotBuilt),
    /// `exefs/main` is a build of the game we do not play. `found` is its build id (32
    /// uppercase hex digits).
    WrongVersion { found: String },
    /// The file is not Animal Crossing: New Horizons, or not a Switch executable (the string
    /// says what was found).
    NotAcnh(String),
    /// The game folder is missing a file or folder (the string names it).
    BadGameFolder(String),
    /// A folder the rules need is not set (an environment variable, the install folder).
    Env(String),
    /// Nothing is built or run this way on this device.
    Unsupported(String),
    /// `config.ini` or `launcher.ini`: a line that does not parse.
    Config(String),
    /// Steam's files: shortcuts.vdf unreadable, no Steam user found, Steam still running.
    Steam(String),
    /// The island's save folder or a backup.
    Saves(String),
    /// A prepare stage failed; the string is its last log lines, redacted.
    Build(String),
    /// A build hub, its bundles or its pairing (ADR 0004).
    Hub(String),
    /// The configured relay could not be reached before play.
    RelayUnreachable,
    /// A local file operation failed.
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The stable code. Never rename one.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotBuilt(_) => "not_built",
            Error::WrongVersion { .. } => "wrong_version",
            Error::NotAcnh(_) => "not_acnh",
            Error::BadGameFolder(_) => "bad_game_folder",
            Error::Env(_) => "env",
            Error::Unsupported(_) => "unsupported",
            Error::Config(_) => "config",
            Error::Steam(_) => "steam",
            Error::Saves(_) => "saves",
            Error::Build(_) => "build",
            Error::Hub(_) => "hub",
            Error::RelayUnreachable => "relay_unreachable",
            Error::Io(_) => "io",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotBuilt(NotBuilt(what)) => write!(f, "not built yet: {what}"),
            Error::WrongVersion { found } => write!(
                f,
                "This game folder holds a build we do not play ({found}). ACNH Universal \
                 plays 3.0.2 (recommended), 3.0.3, 2.0.7 and 1.0.0 only: dump one of those \
                 from your console and pick that folder."
            ),
            Error::NotAcnh(m) => write!(f, "This is not Animal Crossing: New Horizons: {m}"),
            Error::BadGameFolder(m) => write!(f, "This game folder is not usable: {m}"),
            Error::Env(m) => write!(f, "A folder is not set: {m}"),
            Error::Unsupported(m) => write!(f, "Not on this device: {m}"),
            Error::Config(m) => write!(f, "Settings file: {m}"),
            Error::Steam(m) => write!(f, "Steam: {m}"),
            Error::Saves(m) => write!(f, "Saves: {m}"),
            Error::Build(m) => write!(f, "The build failed: {m}"),
            Error::Hub(m) => write!(f, "Build hub: {m}"),
            Error::RelayUnreachable => write!(
                f,
                "save-relay could not be reached and the game was not started."
            ),
            Error::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e.to_string())
    }
}

/// The body of a stub whose signature returns `Result`.
pub fn not_built<T>(what: &'static str) -> Result<T> {
    Err(Error::NotBuilt(NotBuilt(what)))
}
