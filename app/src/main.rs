use acnh_app::shell::{run, ShellConfig};
use acnh_core::platform::{AppDirs, Os, Vars};

fn fullscreen(flag: bool, desktop: Option<&str>, gamepad_ui: bool) -> bool {
    flag || desktop == Some("gamescope") || gamepad_ui
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let os = Os::current();
    let vars = Vars::from_lookup(os, |name| std::env::var(name).ok())?;
    let dirs = AppDirs::for_os(os, &vars);
    let launcher = std::env::current_exe()?;
    let picture = acnh_core::steam::assets_dir()?.join("acnhrecomp.png");
    let config = ShellConfig {
        vars,
        dirs,
        os,
        launcher,
        picture,
        fullscreen: fullscreen(
            std::env::args_os().any(|arg| arg == "--fullscreen"),
            std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
            std::env::var_os("SteamGamepadUI").is_some(),
        ),
    };
    run(config, Box::new(|_| Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_follows_arguments_and_gamescope() {
        assert!(!fullscreen(false, None, false));
        assert!(fullscreen(true, None, false));
        assert!(fullscreen(false, Some("gamescope"), false));
        assert!(fullscreen(false, None, true));
        assert!(!fullscreen(false, Some("desktop"), false));
    }
}
