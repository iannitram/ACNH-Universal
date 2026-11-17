//! Device profiles (SPEC.md M2): the settings a device starts the game with, written as
//! `config.ini` entries (`config`) under the player's own lines, which win. Values are
//! starting points, replaced by measured ones in the device runs. Power is advice shown on the
//! Play page: setting a power limit is the player's choice in Steam's performance menu, never
//! ours. Shape from iannitram/totk-universal's `profile` (MIT).

/// One device's profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    /// "amd-handheld", "strix-halo-tablet", "desktop".
    pub id: &'static str,
    pub name: &'static str,
    /// The hardware it is for, in words.
    pub hardware: &'static str,
    /// `config.ini` entries (`section.key`, value) the launcher sets when they are not set.
    pub settings: &'static [(&'static str, &'static str)],
    /// Power advice for the Play page; empty when there is none.
    pub power: &'static str,
}

/// The built-in profiles, in this order, with exactly these values:
///
/// 1. id "amd-handheld", name "AMD handheld", hardware "Ryzen Z1 Extreme, Radeon 780M,
///    2560x1600 panel", settings `video.fullscreen = 1`, `video.window = 1280x800`,
///    `video.frame_cap = 30`; power "20 W in Steam's performance menu".
/// 2. id "strix-halo-tablet", name "Strix Halo tablet", hardware "Ryzen AI Max+ 395,
///    Radeon 8060S, 2560x1600 panel", settings `video.fullscreen = 1`,
///    `video.window = 2560x1600`, `video.frame_cap = 60`; power "plugged in, or 45 W on
///    battery".
/// 3. id "desktop", name "Desktop", hardware "any other PC", settings empty, power "".
///
/// The keys are upstream's (`runtime/src/config.cpp`: `video.fullscreen` is `NX_FULLSCREEN`,
/// `video.window` is `NX_RESOLUTION`, `video.frame_cap` is `NX_FPS_CAP`).
pub fn builtin() -> &'static [DeviceProfile] {
    const PROFILES: &[DeviceProfile] = &[
        DeviceProfile {
            id: "amd-handheld",
            name: "AMD handheld",
            hardware: "Ryzen Z1 Extreme, Radeon 780M, 2560x1600 panel",
            settings: &[
                ("video.fullscreen", "1"),
                ("video.window", "1280x800"),
                ("video.frame_cap", "30"),
            ],
            power: "20 W in Steam's performance menu",
        },
        DeviceProfile {
            id: "strix-halo-tablet",
            name: "Strix Halo tablet",
            hardware: "Ryzen AI Max+ 395, Radeon 8060S, 2560x1600 panel",
            settings: &[
                ("video.fullscreen", "1"),
                ("video.window", "2560x1600"),
                ("video.frame_cap", "60"),
            ],
            power: "plugged in, or 45 W on battery",
        },
        DeviceProfile {
            id: "desktop",
            name: "Desktop",
            hardware: "any other PC",
            settings: &[],
            power: "",
        },
    ];
    PROFILES
}

/// The built-in profile with this id.
pub fn by_id(id: &str) -> Option<&'static DeviceProfile> {
    builtin().iter().find(|profile| profile.id == id)
}

/// Pick a profile from the CPU model name (`/proc/cpuinfo` "model name", or the OS's own):
/// it contains "Z1 Extreme" → amd-handheld; it contains "AI Max+ 395" or "AI MAX+ 395" →
/// strix-halo-tablet; anything else → desktop. Case as given.
pub fn detect(cpu_model: &str) -> &'static DeviceProfile {
    let id = if cpu_model.contains("Z1 Extreme") {
        "amd-handheld"
    } else if cpu_model.contains("AI Max+ 395") || cpu_model.contains("AI MAX+ 395") {
        "strix-halo-tablet"
    } else {
        "desktop"
    };
    by_id(id).expect("detected profile must be built in")
}
