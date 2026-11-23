//! The desktop launcher shell and its plain Rust view models.

pub mod build_actions;
pub mod first_run;
pub mod input;
pub mod play;
pub mod player_env;
pub mod relay;
pub mod relay_setup;
pub mod ui;
pub mod vm;

slint::include_modules!();
