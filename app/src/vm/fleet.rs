//! Presentation of authenticated hub fleet records.
use acnh_core::fleet::{self, DeviceRecord, Drift};
use acnh_core::platform::AppDirs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRow {
    pub device: String,
    pub details: String,
    pub status: String,
    pub drifting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetVm {
    pub rows: Vec<FleetRow>,
    pub message: String,
    pub comparison: String,
}

impl FleetVm {
    pub fn load(dirs: &AppDirs, current: Option<&DeviceRecord>, now: u64) -> Self {
        match acnh_core::hub::read_fleet(dirs) {
            Ok(records) => Self::from_records(&records, current, now),
            Err(_) => Self {
                rows: vec![],
                message: "Could not read fleet records.".into(),
                comparison: String::new(),
            },
        }
    }

    pub fn from_records(
        records: &[DeviceRecord],
        current: Option<&DeviceRecord>,
        now: u64,
    ) -> Self {
        let synced = |record: &DeviceRecord| (1..=now).contains(&record.last_sync_unix);
        let reference = current.or_else(|| {
            records
                .iter()
                .filter(|record| synced(record))
                .max_by_key(|r| (r.last_sync_unix, &r.device))
                .or_else(|| {
                    records
                        .iter()
                        .filter(|r| !r.version.is_empty() || !r.build_id.is_empty())
                        .min_by(|a, b| a.device.cmp(&b.device))
                })
        });
        let comparison = match (current, reference) {
            (Some(r), _) => format!("Comparing with this device: {}", r.device),
            (None, Some(r)) if synced(r) => {
                format!("Comparing with latest synced device: {}", r.device)
            }
            (None, Some(r)) => format!("Comparing with device (sync unknown): {}", r.device),
            _ => "No comparison device available".into(),
        };
        let rows = records
            .iter()
            .map(|record| {
                let value = |s: &str| {
                    if s.is_empty() {
                        "Unknown".to_owned()
                    } else {
                        s.to_owned()
                    }
                };
                let sync = if record.last_sync_unix == 0 {
                    "Never or unknown".into()
                } else if record.last_sync_unix > now {
                    "In the future (check clock)".into()
                } else {
                    let seconds = now - record.last_sync_unix;
                    if seconds < 60 {
                        "Just now".into()
                    } else if seconds < 3600 {
                        format!("{} min ago", seconds / 60)
                    } else if seconds < 86400 {
                        format!("{} h ago", seconds / 3600)
                    } else {
                        format!("{} days ago", seconds / 86400)
                    }
                };
                let free = if record.free_bytes == 0 {
                    "Unknown or 0 bytes".into()
                } else {
                    format!("{} bytes", record.free_bytes)
                };
                let (status, drifting) = reference
                    .map(|reference| compare_record(record, reference))
                    .unwrap_or(("Comparison unavailable", false));
                FleetRow {
                    device: format!(
                        "{}{}",
                        record.device,
                        if current.is_some_and(|r| r.device == record.device) {
                            " (this device)"
                        } else {
                            ""
                        }
                    ),
                    details: format!(
                        "{} · Version {} · Build {}\nLast sync: {} · Free space: {}",
                        record.os.key(),
                        value(&record.version),
                        value(&record.build_id),
                        sync,
                        free
                    ),
                    status: status.into(),
                    drifting,
                }
            })
            .collect();
        Self {
            rows,
            message: if records.is_empty() {
                "Only this device so far".into()
            } else {
                String::new()
            },
            comparison,
        }
    }
}

fn compare_record(record: &DeviceRecord, reference: &DeviceRecord) -> (&'static str, bool) {
    let versions_known = !record.version.is_empty() && !reference.version.is_empty();
    let builds_known = !record.build_id.is_empty() && !reference.build_id.is_empty();
    match fleet::drift(record, reference) {
        Drift::Version { .. } if versions_known => ("Version differs", true),
        _ if builds_known && record.build_id != reference.build_id => ("Build differs", true),
        Drift::None if versions_known && builds_known => ("Matches", false),
        _ => ("Comparison unavailable", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acnh_core::platform::Os;
    fn record(name: &str, version: &str, build: &str, sync: u64, free: u64) -> DeviceRecord {
        DeviceRecord {
            device: name.into(),
            os: Os::Linux,
            version: version.into(),
            build_id: build.into(),
            last_sync_unix: sync,
            free_bytes: free,
        }
    }
    #[test]
    fn records_show_fields_and_reference_drift() {
        let a = record("One", "3.0.2", "abc", 100, 1234);
        let b = record("Two", "3.0.3", "def", 200, 0);
        let vm = FleetVm::from_records(&[a.clone(), b.clone()], Some(&a), 260);
        assert!(vm.comparison.contains("this device: One"));
        assert!(vm.rows[0].details.contains("3.0.2") && vm.rows[0].details.contains("abc"));
        assert!(
            vm.rows[0].details.contains("1234 bytes") && vm.rows[0].details.contains("2 min ago")
        );
        assert_eq!(vm.rows[1].status, "Version differs");
        assert!(vm.rows[1].drifting);
        assert!(!vm.rows[0].drifting);
        assert!(vm.rows[1]
            .details
            .contains("Free space: Unknown or 0 bytes"));
        let vm = FleetVm::from_records(&[a, b], None, 260);
        assert!(vm.comparison.contains("latest synced device: Two"));
        assert_eq!(vm.rows[0].status, "Version differs");
    }
    #[test]
    fn unknown_values_are_not_claimed_as_matching_or_drifting() {
        let a = record("One", "", "", 0, 0);
        let b = record("Two", "3.0.2", "build", 0, 0);
        let vm = FleetVm::from_records(&[a.clone(), b], Some(&a), 0);
        assert_eq!(vm.rows[1].status, "Comparison unavailable");
        assert!(vm.rows[0].details.contains("Never or unknown"));
        assert_eq!(
            FleetVm::from_records(&[], None, 0).message,
            "Only this device so far"
        );
    }
    #[test]
    fn loads_records_and_reports_invalid_files_without_echoing_them() {
        let root = crate::vm::scratch("fleet-file");
        std::fs::create_dir_all(&root).unwrap();
        let dirs = AppDirs {
            config: root.clone(),
            data: root.clone(),
        };
        let one = record("One", "3.0.2", "build", 100, 500);
        std::fs::write(
            root.join("fleet.txt"),
            format!("{}\n", fleet::to_line(&one).unwrap()),
        )
        .unwrap();
        assert_eq!(FleetVm::load(&dirs, None, 200).rows.len(), 1);
        std::fs::write(root.join("fleet.txt"), "bad record with private text\n").unwrap();
        let result = FleetVm::load(&dirs, None, 200);
        assert_eq!(result.message, "Could not read fleet records.");
        assert!(result.rows.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn same_version_with_another_build_is_marked() {
        let here = record("Here", "3.0.2", "one", 10, 20);
        let other = record("Other", "3.0.2", "two", 20, 30);
        let vm = FleetVm::from_records(&[here.clone(), other], Some(&here), 30);
        assert_eq!(vm.rows[0].status, "Matches");
        assert_eq!(vm.rows[1].status, "Build differs");
    }
    #[test]
    fn build_only_records_get_a_deterministic_comparison() {
        let z = record("Zulu", "3.0.3", "two", 0, 0);
        let a = record("Alpha", "3.0.2", "one", 0, 0);
        let vm = FleetVm::from_records(&[z, a], None, 0);
        assert_eq!(vm.comparison, "Comparing with device (sync unknown): Alpha");
        assert_eq!(vm.rows[0].status, "Version differs");
    }
}
