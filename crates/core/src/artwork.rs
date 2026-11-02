//! The Steam library images for our shortcut (SPEC.md M2): their file names under
//! `userdata/<user>/config/grid/` and their sizes. The images are made at build time from the
//! game folder's own icon when it has one, else from the project's `assets/acnhrecomp.png`;
//! nothing is downloaded and no game image is kept in this repo (M14). Copied from
//! iannitram/totk-universal's `artwork` (MIT) by its issue.

use std::path::{Path, PathBuf};

/// One of the images Steam shows for a non-Steam game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Art {
    /// The portrait capsule in the library grid.
    Grid,
    /// The wide capsule (recent games).
    WideGrid,
    /// The banner across the top of the game's page.
    Hero,
    /// The logo drawn over the hero.
    Logo,
    /// The small icon.
    Icon,
}

/// Every `Art`, in the order above.
pub const ALL: [Art; 5] = [Art::Grid, Art::WideGrid, Art::Hero, Art::Logo, Art::Icon];

/// The file name Steam reads for `art` of the shortcut `app_id` (decimal): Grid `<id>p.png`,
/// WideGrid `<id>.png`, Hero `<id>_hero.png`, Logo `<id>_logo.png`, Icon `<id>_icon.png`.
pub fn file_name(art: Art, app_id: u32) -> String {
    not_built!("artwork::file_name")
}

/// The size in pixels (width, height) we make `art` at: Grid 600x900, WideGrid 920x430,
/// Hero 1920x620, Logo 1280x720, Icon 256x256.
pub fn size(art: Art) -> (u32, u32) {
    not_built!("artwork::size")
}

/// The grid folder of one Steam user folder (`userdata/<id>`): `<user_dir>/config/grid`.
pub fn grid_dir(user_dir: &Path) -> PathBuf {
    not_built!("artwork::grid_dir")
}

/// Every image's full path for `app_id` in `user_dir`, in `ALL` order:
/// `grid_dir(user_dir)` joined with `file_name`.
pub fn paths(user_dir: &Path, app_id: u32) -> Vec<(Art, PathBuf)> {
    not_built!("artwork::paths")
}
