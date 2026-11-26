//! Split from the old `kernel.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- IPC (HIPC / CMIF / TIPC) ------------------------------------------------------------------

pub(crate) struct DomainState {
    pub(crate) objects: BTreeMap<u32, u64>,
    pub(crate) next_object: u32,
}

impl DomainState {
    pub(crate) fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_object: 1,
        }
    }
}

pub(crate) struct KSession {
    pub(crate) service: u64,
    pub(crate) domain: bool,
    pub(crate) dom: Arc<Mutex<DomainState>>,
}

pub(crate) static IPC_LOCK: ReentrantMutex = ReentrantMutex::new();
pub(crate) static SESSIONS: OnceLock<Mutex<HashMap<u32, KSession>>> = OnceLock::new();

pub(crate) fn sessions() -> &'static Mutex<HashMap<u32, KSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn ipc_close_session(h: u32) {
    let _il = IPC_LOCK.lock();
    if let Some(sess) = sessions().lock().unwrap().remove(&h) {
        if Arc::strong_count(&sess.dom) == 1 {
            let objs: Vec<u64> = sess.dom.lock().unwrap().objects.values().copied().collect();
            for s in objs {
                if s != sess.service {
                    (host().service_drop)(s);
                }
            }
        }
        (host().service_drop)(sess.service);
    }
}

pub(crate) fn new_session_handle(service: u64) -> u32 {
    let h = alloc_handle();
    sessions().lock().unwrap().insert(
        h,
        KSession {
            service,
            domain: false,
            dom: Arc::new(Mutex::new(DomainState::new())),
        },
    );
    h
}

pub(crate) fn service_name_str(service: u64) -> String {
    let mut buf = [0u8; 64];
    let len = (host().service_name)(service, buf.as_mut_ptr().cast::<c_char>(), buf.len());
    let n = len.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

pub struct ParsedHipcHeader {
    pub msg_type: u32,
    pub ndata: u32,
    pub pid: u64,
    pub in_copy: Vec<u32>,
    pub in_move: Vec<u32>,
    pub x: Vec<AcnhIpcBuffer>,
    pub a: Vec<AcnhIpcBuffer>,
    pub b: Vec<AcnhIpcBuffer>,
    pub c: Vec<AcnhIpcBuffer>,
    pub data_start: usize,
}

pub fn parse_hipc_words<F>(w: &[u32], fix_addr: F) -> ParsedHipcHeader
where
    F: Fn(u64) -> u64,
{
    let msg_type = w[0] & 0xFFFF;
    let nx = ((w[0] >> 16) & 0xF) as usize;
    let na = ((w[0] >> 20) & 0xF) as usize;
    let nb = ((w[0] >> 24) & 0xF) as usize;
    let nw = ((w[0] >> 28) & 0xF) as usize;
    let ndata = w[1] & 0x3FF;
    let cflags = (w[1] >> 10) & 0xF;
    let special = (w[1] >> 31) != 0;

    let mut p = 2usize;
    let mut pid = 0u64;
    let mut in_copy = Vec::new();
    let mut in_move = Vec::new();
    if special {
        let sh = w[p];
        p += 1;
        let has_pid = (sh & 1) != 0;
        let ncopy = ((sh >> 1) & 0xF) as usize;
        let nmove = ((sh >> 5) & 0xF) as usize;
        if has_pid {
            pid = (w[p] as u64) | ((w[p + 1] as u64) << 32);
            p += 2;
        }
        for _ in 0..ncopy {
            in_copy.push(w[p]);
            p += 1;
        }
        for _ in 0..nmove {
            in_move.push(w[p]);
            p += 1;
        }
    }

    let mut x = Vec::with_capacity(nx);
    for _ in 0..nx {
        let d0 = w[p];
        let d1 = w[p + 1];
        p += 2;
        let addr =
            (d1 as u64) | ((((d0 >> 12) & 0xF) as u64) << 32) | ((((d0 >> 6) & 0x7) as u64) << 36);
        x.push(AcnhIpcBuffer {
            addr: fix_addr(addr),
            size: (d0 >> 16) as u64,
        });
    }

    let mut read_abw = |n: usize| -> Vec<AcnhIpcBuffer> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let size_lo = w[p];
            let addr_lo = w[p + 1];
            let d2 = w[p + 2];
            p += 3;
            let addr = (addr_lo as u64)
                | ((((d2 >> 28) & 0xF) as u64) << 32)
                | ((((d2 >> 2) & 0x7) as u64) << 36);
            let size = (size_lo as u64) | ((((d2 >> 24) & 0xF) as u64) << 32);
            out.push(AcnhIpcBuffer {
                addr: fix_addr(addr),
                size,
            });
        }
        out
    };

    let a = read_abw(na);
    let b = read_abw(nb);
    let _w = read_abw(nw);
    let data_start = p;

    let mut c = Vec::new();
    if cflags >= 2 {
        let mut q = data_start + ndata as usize;
        let nc = if cflags == 2 { 1 } else { cflags - 2 };
        for _ in 0..nc {
            if q + 1 < w.len() {
                let addr = (w[q] as u64) | (((w[q + 1] & 0xFFFF) as u64) << 32);
                c.push(AcnhIpcBuffer {
                    addr,
                    size: (w[q + 1] >> 16) as u64,
                });
            }
            q += 2;
        }
    }

    ParsedHipcHeader {
        msg_type,
        ndata,
        pid,
        in_copy,
        in_move,
        x,
        a,
        b,
        c,
        data_start,
    }
}

pub(crate) fn ipc_connect_ptr(port: *const c_char, out_handle: Option<&mut u64>) -> u32 {
    let port_str = if port.is_null() {
        String::new()
    } else {
        // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
        unsafe { CStr::from_ptr(port) }
            .to_string_lossy()
            .into_owned()
    };
    host_log(&format!("ConnectToNamedPort({port_str})"));
    let c_port = CString::new(port_str).unwrap_or_default();
    let svc = (host().service_make)(c_port.as_ptr());
    let h = new_session_handle(svc);
    if let Some(out) = out_handle {
        *out = h as u64;
    }
    RC_OK
}

/// Bytes of the guest message area a HIPC request/reply may use.
const IPC_MSG_AREA: u64 = 0x100;

/// Smallest message area that still holds a HIPC header (two words) plus the reply code.
const IPC_MIN_MSG_AREA: u64 = 0x10;

/// End offset, in bytes from the message start, of a TIPC reply: header, optional special
/// header and handles (`special_words`), the result word, then the payload rounded to words.
pub(crate) fn tipc_reply_end(special_words: usize, out_len: usize) -> u64 {
    ((2 + special_words + 1) * 4 + ((out_len + 3) & !3)) as u64
}

/// End offset, in bytes from the message start, of a CMIF reply written at `buf`: header,
/// special header and handles (`special_words`), the 16-byte aligned raw data (a domain
/// header first when `domain_hdr`), the SFCO header, the payload rounded to words and the
/// returned domain object ids.
pub(crate) fn cmif_reply_end(
    buf: u64,
    special_words: usize,
    domain_hdr: bool,
    out_len: usize,
    n_obj_ids: usize,
) -> u64 {
    let rawo = (buf + ((2 + special_words) * 4) as u64 + 15) & !15u64;
    let o = rawo + if domain_hdr { 16 } else { 0 };
    o + 16 + ((out_len + 3) & !3) as u64 + 4 * n_obj_ids as u64 - buf
}

/// Message area for an IPC request: the guest-supplied size, never above the HIPC area.
pub(crate) fn ipc_msg_capacity(size: u64) -> u64 {
    size.min(IPC_MSG_AREA)
}

pub(crate) fn ipc_request_inner(c: &mut AcnhCpu, handle: u32, buf: u64, size: u64) -> u32 {
    let _il = IPC_LOCK.lock();
    let cap = ipc_msg_capacity(size);
    if cap < IPC_MIN_MSG_AREA {
        host_log(&format!(
            "ipc: message area of {size:#x} bytes is too small"
        ));
        return RC_OUT_OF_RANGE;
    }
    let (sess_service, sess_domain, sess_dom) = {
        let map = sessions().lock().unwrap();
        let Some(sess) = map.get(&handle) else {
            host_log(&format!("ipc: request on invalid handle {handle:x}"));
            return RC_INVALID_HANDLE;
        };
        (sess.service, sess.domain, Arc::clone(&sess.dom))
    };

    // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
    let w = unsafe { std::slice::from_raw_parts_mut(buf as *mut u32, 0x40) };
    let hdr = parse_hipc_words(w, desc_addr);

    if hdr.msg_type == 2 || hdr.msg_type == 15 {
        close_handle(handle);
        return RC_OK;
    }

    let mut out_buf = [0u8; 0x100];
    let mut out_len = 0usize;
    let mut out_copy = [0u32; 16];
    let mut out_copy_len = 0usize;
    let mut out_move = [0u32; 16];
    let mut out_move_len = 0usize;
    let mut out_services = [0u64; 16];
    let mut out_services_len = 0usize;

    if hdr.msg_type >= 16 {
        let cmd = hdr.msg_type - 16;
        let in_ptr = (buf + (hdr.data_start * 4) as u64) as *const u8;
        let in_size = (hdr.ndata as usize) * 4;
        if (host().get_trace)() {
            host_log(&format!(
                "ipc: tipc {} cmd {cmd}",
                service_name_str(sess_service)
            ));
        }
        let mut dispatch = AcnhIpcDispatch {
            cpu: c,
            service: sess_service,
            cmd,
            msg_type: hdr.msg_type,
            pid: hdr.pid,
            in_ptr,
            in_size,
            in_copy: hdr.in_copy.as_ptr(),
            in_copy_len: hdr.in_copy.len(),
            in_move: hdr.in_move.as_ptr(),
            in_move_len: hdr.in_move.len(),
            a_bufs: hdr.a.as_ptr(),
            a_len: hdr.a.len(),
            b_bufs: hdr.b.as_ptr(),
            b_len: hdr.b.len(),
            x_bufs: hdr.x.as_ptr(),
            x_len: hdr.x.len(),
            c_bufs: hdr.c.as_ptr(),
            c_len: hdr.c.len(),
            in_services: std::ptr::null(),
            in_services_len: 0,
            out_buf: out_buf.as_mut_ptr(),
            out_cap: out_buf.len(),
            out_len: &mut out_len,
            out_copy: out_copy.as_mut_ptr(),
            out_copy_cap: out_copy.len(),
            out_copy_len: &mut out_copy_len,
            out_move: out_move.as_mut_ptr(),
            out_move_cap: out_move.len(),
            out_move_len: &mut out_move_len,
            out_services: out_services.as_mut_ptr(),
            out_services_cap: out_services.len(),
            out_services_len: &mut out_services_len,
        };
        let rc = (host().service_dispatch)(&mut dispatch);
        for &o in &out_services[..out_services_len] {
            if out_move_len < out_move.len() {
                out_move[out_move_len] = new_session_handle(o);
                out_move_len += 1;
            }
        }
        let special = if out_copy_len > 0 || out_move_len > 0 {
            1 + out_copy_len + out_move_len
        } else {
            0
        };
        let end = tipc_reply_end(special, out_len);
        // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
        unsafe { std::ptr::write_bytes(buf as *mut u8, 0, cap as usize) };
        if end > cap {
            host_log(&format!(
                "ipc: {} tipc cmd {cmd} reply of {out_len:#x} bytes needs {end:#x} bytes but the message area is {cap:#x}",
                service_name_str(sess_service)
            ));
            return RC_OUT_OF_RANGE;
        }
        let has_special = out_copy_len > 0 || out_move_len > 0;
        let mut q = 2usize;
        if has_special {
            w[q] = ((out_copy_len as u32) << 1) | ((out_move_len as u32) << 5);
            q += 1;
            for &h in &out_copy[..out_copy_len] {
                w[q] = h;
                q += 1;
            }
            for &h in &out_move[..out_move_len] {
                w[q] = h;
                q += 1;
            }
        }
        w[q] = rc;
        // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                out_buf.as_ptr(),
                (&mut w[q + 1] as *mut u32).cast::<u8>(),
                out_len,
            );
        }
        w[0] = hdr.msg_type;
        w[1] = (1 + out_len.div_ceil(4) as u32) | if has_special { 0x8000_0000 } else { 0 };
        return RC_OK;
    }

    let mut raw = (buf + (hdr.data_start * 4) as u64 + 15) & !15u64;
    if (host().get_trace)() {
        host_log(&format!(
            "ipc: request type {} on {} (handle {handle:x}, domain {}, raw word {:08x})",
            hdr.msg_type,
            service_name_str(sess_service),
            sess_domain as u8,
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            unsafe { ld_u32(raw) }
        ));
    }

    let mut target = sess_service;
    let mut in_objects = Vec::new();
    if sess_domain && (hdr.msg_type == 4 || hdr.msg_type == 6) {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let domain_cmd = unsafe { ld_u8(raw) };
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let nobj = unsafe { ld_u8(raw + 1) } as u32;
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let dsize = unsafe { ld_u16(raw + 2) } as u64;
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let domain_obj = unsafe { ld_u32(raw + 4) };
        for i in 0..nobj {
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            in_objects.push(unsafe { ld_u32(raw + 16 + dsize + 4 * (i as u64)) });
        }
        raw += 16;
        if domain_cmd == 2 {
            sess_dom.lock().unwrap().objects.remove(&domain_obj);
            // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
            unsafe { std::ptr::write_bytes(buf as *mut u8, 0, cap as usize) };
            w[1] = 8;
            return RC_OK;
        }
        let found = sess_dom.lock().unwrap().objects.get(&domain_obj).copied();
        let Some(t) = found else {
            host_log(&format!(
                "ipc: {} domain object {domain_obj} not found",
                service_name_str(sess_service)
            ));
            return RC_IPC_DOMAIN_NOT_FOUND;
        };
        target = t;
    }

    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    let magic = unsafe { ld_u32(raw) };
    // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
    let cmd = unsafe { ld_u32(raw + 8) };
    let in_ptr = (raw + 16) as *const u8;
    let in_size = if hdr.ndata * 4 > 32 {
        (hdr.ndata * 4 - 32) as usize
    } else {
        0
    };
    if magic != SFCI {
        host_log(&format!(
            "ipc: {} bad magic {magic:08x} (data_start {}, ndata {})",
            service_name_str(target),
            hdr.data_start,
            hdr.ndata
        ));
    }

    let rc: u32;
    if hdr.msg_type == 5 || hdr.msg_type == 7 {
        match cmd {
            0 => {
                // ConvertCurrentObjectToDomain
                if let Some(sess) = sessions().lock().unwrap().get_mut(&handle) {
                    sess.domain = true;
                }
                let mut dom = sess_dom.lock().unwrap();
                let id = dom.next_object;
                dom.next_object += 1;
                dom.objects.insert(id, sess_service);
                out_buf[..4].copy_from_slice(&id.to_le_bytes());
                out_len = 4;
                rc = 0;
            }
            1 => rc = 0,
            2 | 4 => {
                // CloneCurrentObject / CloneCurrentObjectEx
                let clone_h = alloc_handle();
                sessions().lock().unwrap().insert(
                    clone_h,
                    KSession {
                        service: sess_service,
                        domain: sess_domain,
                        dom: Arc::clone(&sess_dom),
                    },
                );
                out_move[out_move_len] = clone_h;
                out_move_len += 1;
                rc = 0;
            }
            3 => {
                // QueryPointerBufferSize
                out_buf[..2].copy_from_slice(&0x8000u16.to_le_bytes());
                out_len = 2;
                rc = 0;
            }
            _ => {
                host_log(&format!("ipc: control cmd {cmd}"));
                rc = 0;
            }
        }
    } else {
        if (host().get_trace)() {
            host_log(&format!("ipc: {} cmd {cmd}", service_name_str(target)));
        }
        let mut in_services = Vec::new();
        if sess_domain {
            let dom = sess_dom.lock().unwrap();
            for id in in_objects {
                in_services.push(dom.objects.get(&id).copied().unwrap_or(0));
            }
        } else {
            let map = sessions().lock().unwrap();
            for &h in &hdr.in_move {
                in_services.push(map.get(&h).map_or(0, |s| s.service));
            }
        }
        let mut dispatch = AcnhIpcDispatch {
            cpu: c,
            service: target,
            cmd,
            msg_type: hdr.msg_type,
            pid: hdr.pid,
            in_ptr,
            in_size,
            in_copy: hdr.in_copy.as_ptr(),
            in_copy_len: hdr.in_copy.len(),
            in_move: hdr.in_move.as_ptr(),
            in_move_len: hdr.in_move.len(),
            a_bufs: hdr.a.as_ptr(),
            a_len: hdr.a.len(),
            b_bufs: hdr.b.as_ptr(),
            b_len: hdr.b.len(),
            x_bufs: hdr.x.as_ptr(),
            x_len: hdr.x.len(),
            c_bufs: hdr.c.as_ptr(),
            c_len: hdr.c.len(),
            in_services: in_services.as_ptr(),
            in_services_len: in_services.len(),
            out_buf: out_buf.as_mut_ptr(),
            out_cap: out_buf.len(),
            out_len: &mut out_len,
            out_copy: out_copy.as_mut_ptr(),
            out_copy_cap: out_copy.len(),
            out_copy_len: &mut out_copy_len,
            out_move: out_move.as_mut_ptr(),
            out_move_cap: out_move.len(),
            out_move_len: &mut out_move_len,
            out_services: out_services.as_mut_ptr(),
            out_services_cap: out_services.len(),
            out_services_len: &mut out_services_len,
        };
        rc = (host().service_dispatch)(&mut dispatch);
    }

    let mut out_obj_ids = Vec::new();
    let is_dom = sessions()
        .lock()
        .unwrap()
        .get(&handle)
        .is_some_and(|s| s.domain);
    for &o in &out_services[..out_services_len] {
        if is_dom {
            let mut dom = sess_dom.lock().unwrap();
            let id = dom.next_object;
            dom.next_object += 1;
            dom.objects.insert(id, o);
            out_obj_ids.push(id);
        } else if out_move_len < out_move.len() {
            out_move[out_move_len] = new_session_handle(o);
            out_move_len += 1;
        }
    }

    let special = if out_copy_len > 0 || out_move_len > 0 {
        1 + out_copy_len + out_move_len
    } else {
        0
    };
    let end = cmif_reply_end(
        buf,
        special,
        is_dom && (hdr.msg_type == 4 || hdr.msg_type == 6),
        out_len,
        out_obj_ids.len(),
    );
    // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
    unsafe { std::ptr::write_bytes(buf as *mut u8, 0, cap as usize) };
    if end > cap {
        host_log(&format!(
            "ipc: {} cmif cmd {cmd} reply of {out_len:#x} bytes needs {end:#x} bytes but the message area is {cap:#x}",
            service_name_str(target)
        ));
        return RC_OUT_OF_RANGE;
    }
    let has_special = out_copy_len > 0 || out_move_len > 0;
    let mut q = 2usize;
    if has_special {
        w[q] = ((out_copy_len as u32) << 1) | ((out_move_len as u32) << 5);
        q += 1;
        for &h in &out_copy[..out_copy_len] {
            w[q] = h;
            q += 1;
        }
        for &h in &out_move[..out_move_len] {
            w[q] = h;
            q += 1;
        }
    }
    let dstart = q;
    let rawo = (buf + (q * 4) as u64 + 15) & !15u64;
    let mut o = rawo;
    if is_dom && (hdr.msg_type == 4 || hdr.msg_type == 6) {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { st_u32(o, out_obj_ids.len() as u32) };
        o += 16;
    }
    // SAFETY: `o` is inside the guest IPC message buffer, which the guest sized for the reply header plus `out_len` bytes; it is mapped for the life of the process.
    unsafe {
        st_u32(o, SFCO);
        st_u32(o + 4, 0);
        st_u32(o + 8, rc);
        st_u32(o + 12, 0);
        std::ptr::copy_nonoverlapping(out_buf.as_ptr(), (o + 16) as *mut u8, out_len);
    }
    let mut end = o + 16 + ((out_len + 3) & !3) as u64;
    for id in out_obj_ids {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        unsafe { st_u32(end, id) };
        end += 4;
    }
    let words = ((end - (buf + (dstart * 4) as u64)) / 4) as u32 + 4;
    w[0] = 0;
    w[1] = words | if has_special { 0x8000_0000 } else { 0 };
    RC_OK
}
