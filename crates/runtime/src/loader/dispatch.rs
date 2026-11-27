//! Indirect-call dispatch, the fast call cache and the HLE thunks.

use super::*;

// ---- Dispatch & fast call cache ----------------------------------------------------------------

pub(super) struct FastModule {
    pub(super) base: u64,
    pub(super) text_size: u64,
    pub(super) funcs: Vec<AcnhFuncEntry>,
    pub(super) block: Vec<u32>,
}

// SAFETY: `AcnhFuncEntry` holds an offset and function pointer.
unsafe impl Send for FastModule {}
// SAFETY: `AcnhFuncEntry` holds an offset and function pointer.
unsafe impl Sync for FastModule {}

pub(super) struct FastDispatch {
    pub(super) modules: [Option<FastModule>; K_MAX_FAST_MODULES],
    pub(super) count: usize,
    pub(super) cache_base: u64,
    pub(super) cache_span: u64,
    pub(super) anchor: usize,
}

pub(super) static FAST_DISPATCH: Mutex<FastDispatch> = Mutex::new(FastDispatch {
    modules: [const { None }; K_MAX_FAST_MODULES],
    count: 0,
    cache_base: 0,
    cache_span: 0,
    anchor: 0,
});

pub(super) static CALL_CACHE: [AtomicU64; K_CALL_CACHE_SIZE] =
    [const { AtomicU64::new(0) }; K_CALL_CACHE_SIZE];

pub(super) static CACHE_BASE: AtomicU64 = AtomicU64::new(0);
pub(super) static CACHE_SPAN: AtomicU64 = AtomicU64::new(0);
pub(super) static CALL_ANCHOR: AtomicU64 = AtomicU64::new(0);

pub(super) extern "C" fn default_call_anchor(_: *mut AcnhCpu) {}

#[inline]
pub(super) fn cache_slot(rel: u64) -> usize {
    let h = ((rel >> 2) as u32).wrapping_mul(0x9E37_79B1);
    (h >> (32 - K_CALL_CACHE_BITS)) as usize
}

pub(super) fn build_fast_module(mi: &AcnhModuleInfo) -> FastModule {
    let mut funcs = if mi.funcs.is_null() || mi.func_count == 0 {
        Vec::new()
    } else {
        // SAFETY: `mi.funcs` is valid for `mi.func_count` entries.
        unsafe { std::slice::from_raw_parts(mi.funcs, mi.func_count) }.to_vec()
    };
    funcs.sort_by_key(|f| f.offset);
    funcs.push(AcnhFuncEntry {
        offset: u64::MAX,
        fn_ptr: None,
    });
    let blocks = (mi.text_size as usize).div_ceil(64);
    let mut block = Vec::with_capacity(blocks);
    let mut k = 0usize;
    for b in 0..blocks {
        let limit = (b as u64) * 64;
        while k < mi.func_count && funcs[k].offset < limit {
            k += 1;
        }
        block.push(k as u32);
    }
    FastModule {
        base: mi.base,
        text_size: mi.text_size,
        funcs,
        block,
    }
}

pub(super) fn fast_lookup(m: &FastModule, off: u64) -> Option<HleFn> {
    let blk = (off >> 6) as usize;
    let mut idx = *m.block.get(blk)? as usize;
    while m.funcs[idx].offset < off {
        idx += 1;
    }
    if m.funcs[idx].offset == off {
        m.funcs[idx].fn_ptr
    } else {
        None
    }
}

pub(super) fn module_at_internal(addr: u64) -> Option<AcnhLoadedModule> {
    let mods = modules().lock().unwrap();
    mods.iter()
        .find(|m| addr.wrapping_sub(m.base) < m.size)
        .copied()
}

pub(super) fn describe_internal(addr: u64) -> String {
    let lay = layout();
    let thunk_off = addr.wrapping_sub(lay.thunk_base);
    if thunk_off < lay.thunk_size {
        let idx = (thunk_off / 16) as usize;
        let table = thunks().lock().unwrap();
        let name = table
            .list
            .get(idx)
            .and_then(|t| t.name.to_str().ok())
            .unwrap_or("?");
        return format!("hle:{name}");
    }
    let Some(lm) = module_at_internal(addr) else {
        return format!("{addr:x}");
    };
    // SAFETY: `lm.info` points to static `AcnhModuleInfo`.
    let mi = unsafe { &*lm.info };
    let mod_name = cstr_or_empty(mi.name);
    let off = addr.wrapping_sub(lm.base);
    let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
        &[]
    } else {
        // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
        unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
    };
    let mut best: Option<&AcnhSymbol> = None;
    for s in syms {
        if s.imported != 0 || s.sym_type != 2 || s.value > off {
            continue;
        }
        if best.is_none_or(|b| s.value > b.value) {
            best = Some(s);
        }
    }
    if let Some(b) = best {
        if off - b.value < 0x10000 {
            return format!(
                "{mod_name}+{off:x} ({}+{:x})",
                cstr_or_empty(b.name),
                off - b.value
            );
        }
    }
    format!("{mod_name}+{off:x}")
}

pub fn hle_vtable_override_name(name: &str) -> bool {
    if name.starts_with("_ZN2nn3nfp6client") || name.starts_with("_ZN2nn3nfc6client") {
        return false;
    }
    if name.starts_with("_ZN2nn3nfp") || name.starts_with("_ZN2nn3nfc") {
        return true;
    }
    if name.starts_with("_ZN2nn3hid6system") && name.contains("Nfc") {
        return true;
    }
    if name.starts_with("_ZN2nn8settings6system") && name.contains("Nfc") {
        return true;
    }
    false
}

pub(super) fn register_hle_call_overrides() {
    let mods = modules().lock().unwrap().clone();
    let mut ov = hle_call_overrides().lock().unwrap();
    ov.clear();
    let mut n = 0usize;
    for lm in mods {
        // SAFETY: `lm.info` points to static `AcnhModuleInfo`.
        let mi = unsafe { &*lm.info };
        let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
            unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
        };
        for s in syms {
            if s.imported != 0 || s.sym_type != 2 {
                continue;
            }
            let name = cstr_or_empty(s.name);
            if name.is_empty() || !hle_vtable_override_name(name) {
                continue;
            }
            if let Some(fn_ptr) = hle_lookup_internal(name) {
                ov.insert(lm.base + s.value, fn_ptr);
                n += 1;
            }
        }
    }
    if n > 0 {
        log_msg(&format!(
            "loader: {n} NFC-related SDK entry points wired to HLE vtables"
        ));
    }
}

pub(super) fn dump_regs_and_backtrace(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context.
    let cpu = unsafe { &*c };
    for k in (0..31).step_by(4) {
        let x3 = if k + 3 < 31 { cpu.x[k + 3] } else { cpu.x[31] };
        log_msg(&format!(
            "  x{:<2} {:016x} {:016x} {:016x} {:016x}",
            k,
            cpu.x[k],
            cpu.x[k + 1],
            cpu.x[k + 2],
            x3
        ));
    }
    if let Some(bt) = host().backtrace {
        bt(c);
    }
}

#[inline(never)]
pub(super) fn call_slow(c: *mut AcnhCpu, t: u64) {
    if let Some(fn_ptr) = hle_call_overrides().lock().unwrap().get(&t).copied() {
        fn_ptr(c);
        return;
    }
    {
        let fd = FAST_DISPATCH.lock().unwrap();
        for k in 0..fd.count {
            if let Some(m) = &fd.modules[k] {
                let off = t.wrapping_sub(m.base);
                if off < m.text_size {
                    let Some(fn_ptr) = fast_lookup(m, off) else {
                        drop(fd);
                        dump_regs_and_backtrace(c);
                        // SAFETY: `c` is a valid guest CPU context.
                        let lr = unsafe { (*c).x[30] };
                        fatal_msg(&format!(
                            "call into the middle of a function: {} (lr {})",
                            describe_internal(t),
                            describe_internal(lr)
                        ));
                    };
                    let rel = t.wrapping_sub(fd.cache_base);
                    let delta = (fn_ptr as usize as isize) - (fd.anchor as isize);
                    if rel < fd.cache_span
                        && (rel & 3) == 0
                        && delta >= i32::MIN as isize
                        && delta <= i32::MAX as isize
                    {
                        let entry = (((rel >> 2) + 1) << 32) | ((delta as i32 as u32) as u64);
                        CALL_CACHE[cache_slot(rel)].store(entry, Ordering::Relaxed);
                    }
                    drop(fd);
                    fn_ptr(c);
                    return;
                }
            }
        }
    }
    let lay = layout();
    let off = t.wrapping_sub(lay.thunk_base);
    if off < lay.thunk_size {
        let idx = (off / 16) as usize;
        let thunk_info = {
            let table = thunks().lock().unwrap();
            table.list.get(idx).map(|th| {
                (
                    th.fn_ptr,
                    th.leaf,
                    th.name.as_ptr(),
                    th.name.to_string_lossy().into_owned(),
                )
            })
        };
        if let Some((fn_ptr, leaf, name_ptr, name_str)) = thunk_info {
            let trace = TRACE.load(Ordering::Relaxed);
            if leaf && !trace {
                fn_ptr(c);
                return;
            }
            if trace {
                log_msg(&format!("hle {name_str}"));
            }
            let prev = T_MISSING_NAME.with(|cell| {
                let p = cell.get();
                cell.set(name_ptr);
                p
            });
            fn_ptr(c);
            T_MISSING_NAME.with(|cell| cell.set(prev));
            return;
        }
    }
    // SAFETY: `c` is a valid guest CPU context.
    let lr = unsafe { (*c).x[30] };
    fatal_msg(&format!(
        "call to {t:x} ({}), lr {}",
        describe_internal(t),
        describe_internal(lr)
    ));
}

pub(super) fn call_internal(c: *mut AcnhCpu, t: u64) {
    let base = CACHE_BASE.load(Ordering::Relaxed);
    let span = CACHE_SPAN.load(Ordering::Relaxed);
    let rel = t.wrapping_sub(base);
    if rel < span {
        let e = CALL_CACHE[cache_slot(rel)].load(Ordering::Relaxed);
        if (e >> 32) as u32 == ((rel >> 2) + 1) as u32 && (rel & 3) == 0 {
            let anchor = CALL_ANCHOR.load(Ordering::Relaxed) as usize;
            let delta = (e as u32 as i32) as isize;
            let addr = (anchor as isize).wrapping_add(delta) as usize;
            // SAFETY: `addr` was stored from a valid `HleFn` in `call_slow`.
            let fn_ptr: HleFn = unsafe { std::mem::transmute(addr) };
            fn_ptr(c);
            return;
        }
    }
    call_slow(c, t);
}
