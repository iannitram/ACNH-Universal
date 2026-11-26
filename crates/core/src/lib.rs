//! acnh-core: the launcher's engine (docs/code-map.md). The Slint app (`app/`) and
//! the `acnh-universal` command line both drive it; neither has logic of its own. The Android
//! and iOS players link it too (the in-app download, `appgame`).
//!
//! The generic modules (platform, vdf, steam, artwork, backup, bundle, fleet, launch, update,
//! hub) follow iannitram/totk-universal (MIT), copied here with credit by their issues. MIT.
//!
//! Never in this crate: game data, keys, firmware, or code that copies, uploads or logs them
//! beyond my own devices (SPEC.md M1, M14).

#![forbid(unsafe_code)]

pub mod appgame;
pub mod archive;
pub mod artwork;
pub mod assemble;
pub mod backup;
pub mod bundle;
pub mod config;
pub mod error;
pub mod fleet;
pub(crate) mod fsutil;
pub mod gamefolder;
pub mod glyphs;
#[cfg(feature = "hub")]
pub mod hub;
pub mod launch;
pub mod msg;
pub mod nso;
pub mod nso2bin;
pub mod pack;
pub mod pack_folder;
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

pub use error::{Error, Result};
