//! Play (SPEC.md M6): pull the island, start the game with the device profile, wait for it to
//! quit, push the island. save-relay does the syncing (through `Relay`, implemented by
//! `relay.rs` once save-relay's library is linked); the game is started through `Game`; both
//! are traits so the flow is tested with fakes. Copied from iannitram/totk-universal's
//! `launch` (MIT) by its issue.

use crate::error::{Error, Result};
use crate::savecard::SaveCard;
use std::path::Path;

/// What one save-relay pull or push did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sync {
    /// Done (pulled, pushed, or nothing to do); the message is save-relay's.
    Done(String),
    /// A conflict was settled and the other side retained as a history entry.
    Resolved { message: String, backup: String },
    /// The configured relay is unreachable: refuse the pull or retry the push later.
    Offline,
    /// Sync is not configured.
    Off,
    /// Another device has the island open.
    Held { device: String, since: String },
    /// Both sides changed since the last sync; nothing was touched.
    Conflict {
        server: SaveCard,
        local: SaveCard,
        backup: Option<String>,
    },
}

/// Status before play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayStatus {
    Ready,
    RelayNewer(SaveCard),
    LocalNewer,
    Held { device: String, since: String },
    Unavailable,
    Conflict { server: SaveCard, local: SaveCard },
    Off,
}

/// The player's answer to a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    Server,
    Local,
}

/// save-relay, as the launcher uses it.
pub trait Relay {
    fn pull(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync>;
    fn push(&self, save_dir: &Path, resolve: Option<Keep>) -> Result<Sync>;
    fn pull_anyway(&self, save_dir: &Path) -> Result<Sync> {
        self.pull(save_dir, None)
    }
    fn status(&self, _save_dir: &Path) -> Result<RelayStatus> {
        Ok(RelayStatus::Ready)
    }
}

/// The built game (the player, started with `NX_DATA_ROOT` set to the data root).
pub trait Game {
    /// Start the game with `env` added and wait until it quits; Ok with its exit code.
    fn run(&self, env: &[(String, String)]) -> Result<i32>;
}

/// What a play session did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    pub pulled: Sync,
    pub exit_code: i32,
    pub pushed: Sync,
}

/// The play flow: `relay.pull`; on `Conflict` ask `choose` and pull again with its answer;
/// `Offline` refuses play; `Off` plays without syncing. Then `game.run(env)`, then
/// `relay.push` (a conflict asks again). A
/// relay error before the game starts stops the flow (the game does not start); a push error
/// is returned after the game has quit.
pub fn play(
    relay: &dyn Relay,
    game: &dyn Game,
    save_dir: &Path,
    env: &[(String, String)],
    choose: &dyn Fn(&Sync) -> Keep,
) -> Result<Played> {
    let pulled = sync_step(|resolve| relay.pull(save_dir, resolve), choose)?;
    match &pulled {
        Sync::Offline => return Err(Error::RelayUnreachable),
        Sync::Held { .. } | Sync::Conflict { .. } => {
            return Err(Error::Saves("the island is not ready to play".into()))
        }
        _ => {}
    }
    let exit_code = game.run(env)?;
    let pushed = sync_step(|resolve| relay.push(save_dir, resolve), choose)?;
    Ok(Played {
        pulled,
        exit_code,
        pushed,
    })
}

fn sync_step<F>(mut step: F, choose: &dyn Fn(&Sync) -> Keep) -> Result<Sync>
where
    F: FnMut(Option<Keep>) -> Result<Sync>,
{
    match step(None)? {
        conflict @ Sync::Conflict { .. } => {
            let choice = choose(&conflict);
            step(Some(choice))
        }
        other => Ok(other),
    }
}
