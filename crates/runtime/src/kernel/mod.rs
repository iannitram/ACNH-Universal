//! Horizon kernel, synchronization, IPC (HIPC/CMIF/TIPC), and diagnostics in Rust.
//! Replaces `runtime/src/kernel.cpp`, `runtime/src/ipc.cpp`, and `runtime/src/kernel_watch.cpp`.

#[allow(unused_imports)]
use std::collections::{BTreeMap, HashMap, HashSet};
#[allow(unused_imports)]
use std::ffi::{c_char, c_void, CStr, CString};
#[allow(unused_imports)]
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
#[allow(unused_imports)]
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[allow(unused_imports)]
use std::time::{Duration, Instant};

// ---- Constants & result codes ------------------------------------------------------------------

pub const HANDLE_WAIT_MASK: u32 = 0x4000_0000;
pub const PSEUDO_CURRENT_THREAD: u32 = 0xFFFF_8000;
pub const PSEUDO_CURRENT_PROCESS: u32 = 0xFFFF_8001;

pub const RC_OK: u32 = 0;
pub const RC_OUT_OF_MEMORY: u32 = 0xD001;
pub const RC_INVALID_HANDLE: u32 = 0xE401;
pub const RC_TIMEOUT: u32 = 0xEA01;
pub const RC_CANCELLED: u32 = 0xEC01;
pub const RC_OUT_OF_RANGE: u32 = 0xDC01;
pub const RC_INVALID_ENUM: u32 = 0xF001;
pub const RC_IPC_DOMAIN_NOT_FOUND: u32 = 0xF601;
pub const RC_INVALID_STATE: u32 = 0xFA01;

pub const MEM_FREE: u32 = 0x00;
pub const MEM_SHARED: u32 = 0x06;
pub const MEM_HEAP: u32 = 0x05;
pub const MEM_STACK: u32 = 0x0B;
pub const MEM_ALIAS: u32 = 0x0E;
pub const MEM_INACCESSIBLE: u32 = 0x10;

pub const SFCI: u32 = 0x4943_4653;
pub const SFCO: u32 = 0x4F43_4653;

// ---- C-ABI structs -----------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcnhV128 {
    pub u64: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AcnhCpu {
    pub x: [u64; 32],
    pub v: [AcnhV128; 32],
    pub nf: u8,
    pub zf: u8,
    pub cf: u8,
    pub vf: u8,
    pub fpcr: u64,
    pub fpsr: u64,
    pub tpidr: u64,
    pub tpidrro: u64,
    pub excl_addr: u64,
    pub excl_val: u64,
    pub excl_val_hi: u64,
}

impl Default for AcnhCpu {
    fn default() -> Self {
        Self {
            x: [0; 32],
            v: [AcnhV128::default(); 32],
            nf: 0,
            zf: 0,
            cf: 0,
            vf: 0,
            fpcr: 0,
            fpsr: 0,
            tpidr: 0,
            tpidrro: 0,
            excl_addr: !0u64,
            excl_val: 0,
            excl_val_hi: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcnhRegionBounds {
    pub module_base: u64,
    pub heap_base: u64,
    pub heap_max: u64,
    pub alias_base: u64,
    pub alias_size: u64,
    pub stack_base: u64,
    pub stack_size: u64,
    pub guest_shift: u64,
    pub compact_layout: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcnhIpcBuffer {
    pub addr: u64,
    pub size: u64,
}

#[repr(C)]
pub struct AcnhIpcDispatch {
    pub cpu: *mut AcnhCpu,
    pub service: u64,
    pub cmd: u32,
    pub msg_type: u32,
    pub pid: u64,
    pub in_ptr: *const u8,
    pub in_size: usize,
    pub in_copy: *const u32,
    pub in_copy_len: usize,
    pub in_move: *const u32,
    pub in_move_len: usize,
    pub a_bufs: *const AcnhIpcBuffer,
    pub a_len: usize,
    pub b_bufs: *const AcnhIpcBuffer,
    pub b_len: usize,
    pub x_bufs: *const AcnhIpcBuffer,
    pub x_len: usize,
    pub c_bufs: *const AcnhIpcBuffer,
    pub c_len: usize,
    pub in_services: *const u64,
    pub in_services_len: usize,
    pub out_buf: *mut u8,
    pub out_cap: usize,
    pub out_len: *mut usize,
    pub out_copy: *mut u32,
    pub out_copy_cap: usize,
    pub out_copy_len: *mut usize,
    pub out_move: *mut u32,
    pub out_move_cap: usize,
    pub out_move_len: *mut usize,
    pub out_services: *mut u64,
    pub out_services_cap: usize,
    pub out_services_len: *mut usize,
}

#[repr(C)]
pub struct AcnhKernelHost {
    pub monotonic_ns: extern "C" fn() -> u64,
    pub sleep_ns: extern "C" fn(ns: i64),
    pub yield_thread: extern "C" fn(),
    pub current_thread_id: extern "C" fn() -> i64,
    pub set_thread_name: extern "C" fn(name: *const c_char),
    pub start_thread:
        extern "C" fn(entry: extern "C" fn(*mut c_void), arg: *mut c_void, stack_size: usize),
    pub exit_thread: extern "C" fn(),
    pub exit_process: extern "C" fn(code: i32),
    pub window_minimized: extern "C" fn() -> bool,
    pub data_root: extern "C" fn(buf: *mut c_char, cap: usize) -> usize,
    pub profile_samples: extern "C" fn(
        tids: *const i64,
        lrs: *const *const u64,
        count: usize,
        seconds: i32,
        out_path: *const c_char,
    ) -> bool,
    pub log: extern "C" fn(msg: *const c_char),
    pub fatal: extern "C" fn(msg: *const c_char),
    pub describe: extern "C" fn(addr: u64, buf: *mut c_char, cap: usize) -> usize,
    pub kernel_alloc: extern "C" fn(size: u64, align: u64) -> u64,
    pub commit: extern "C" fn(addr: u64, size: u64) -> bool,
    pub decommit: extern "C" fn(addr: u64, size: u64),
    pub vm_set: extern "C" fn(addr: u64, size: u64, state: u32, perm: u32),
    pub vm_query: extern "C" fn(
        addr: u64,
        out_addr: *mut u64,
        out_size: *mut u64,
        out_state: *mut u32,
        out_perm: *mut u32,
    ),
    pub call_guest: extern "C" fn(cpu: *mut AcnhCpu, target: u64),
    pub cpu_profile_register: extern "C" fn(name: *const c_char),
    pub init_tls_block: extern "C" fn(block: u64),
    pub get_info: extern "C" fn(info_type: u32, handle: u32, sub: u64, out_rc: *mut u32) -> u64,
    pub set_thread_core_mask:
        extern "C" fn(handle: u32, ideal: i32, mask: u64, out_core: *mut i32) -> u32,
    pub map_transfer_memory: extern "C" fn(handle: u32, addr: u64, size: u64) -> u32,
    pub unmap_transfer_memory: extern "C" fn(handle: u32, addr: u64, size: u64) -> u32,
    pub is_hades: extern "C" fn() -> bool,
    pub get_trace: extern "C" fn() -> bool,
    pub get_frame: extern "C" fn() -> u64,
    pub get_guest_base: extern "C" fn() -> u64,
    pub get_region_bounds: extern "C" fn(out: *mut AcnhRegionBounds),
    pub obj_signaled: extern "C" fn(handle: u32) -> i32,
    pub obj_consume: extern "C" fn(handle: u32),
    pub event_signal: extern "C" fn(handle: u32) -> u32,
    pub event_clear: extern "C" fn(handle: u32, reset_signal: bool) -> u32,
    pub on_thread_created:
        extern "C" fn(handle: u32, id: u64, entry: u64, tls: u64, prio: i32, core: i32),
    pub on_thread_exited: extern "C" fn(handle: u32),
    pub on_event_created: extern "C" fn(w_handle: u32, r_handle: u32),
    pub on_transfer_memory_created: extern "C" fn(handle: u32, addr: u64, size: u64),
    pub on_handle_close: extern "C" fn(handle: u32),
    pub service_make: extern "C" fn(name: *const c_char) -> u64,
    pub service_name: extern "C" fn(service: u64, buf: *mut c_char, cap: usize) -> usize,
    pub service_dispatch: extern "C" fn(dispatch: *mut AcnhIpcDispatch) -> u32,
    pub service_drop: extern "C" fn(service: u64),
    pub shared_mem_create: extern "C" fn(name: *const c_char, size: u64, host_addr: u64) -> u32,
    pub shared_mem_map: extern "C" fn(handle: u32, addr: u64, size: u64) -> u32,
}

// Safety: AcnhKernelHost only contains plain C function pointers.
unsafe impl Send for AcnhKernelHost {}
// SAFETY: the host table holds only plain function pointers installed once and never mutated afterwards, so sharing it across threads is sound.
unsafe impl Sync for AcnhKernelHost {}

pub(crate) static HOST: OnceLock<AcnhKernelHost> = OnceLock::new();

pub(crate) fn host() -> &'static AcnhKernelHost {
    HOST.get()
        .expect("acnh_kernel_set_host must be called first")
}

pub(crate) fn host_log(msg: &str) {
    if let Some(h) = HOST.get() {
        if let Ok(c) = CString::new(msg) {
            (h.log)(c.as_ptr());
        }
    }
}

pub(crate) fn host_fatal(msg: &str) -> ! {
    if let Some(h) = HOST.get() {
        if let Ok(c) = CString::new(msg) {
            (h.fatal)(c.as_ptr());
        }
    }
    std::process::abort();
}

pub(crate) fn host_describe(addr: u64) -> String {
    let Some(h) = HOST.get() else {
        return format!("{addr:#x}");
    };
    let mut buf = [0u8; 256];
    let len = (h.describe)(addr, buf.as_mut_ptr().cast::<c_char>(), buf.len());
    let n = len.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

pub(crate) fn host_data_root() -> String {
    let Some(h) = HOST.get() else {
        return String::new();
    };
    let mut buf = [0u8; 512];
    let len = (h.data_root)(buf.as_mut_ptr().cast::<c_char>(), buf.len());
    let n = len.min(buf.len().saturating_sub(1));
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

pub(crate) fn region_bounds() -> AcnhRegionBounds {
    let mut b = AcnhRegionBounds::default();
    if let Some(h) = HOST.get() {
        (h.get_region_bounds)(&mut b);
    }
    b
}

// ---- Reentrant kernel lock ---------------------------------------------------------------------

pub(crate) struct ReentrantState {
    pub(crate) owner: Option<std::thread::ThreadId>,
    pub(crate) depth: usize,
}

pub(crate) struct ReentrantMutex {
    pub(crate) state: Mutex<ReentrantState>,
    pub(crate) cv: Condvar,
}

impl ReentrantMutex {
    const fn new() -> Self {
        Self {
            state: Mutex::new(ReentrantState {
                owner: None,
                depth: 0,
            }),
            cv: Condvar::new(),
        }
    }

    pub(crate) fn lock(&self) -> ReentrantGuard<'_> {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        while let Some(owner) = st.owner {
            if owner == me {
                st.depth += 1;
                return ReentrantGuard { mutex: self };
            }
            st = self.cv.wait(st).unwrap();
        }
        st.owner = Some(me);
        st.depth = 1;
        ReentrantGuard { mutex: self }
    }

    pub(crate) fn try_lock(&self) -> Option<ReentrantGuard<'_>> {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        match st.owner {
            None => {
                st.owner = Some(me);
                st.depth = 1;
                Some(ReentrantGuard { mutex: self })
            }
            Some(owner) if owner == me => {
                st.depth += 1;
                Some(ReentrantGuard { mutex: self })
            }
            Some(_) => None,
        }
    }

    pub(crate) fn unlock_one(&self) {
        let mut st = self.state.lock().unwrap();
        st.depth -= 1;
        if st.depth == 0 {
            st.owner = None;
            self.cv.notify_one();
        }
    }

    pub(crate) fn unlock_all(&self) -> usize {
        let mut st = self.state.lock().unwrap();
        let saved = st.depth;
        st.depth = 0;
        st.owner = None;
        self.cv.notify_one();
        saved
    }

    pub(crate) fn relock_all(&self, depth: usize) {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        while st.owner.is_some() {
            st = self.cv.wait(st).unwrap();
        }
        st.owner = Some(me);
        st.depth = depth;
    }
}

pub(crate) struct ReentrantGuard<'a> {
    pub(crate) mutex: &'a ReentrantMutex,
}

impl Drop for ReentrantGuard<'_> {
    fn drop(&mut self) {
        self.mutex.unlock_one();
    }
}

pub(crate) static KERNEL_LOCK: ReentrantMutex = ReentrantMutex::new();

mod state;
#[allow(unused_imports)]
pub use state::*;
mod svc;
#[allow(unused_imports)]
pub use svc::*;
mod ipc;
#[allow(unused_imports)]
pub use ipc::*;
mod diag;
#[allow(unused_imports)]
pub use diag::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_reply_end_math() {
        // The largest TIPC and CMIF replies that fit with no handles.
        assert_eq!(tipc_reply_end(0, 0xF4), 0x100);
        assert!(tipc_reply_end(0, 0xF5) > 0x100);
        assert_eq!(cmif_reply_end(0x1000, 0, false, 0xE0, 0), 0x100);
        assert!(cmif_reply_end(0x1000, 0, false, 0xE1, 0) > 0x100);
        // A domain header takes another 16 bytes.
        assert_eq!(cmif_reply_end(0x1000, 0, true, 0xD0, 0), 0x100);
        assert!(cmif_reply_end(0x1000, 0, true, 0xD1, 0) > 0x100);
        // Handles lower the threshold.
        assert!(tipc_reply_end(2, 0xF4) > 0x100);
        // Worst case (16 copy + 16 move handles, domain, 0x100 payload, 16 ids).
        assert_eq!(cmif_reply_end(0x1000, 1 + 32, true, 0x100, 16), 0x1F0);
        // The user-buffer size is honoured but never raised above the HIPC area.
        assert_eq!(ipc_msg_capacity(0x40), 0x40);
        assert_eq!(ipc_msg_capacity(0x1000), 0x100);
        assert_eq!(ipc_msg_capacity(0), 0);
    }

    #[test]
    fn yield_backoff_matches_nxrt_table() {
        assert_eq!(yield_backoff_sleep_ns(999, 1000, 250_000), 0);
        assert_eq!(yield_backoff_sleep_ns(1000, 1000, 250_000), 10_000);
        assert_eq!(yield_backoff_sleep_ns(800_000, 1000, 250_000), 100_000);
        assert_eq!(yield_backoff_sleep_ns(100_000_000, 1000, 250_000), 250_000);

        let a = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let b = [1, 2, 3, 4, 5, 6, 7, 8, 9, 11];
        assert!(!yield_backoff_progress(&a, &a));
        assert!(yield_backoff_progress(&a, &b));
    }

    #[test]
    fn yield_backoff_env_parsing() {
        assert_eq!(parse_yield_backoff_env(None), (true, 2_000_000, 250_000));
        assert_eq!(
            parse_yield_backoff_env(Some("0")),
            (false, 2_000_000, 250_000)
        );
        assert_eq!(
            parse_yield_backoff_env(Some("")),
            (false, 2_000_000, 250_000)
        );
        assert_eq!(
            parse_yield_backoff_env(Some("500,5")),
            (true, 500_000, 10_000)
        );
        assert_eq!(
            parse_yield_backoff_env(Some("3000,120")),
            (true, 3_000_000, 120_000)
        );
    }

    #[test]
    fn system_tick_is_19_2_mhz() {
        assert_eq!(compute_system_tick(0), 0);
        assert_eq!(compute_system_tick(1_000_000_000), 19_200_000);
    }

    #[test]
    fn desc_addr_handles_shift_and_compact() {
        assert_eq!(desc_addr_with(0, 0, 0, false), 0);
        assert_eq!(desc_addr_with(0x40_1234_5678, 0, 0, false), 0x40_1234_5678);
        // macOS shift: 0xC0_0000_0000 + 0x10_1234_5678 -> low 39 bits only -> restored
        let shift = 0xC0_0000_0000u64;
        let full = shift + 0x10_1234_5678u64;
        let truncated = full & 0x7F_FFFF_FFFF;
        assert_eq!(desc_addr_with(truncated, 0, shift, false), full);
        // iOS compact layout
        let base = 0x1_4000_0000u64;
        let compact_full = base + 0x1234_5678u64;
        let compact_trunc = compact_full & 0x7F_FFFF_FFFF;
        assert_eq!(desc_addr_with(compact_trunc, base, 0, true), compact_full);
    }

    #[test]
    fn signal_to_address_count_math() {
        assert_eq!(compute_signal_to_address_update(2, 10, 0, 0), Ok(11));
        assert_eq!(compute_signal_to_address_update(2, 10, 0, 3), Ok(8));
        assert_eq!(compute_signal_to_address_update(2, 10, 2, 2), Ok(9));
        assert_eq!(compute_signal_to_address_update(2, 10, 2, 3), Ok(10));
        assert_eq!(compute_signal_to_address_update(2, 10, 2, 0), Ok(11));
        assert_eq!(
            compute_signal_to_address_update(9, 10, 1, 0),
            Err(RC_INVALID_ENUM)
        );
    }

    #[test]
    fn parse_hipc_header_extracts_special_and_descriptors() {
        // type = 4, nx = 1, na = 1, nb = 0, nw = 0; ndata = 8, cflags = 0, special = 1
        let w0 = 4u32 | (1 << 16) | (1 << 20);
        let w1 = 8u32 | (1 << 31);
        // special: pid=1, ncopy=1, nmove=1
        let sh = 1u32 | (1 << 1) | (1 << 5);
        // x0: size = 0x20, addr = 0x12_3456_789A
        let x_d0 = (0x20u32 << 16) | (0x2 << 12) | (0x1 << 6);
        let x_d1 = 0x3456_789Au32;
        // a0: size = 0x100, addr = 0x05_1122_3344
        let a_sz = 0x100u32;
        let a_ad = 0x1122_3344u32;
        let a_d2 = 0x5u32 << 28;
        let words = [
            w0,
            w1,
            sh,
            0xAAAA_BBBB,
            0x0000_0001,
            0x44,
            0x55,
            x_d0,
            x_d1,
            a_sz,
            a_ad,
            a_d2,
        ];
        let parsed = parse_hipc_words(&words, |a| a);
        assert_eq!(parsed.msg_type, 4);
        assert_eq!(parsed.ndata, 8);
        assert_eq!(parsed.pid, 0x0000_0001_AAAA_BBBB);
        assert_eq!(parsed.in_copy, vec![0x44]);
        assert_eq!(parsed.in_move, vec![0x55]);
        assert_eq!(parsed.x.len(), 1);
        assert_eq!(parsed.x[0].addr, 0x12_3456_789A);
        assert_eq!(parsed.x[0].size, 0x20);
        assert_eq!(parsed.a.len(), 1);
        assert_eq!(parsed.a[0].addr, 0x05_1122_3344);
        assert_eq!(parsed.a[0].size, 0x100);
        assert_eq!(parsed.data_start, 12);
    }
}
