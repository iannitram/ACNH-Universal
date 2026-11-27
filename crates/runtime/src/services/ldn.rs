//! Split from the old `services.rs` (issue #356).

use super::*;
#[allow(unused_imports)]
use crate::sync::LockExt;

// ---- 9. LDN (hle_ldn.cpp) -------------------------------------------------------------------------------------------

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LdnState {
    None = 0,
    Initialized = 1,
    AccessPoint = 2,
    AccessPointCreated = 3,
    Station = 4,
    StationConnected = 5,
    Error = 6,
}

impl LdnState {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Initialized => "Initialized",
            Self::AccessPoint => "AccessPoint",
            Self::AccessPointCreated => "AccessPointCreated",
            Self::Station => "Station",
            Self::StationConnected => "StationConnected",
            Self::Error => "Error",
        }
    }
}

pub(crate) const NI_SESSION_ID: usize = 0x10;
pub(crate) const NI_BSSID: usize = 0x20;
pub(crate) const NI_SSID: usize = 0x26;
pub(crate) const NI_CHANNEL: usize = 0x48;
pub(crate) const NI_LINK_LEVEL: usize = 0x4A;
pub(crate) const NI_NETWORK_TYPE: usize = 0x4B;
pub(crate) const NI_SEC_PARAM: usize = 0x50;
pub(crate) const NI_SEC_MODE: usize = 0x60;
pub(crate) const NI_ACCEPT_POLICY: usize = 0x62;
pub(crate) const NI_NODE_MAX: usize = 0x66;
pub(crate) const NI_NODE_COUNT: usize = 0x67;
pub(crate) const NI_NODES: usize = 0x68;
pub(crate) const NI_ADV_SIZE: usize = 0x26A;
pub(crate) const NI_ADV: usize = 0x26C;
pub(crate) const NI_AUTH_ID: usize = 0x478;

pub(crate) const NODE_IP: usize = 0;
pub(crate) const NODE_MAC: usize = 4;
pub(crate) const NODE_ID: usize = 0xA;
pub(crate) const NODE_CONNECTED: usize = 0xB;
pub(crate) const NODE_NAME: usize = 0xC;
pub(crate) const NODE_VERSION: usize = 0x2E;

pub(crate) struct LdnInner {
    pub(crate) state: LdnState,
    pub(crate) state_event: u32,
    pub(crate) advertise: [u8; LDN_ADVERTISE_MAX],
    pub(crate) advertise_size: usize,
    pub(crate) info: [u8; LDN_INFO_SIZE],
    pub(crate) mac: [u8; 6],
    pub(crate) accept_policy: u8,
    pub(crate) disconnect_reason: u32,
    pub(crate) rng_state: u64,
}

impl LdnInner {
    const fn new() -> Self {
        Self {
            state: LdnState::None,
            state_event: 0,
            advertise: [0; LDN_ADVERTISE_MAX],
            advertise_size: 0,
            info: [0; LDN_INFO_SIZE],
            mac: [0; 6],
            accept_policy: 0,
            disconnect_reason: 0,
            rng_state: 0x9E37_79B9_7F4A_7C15,
        }
    }

    pub(crate) fn random_bytes(&mut self, out: &mut [u8]) {
        if self.rng_state == 0x9E37_79B9_7F4A_7C15 {
            self.rng_state ^= host_monotonic_ns().wrapping_add(1);
        }
        for b in out.iter_mut() {
            let mut x = self.rng_state;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.rng_state = x;
            *b = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }

    pub(crate) fn ensure_mac(&mut self) {
        if self.mac[0] == 0 && self.mac[1] == 0 {
            let mut m = [0u8; 6];
            self.random_bytes(&mut m);
            m[0] = (m[0] | 2) & 0xFE;
            self.mac = m;
        }
    }

    pub(crate) fn signal_change(&self) {
        if self.state_event != 0 {
            host_event_signal(self.state_event, true);
            host_notify_all();
        }
    }

    pub(crate) fn set_state(&mut self, next: LdnState) {
        if self.state == next {
            return;
        }
        host_log(&format!(
            "ldn: state {} -> {}",
            self.state.name(),
            next.name()
        ));
        self.state = next;
        self.signal_change();
    }
}

pub(crate) static LDN_SESSION: AtomicBool = AtomicBool::new(false);
pub(crate) static LDN_OWN_IP: AtomicU32 = AtomicU32::new(0);
pub(crate) static LDN_LOCK: Mutex<LdnInner> = Mutex::new(LdnInner::new());

pub(crate) fn ldn_wanted() -> bool {
    with_host(|h| {
        h.nextendo_ldn_wanted
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f() })
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

pub fn ldn_info_is_valid(info: &mut [u8]) -> bool {
    if info.len() < LDN_INFO_SIZE {
        return false;
    }
    let adv = u16::from_le_bytes([info[NI_ADV_SIZE], info[NI_ADV_SIZE + 1]]) as usize;
    let max = info[NI_NODE_MAX];
    let count = info[NI_NODE_COUNT];
    if info[NI_SSID] > 32
        || !(1..=8).contains(&max)
        || count < 1
        || count > max
        || adv > LDN_ADVERTISE_MAX
    {
        return false;
    }
    info[NI_SSID + 0x21] = 0;
    for i in 0..8usize {
        let node = &mut info[NI_NODES + i * 0x40..NI_NODES + (i + 1) * 0x40];
        if node[NODE_CONNECTED] == 0 {
            node.fill(0);
            continue;
        }
        node[NODE_CONNECTED] = 1;
        node[NODE_ID] = i as u8;
        node[NODE_NAME + 0x20] = 0;
    }
    true
}

pub(crate) fn ldn_visited_island_string() -> String {
    let g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::StationConnected {
        return String::new();
    }
    let mut buf = [0 as c_char; 128];
    let n = with_host(|h| {
        h.ldn_island_name
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f(g.info.as_ptr(), buf.as_mut_ptr(), buf.len()) })
            .unwrap_or(0)
    })
    .unwrap_or(0);
    if n == 0 {
        String::new()
    } else {
        // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_visited_island(out: *mut c_char, cap: usize) -> usize {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let s = ldn_visited_island_string();
    let bytes = s.as_bytes();
    let n = bytes.len().min(cap - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), n);
    *out.add(n) = 0;
    n
}

#[no_mangle]
pub extern "C" fn acnh_ldn_enabled() -> bool {
    LDN_SESSION.load(Ordering::Acquire)
}

#[no_mangle]
pub extern "C" fn acnh_ldn_net_up() -> bool {
    LDN_OWN_IP.load(Ordering::Acquire) != 0
}

#[no_mangle]
pub extern "C" fn acnh_ldn_own_ip() -> u32 {
    LDN_OWN_IP.load(Ordering::Acquire)
}

#[no_mangle]
pub extern "C" fn acnh_ldn_broadcast_ip() -> u32 {
    LDN_OWN_IP.load(Ordering::Acquire) | 0xFF
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_info_valid(info_ptr: *mut u8) -> bool {
    if info_ptr.is_null() {
        return false;
    }
    ldn_info_is_valid(std::slice::from_raw_parts_mut(info_ptr, LDN_INFO_SIZE))
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_link_info(out_ptr: *mut u8) -> bool {
    if out_ptr.is_null() {
        return false;
    }
    let g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::AccessPointCreated {
        return false;
    }
    std::ptr::copy_nonoverlapping(g.info.as_ptr(), out_ptr, LDN_INFO_SIZE);
    true
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_link_add_node(station_ptr: *const u8, own_ip: u32) -> i32 {
    if station_ptr.is_null() {
        return -1;
    }
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::AccessPointCreated {
        return -1;
    }
    if own_ip != 0 && g.info[NI_NODE_COUNT] == 1 {
        g.info[NI_NODES + NODE_IP..NI_NODES + NODE_IP + 4].copy_from_slice(&own_ip.to_le_bytes());
        LDN_OWN_IP.store(own_ip, Ordering::Release);
    }
    let max = (g.info[NI_NODE_MAX] as usize).min(8);
    let station = std::slice::from_raw_parts(station_ptr, 0x40);
    for i in 1..max {
        let base = NI_NODES + i * 0x40;
        if g.info[base + NODE_CONNECTED] != 0 {
            continue;
        }
        g.info[base..base + 0x40].copy_from_slice(station);
        g.info[base + NODE_ID] = i as u8;
        g.info[base + NODE_CONNECTED] = 1;
        g.info[base + NODE_NAME + 0x20] = 0;
        g.info[NI_NODE_COUNT] += 1;
        g.signal_change();
        return i as i32;
    }
    -1
}

#[no_mangle]
pub extern "C" fn acnh_ldn_link_remove_node(n: i32) {
    if !(0..8).contains(&n) {
        return;
    }
    let mut g = LDN_LOCK.lock_or_recover();
    let base = NI_NODES + (n as usize) * 0x40;
    if g.info[base + NODE_CONNECTED] == 0 {
        return;
    }
    g.info[base..base + 0x40].fill(0);
    g.info[NI_NODE_COUNT] = g.info[NI_NODE_COUNT].saturating_sub(1);
    g.signal_change();
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_peer_ips(out_ptr: *mut u32) -> i32 {
    if out_ptr.is_null() {
        return 0;
    }
    let g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::AccessPointCreated && g.state != LdnState::StationConnected {
        return 0;
    }
    let own = LDN_OWN_IP.load(Ordering::Acquire);
    let mut n = 0i32;
    for i in 0..8usize {
        let base = NI_NODES + i * 0x40;
        let ip = u32::from_le_bytes([
            g.info[base + NODE_IP],
            g.info[base + NODE_IP + 1],
            g.info[base + NODE_IP + 2],
            g.info[base + NODE_IP + 3],
        ]);
        if g.info[base + NODE_CONNECTED] != 0 && ip != 0 && ip != own {
            *out_ptr.add(n as usize) = ip;
            n += 1;
        }
    }
    n
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_link_set_info(info_ptr: *const u8) {
    if info_ptr.is_null() {
        return;
    }
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::StationConnected {
        return;
    }
    std::ptr::copy_nonoverlapping(info_ptr, g.info.as_mut_ptr(), LDN_INFO_SIZE);
    g.signal_change();
}

#[no_mangle]
pub extern "C" fn acnh_ldn_link_lost() {
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::StationConnected {
        return;
    }
    LDN_OWN_IP.store(0, Ordering::Release);
    g.disconnect_reason = 6;
    g.set_state(LdnState::Error);
}

#[no_mangle]
pub extern "C" fn acnh_ldn_disabled() -> u32 {
    if ldn_wanted() {
        SVC_OK
    } else {
        LDN_ERR_DEVICE_DISABLED
    }
}

#[no_mangle]
pub extern "C" fn acnh_ldn_initialize() -> u32 {
    if !ldn_wanted() {
        if let Some(Some(f)) = with_host(|h| h.ui_open_online) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
        return LDN_ERR_DEVICE_DISABLED;
    }
    LDN_SESSION.store(true, Ordering::Release);
    if let Some(Some(f)) = with_host(|h| h.wifi_multicast) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(true) };
    }
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state == LdnState::None {
        g.set_state(LdnState::Initialized);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_finalize() -> u32 {
    if LDN_SESSION.load(Ordering::Acquire) {
        if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
        if let Some(Some(f)) = with_host(|h| h.vsock_reset) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
    }
    if let Some(Some(f)) = with_host(|h| h.wifi_multicast) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(false) };
    }
    let mut g = LDN_LOCK.lock_or_recover();
    LDN_SESSION.store(false, Ordering::Release);
    LDN_OWN_IP.store(0, Ordering::Release);
    g.state = LdnState::None;
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_get_state() -> u32 {
    let g = LDN_LOCK.lock_or_recover();
    if LDN_SESSION.load(Ordering::Acquire) {
        g.state as u32
    } else {
        LdnState::None as u32
    }
}

#[no_mangle]
pub extern "C" fn acnh_ldn_set_state_event(ev: u32) {
    let mut g = LDN_LOCK.lock_or_recover();
    g.state_event = ev;
}

#[no_mangle]
pub extern "C" fn acnh_ldn_open_access_point() -> u32 {
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::Initialized {
        return LDN_ERR_INVALID_STATE;
    }
    g.set_state(LdnState::AccessPoint);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_close_access_point() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock_or_recover();
    LDN_OWN_IP.store(0, Ordering::Release);
    g.set_state(LdnState::Initialized);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_open_station() -> u32 {
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::Initialized {
        return LDN_ERR_INVALID_STATE;
    }
    g.set_state(LdnState::Station);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_close_station() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock_or_recover();
    LDN_OWN_IP.store(0, Ordering::Release);
    g.set_state(LdnState::Initialized);
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_create_network(
    cfg_ptr: *const u8,
    sec_ptr: *const u8,
    sec_param_ptr: *const u8,
    user_ptr: *const u8,
) -> u32 {
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::AccessPoint
        || cfg_ptr.is_null()
        || sec_ptr.is_null()
        || user_ptr.is_null()
    {
        return LDN_ERR_INVALID_STATE;
    }
    let cfg = std::slice::from_raw_parts(cfg_ptr, 0x20);
    let sec = std::slice::from_raw_parts(sec_ptr, 0x44);
    let user = std::slice::from_raw_parts(user_ptr, 0x30);
    g.ensure_mac();
    g.info.fill(0);
    g.info[0..0x10].copy_from_slice(&cfg[0..0x10]);
    if !sec_param_ptr.is_null() {
        let sp = std::slice::from_raw_parts(sec_param_ptr, 0x20);
        g.info[NI_SEC_PARAM..NI_SEC_PARAM + 0x10].copy_from_slice(&sp[0..0x10]);
        g.info[NI_SESSION_ID..NI_SESSION_ID + 0x10].copy_from_slice(&sp[0x10..0x20]);
    } else {
        let mut r1 = [0u8; 16];
        let mut r2 = [0u8; 16];
        g.random_bytes(&mut r1);
        g.random_bytes(&mut r2);
        g.info[NI_SEC_PARAM..NI_SEC_PARAM + 0x10].copy_from_slice(&r1);
        g.info[NI_SESSION_ID..NI_SESSION_ID + 0x10].copy_from_slice(&r2);
    }
    let mac = g.mac;
    g.info[NI_BSSID..NI_BSSID + 6].copy_from_slice(&mac);
    let mut r = [0u8; 8];
    g.random_bytes(&mut r);
    let ssid = format!("ACNHRecomp{:02x}{:02x}{:02x}{:02x}", r[0], r[1], r[2], r[3]);
    let sb = ssid.as_bytes();
    g.info[NI_SSID] = sb.len() as u8;
    g.info[NI_SSID + 1..NI_SSID + 1 + sb.len()].copy_from_slice(sb);
    let ch = u16::from_le_bytes([cfg[0x10], cfg[0x11]]);
    let ch_val = if ch != 0 { ch } else { 6 };
    g.info[NI_CHANNEL..NI_CHANNEL + 2].copy_from_slice(&ch_val.to_le_bytes());
    g.info[NI_LINK_LEVEL] = 3;
    g.info[NI_NETWORK_TYPE] = 2;
    g.info[NI_SEC_MODE..NI_SEC_MODE + 2].copy_from_slice(&sec[0..2]);
    g.info[NI_ACCEPT_POLICY] = g.accept_policy;
    let max = cfg[0x12];
    g.info[NI_NODE_MAX] = if (1..=8).contains(&max) { max } else { 8 };
    g.info[NI_NODE_COUNT] = 1;
    let ip = with_host(|h| h.ldn_local_address.map(|f| f()))
        .flatten()
        .unwrap_or(0);
    LDN_OWN_IP.store(ip, Ordering::Release);
    g.info[NI_NODES + NODE_IP..NI_NODES + NODE_IP + 4].copy_from_slice(&ip.to_le_bytes());
    g.info[NI_NODES + NODE_MAC..NI_NODES + NODE_MAC + 6].copy_from_slice(&mac);
    g.info[NI_NODES + NODE_ID] = 0;
    g.info[NI_NODES + NODE_CONNECTED] = 1;
    g.info[NI_NODES + NODE_NAME..NI_NODES + NODE_NAME + 0x20].copy_from_slice(&user[0..0x20]);
    g.info[NI_NODES + NODE_VERSION..NI_NODES + NODE_VERSION + 2].copy_from_slice(&cfg[0x14..0x16]);
    let adv_sz = g.advertise_size;
    g.info[NI_ADV_SIZE..NI_ADV_SIZE + 2].copy_from_slice(&(adv_sz as u16).to_le_bytes());
    let adv = g.advertise;
    g.info[NI_ADV..NI_ADV + adv_sz].copy_from_slice(&adv[..adv_sz]);
    g.info[NI_AUTH_ID..NI_AUTH_ID + 8].copy_from_slice(&r);
    if let Some(Some(f)) = with_host(|h| h.ldn_link_host_start) {
        f();
    }
    g.set_state(LdnState::AccessPointCreated);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_destroy_network() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock_or_recover();
    LDN_OWN_IP.store(0, Ordering::Release);
    if g.state == LdnState::AccessPointCreated {
        g.set_state(LdnState::AccessPoint);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_set_advertise_data(data_ptr: *const u8, size: u64) -> u32 {
    let mut g = LDN_LOCK.lock_or_recover();
    let n = size as usize;
    if n > LDN_ADVERTISE_MAX {
        return LDN_ERR_INVALID_STATE;
    }
    if n > 0 && !data_ptr.is_null() {
        let src = std::slice::from_raw_parts(data_ptr, n);
        g.advertise[..n].copy_from_slice(src);
    }
    g.advertise_size = n;
    if g.state == LdnState::AccessPointCreated {
        g.info[NI_ADV_SIZE..NI_ADV_SIZE + 2].copy_from_slice(&(n as u16).to_le_bytes());
        g.info[NI_ADV..NI_ADV + LDN_ADVERTISE_MAX].fill(0);
        let adv = g.advertise;
        g.info[NI_ADV..NI_ADV + n].copy_from_slice(&adv[..n]);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_set_station_accept_policy(policy: u32) -> u32 {
    let mut g = LDN_LOCK.lock_or_recover();
    g.accept_policy = policy as u8;
    g.info[NI_ACCEPT_POLICY] = g.accept_policy;
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_connect(
    net_ptr: *const u8,
    user_ptr: *const u8,
    version: u16,
) -> u32 {
    let mut g = LDN_LOCK.lock_or_recover();
    if g.state != LdnState::Station || net_ptr.is_null() || user_ptr.is_null() {
        return LDN_ERR_INVALID_STATE;
    }
    g.ensure_mac();
    let mut me = [0u8; 0x40];
    me[NODE_MAC..NODE_MAC + 6].copy_from_slice(&g.mac);
    me[NODE_CONNECTED] = 1;
    let user = std::slice::from_raw_parts(user_ptr, 0x20);
    me[NODE_NAME..NODE_NAME + 0x20].copy_from_slice(user);
    me[NODE_VERSION..NODE_VERSION + 2].copy_from_slice(&version.to_le_bytes());
    let mut info = [0u8; LDN_INFO_SIZE];
    let node = with_host(|h| {
        h.ldn_link_join
            .map(|f| f(net_ptr.add(NI_BSSID), me.as_ptr(), info.as_mut_ptr()))
            .unwrap_or(-1)
    })
    .unwrap_or(-1);
    if node < 0 {
        return LDN_ERR_INVALID_STATE;
    }
    g.info = info;
    let base = NI_NODES + (node as usize) * 0x40 + NODE_IP;
    let ip = u32::from_le_bytes([info[base], info[base + 1], info[base + 2], info[base + 3]]);
    LDN_OWN_IP.store(ip, Ordering::Release);
    g.disconnect_reason = 0;
    g.set_state(LdnState::StationConnected);
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_ldn_disconnect() -> u32 {
    if let Some(Some(f)) = with_host(|h| h.ldn_link_stop) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    let mut g = LDN_LOCK.lock_or_recover();
    LDN_OWN_IP.store(0, Ordering::Release);
    if g.state == LdnState::StationConnected {
        g.set_state(LdnState::Station);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_get_network_info(out_ptr: *mut u8) -> u32 {
    let g = LDN_LOCK.lock_or_recover();
    let up = g.state == LdnState::AccessPointCreated || g.state == LdnState::StationConnected;
    if !out_ptr.is_null() {
        if up {
            std::ptr::copy_nonoverlapping(g.info.as_ptr(), out_ptr, LDN_INFO_SIZE);
        } else {
            std::ptr::write_bytes(out_ptr, 0, LDN_INFO_SIZE);
        }
    }
    if up {
        SVC_OK
    } else {
        LDN_ERR_INVALID_STATE
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_get_network_config(out_ptr: *mut u8) -> u32 {
    let g = LDN_LOCK.lock_or_recover();
    let up = g.state == LdnState::AccessPointCreated || g.state == LdnState::StationConnected;
    if !out_ptr.is_null() {
        std::ptr::write_bytes(out_ptr, 0, 0x20);
        if up {
            let out = std::slice::from_raw_parts_mut(out_ptr, 0x20);
            out[0..0x10].copy_from_slice(&g.info[0..0x10]);
            out[0x10..0x12].copy_from_slice(&g.info[NI_CHANNEL..NI_CHANNEL + 2]);
            out[0x12] = g.info[NI_NODE_MAX];
            out[0x14..0x16]
                .copy_from_slice(&g.info[NI_NODES + NODE_VERSION..NI_NODES + NODE_VERSION + 2]);
        }
    }
    if up {
        SVC_OK
    } else {
        LDN_ERR_INVALID_STATE
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_ldn_get_security_parameter(out_ptr: *mut u8) -> u32 {
    let g = LDN_LOCK.lock_or_recover();
    let up = g.state == LdnState::AccessPointCreated || g.state == LdnState::StationConnected;
    if !out_ptr.is_null() {
        std::ptr::write_bytes(out_ptr, 0, 0x20);
        if up {
            let out = std::slice::from_raw_parts_mut(out_ptr, 0x20);
            out[0..0x10].copy_from_slice(&g.info[NI_SEC_PARAM..NI_SEC_PARAM + 0x10]);
            out[0x10..0x20].copy_from_slice(&g.info[NI_SESSION_ID..NI_SESSION_ID + 0x10]);
        }
    }
    if up {
        SVC_OK
    } else {
        LDN_ERR_INVALID_STATE
    }
}

#[no_mangle]
pub extern "C" fn acnh_ldn_get_disconnect_reason() -> u32 {
    LDN_LOCK.lock_or_recover().disconnect_reason
}
