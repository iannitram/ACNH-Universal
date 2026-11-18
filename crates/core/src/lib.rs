//! acnh-core: the launcher's engine (docs/code-map.md). The Slint app (`app/`, planned) and
//! the `acnh-universal` command line both drive it; neither has logic of its own.
//!
//! Skeleton (2026-11-02): every module has its final public signatures, in the pattern of
//! iannitram/totk-universal (MIT), whose generic modules (platform, vdf, steam, artwork,
//! backup, bundle, fleet, launch, update, hub) are copied here with credit by their issues.
//! A function that is not built yet returns `Err(Error::NotBuilt(..))`, or panics with "not
//! built yet" when its signature cannot fail; each one is filled in by its own issue. MIT.
//!
//! Never in this crate: game data, keys, firmware, or code that copies, uploads or logs them
//! beyond my own devices (SPEC.md M1, M14).

// Skeleton only: stubs ignore their inputs. Remove this line when the last stub is built.
#![allow(unused_variables)]
#![forbid(unsafe_code)]

/// Panics with "not built yet: <what>"; the body of a stub whose signature cannot fail.
/// Kept (allowed unused) until the last stub is built, then delete it with `error::not_built`.
#[allow(unused_macros)]
macro_rules! not_built {
    ($what:expr) => {
        panic!("not built yet: {}", $what)
    };
}

pub mod artwork;
pub mod backup;
pub mod bundle;
pub mod config;
pub mod error;
pub mod fleet;
pub mod gamefolder;
#[cfg(feature = "hub")]
pub mod hub;
pub mod launch;
pub mod nso;
pub mod nso2bin;
pub mod pack;
pub mod platform;
pub mod prepare;
pub mod profile;
pub mod recipe;
#[cfg(feature = "relay")]
pub mod relay;
pub mod sarc;
pub mod savecard;
pub mod saves;
pub mod shaders;
pub mod steam;
pub mod update;
pub mod vdf;
pub mod version;

pub use error::{Error, NotBuilt, Result};
