//! This launcher's place in the fleet (SPEC.md M11): which device it is, which build it runs,
//! who made the island on disk, and the report it sends its paired hub after each sync.

use acnh_core::config::Config;
use acnh_core::fleet::DeviceRecord;
use acnh_core::platform::{AppDirs, Os};
use acnh_core::savecard::SaveCard;
use acnh_core::{bundle, gamefolder, hub, Result};
use std::net::SocketAddr;
use std::path::Path;

/// What sends a record to a hub: `hub::report_hub`, or a recorder in a test.
pub type Sender<'a> = dyn Fn(SocketAddr, &AppDirs, &str, &DeviceRecord) -> Result<()> + 'a;

/// The game version and build id this device runs: the installed bundle's when it was pulled
/// from a hub, else the version of the game folder a local prepare used, with this launcher's
/// own version as the build. `None` when neither is known (the page then warns of nothing).
fn installed_build(dirs: &AppDirs, os: Os) -> Option<(String, String)> {
    let manifest = std::fs::read_to_string(dirs.config.join("installed-bundle.txt"))
        .ok()
        .and_then(|text| bundle::from_text(&text).ok())
        .filter(|manifest| manifest.target == os);
    if let Some(manifest) = manifest {
        return Some((manifest.version, manifest.build_id));
    }
    let folder = crate::build_actions::BuildActions {
        dirs: dirs.clone(),
        os,
    }
    .game_folder()
    .ok()
    .flatten()?;
    let version = gamefolder::check(&folder).ok()?.version.name();
    Some((
        version.to_owned(),
        format!("app-{}", env!("CARGO_PKG_VERSION")),
    ))
}

/// Free bytes where the install and the island live; 0 when it cannot be read.
pub fn free_bytes(dirs: &AppDirs) -> u64 {
    let mut path: &Path = &dirs.data;
    while !path.exists() {
        match path.parent() {
            Some(parent) => path = parent,
            None => return 0,
        }
    }
    fs4::available_space(path).unwrap_or(0)
}

/// This device as the fleet knows it, `last_sync_unix` as given (0 for never).
pub fn this_device(dirs: &AppDirs, os: Os, last_sync_unix: u64) -> Option<DeviceRecord> {
    let (version, build_id) = installed_build(dirs, os)?;
    Some(DeviceRecord {
        device: crate::first_run::device(dirs).ok()?,
        os,
        version,
        build_id,
        last_sync_unix,
        free_bytes: free_bytes(dirs),
    })
}

/// The device that made the island `card` shows, for the drift check. The hub's fleet record
/// of that device when it names it; otherwise what the card itself says: the game version it
/// was saved with, on a build taken to be this device's (the card holds no build). `None` when
/// the card names neither a device nor a version.
pub fn saved_by(
    card: &SaveCard,
    fleet: &[DeviceRecord],
    here: &DeviceRecord,
) -> Option<DeviceRecord> {
    if !card.device.is_empty() {
        if let Some(record) = fleet.iter().find(|record| record.device == card.device) {
            return Some(record.clone());
        }
    }
    if card.version.is_empty() {
        return None;
    }
    Some(DeviceRecord {
        device: if card.device.is_empty() {
            "Another device".to_owned()
        } else {
            card.device.clone()
        },
        os: here.os,
        version: card.version.clone(),
        build_id: here.build_id.clone(),
        last_sync_unix: card.saved_unix,
        free_bytes: 0,
    })
}

/// The paired hub from `launcher.ini`: its name and the address it was paired at.
fn paired_hub(dirs: &AppDirs) -> Option<(String, SocketAddr)> {
    let text = std::fs::read_to_string(dirs.config.join("launcher.ini")).ok()?;
    let config = Config::parse(&text).ok()?;
    let name = config.get("hub_name").filter(|name| !name.is_empty())?;
    let addr = config.get("hub_address")?.parse().ok()?;
    Some((name.to_owned(), addr))
}

/// Tells the paired hub this device's build, last sync (`now`) and free space. Nothing to do
/// (`Ok`) when no hub is paired or this device's build is not known. The hub is tried at the
/// address it was paired at, then where the network says it is now.
pub fn report_with(dirs: &AppDirs, os: Os, now: u64, send: &Sender<'_>) -> Result<()> {
    let Some((name, addr)) = paired_hub(dirs) else {
        return Ok(());
    };
    let Some(record) = this_device(dirs, os, now) else {
        return Ok(());
    };
    let first = send(addr, dirs, &name, &record);
    if first.is_ok() {
        return first;
    }
    let found = hub::discover(1500)
        .unwrap_or_default()
        .into_iter()
        .find(|found| found.hub == name && found.targets.contains(&os))
        .map(|found| SocketAddr::new(found.addr, found.port));
    match found {
        Some(found) if found != addr => send(found, dirs, &name, &record),
        _ => first,
    }
}

/// `report_with` to the real hub.
pub fn report_after_sync(dirs: &AppDirs, os: Os, now: u64) -> Result<()> {
    report_with(dirs, os, now, &|addr, dirs, name, record| {
        hub::report_hub(addr, dirs, name, record)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use acnh_core::platform::Vars;
    use std::cell::RefCell;

    fn dirs(root: &Path) -> AppDirs {
        AppDirs::new(&Vars {
            home: root.to_path_buf(),
            data: root.join("data"),
            config: root.join("config"),
        })
    }

    fn install_bundle(dirs: &AppDirs, os: Os, version: &str, build: &str) {
        std::fs::create_dir_all(dirs.install()).unwrap();
        std::fs::write(dirs.install().join("player"), b"p").unwrap();
        let manifest = bundle::manifest(&dirs.install(), os, version, "c").unwrap();
        let manifest = bundle::Bundle {
            build_id: build.into(),
            ..manifest
        };
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::write(
            dirs.config.join("installed-bundle.txt"),
            bundle::to_text(&manifest),
        )
        .unwrap();
    }

    fn card(device: &str, version: &str) -> SaveCard {
        SaveCard {
            saved_unix: 500,
            device: device.into(),
            play_seconds: 0,
            version: version.into(),
            picture: Default::default(),
        }
    }

    #[test]
    fn this_device_is_the_installed_build_with_free_space() {
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs(root.path());
        assert!(this_device(&dirs, Os::Linux, 0).is_none());
        install_bundle(&dirs, Os::Linux, "3.0.2", "build-a");
        let here = this_device(&dirs, Os::Linux, 77).unwrap();
        assert_eq!(
            (here.version.as_str(), here.build_id.as_str()),
            ("3.0.2", "build-a")
        );
        assert_eq!(here.last_sync_unix, 77);
        assert!(here.free_bytes > 0);
        assert_eq!(here.device, crate::first_run::device(&dirs).unwrap());
        // A bundle for another target is not this device's build.
        assert!(this_device(&dirs, Os::Windows, 0).is_none());
    }

    #[test]
    fn saved_by_prefers_the_fleet_record_and_falls_back_to_the_card() {
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs(root.path());
        install_bundle(&dirs, Os::Linux, "3.0.2", "build-a");
        let here = this_device(&dirs, Os::Linux, 0).unwrap();
        let theirs = DeviceRecord {
            device: "handheld".into(),
            os: Os::Android,
            version: "3.0.3".into(),
            build_id: "build-b".into(),
            last_sync_unix: 9,
            free_bytes: 1,
        };
        let by = saved_by(
            &card("handheld", "3.0.2"),
            std::slice::from_ref(&theirs),
            &here,
        )
        .unwrap();
        assert_eq!(by, theirs);
        let by = saved_by(&card("handheld", "3.0.3"), &[], &here).unwrap();
        assert_eq!(
            (by.device.as_str(), by.version.as_str()),
            ("handheld", "3.0.3")
        );
        assert_eq!(by.build_id, here.build_id);
        assert!(saved_by(&card("", ""), &[theirs], &here).is_none());
    }

    #[test]
    fn a_save_from_another_version_warns_in_the_continue_page() {
        use crate::vm::ContinuePageVm;
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs(root.path());
        install_bundle(&dirs, Os::Linux, "3.0.2", "build-a");
        let here = this_device(&dirs, Os::Linux, 0).unwrap();
        let island = card("handheld", "3.0.3");
        let mut vm = ContinuePageVm::new(Some(island.clone()), Some(here.clone()));
        vm.set_saved_by(saved_by(&island, &[], &here).unwrap());
        assert!(vm
            .view()
            .warning
            .contains("handheld made this save with version 3.0.3"));
        // The same version raises nothing.
        let same = card("handheld", "3.0.2");
        let mut vm = ContinuePageVm::new(Some(same.clone()), Some(here.clone()));
        vm.update_saved_by(saved_by(&same, &[], &here).unwrap());
        assert_eq!(vm.view().warning, "");
    }

    #[test]
    fn a_sync_reports_the_real_last_sync_and_free_space_to_the_paired_hub() {
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs(root.path());
        let sent = RefCell::new(Vec::new());
        let record = |_: SocketAddr, _: &AppDirs, name: &str, record: &DeviceRecord| {
            sent.borrow_mut().push((name.to_owned(), record.clone()));
            Ok(())
        };
        // No hub paired, no build known: nothing is sent.
        report_with(&dirs, Os::Linux, 1234, &record).unwrap();
        assert!(sent.borrow().is_empty());

        install_bundle(&dirs, Os::Linux, "3.0.2", "build-a");
        std::fs::write(
            dirs.config.join("launcher.ini"),
            "hub_name=home-hub\nhub_address=192.168.1.5:7000\n",
        )
        .unwrap();
        report_with(&dirs, Os::Linux, 1234, &record).unwrap();
        let sent = sent.borrow();
        assert_eq!(sent.len(), 1);
        let (name, record) = &sent[0];
        assert_eq!(name, "home-hub");
        assert_eq!(record.last_sync_unix, 1234);
        assert!(record.free_bytes > 0);
        assert_eq!(record.version, "3.0.2");
        assert_eq!(record.build_id, "build-a");
    }
}
