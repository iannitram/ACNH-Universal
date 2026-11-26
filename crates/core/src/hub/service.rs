//! TCP hub request routing and authenticated fleet reports.
use super::*;
use crate::fleet::{self, DeviceRecord};
use std::net::SocketAddr;

const ROUTE: &[u8; 8] = b"ACNHREQ1";
const FLEET_WIRE: &[u8; 8] = b"ACNHFLT1";
const BUILD_REPORT: &str = "build\n";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RECORDS: usize = 1024 * 1024;
static FLEET_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
enum Operation {
    Pair = 1,
    Bundle = 2,
    Prepared = 3,
    Report = 4,
}

fn name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

fn write_route(t: &mut dyn Transport, op: Operation, name: &str, target: Os) -> Result<()> {
    if !name_ok(name) {
        return Err(hub_error("invalid device name"));
    }
    t.write_all(ROUTE)?;
    let target_byte = match target {
        Os::Linux => 0,
        Os::Windows => 1,
        Os::Mac => 2,
        Os::Ios => 3,
        Os::Android => 4,
    };
    t.write_all(&[op as u8, target_byte, name.len() as u8])?;
    t.write_all(name.as_bytes())?;
    t.flush()?;
    Ok(())
}

fn read_route(t: &mut dyn Transport) -> Result<(Operation, String, Os)> {
    let mut prefix = [0; 11];
    t.read_exact(&mut prefix)?;
    if &prefix[..8] != ROUTE {
        return Err(hub_error("unsupported hub request"));
    }
    let op = match prefix[8] {
        1 => Operation::Pair,
        2 => Operation::Bundle,
        3 => Operation::Prepared,
        4 => Operation::Report,
        _ => return Err(hub_error("unsupported hub request")),
    };
    let target = match prefix[9] {
        0 => Os::Linux,
        1 => Os::Windows,
        2 => Os::Mac,
        3 => Os::Ios,
        4 => Os::Android,
        _ => return Err(hub_error("invalid target")),
    };
    let len = prefix[10] as usize;
    if len == 0 || len > 128 {
        return Err(hub_error("invalid device name"));
    }
    let mut bytes = vec![0; len];
    t.read_exact(&mut bytes)?;
    let name = String::from_utf8(bytes).map_err(|_| hub_error("invalid device name"))?;
    if !name_ok(&name) {
        return Err(hub_error("invalid device name"));
    }
    Ok((op, name, target))
}

fn pair_context(device: &str, target: Os) -> Vec<u8> {
    let mut context = b"acnh-universal/hub/service-pair/v1/".to_vec();
    context.extend_from_slice(target.key().as_bytes());
    context.push(0);
    context.extend_from_slice(device.as_bytes());
    context
}

fn remembered(dirs: &AppDirs, name: &str) -> Result<Peer> {
    read_peers(dirs)?
        .into_iter()
        .find(|p| p.device == name)
        .ok_or_else(|| hub_error("this device is not paired"))
}

fn record_line(record: &DeviceRecord, identity: &str) -> Result<String> {
    if record.device != identity
        || !name_ok(identity)
        || record.version.is_empty()
        || record.version.len() > 64
        || record.build_id.is_empty()
        || record.build_id.len() > 128
        || record.version.chars().any(char::is_control)
        || record.build_id.chars().any(char::is_control)
    {
        return Err(hub_error("invalid device record"));
    }
    fleet::to_line(record).map_err(|_| hub_error("invalid device record"))
}

/// Read the hub's fleet; malformed or excessive records are rejected without echoing them.
pub fn read_fleet(dirs: &AppDirs) -> Result<Vec<DeviceRecord>> {
    let path = dirs.config.join("fleet.txt");
    match fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() && m.len() <= MAX_RECORDS as u64 => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        _ => return Err(hub_error("invalid fleet file")),
    }
    let mut text = String::new();
    File::open(&path)
        .and_then(|file| {
            file.take((MAX_RECORDS + 1) as u64)
                .read_to_string(&mut text)
        })
        .map_err(|_| hub_error("invalid fleet file"))?;
    if text.len() > MAX_RECORDS {
        return Err(hub_error("invalid fleet file"));
    }
    let mut result = Vec::new();
    let mut names = BTreeSet::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record = fleet::from_line(line).map_err(|_| hub_error("invalid fleet file"))?;
        if record_line(&record, &record.device)? != line || !names.insert(record.device.clone()) {
            return Err(hub_error("invalid fleet file"));
        }
        result.push(record);
    }
    Ok(result)
}

fn save_record(dirs: &AppDirs, mut record: DeviceRecord, build_only: bool) -> Result<()> {
    let _guard = FLEET_LOCK.lock().map_err(|_| hub_error("fleet is busy"))?;
    let mut records = read_fleet(dirs)?;
    if let Some(previous) = records
        .iter()
        .find(|item| build_only && item.device == record.device)
    {
        record.last_sync_unix = previous.last_sync_unix;
        record.free_bytes = previous.free_bytes;
    }
    records.retain(|old| old.device != record.device);
    records.push(record);
    records.sort_by(|a, b| a.device.cmp(&b.device));
    let mut text = String::new();
    for record in records {
        text.push_str(&record_line(&record, &record.device)?);
        text.push('\n');
    }
    if text.len() > MAX_RECORDS {
        return Err(hub_error("fleet is full"));
    }
    fs::create_dir_all(&dirs.config)?;
    let mut file = tempfile::NamedTempFile::new_in(&dirs.config)?;
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(dirs.config.join("fleet.txt"))
        .map_err(|_| hub_error("could not save fleet"))?;
    Ok(())
}

/// Handle one already accepted home-network connection. `pairing` is present only while
/// the owner has explicitly armed pairing; its expiry and attempt limit persist across calls.
/// It is shared between connection threads and locked only while a pairing exchange runs.
pub fn serve_request(
    t: &mut TcpTransport,
    dirs: &AppDirs,
    builds: &Path,
    prepared: Option<&Path>,
    pairing: Option<&Mutex<PairingSession>>,
) -> Result<()> {
    (|| {
        t.0.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        t.0.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let (op, device, target) = read_route(t)?;
        if matches!(op, Operation::Bundle | Operation::Prepared) {
            TcpTransport::configure(&t.0)?;
        }
        match op {
            Operation::Pair => {
                let session = pairing.ok_or_else(|| hub_error("pairing is not active"))?;
                let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
                pair_with_context(
                    t,
                    &mut session,
                    &device,
                    dirs,
                    &pair_context(&device, target),
                )?;
            }
            Operation::Bundle => {
                let peer = remembered(dirs, &device)?;
                serve(t, &peer, builds, target)?;
            }
            Operation::Prepared => {
                let root = prepared.ok_or_else(|| hub_error("prepared data is not available"))?;
                serve_prepared(t, dirs, &device, target, root)?;
            }
            Operation::Report => {
                let peer = remembered(dirs, &device)?;
                let key = pairing_key(&peer)?;
                let mut channel = handshake_for(t, &key, target, true, FLEET_WIRE)?;
                let bytes = channel.receive(t, b"fleet-record/v1", 1024)?;
                let text =
                    std::str::from_utf8(&bytes).map_err(|_| hub_error("invalid device record"))?;
                let (build_only, text) = match text.strip_prefix(BUILD_REPORT) {
                    Some(line) => (true, line),
                    None => (false, text),
                };
                let record =
                    fleet::from_line(text).map_err(|_| hub_error("invalid device record"))?;
                if record.os != target || record_line(&record, &device)? != text {
                    return Err(hub_error("invalid device record"));
                }
                save_record(dirs, record, build_only)?;
                channel.send(t, b"fleet-record/v1", vec![1])?;
            }
        }
        Ok(())
    })()
    .map_err(transfer_error)
}

fn connect(addr: SocketAddr, op: Operation, name: &str, target: Os) -> Result<TcpTransport> {
    let mut t = TcpTransport::connect(addr)?;
    write_route(&mut t, op, name, target)?;
    Ok(t)
}

/// Pair with an explicitly armed hub; the code is never placed in the routing preface.
pub fn pair_hub(
    addr: SocketAddr,
    dirs: &AppDirs,
    this_device: &str,
    hub_name: &str,
    code: &str,
    target: Os,
) -> Result<Peer> {
    let mut session = PairingSession::new(code)?;
    let mut t = connect(addr, Operation::Pair, this_device, target)?;
    pair_with_context(
        &mut t,
        &mut session,
        hub_name,
        dirs,
        &pair_context(this_device, target),
    )
    .map_err(transfer_error)
}

/// Pull the ordinary, game-data-free bundle to a staging directory.
pub fn pull_hub(
    addr: SocketAddr,
    dirs: &AppDirs,
    this_device: &str,
    hub_name: &str,
    target: Os,
    staged: &Path,
) -> Result<Bundle> {
    let peer = remembered(dirs, hub_name)?;
    let mut t = connect(addr, Operation::Bundle, this_device, target)?;
    pull(&mut t, &peer, target, staged)
}

/// Pull prepared data into Android app-private storage.
pub fn pull_hub_prepared(
    addr: SocketAddr,
    dirs: &AppDirs,
    this_device: &str,
    hub_name: &str,
    target: Os,
    data_root: &Path,
) -> Result<()> {
    if target != Os::Android {
        return Err(hub_error("prepared data is only available for Android"));
    }
    let mut t = connect(addr, Operation::Prepared, this_device, target)?;
    pull_prepared(&mut t, dirs, hub_name, target, data_root)
}

/// Pull prepared Android game library and data from a paired hub and install it into
/// `<private_dir>/game/` through [`crate::appgame::install`].
pub fn pull_hub_prepared_app(
    addr: SocketAddr,
    dirs: &AppDirs,
    this_device: &str,
    hub_name: &str,
    target: Os,
    private_dir: &Path,
    free_bytes: u64,
) -> Result<PathBuf> {
    if target != Os::Android {
        return Err(hub_error("prepared data is only available for Android"));
    }
    let mut t = connect(addr, Operation::Prepared, this_device, target)?;
    pull_prepared_app(&mut t, dirs, hub_name, target, private_dir, free_bytes)
}

/// Like [`pull_hub_prepared_app`], with an explicit `ci` flag for deterministic host tests.
#[allow(clippy::too_many_arguments)]
pub fn pull_hub_prepared_app_with(
    addr: SocketAddr,
    dirs: &AppDirs,
    this_device: &str,
    hub_name: &str,
    target: Os,
    private_dir: &Path,
    free_bytes: u64,
    ci: bool,
) -> Result<PathBuf> {
    if target != Os::Android {
        return Err(hub_error("prepared data is only available for Android"));
    }
    let mut t = connect(addr, Operation::Prepared, this_device, target)?;
    pull_prepared_app_with(&mut t, dirs, hub_name, target, private_dir, free_bytes, ci)
}

/// Report this authenticated device after a sync.
pub fn report_hub(
    addr: SocketAddr,
    dirs: &AppDirs,
    hub_name: &str,
    record: &DeviceRecord,
) -> Result<()> {
    send_report(addr, dirs, hub_name, record, false)
}

/// Report a newly installed build while retaining earlier save-sync and disk-space fields.
pub fn report_hub_build(
    addr: SocketAddr,
    dirs: &AppDirs,
    this_device: &str,
    hub_name: &str,
    bundle: &Bundle,
) -> Result<()> {
    let record = DeviceRecord {
        device: this_device.into(),
        os: bundle.target,
        version: bundle.version.clone(),
        build_id: bundle.build_id.clone(),
        last_sync_unix: 0,
        free_bytes: 0,
    };
    send_report(addr, dirs, hub_name, &record, true)
}

fn send_report(
    addr: SocketAddr,
    dirs: &AppDirs,
    hub_name: &str,
    record: &DeviceRecord,
    build_only: bool,
) -> Result<()> {
    let line = record_line(record, &record.device)?;
    let payload = if build_only {
        format!("{BUILD_REPORT}{line}")
    } else {
        line
    };
    let peer = remembered(dirs, hub_name)?;
    let key = pairing_key(&peer)?;
    let mut t = connect(addr, Operation::Report, &record.device, record.os)?;
    let mut channel = handshake_for(&mut t, &key, record.os, false, FLEET_WIRE)?;
    channel.send(&mut t, b"fleet-record/v1", payload.into_bytes())?;
    if channel.receive(&mut t, b"fleet-record/v1", 1)? != [1] {
        return Err(hub_error("fleet report not confirmed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_context_rejects_changed_label_or_target_without_remembering() {
        for (name, target) in [("other", Os::Android), ("client", Os::Linux)] {
            let root = tempfile::tempdir().unwrap();
            let dirs = |part: &str| AppDirs {
                config: root.path().join(part),
                data: root.path().join(format!("{part}-data")),
            };
            let a = dirs("a");
            let b = dirs("b");
            let (mut left, mut right) = MemoryTransport::pair();
            std::thread::scope(|s| {
                let worker = s.spawn(move || {
                    pair_with_context(
                        &mut left,
                        &mut PairingSession::new("123456").unwrap(),
                        "client",
                        &a,
                        &pair_context(name, target),
                    )
                });
                let result = pair_with_context(
                    &mut right,
                    &mut PairingSession::new("123456").unwrap(),
                    "hub",
                    &b,
                    &pair_context("client", Os::Android),
                );
                assert!(result.is_err());
                assert!(worker.join().unwrap().is_err());
                assert!(read_peers(&b).unwrap().is_empty());
            });
            assert!(!root.path().join("a/peers.json").exists());
        }
    }

    #[test]
    fn fleet_records_cannot_spoof_identity_or_inject_fields() {
        let mut record = DeviceRecord {
            device: "client".into(),
            os: Os::Android,
            version: "3.0.2".into(),
            build_id: "test".into(),
            last_sync_unix: 0,
            free_bytes: 0,
        };
        assert!(record_line(&record, "client").is_ok());
        assert!(record_line(&record, "other").is_err());
        record.version = "3.0.2\nother".into();
        assert!(record_line(&record, "client").is_err());
    }
}
