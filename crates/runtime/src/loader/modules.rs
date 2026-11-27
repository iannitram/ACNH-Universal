//! Module loading and linking, the module initializers and the boot sequence (`main.cpp`).

use super::*;

// ---- Module loading & linking ------------------------------------------------------------------

pub(super) fn put_env(name: &str, value: &str) {
    // SAFETY: Single-threaded during boot before game threads spawn.
    unsafe {
        std::env::set_var(name, value);
    }
}

pub(super) fn find_symbol_internal(name: &str) -> u64 {
    exports().lock().unwrap().get(name).copied().unwrap_or(0)
}

pub(super) fn load_modules_internal(mods: &[*const AcnhModuleInfo], data_dir: &str, dynamic: bool) {
    let mut variant_on = std::env::var("NX_SPEEDFIX").as_deref() == Ok("1");
    let has_main_variant = variants().lock().unwrap().contains_key("main");
    if variant_on && !has_main_variant {
        log_msg("loader: frame rate 60 asked, but this game's code has no SpeedFix: 30 fps");
        variant_on = false;
        put_env("NX_SPEEDFIX", "0");
        put_env("NX_PRESENT_INTERVAL", "2");
        if let Ok(m) = std::env::var("NX_MODS") {
            let kept: Vec<&str> = m
                .split(',')
                .filter(|one| !one.is_empty() && *one != "speedfix60")
                .collect();
            put_env("NX_MODS", &kept.join(","));
        }
    }
    let mut variant_lo = 0u64;
    let mut variant_hi = u64::MAX;
    if let Ok(r) = std::env::var("NX_VARIANT_RANGE") {
        if let Some((a, b)) = r.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) {
                variant_lo = lo;
                variant_hi = hi;
            }
        }
    }
    let variant_code = std::env::var("NX_VARIANT_CODE").as_deref() != Ok("0");
    let words_on = std::env::var("NX_VARIANT_WORDS").as_deref() != Ok("0");
    let cache_on = std::env::var("NX_CALL_CACHE").as_deref() != Ok("0");

    let h = host();
    for &mi_ptr in mods {
        if mi_ptr.is_null() {
            continue;
        }
        // SAFETY: `mi_ptr` points to static `AcnhModuleInfo`.
        let mi = unsafe { &*mi_ptr };
        let mod_name = cstr_or_empty(mi.name);
        let image = mi.data + mi.data_size;
        let size = (mi.bss + mi.bss_size + 0xFFF) & !0xFFF;
        if let Some(commit) = h.commit {
            if !commit(mi.base, size) {
                fatal_msg(&format!("cannot commit module {mod_name}"));
            }
        }
        let path = format!("{data_dir}/{mod_name}.bin");
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(_) => fatal_msg(&format!("missing {path} (run tools/nso2bin.py)")),
        };
        let have = f.metadata().map(|m| m.len()).unwrap_or(0);
        if have != image {
            fatal_msg(&format!(
                "{path} is from another version of the game than its recompiled code ({have:x} bytes, expected {image:x}): the game folder mixes two versions. Prepare the game again into a new, empty folder, then copy your user folder (the save) into it."
            ));
        }
        // SAFETY: `mi.base..mi.base + image` was just committed for read/write.
        let dst = unsafe { std::slice::from_raw_parts_mut(mi.base as *mut u8, image as usize) };
        if f.read_exact(dst).is_err() {
            fatal_msg(&format!("{path}: expected {image:x} bytes"));
        }
        if let Some(words) = variants().lock().unwrap().get(mod_name).cloned() {
            let count = words.len() / 2;
            if variant_on && words_on {
                for &[off, word] in words.as_chunks::<2>().0 {
                    // SAFETY: `mi.base + off` is within the committed module image.
                    unsafe {
                        std::ptr::write_unaligned((mi.base + off as u64) as *mut u32, word);
                    }
                }
            }
            log_msg(&format!(
                "loader: {mod_name}: variant {} ({count} words)",
                if variant_on { "on" } else { "off" }
            ));
        }
        if !variant_code {
            variant_on = false;
        }
        if let Some(vm_set) = h.vm_set {
            vm_set(mi.base + mi.text, mi.text_size, K_MEM_CODE, 5);
            vm_set(mi.base + mi.ro, mi.ro_size, K_MEM_CODE, 1);
            vm_set(mi.base + mi.data, size - mi.data, K_MEM_CODE_DATA, 3);
        }

        modules().lock().unwrap().push(AcnhLoadedModule {
            info: mi_ptr,
            base: mi.base,
            size,
        });

        {
            let mut fd = FAST_DISPATCH.lock().unwrap();
            if dynamic && fd.anchor == 0 && mi.func_count > 0 && !mi.funcs.is_null() {
                // SAFETY: `mi.funcs` has at least one entry.
                let first = unsafe { (*mi.funcs).fn_ptr };
                if let Some(fp) = first {
                    fd.anchor = fp as usize;
                    CALL_ANCHOR.store(fd.anchor as u64, Ordering::Relaxed);
                }
            } else if !dynamic && fd.anchor == 0 {
                fd.anchor = default_call_anchor as *const () as usize;
                CALL_ANCHOR.store(fd.anchor as u64, Ordering::Relaxed);
            }
            if fd.count < K_MAX_FAST_MODULES {
                let idx = fd.count;
                fd.modules[idx] = Some(build_fast_module(mi));
                fd.count += 1;
            }
            let mut lo = u64::MAX;
            let mut hi = 0u64;
            for m in fd.modules.iter().take(fd.count).flatten() {
                lo = lo.min(m.base);
                hi = hi.max(m.base + m.text_size);
            }
            fd.cache_span = 0;
            CACHE_SPAN.store(0, Ordering::Relaxed);
            if cache_on && hi >= lo && (hi - lo) < (1u64 << 34) {
                fd.cache_base = lo;
                fd.cache_span = hi - lo;
                CACHE_BASE.store(lo, Ordering::Relaxed);
                CACHE_SPAN.store(hi - lo, Ordering::Relaxed);
            }
            for e in &CALL_CACHE {
                e.store(0, Ordering::Relaxed);
            }
        }

        let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
            unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
        };
        {
            let mut exp = exports().lock().unwrap();
            for s in syms {
                if s.imported != 0 || s.bind == 0 {
                    continue;
                }
                let sname = cstr_or_empty(s.name);
                if sname.is_empty() {
                    continue;
                }
                exp.entry(sname.to_owned()).or_insert(mi.base + s.value);
            }
        }
        log_msg(&format!(
            "loaded {mod_name} at {:x} ({} functions, {} relocs)",
            mi.base, mi.func_count, mi.reloc_count
        ));
    }

    VARIANT_ON.store(variant_on, Ordering::Relaxed);
    VARIANT_LO.store(variant_lo, Ordering::Relaxed);
    VARIANT_HI.store(variant_hi, Ordering::Relaxed);
    if let Some(sync) = h.sync_variant {
        sync(variant_on, variant_lo, variant_hi);
    }

    let mut hle = 0usize;
    let mut missing = 0usize;
    let mut linked = 0usize;
    let loaded = modules().lock().unwrap().clone();
    for lm in loaded {
        // SAFETY: `lm.info` points to static `AcnhModuleInfo`.
        let mi = unsafe { &*lm.info };
        let mod_name = cstr_or_empty(mi.name);
        let relocs = if mi.relocs.is_null() || mi.reloc_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.relocs` is valid for `mi.reloc_count` entries.
            unsafe { std::slice::from_raw_parts(mi.relocs, mi.reloc_count) }
        };
        let syms = if mi.symbols.is_null() || mi.symbol_count == 0 {
            &[]
        } else {
            // SAFETY: `mi.symbols` is valid for `mi.symbol_count` entries.
            unsafe { std::slice::from_raw_parts(mi.symbols, mi.symbol_count) }
        };
        for r in relocs {
            let slot = (lm.base + r.offset) as *mut u64;
            if r.reloc_type == R_AARCH64_RELATIVE {
                // SAFETY: `slot` is inside committed module memory.
                unsafe {
                    *slot = lm.base.wrapping_add(r.addend as u64);
                }
                continue;
            }
            if r.reloc_type != R_AARCH64_GLOB_DAT
                && r.reloc_type != R_AARCH64_JUMP_SLOT
                && r.reloc_type != R_AARCH64_ABS64
            {
                log_msg(&format!(
                    "{mod_name}: relocation type {} at {:x} not handled",
                    r.reloc_type, r.offset
                ));
                continue;
            }
            let s = &syms[r.sym as usize];
            let sname = cstr_or_empty(s.name);
            let mut target = 0u64;
            if (s.sym_type == 2 || s.imported != 0) && hle_lookup_internal(sname).is_some() {
                target = hle_address_or_internal(sname, missing_stub);
                hle += 1;
            }
            if target == 0 {
                target = find_symbol_internal(sname);
                if target != 0 {
                    linked += 1;
                }
            }
            if target == 0 && s.imported == 0 {
                target = lm.base + s.value;
            }
            if target == 0 {
                if s.sym_type == 2 || s.sym_type == 0 {
                    target = hle_address_or_internal(sname, missing_stub);
                }
                missing += 1;
            }
            // SAFETY: `slot` is inside committed module memory.
            unsafe {
                *slot = target.wrapping_add(r.addend as u64);
            }
        }
    }
    log_msg(&format!(
        "linked: {hle} to HLE, {linked} across modules, {missing} unresolved"
    ));
    if dynamic {
        if let Some(set_var) = h.game_set_variant {
            set_var(variant_on, variant_lo, variant_hi);
        }
    }
    register_hle_call_overrides();
}

// ---- Module initializers & boot (`main.cpp`) ---------------------------------------------------

/// Read `DT_INIT` from a module's `MOD0` + `.dynamic` table, falling back to `base + 0x30`.
///
/// # Safety
/// `base..base + 0x1000` (and the `MOD0` / `.dynamic` offsets) must be readable guest memory.
pub unsafe fn dt_init_from_memory(base: u64) -> u64 {
    let mod0 = base + (std::ptr::read_unaligned((base + 4) as *const u32) as u64);
    let dyn_off = std::ptr::read_unaligned((mod0 + 4) as *const i32) as i64;
    let mut p = mod0.wrapping_add(dyn_off as u64);
    loop {
        let tag = std::ptr::read_unaligned(p as *const i64);
        if tag == 0 {
            break;
        }
        if tag == 12 {
            return base + std::ptr::read_unaligned((p + 8) as *const u64);
        }
        p += 16;
    }
    base + 0x30
}

/// Scan `DT_INIT` for `adrp xN, page` ... `ldr w8, [xN, #off]` to locate the init status word.
///
/// # Safety
/// `init..init + 0x40` must be readable guest memory.
pub unsafe fn init_status_addr_from_memory(init: u64) -> Option<u64> {
    let mut page = [0u64; 32];
    let mut pc = init;
    while pc < init + 0x40 {
        let w = std::ptr::read_unaligned(pc as *const u32);
        if (w & 0x9F00_0000) == 0x9000_0000 {
            let immlo = ((w >> 29) & 3) as i64;
            let immhi = ((w >> 5) & 0x7FFFF) as i64;
            let imm = ((immhi << 2) | immlo) << 43 >> 31;
            page[(w & 31) as usize] = (pc & !0xFFF).wrapping_add(imm as u64);
        } else if (w & 0xFFC0_001F) == 0xB940_0008 {
            let rn = ((w >> 5) & 31) as usize;
            if page[rn] != 0 {
                let off = (((w >> 10) & 0xFFF) as u64) * 4;
                return Some(page[rn] + off);
            }
        }
        pc += 4;
    }
    None
}

/// Scan `DT_INIT` GOT slots for `.tdata` start/end (slots 13 and 14).
///
/// # Safety
/// `init..init + 0x100` and the referenced GOT slots must be readable guest memory.
pub unsafe fn tdata_range_from_memory(base: u64, init: u64) -> Option<(u64, u64)> {
    let mut page = [0u64; 32];
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    let mut pc = init;
    while pc < init + 0x100 {
        let w = std::ptr::read_unaligned(pc as *const u32);
        if (w & 0x9F00_0000) == 0x9000_0000 {
            let immlo = ((w >> 29) & 3) as i64;
            let immhi = ((w >> 5) & 0x7FFFF) as i64;
            let imm = ((immhi << 2) | immlo) << 43 >> 31;
            page[(w & 31) as usize] = (pc & !0xFFF).wrapping_add(imm as u64);
        } else if (w & 0xFFC0_0000) == 0xF940_0000 {
            let rn = ((w >> 5) & 31) as usize;
            if page[rn] != 0 {
                let slot = page[rn] + (((w >> 10) & 0xFFF) as u64) * 8;
                lo = lo.min(slot);
                hi = hi.max(slot);
            }
        }
        pc += 4;
    }
    if hi < lo || (hi - lo) / 8 + 1 < 15 {
        return None;
    }
    let start = std::ptr::read_unaligned((lo + 13 * 8) as *const u64);
    let end = std::ptr::read_unaligned((lo + 14 * 8) as *const u64);
    if start <= end && end - start <= 0x1000 && start >= base && end <= base + 0x1000_0000 {
        Some((start, end))
    } else {
        None
    }
}

pub(super) fn call_preserving(c: *mut AcnhCpu, target: u64) {
    // SAFETY: `c` is a valid guest CPU context.
    unsafe {
        let saved: [u64; 13] = std::array::from_fn(|i| (*c).x[19 + i]);
        call_internal(c, target);
        for (i, val) in saved.into_iter().enumerate() {
            (*c).x[19 + i] = val;
        }
    }
}

pub(super) fn init_module(c: *mut AcnhCpu, m: &AcnhModuleInfo) {
    // SAFETY: `m.base` is a mapped guest module.
    let init = unsafe { dt_init_from_memory(m.base) };
    call_preserving(c, init);
    // SAFETY: `init` is in the mapped guest module.
    let Some(status_addr) = (unsafe { init_status_addr_from_memory(init) }) else {
        fatal_msg(&format!(
            "{}: DT_INIT does not load its init status as expected",
            cstr_or_empty(m.name)
        ));
    };
    // SAFETY: `status_addr` points to the module's init status word in guest memory.
    let status = unsafe { std::ptr::read_unaligned(status_addr as *const u32) };
    if status == 1 && !m.init_array.is_null() {
        // SAFETY: `m.init_array` is valid for `m.init_count` entries.
        let arr = unsafe { std::slice::from_raw_parts(m.init_array, m.init_count) };
        for &off in arr {
            call_preserving(c, m.base + off);
        }
    }
    log_msg(&format!(
        "{} constructors: {}",
        cstr_or_empty(m.name),
        if status == 1 { m.init_count } else { 0 }
    ));
}

pub(super) fn game_module(name: &str) -> Option<&'static AcnhModuleInfo> {
    let cb = host().game_module?;
    let c_name = CString::new(name).ok()?;
    let ptr = cb(c_name.as_ptr());
    if ptr.is_null() {
        None
    } else {
        // SAFETY: `ptr` points to static `AcnhModuleInfo`.
        Some(unsafe { &*ptr })
    }
}

pub(super) extern "C" fn hle_nn_main(c: *mut AcnhCpu) {
    if let Some(sdk) = game_module("sdk") {
        init_module(c, sdk);
    }
    if let Some(sub) = game_module("subsdk0") {
        init_module(c, sub);
    }
    let Some(main_mod) = game_module("main") else {
        fatal_msg("main module missing in nnMain");
    };
    init_module(c, main_mod);
    if let Some(cb) = host().after_init {
        cb();
    }
    log_msg("entering nnMain");
    let mut real = 0u64;
    if !main_mod.symbols.is_null() && main_mod.symbol_count > 0 {
        // SAFETY: `main_mod.symbols` is valid for `main_mod.symbol_count` entries.
        let syms = unsafe { std::slice::from_raw_parts(main_mod.symbols, main_mod.symbol_count) };
        for s in syms {
            if s.imported == 0 && cstr_or_empty(s.name) == "nnMain" {
                real = main_mod.base + s.value;
            }
        }
    }
    call_internal(c, real);
}

pub(super) extern "C" fn hle_long_jmp(c: *mut AcnhCpu) {
    // SAFETY: `c` is a valid guest CPU context.
    let (buf, val) = unsafe { ((*c).x[0], ((*c).x[1] as u32) as u64) };
    if let Some(cb) = host().game_longjmp {
        cb(buf, val);
    }
    fatal_msg("longjmp handler missing");
}

pub(super) extern "C" fn hle_rtld_noop(_: *mut AcnhCpu) {}

pub(super) extern "C" fn hle_process_return(_: *mut AcnhCpu) {
    log_msg("nn::init::Start returned");
    if let Some(cb) = host().exit_process {
        cb(0);
    }
    std::process::exit(0);
}
