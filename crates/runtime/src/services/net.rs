//! Split from the old `services.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- 7. Networking & Friends (hle_net.cpp, hle_friends.cpp) ---------------------------------------------------------

pub(crate) static NET_LAST_AVAIL: AtomicI32 = AtomicI32::new(-1);

pub(crate) fn net_up() -> bool {
    with_host(|h| {
        h.nextendo_online_active
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            .map(|f| unsafe { f() })
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn acnh_net_is_network_available() -> u64 {
    let up = net_up();
    let up_i = i32::from(up);
    if NET_LAST_AVAIL.swap(up_i, Ordering::Relaxed) != up_i {
        if let Some(Some(f)) = with_host(|h| h.time_refresh_clocks) {
            // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
            unsafe { f() };
        }
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        let nextendo = with_host(|h| h.nextendo_is_nextendo.map(|f| unsafe { f() }))
            .flatten()
            .unwrap_or(false);
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        let signed = with_host(|h| h.nextendo_signed_in.map(|f| unsafe { f() }))
            .flatten()
            .unwrap_or(false);
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        let ver_ok = with_host(|h| h.nextendo_version_ok.map(|f| unsafe { f() }))
            .flatten()
            .unwrap_or(false);
        host_log(&format!(
            "nifm: network {} (Nextendo mode {}, signed in {}, version ok {})",
            if up { "available" } else { "unavailable" },
            i32::from(nextendo),
            i32::from(signed),
            i32::from(ver_ok)
        ));
    }
    u64::from(up)
}

#[no_mangle]
pub extern "C" fn acnh_net_is_any_internet_request_accepted() -> u64 {
    u64::from(net_up())
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_get_current_proxy_setting(out_ptr: u64) -> u32 {
    if !net_up() {
        return NIFM_ERR_NO_NETWORK;
    }
    if out_ptr != 0 {
        std::ptr::write_bytes(out_ptr as *mut u8, 0, 0xAA);
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_net_handle_request_error() -> u32 {
    if net_up() {
        SVC_OK
    } else {
        NIFM_ERR_NO_NETWORK
    }
}

pub fn expand_nsd_fqdn(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if out.len() >= 0xF0 {
            break;
        }
        if ch == '%' {
            out.push_str("lp1");
        } else {
            out.push(ch);
        }
    }
    out
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_resolve_ex(out_ptr: u64, in_ptr: u64) -> u32 {
    if !net_up() || in_ptr == 0 || out_ptr == 0 {
        return NET_ERR_GENERIC_FAIL;
    }
    let input = CStr::from_ptr(in_ptr as *const c_char).to_string_lossy();
    let fqdn = expand_nsd_fqdn(&input);
    let dst = std::slice::from_raw_parts_mut(out_ptr as *mut u8, 0x100);
    let bytes = fqdn.as_bytes();
    let n = bytes.len().min(0xFF);
    dst[..n].copy_from_slice(&bytes[..n]);
    dst[n] = 0;
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_get_current_primary_ip(out_ptr: u64) -> u32 {
    let ip = if net_up() {
        with_host(|h| h.hostnet_local_ip.map(|f| f()))
            .flatten()
            .unwrap_or(0)
    } else {
        0
    };
    if ip == 0 {
        return NIFM_ERR_NO_NETWORK;
    }
    if out_ptr != 0 {
        std::ptr::write_unaligned(out_ptr as *mut u32, ip);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_net_get_current_ip_config_info(
    out0: u64,
    out1: u64,
    out2: u64,
    out3: u64,
    out4: u64,
) -> u32 {
    let ip = if net_up() {
        with_host(|h| h.hostnet_local_ip.map(|f| f()))
            .flatten()
            .unwrap_or(0)
    } else {
        0
    };
    if ip == 0 {
        return NIFM_ERR_NO_NETWORK;
    }
    let mask = 0xFFFF_FF00u32.to_be();
    let gw = (ip & mask) | 1u32.to_be();
    let outs = [out0, out1, out2, out3, out4];
    let vals = [ip, mask, gw, gw, gw];
    for (o, v) in outs.into_iter().zip(vals) {
        if o != 0 {
            std::ptr::write_unaligned(o as *mut u32, v);
        }
    }
    SVC_OK
}

pub(crate) const ACNH_TITLE_ID: u64 = 0x0100_6F80_0232_6000;

pub(crate) fn friend_announced_id(f: &AcnhFriendInfo, own_nsa: u64) -> u64 {
    if f.nsa != 0 && own_nsa != 0 {
        f.nsa
    } else {
        f.pid
    }
}

pub(crate) fn friend_same_game(f: &AcnhFriendInfo) -> bool {
    if f.status == 0 {
        return false;
    }
    // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
    let app = unsafe { CStr::from_ptr(f.app_id.as_ptr()) }.to_string_lossy();
    u64::from_str_radix(app.trim().trim_start_matches("0x"), 16).unwrap_or(0) == ACNH_TITLE_ID
}

pub(crate) unsafe fn write_friend_record(out: u64, uid: u64, f: &AcnhFriendInfo, own_nsa: u64) {
    std::ptr::write_bytes(out as *mut u8, 0, FRIENDS_RECORD_SIZE);
    if uid != 0 {
        std::ptr::copy_nonoverlapping(uid as *const u8, out as *mut u8, 16);
        std::ptr::copy_nonoverlapping(uid as *const u8, (out + 0x40) as *mut u8, 16);
    }
    std::ptr::write_unaligned((out + 0x10) as *mut u64, friend_announced_id(f, own_nsa));
    let name = CStr::from_ptr(f.name.as_ptr()).to_bytes();
    let n = name.len().min(0x20);
    std::ptr::copy_nonoverlapping(name.as_ptr(), (out + 0x18) as *mut u8, n);
    let status = match f.status {
        2 => 2u32,
        1 => 1u32,
        _ => 0u32,
    };
    std::ptr::write_unaligned((out + 0x58) as *mut u32, status);
    std::ptr::write_unaligned((out + 0x5C) as *mut u8, u8::from(friend_same_game(f)));
    std::ptr::write_unaligned((out + 0x120) as *mut u8, u8::from(f.favorite != 0));
    std::ptr::write_unaligned((out + 0x128) as *mut u8, 1);
}

pub(crate) unsafe fn friend_passes_filter(filter: u64, f: &AcnhFriendInfo) -> bool {
    if filter == 0 {
        return true;
    }
    let presence = std::ptr::read_unaligned(filter as *const u32);
    if presence == 1 && f.status != 1 {
        return false;
    }
    if presence == 2 && f.status != 2 {
        return false;
    }
    if presence == 3 && f.status == 0 {
        return false;
    }
    if std::ptr::read_unaligned((filter + 4) as *const u8) != 0 && f.favorite == 0 {
        return false;
    }
    if std::ptr::read_unaligned((filter + 5) as *const u8) != 0 && !friend_same_game(f) {
        return false;
    }
    true
}

pub(crate) fn collect_friends() -> (u64, Vec<AcnhFriendInfo>) {
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    let own_nsa = with_host(|h| h.nextendo_account_nsa.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or(0);
    // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
    let count = with_host(|h| h.nextendo_friends_count.map(|f| unsafe { f() }))
        .flatten()
        .unwrap_or(0);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut info = AcnhFriendInfo {
            nsa: 0,
            pid: 0,
            status: 0,
            favorite: 0,
            _pad: [0; 3],
            app_id: [0; 32],
            name: [0; 33],
            _pad2: [0; 7],
        };
        let ok = with_host(|h| {
            h.nextendo_friends_get
                // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
                .map(|f| unsafe { f(i, &mut info) })
                .unwrap_or(false)
        })
        .unwrap_or(false);
        if ok {
            out.push(info);
        }
    }
    (own_nsa, out)
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_friends_get_list(
    out_count: u64,
    out: u64,
    uid: u64,
    offset: i32,
    max: i32,
    filter: u64,
) -> u32 {
    if !net_up() {
        if out_count != 0 {
            std::ptr::write_unaligned(out_count as *mut i32, 0);
        }
        return SVC_OK;
    }
    let off = offset.max(0) as usize;
    let mx = max.max(0) as usize;
    let (own_nsa, all) = collect_friends();
    let filtered: Vec<&AcnhFriendInfo> = all
        .iter()
        .filter(|f| friend_passes_filter(filter, f))
        .collect();
    let mut written = 0usize;
    for f in filtered.into_iter().skip(off).take(mx) {
        if out != 0 {
            write_friend_record(
                out + (written * FRIENDS_RECORD_SIZE) as u64,
                uid,
                f,
                own_nsa,
            );
        }
        written += 1;
    }
    if out_count != 0 {
        std::ptr::write_unaligned(out_count as *mut i32, written as i32);
    }
    SVC_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_hle_friends_update_info(
    out: u64,
    uid: u64,
    ids: u64,
    count: i32,
) -> u32 {
    if out == 0 || ids == 0 || count <= 0 {
        return SVC_OK;
    }
    let (own_nsa, list) = if net_up() {
        collect_friends()
    } else {
        (0, Vec::new())
    };
    for i in 0..(count as usize) {
        let id = std::ptr::read_unaligned((ids + (i as u64) * 8) as *const u64);
        let dst = out + (i * FRIENDS_RECORD_SIZE) as u64;
        std::ptr::write_bytes(dst as *mut u8, 0, FRIENDS_RECORD_SIZE);
        for f in &list {
            if friend_announced_id(f, own_nsa) == id {
                write_friend_record(dst, uid, f, own_nsa);
            }
        }
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_hle_friends_ensure_available() -> u32 {
    if !net_up() {
        return FRIENDS_ERR_NOT_LINKED;
    }
    if let Some(Some(f)) = with_host(|h| h.nextendo_friends_refresh) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f() };
    }
    SVC_OK
}

#[no_mangle]
pub extern "C" fn acnh_hle_friends_online_ok() -> u32 {
    if net_up() {
        SVC_OK
    } else {
        FRIENDS_ERR_NOT_LINKED
    }
}

#[no_mangle]
pub extern "C" fn acnh_hle_friends_declare_playing(playing: bool) -> u32 {
    if let Some(Some(f)) = with_host(|h| h.nextendo_presence_playing) {
        // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
        unsafe { f(playing) };
    }
    SVC_OK
}
