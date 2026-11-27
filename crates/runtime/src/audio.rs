//! `nn::audio` HLE (`AudioOut`, `AudioIn`, `AudioRenderer`, device enumeration, and I3DL2 reverb
//! bypass), ported from `runtime/src/hle_audio.cpp`.
//!
//! `runtime/src/audio_wrapper.cpp` provides the thin C++ host callbacks (`plat::audio_*`,
//! `plat::monotonic_ns`, `plat::sleep_ns`, `nx::log`, kernel event signaling) and registers the
//! `NX_HLE` entries for guest CPU/IPC dispatch.

use std::collections::VecDeque;
use std::ffi::{c_char, c_void};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config;
use crate::ffi::host_log;

const BYTES_PER_SECOND: u64 = 48_000 * 2 * 2;
const MAX_IN_FLIGHT: u32 = 4;
const AUDIO_OUT_OPAQUE_HANDLE: u64 = 0xA0D10;
const AUDIO_IN_NOT_FOUND_ERR: u64 = (1 << 9) | 153;
const OUTPUT_NAME: &[u8] = b"AudioTvOutput\0";

const FRAME_SAMPLES: usize = 240;
const BLOCKS: usize = 64;
const MAX_QUEUED: u32 = 12;
const PRIME_BLOCKS: usize = 4;
const MAX_CHANNELS: usize = 6;
const MIX_RING_CAPACITY: usize = 48_000 * 2 / 5;

const CMD_DOWNMIX_6CH_TO_2CH: u8 = 17;
const CMD_DEVICE_SINK: u8 = 19;

const RENDERER_VTABLE_OFF: u64 = 0x00d0_4910;
const EXECUTE_RENDERING_OFF: u64 = 0x0020_cac8;
const DEFAULT_MODE_OFF: u64 = 0x2d0;

/// Host callbacks provided by `runtime/src/audio_wrapper.cpp`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AcnhAudioHost {
    pub audio_open: Option<extern "C" fn(done: extern "C" fn(*mut c_void), channels: i32) -> bool>,
    pub audio_submit:
        Option<extern "C" fn(data: *const c_void, bytes: usize, ctx: *mut c_void) -> bool>,
    pub audio_queued: Option<extern "C" fn() -> u32>,
    pub monotonic_ns: Option<extern "C" fn() -> u64>,
    pub sleep_ns: Option<extern "C" fn(ns: i64)>,
    pub log: Option<extern "C" fn(msg: *const c_char)>,
    pub signal_out_event: Option<extern "C" fn()>,
    pub clear_out_event: Option<extern "C" fn()>,
    pub signal_frame_event: Option<extern "C" fn()>,
}

// SAFETY: `AcnhAudioHost` only holds plain function pointers.
unsafe impl Send for AcnhAudioHost {}
// SAFETY: `AcnhAudioHost` only holds plain function pointers.
unsafe impl Sync for AcnhAudioHost {}

static HOST: OnceLock<Mutex<AcnhAudioHost>> = OnceLock::new();
static MONO_START: OnceLock<Instant> = OnceLock::new();
static ACTIVE_CHANNELS: OnceLock<i32> = OnceLock::new();
static DEVICE_OPEN_OK: OnceLock<bool> = OnceLock::new();
static DEVICE_SINK_ID: OnceLock<u8> = OnceLock::new();
static EXECUTE_RENDERING_ADDR: AtomicU64 = AtomicU64::new(0);

fn host_lock() -> &'static Mutex<AcnhAudioHost> {
    HOST.get_or_init(|| Mutex::new(AcnhAudioHost::default()))
}

fn current_host() -> AcnhAudioHost {
    *host_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn host_monotonic_ns() -> u64 {
    if let Some(f) = current_host().monotonic_ns {
        return f();
    }
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn host_sleep_ns(ns: i64) {
    if ns <= 0 {
        return;
    }
    if let Some(f) = current_host().sleep_ns {
        f(ns);
        return;
    }
    std::thread::sleep(Duration::from_nanos(ns as u64));
}

fn host_audio_queued() -> u32 {
    current_host().audio_queued.map_or(0, |f| f())
}

fn host_audio_submit(data: *const c_void, bytes: usize, ctx: *mut c_void) -> bool {
    current_host()
        .audio_submit
        .is_some_and(|f| f(data, bytes, ctx))
}

fn host_signal_out_event() {
    if let Some(f) = current_host().signal_out_event {
        f();
    }
}

fn host_clear_out_event() {
    if let Some(f) = current_host().clear_out_event {
        f();
    }
}

fn host_signal_frame_event() {
    if let Some(f) = current_host().signal_frame_event {
        f();
    }
}

fn active_output_channels() -> i32 {
    if cfg!(target_os = "android") {
        2
    } else {
        *ACTIVE_CHANNELS.get_or_init(|| {
            if config::config_get_bool("audio.surround", false) {
                6
            } else {
                2
            }
        })
    }
}

fn device_open() -> bool {
    *DEVICE_OPEN_OK.get_or_init(|| {
        current_host()
            .audio_open
            .is_some_and(|f| f(device_done_cb, active_output_channels()))
    })
}

// ---- Guest memory helpers ----------------------------------------------------------------------

#[inline]
unsafe fn ld_u16(addr: u64) -> u16 {
    std::ptr::read_unaligned(addr as *const u16)
}

#[inline]
unsafe fn ld_i16(addr: u64) -> i16 {
    std::ptr::read_unaligned(addr as *const i16)
}

#[inline]
unsafe fn ld_u32(addr: u64) -> u32 {
    std::ptr::read_unaligned(addr as *const u32)
}

#[inline]
unsafe fn ld_u64(addr: u64) -> u64 {
    std::ptr::read_unaligned(addr as *const u64)
}

#[inline]
unsafe fn st_u16(addr: u64, val: u16) {
    std::ptr::write_unaligned(addr as *mut u16, val);
}

#[inline]
unsafe fn st_u32(addr: u64, val: u32) {
    std::ptr::write_unaligned(addr as *mut u32, val);
}

#[inline]
unsafe fn st_u64(addr: u64, val: u64) {
    std::ptr::write_unaligned(addr as *mut u64, val);
}

// ---- AudioMixRing ------------------------------------------------------------------------------

/// Fixed 200 ms stereo sample queue; `push()` drops the oldest sample when full.
pub struct AudioMixRing {
    samples: Vec<i16>,
    head: usize,
    len: usize,
}

impl Default for AudioMixRing {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMixRing {
    pub fn new() -> Self {
        Self {
            samples: vec![0; MIX_RING_CAPACITY],
            head: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, sample: i16) {
        if self.len == MIX_RING_CAPACITY {
            self.head = Self::next_idx(self.head);
            self.len -= 1;
        }
        let mut tail = self.head + self.len;
        if tail >= MIX_RING_CAPACITY {
            tail -= MIX_RING_CAPACITY;
        }
        self.samples[tail] = sample;
        self.len += 1;
    }

    pub fn pop_front(&mut self) -> Option<i16> {
        if self.len == 0 {
            return None;
        }
        let sample = self.samples[self.head];
        self.head = Self::next_idx(self.head);
        self.len -= 1;
        Some(sample)
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn next_idx(index: usize) -> usize {
        if index + 1 == MIX_RING_CAPACITY {
            0
        } else {
            index + 1
        }
    }
}

static OUT_MIX: OnceLock<Mutex<AudioMixRing>> = OnceLock::new();

fn out_mix() -> &'static Mutex<AudioMixRing> {
    OUT_MIX.get_or_init(|| Mutex::new(AudioMixRing::new()))
}

// ---- AudioOut state ----------------------------------------------------------------------------

#[derive(Default)]
struct OutState {
    queued: VecDeque<u64>,
    released: VecDeque<u64>,
    started: bool,
    player_started: bool,
}

struct OutSync {
    mu: Mutex<OutState>,
    cv: Condvar,
}

static OUT: OnceLock<OutSync> = OnceLock::new();
static OUT_VOLUME_BITS: AtomicU32 = AtomicU32::new(1.0_f32.to_bits());
static OUT_APPENDED: AtomicU64 = AtomicU64::new(0);
static OUT_PLAYED: AtomicU64 = AtomicU64::new(0);
static OUT_TAKEN: AtomicU64 = AtomicU64::new(0);
static OUT_POLLS: AtomicU64 = AtomicU64::new(0);
static OUT_STATS_NEXT_NS: AtomicU64 = AtomicU64::new(0);
static OUT_STATS_ON: OnceLock<bool> = OnceLock::new();

fn out_sync() -> &'static OutSync {
    OUT.get_or_init(|| OutSync {
        mu: Mutex::new(OutState::default()),
        cv: Condvar::new(),
    })
}

fn out_stats_enabled() -> bool {
    *OUT_STATS_ON.get_or_init(|| std::env::var_os("NX_AUDIO_OUT_STATS").is_some())
}

fn out_stats(st: &OutState) {
    if !out_stats_enabled() {
        return;
    }
    let now = host_monotonic_ns();
    let next = OUT_STATS_NEXT_NS.load(Ordering::Relaxed);
    if now < next {
        return;
    }
    OUT_STATS_NEXT_NS.store(now.saturating_add(2_000_000_000), Ordering::Relaxed);
    host_log(&format!(
        "audio: AudioOut {}: {} appended, {} played, {} taken back ({} polls); {} queued, {} released",
        if st.started { "started" } else { "stopped" },
        OUT_APPENDED.load(Ordering::Relaxed),
        OUT_PLAYED.load(Ordering::Relaxed),
        OUT_TAKEN.load(Ordering::Relaxed),
        OUT_POLLS.load(Ordering::Relaxed),
        st.queued.len(),
        st.released.len(),
    ));
}

fn release_out_buffer(buf: u64) {
    let sync = out_sync();
    {
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        OUT_PLAYED.fetch_add(1, Ordering::Relaxed);
        st.released.push_back(buf);
        host_signal_out_event();
        sync.cv.notify_all();
    }
}

// ---- RenderOut & AudioRenderer state -----------------------------------------------------------

struct RenderOut {
    mix: [i32; FRAME_SAMPLES * MAX_CHANNELS],
    mixed: bool,
    blocks: Box<[[i16; FRAME_SAMPLES * MAX_CHANNELS]; BLOCKS]>,
    free: Vec<usize>,
    held: Vec<usize>,
    priming: bool,
    frames: u64,
    dropped: u64,
    underruns: u64,
    silent: u64,
    sum_sq: f64,
    peak: i32,
    dump: Vec<i16>,
    last_mix_chans: i32,
}

impl RenderOut {
    fn new() -> Self {
        let free = (0..BLOCKS).rev().collect();
        Self {
            mix: [0; FRAME_SAMPLES * MAX_CHANNELS],
            mixed: false,
            blocks: Box::new([[0; FRAME_SAMPLES * MAX_CHANNELS]; BLOCKS]),
            free,
            held: Vec::new(),
            priming: true,
            frames: 0,
            dropped: 0,
            underruns: 0,
            silent: 0,
            sum_sq: 0.0,
            peak: 0,
            dump: Vec::new(),
            last_mix_chans: 0,
        }
    }

    fn blocks_range(&self) -> (usize, usize) {
        let base = self.blocks.as_ptr() as usize;
        let bytes = BLOCKS * FRAME_SAMPLES * MAX_CHANNELS * std::mem::size_of::<i16>();
        (base, base + bytes)
    }
}

static RENDER_OUT: OnceLock<Mutex<RenderOut>> = OnceLock::new();
static RENDERERS: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
static RENDER_THREAD_STARTED: AtomicBool = AtomicBool::new(false);

fn render_out() -> &'static Mutex<RenderOut> {
    RENDER_OUT.get_or_init(|| Mutex::new(RenderOut::new()))
}

fn renderers_mu() -> &'static Mutex<Vec<u64>> {
    RENDERERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn renderer_running() -> bool {
    !renderers_mu()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
}

fn render_owns(addr: usize) -> bool {
    let ro = render_out().lock().unwrap_or_else(|e| e.into_inner());
    let (base, end) = ro.blocks_range();
    addr >= base && addr < end
}

fn render_block_done(addr: usize) {
    let mut ro = render_out().lock().unwrap_or_else(|e| e.into_inner());
    let (base, end) = ro.blocks_range();
    if addr >= base && addr < end {
        let block_bytes = FRAME_SAMPLES * MAX_CHANNELS * std::mem::size_of::<i16>();
        let idx = (addr - base) / block_bytes;
        ro.free.push(idx);
    }
}

extern "C" fn device_done_cb(ctx: *mut c_void) {
    let addr = ctx as usize;
    if render_owns(addr) {
        render_block_done(addr);
    } else {
        release_out_buffer(addr as u64);
    }
}

// ---- Render system serialization & pacing ------------------------------------------------------

struct BinaryLock {
    held: Mutex<bool>,
    cv: Condvar,
}

impl BinaryLock {
    const fn new() -> Self {
        Self {
            held: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) {
        let mut g = self.held.lock().unwrap_or_else(|e| e.into_inner());
        while *g {
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        *g = true;
    }

    fn unlock(&self) {
        let mut g = self.held.lock().unwrap_or_else(|e| e.into_inner());
        *g = false;
        self.cv.notify_one();
    }
}

#[derive(Default)]
struct PaceState {
    renders: u64,
    renders_at_update: u64,
    waited_logs: u64,
}

struct PaceSync {
    mu: Mutex<PaceState>,
    cv: Condvar,
}

static RENDER_SYSTEM_LOCK: BinaryLock = BinaryLock::new();
static PACE_SYNC: OnceLock<PaceSync> = OnceLock::new();

fn pace_sync() -> &'static PaceSync {
    PACE_SYNC.get_or_init(|| PaceSync {
        mu: Mutex::new(PaceState {
            renders: 0,
            renders_at_update: u64::MAX,
            waited_logs: 0,
        }),
        cv: Condvar::new(),
    })
}

// ---- Player thread -----------------------------------------------------------------------------

fn upmix_stereo_to_6ch(src: &[i16], dst: &mut Vec<i16>) {
    let frames = src.len() / 2;
    dst.resize(frames * 6, 0);
    for s in 0..frames {
        let l = src[2 * s];
        let r = src[2 * s + 1];
        dst[s * 6] = l;
        dst[s * 6 + 1] = r;
        dst[s * 6 + 2] = ((i32::from(l) + i32::from(r)) / 2) as i16;
        dst[s * 6 + 3] = 0;
        dst[s * 6 + 4] = l;
        dst[s * 6 + 5] = r;
    }
}

fn player_loop() {
    let device = device_open();
    let chans = active_output_channels();
    host_log(&format!(
        "audio: {}",
        if device {
            if chans == 6 {
                "device output (48 kHz 5.1 s16)"
            } else {
                "device output (48 kHz stereo s16)"
            }
        } else {
            "no audio device, timer pacing"
        }
    ));

    let mut next = Instant::now();
    let mut upmix: [Vec<i16>; 32] = std::array::from_fn(|_| Vec::new());
    let mut next_upmix = 0usize;

    loop {
        let (buf, size) = {
            let sync = out_sync();
            let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
            let ready = |s: &OutState| {
                s.started
                    && !s.queued.is_empty()
                    && (renderer_running() || !device || host_audio_queued() < MAX_IN_FLIGHT)
            };
            if !ready(&st) {
                let (guard, _) = sync
                    .cv
                    .wait_timeout_while(st, Duration::from_millis(5), |s| !ready(s))
                    .unwrap_or_else(|e| e.into_inner());
                st = guard;
            }
            out_stats(&st);
            if !ready(&st) {
                continue;
            }
            let buf = st.queued.pop_front().unwrap_or(0);
            // SAFETY: `buf` is a guest AudioOutBuffer address queued by the game.
            let size = unsafe { ld_u64(buf + 16) };
            (buf, size)
        };

        let mix = renderer_running();
        if mix {
            // SAFETY: `buf` points to AudioOutBuffer { data_ptr, capacity, size }.
            let data_ptr = unsafe { ld_u64(buf) } as *const i16;
            let sample_count = (size / 2) as usize;
            // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
            let pcm = unsafe { std::slice::from_raw_parts(data_ptr, sample_count) };
            let vol = f32::from_bits(OUT_VOLUME_BITS.load(Ordering::Relaxed));
            let mut ring = out_mix().lock().unwrap_or_else(|e| e.into_inner());
            for &s in pcm {
                ring.push((f32::from(s) * vol) as i16);
            }
        }

        if !mix && device {
            // SAFETY: `buf` points to AudioOutBuffer { data_ptr, capacity, size }.
            let data_ptr = unsafe { ld_u64(buf) };
            let mut send_ptr = data_ptr as *const c_void;
            let mut send_size = size as usize;
            if active_output_channels() == 6 {
                // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
                let pcm = unsafe {
                    std::slice::from_raw_parts(data_ptr as *const i16, (size / 2) as usize)
                };
                let block = &mut upmix[next_upmix];
                upmix_stereo_to_6ch(pcm, block);
                send_ptr = block.as_ptr() as *const c_void;
                send_size = block.len() * std::mem::size_of::<i16>();
                next_upmix = (next_upmix + 1) % upmix.len();
            }
            if host_audio_submit(send_ptr, send_size, buf as *mut c_void) {
                continue;
            }
        }

        let now = Instant::now();
        let floor = now.checked_sub(Duration::from_millis(50)).unwrap_or(now);
        next = next.max(floor) + Duration::from_micros(size * 1_000_000 / BYTES_PER_SECOND);
        if let Some(rem) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(rem);
        }
        release_out_buffer(buf);
    }
}

// ---- WAV dump & render submission --------------------------------------------------------------

fn write_wav(path: &str, pcm: &[i16], channels: i32) {
    let Ok(mut f) = File::create(path) else {
        return;
    };
    let ch = channels as u16;
    let bytes = (pcm.len() * 2) as u32;
    let byte_rate = 48_000_u32 * u32::from(ch) * 2;
    let block_align = ch * 2;
    let mut header = [0u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + bytes).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&ch.to_le_bytes());
    header[24..28].copy_from_slice(&48_000_u32.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&block_align.to_le_bytes());
    header[34..36].copy_from_slice(&16_u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&bytes.to_le_bytes());
    if f.write_all(&header).is_err() {
        return;
    }
    // SAFETY: `pcm` is a slice of initialized `i16` values.
    let pcm_bytes = unsafe { std::slice::from_raw_parts(pcm.as_ptr() as *const u8, pcm.len() * 2) };
    if f.write_all(pcm_bytes).is_err() {
        return;
    }
    host_log(&format!(
        "audio: wrote {} ({} frames)",
        path,
        pcm.len() / (channels as usize)
    ));
}

fn render_submit() {
    let device = device_open();
    static DUMP_PATH: OnceLock<Option<String>> = OnceLock::new();
    let dump_path = DUMP_PATH.get_or_init(|| std::env::var("NX_AUDIO_DUMP").ok());

    let mut o = render_out().lock().unwrap_or_else(|e| e.into_inner());
    if !o.mixed {
        return;
    }
    o.mixed = false;
    let chans = active_output_channels();
    let n = if chans == 6 {
        FRAME_SAMPLES * 6
    } else {
        FRAME_SAMPLES * 2
    };
    let mut pcm = [0i16; FRAME_SAMPLES * MAX_CHANNELS];
    let mut any = false;

    {
        let mut ring = out_mix().lock().unwrap_or_else(|e| e.into_inner());
        if o.last_mix_chans != chans {
            o.last_mix_chans = chans;
            ring.clear();
        }
        if chans == 6 {
            for s in 0..FRAME_SAMPLES {
                if ring.len() < 2 {
                    break;
                }
                let l = ring.pop_front().unwrap_or(0);
                let r = ring.pop_front().unwrap_or(0);
                o.mix[6 * s] += i32::from(l);
                o.mix[6 * s + 1] += i32::from(r);
            }
        } else {
            for dst in o.mix.iter_mut().take(FRAME_SAMPLES * 2) {
                let Some(sample) = ring.pop_front() else {
                    break;
                };
                *dst += i32::from(sample);
            }
        }
    }

    for (s, dst) in pcm[..n].iter_mut().enumerate() {
        let v = o.mix[s].clamp(-32_768, 32_767);
        *dst = v as i16;
        o.mix[s] = 0;
        o.peak = o.peak.max(v.abs());
        o.sum_sq += f64::from(v) * f64::from(v);
        any |= v != 0;
    }
    if !any {
        o.silent += 1;
    }

    if let Some(path) = dump_path {
        let target = 48_000 * (chans as usize) * 10;
        if o.dump.len() < target && (any || !o.dump.is_empty()) {
            o.dump.extend_from_slice(&pcm[..n]);
            if o.dump.len() >= target {
                write_wav(path, &o.dump, chans);
            }
        }
    }

    o.frames += 1;
    if o.frames.is_multiple_of(2000) {
        let rms = (o.sum_sq / (2000.0 * (n as f64))).sqrt();
        host_log(&format!(
            "audio: mix out: peak {}, rms {:.0}, {} of 2000 frames silent, {} dropped, {} underruns, queue {}",
            o.peak,
            rms,
            o.silent,
            o.dropped,
            o.underruns,
            if device { host_audio_queued() } else { 0 },
        ));
        o.peak = 0;
        o.sum_sq = 0.0;
        o.silent = 0;
        o.dropped = 0;
        o.underruns = 0;
    }

    if !device {
        return;
    }
    let queued = host_audio_queued();
    if queued as usize + o.held.len() >= MAX_QUEUED as usize || o.free.is_empty() {
        o.dropped += 1;
        return;
    }
    if queued == 0 && !o.priming {
        o.priming = true;
        o.underruns += 1;
    }
    let k = o.free.pop().unwrap();
    let block_samples = FRAME_SAMPLES * (chans as usize);
    let block_bytes = block_samples * std::mem::size_of::<i16>();
    o.blocks[k][..block_samples].copy_from_slice(&pcm[..block_samples]);
    o.held.push(k);
    if o.priming && o.held.len() < PRIME_BLOCKS {
        return;
    }
    o.priming = false;
    let held = std::mem::take(&mut o.held);
    for h in held {
        let ptr = o.blocks[h].as_ptr() as *const c_void;
        if !host_audio_submit(ptr, block_bytes, ptr as *mut c_void) {
            o.free.push(h);
            o.dropped += 1;
        }
    }
}

// ---- ARM64 instruction decoding & renderer discovery -------------------------------------------

/// Decode `ldr w0, [x0, #imm12*4]` or `ldp wT1, wT2, [x0, #imm7*4]` for `mode_off`.
pub fn decode_execution_mode_offset(
    get_exec_mode_word: Option<u32>,
    exec_word: Option<u32>,
) -> u64 {
    if let Some(w) = get_exec_mode_word {
        if (w & 0xFFC0_03FF) == 0xB940_0000 {
            return u64::from((w >> 10) & 0xFFF) * 4;
        }
    }
    if let Some(w) = exec_word {
        if (w & 0xFFC0_03E0) == 0x2940_0000 {
            let imm7 = ((w << 10) as i32) >> 25;
            return (i64::from(imm7) * 4) as u64;
        }
    }
    DEFAULT_MODE_OFF
}

/// Decode `bl` target in `GenerateDeviceSinkCommand` and `mov w8, #imm16; ...; strb w8, [x20, #5]`.
unsafe fn decode_device_sink_id(gen_device_sink_sym: u64) -> u8 {
    if gen_device_sink_sym == 0 {
        return CMD_DEVICE_SINK;
    }
    let mut gen = 0u64;
    let mut pc = gen_device_sink_sym;
    while pc < gen_device_sink_sym + 0x100 {
        let w = ld_u32(pc);
        if (w & 0xFC00_0000) == 0x9400_0000 {
            let off = i64::from(((w << 6) as i32) >> 4);
            gen = pc.wrapping_add(off as u64);
        }
        if w == 0xD65F_03C0 {
            break;
        }
        pc += 4;
    }
    if gen == 0 {
        return CMD_DEVICE_SINK;
    }
    let mut last = u32::MAX;
    let mut pc = gen;
    while pc < gen + 0x80 {
        let w = ld_u32(pc);
        if (w & 0xFFE0_001F) == 0x5280_0008 {
            last = (w >> 5) & 0xFFFF;
        }
        if w == 0x3900_1688 && last < 0x100 {
            host_log(&format!("audio: DeviceSink command id {last}"));
            return last as u8;
        }
        pc += 4;
    }
    CMD_DEVICE_SINK
}

fn find_renderer(handle: u64, vtable_addr: u64, min_addr: u64, max_addr: u64) -> u64 {
    let vptr = vtable_addr.wrapping_add(0x10);
    let valid = |p: u64| p >= min_addr && p < max_addr && (p & 7) == 0;
    if handle == 0 {
        return 0;
    }
    for a in (0..0x40).step_by(8) {
        // SAFETY: `handle` is a guest AudioRendererHandle pointer.
        let p = unsafe { ld_u64(handle + a) };
        if !valid(p) {
            continue;
        }
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        if unsafe { ld_u64(p) } == vptr {
            return p + 0x10;
        }
        for b in (0..0x40).step_by(8) {
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            let q = unsafe { ld_u64(p + b) };
            // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
            if valid(q) && unsafe { ld_u64(q) } == vptr {
                return q + 0x10;
            }
        }
    }
    0
}

fn mix_device_sink_into(
    mix: &mut [i32],
    inputs: u32,
    ch_ptrs: &[&[i32]],
    discrete: bool,
    downmixed: bool,
) {
    let l = ch_ptrs[0];
    let r = if inputs > 1 { ch_ptrs[1] } else { l };
    let mut c2 = None;
    let mut lfe = None;
    let mut rl = None;
    let mut rr = None;
    if discrete {
        if inputs > 2 {
            c2 = Some(ch_ptrs[2]);
        }
        if inputs > 3 {
            lfe = Some(ch_ptrs[3]);
        }
        if inputs > 4 {
            rl = Some(ch_ptrs[4]);
        }
        if inputs > 5 {
            rr = Some(ch_ptrs[5]);
        }
    } else if inputs >= 6 && !downmixed {
        c2 = Some(ch_ptrs[2]);
        rl = Some(ch_ptrs[4]);
        rr = Some(ch_ptrs[5]);
    }

    for s in 0..FRAME_SAMPLES {
        if discrete {
            mix[6 * s] += l[s];
            mix[6 * s + 1] += r[s];
            mix[6 * s + 2] += c2.map_or(0, |b| b[s]);
            mix[6 * s + 3] += lfe.map_or(0, |b| b[s]);
            mix[6 * s + 4] += rl.map_or(0, |b| b[s]);
            mix[6 * s + 5] += rr.map_or(0, |b| b[s]);
            continue;
        }
        let mut a = i64::from(l[s]);
        let mut b = i64::from(r[s]);
        if let (Some(c), Some(rl_b), Some(rr_b)) = (c2, rl, rr) {
            a += (i64::from(c[s]) + i64::from(rl_b[s])) * 707 / 1000;
            b += (i64::from(c[s]) + i64::from(rr_b[s])) * 707 / 1000;
        }
        mix[2 * s] += a.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        mix[2 * s + 1] += b.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    }
}

// ---- Public FFI --------------------------------------------------------------------------------

/// # Safety
/// `host` must be null or point to a valid `AcnhAudioHost`.
#[no_mangle]
pub unsafe extern "C" fn acnh_audio_set_host(host: *const AcnhAudioHost) {
    let mut slot = host_lock().lock().unwrap_or_else(|e| e.into_inner());
    if host.is_null() {
        *slot = AcnhAudioHost::default();
    } else {
        // SAFETY: caller guarantees `host` points to a valid `AcnhAudioHost`.
        *slot = unsafe { *host };
        crate::ffi::set_log(slot.log);
    }
}

#[no_mangle]
pub extern "C" fn acnh_audio_active_output_channel_count() -> u64 {
    active_output_channels() as u64
}

#[no_mangle]
pub extern "C" fn acnh_audio_list_output_device_name(names_ptr: u64, count: i32) -> i32 {
    if count < 1 || names_ptr == 0 {
        return 0;
    }
    // SAFETY: `names_ptr` points to a guest `AudioDeviceName` buffer of at least 256 bytes.
    unsafe {
        let dst = names_ptr as *mut u8;
        std::ptr::write_bytes(dst, 0, 256);
        std::ptr::copy_nonoverlapping(OUTPUT_NAME.as_ptr(), dst, OUTPUT_NAME.len());
    }
    1
}

#[no_mangle]
pub extern "C" fn acnh_audio_get_active_output_device_name(name_ptr: u64) -> i32 {
    if name_ptr != 0 {
        // SAFETY: `name_ptr` points to a guest `AudioDeviceName` buffer of at least 256 bytes.
        unsafe {
            let dst = name_ptr as *mut u8;
            std::ptr::write_bytes(dst, 0, 256);
            std::ptr::copy_nonoverlapping(OUTPUT_NAME.as_ptr(), dst, OUTPUT_NAME.len());
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_init_parameter(param_ptr: u64) {
    if param_ptr == 0 {
        return;
    }
    // SAFETY: `param_ptr` points to `AudioOutParameter { u32 sample_rate; u16 channels; u16 reserved }`.
    unsafe {
        st_u32(param_ptr, 0);
        st_u16(param_ptr + 4, 0);
        st_u16(param_ptr + 6, 0);
    }
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_open(out_ptr: u64, param_ptr: u64) -> u64 {
    let (asked_hz, asked_ch) = if param_ptr != 0 {
        // SAFETY: `param_ptr` points to `AudioOutParameter`.
        unsafe { (ld_u32(param_ptr), ld_u16(param_ptr + 4)) }
    } else {
        (0, 0)
    };
    host_log(&format!(
        "audio: AudioOut opened ({asked_hz} Hz, {asked_ch} channels asked; {} output channels)",
        active_output_channels()
    ));
    let sync = out_sync();
    {
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        st.queued.clear();
        st.released.clear();
        if !st.player_started {
            st.player_started = true;
            std::thread::spawn(player_loop);
        }
    }
    if out_ptr != 0 {
        // SAFETY: `out_ptr` points to guest `AudioOut` opaque storage.
        unsafe { st_u64(out_ptr, AUDIO_OUT_OPAQUE_HANDLE) };
    }
    0
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_start() -> u64 {
    let sync = out_sync();
    {
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        st.started = true;
        sync.cv.notify_all();
    }
    0
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_stop() {
    host_log("audio: AudioOut stopped");
    let sync = out_sync();
    {
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        st.started = false;
        sync.cv.notify_all();
    }
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_close() {
    host_log("audio: AudioOut closed");
    let sync = out_sync();
    {
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        st.started = false;
        st.queued.clear();
        st.released.clear();
        sync.cv.notify_all();
    }
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_sample_rate() -> u64 {
    48_000
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_channel_count() -> u64 {
    active_output_channels() as u64
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_sample_format() -> u64 {
    2
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_set_volume(vol: f32) {
    OUT_VOLUME_BITS.store(vol.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_buffer_data_size(buf_ptr: u64) -> u64 {
    if buf_ptr == 0 {
        return 0;
    }
    // SAFETY: `buf_ptr` points to guest `AudioOutBuffer`.
    unsafe { ld_u64(buf_ptr + 16) }
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_set_buffer_info(
    buf_ptr: u64,
    data: u64,
    capacity: u64,
    size: u64,
) {
    if buf_ptr == 0 {
        return;
    }
    // SAFETY: `buf_ptr` points to guest `AudioOutBuffer`.
    unsafe {
        st_u64(buf_ptr, data);
        st_u64(buf_ptr + 8, capacity);
        st_u64(buf_ptr + 16, size);
    }
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_buffer_data_pointer(buf_ptr: u64) -> u64 {
    if buf_ptr == 0 {
        return 0;
    }
    // SAFETY: `buf_ptr` points to guest `AudioOutBuffer`.
    unsafe { ld_u64(buf_ptr) }
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_append_buffer(buf_ptr: u64) -> u64 {
    let idx = OUT_APPENDED.fetch_add(1, Ordering::Relaxed);
    if idx.is_multiple_of(500) && buf_ptr != 0 {
        // SAFETY: `buf_ptr` points to guest `AudioOutBuffer`.
        let size = unsafe { ld_u64(buf_ptr + 16) };
        host_log(&format!(
            "audio: AudioOut buffer {} appended ({size} bytes)",
            idx + 1
        ));
    }
    let sync = out_sync();
    {
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        st.queued.push_back(buf_ptr);
        sync.cv.notify_all();
    }
    1
}

#[no_mangle]
pub extern "C" fn acnh_audio_out_get_released_buffer() -> u64 {
    let sync = out_sync();
    let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
    OUT_POLLS.fetch_add(1, Ordering::Relaxed);
    if st.released.is_empty() && st.started && !st.queued.is_empty() {
        let (guard, _) = sync
            .cv
            .wait_timeout_while(st, Duration::from_millis(2), |s| {
                s.released.is_empty() && s.started
            })
            .unwrap_or_else(|e| e.into_inner());
        st = guard;
    }
    let Some(buf) = st.released.pop_front() else {
        return 0;
    };
    OUT_TAKEN.fetch_add(1, Ordering::Relaxed);
    if st.released.is_empty() {
        host_clear_out_event();
    }
    buf
}

#[no_mangle]
pub extern "C" fn acnh_audio_list_audio_ins() -> u64 {
    0
}

#[no_mangle]
pub extern "C" fn acnh_audio_open_audio_in() -> u64 {
    AUDIO_IN_NOT_FOUND_ERR
}

#[no_mangle]
pub extern "C" fn acnh_audio_renderer_opened(
    rc: u64,
    handle: u64,
    vtable_sym: u64,
    sdk_base: u64,
    get_exec_mode_sym: u64,
    sys_exec_sym: u64,
    impl_exec_sym: u64,
    min_addr: u64,
    max_addr: u64,
) -> bool {
    let exec_addr = if impl_exec_sym != 0 {
        impl_exec_sym
    } else {
        sdk_base.wrapping_add(EXECUTE_RENDERING_OFF)
    };
    EXECUTE_RENDERING_ADDR.store(exec_addr, Ordering::Relaxed);

    let vtable_addr = if vtable_sym != 0 {
        vtable_sym
    } else {
        sdk_base.wrapping_add(RENDERER_VTABLE_OFF)
    };
    let impl_addr = if rc == 0 {
        find_renderer(handle, vtable_addr, min_addr, max_addr)
    } else {
        0
    };
    host_log(&format!(
        "audio: renderer opened (result {rc:x}), impl {impl_addr:x}"
    ));
    if impl_addr == 0 {
        return false;
    }

    let system = impl_addr + 0x60;
    let get_exec_word = if get_exec_mode_sym != 0 {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        Some(unsafe { ld_u32(get_exec_mode_sym) })
    } else {
        None
    };
    let sys_exec_word = if sys_exec_sym != 0 {
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        Some(unsafe { ld_u32(sys_exec_sym) })
    } else {
        None
    };
    let mode_off = decode_execution_mode_offset(get_exec_word, sys_exec_word);
    // SAFETY: `system + mode_off` points to `AudioRenderSystem` execution mode & rendering device.
    unsafe {
        let old_mode = ld_u32(system + mode_off);
        let old_dev = ld_u32(system + mode_off + 4);
        host_log(&format!(
            "audio: execution mode {old_mode}, rendering device {old_dev} (render system +{mode_off:#x}) -> 1, 1"
        ));
        st_u32(system + mode_off, 1);
        st_u32(system + mode_off + 4, 1);
    }

    {
        let mut r = renderers_mu().lock().unwrap_or_else(|e| e.into_inner());
        r.push(impl_addr);
    }
    !RENDER_THREAD_STARTED.swap(true, Ordering::SeqCst)
}

#[no_mangle]
pub extern "C" fn acnh_audio_execute_rendering_addr() -> u64 {
    EXECUTE_RENDERING_ADDR.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn acnh_audio_capture_device_sink(proc_addr: u64, gen_device_sink_sym: u64) {
    if proc_addr == 0 {
        return;
    }
    // SAFETY: `gen_device_sink_sym` is the address of the guest's device-sink generator, mapped for the life of the process.
    let sink_id =
        *DEVICE_SINK_ID.get_or_init(|| unsafe { decode_device_sink_id(gen_device_sink_sym) });
    // SAFETY: `proc_addr` is `nn::audio::dsp::CommandListProcessor*`.
    let (mut cmd, count, samples, rate) = unsafe {
        (
            ld_u64(proc_addr) + 0x20,
            ld_u32(proc_addr + 0x1c),
            ld_u32(proc_addr + 0x20),
            ld_u32(proc_addr + 0x28),
        )
    };
    let frames = if rate == 48_000 {
        samples
    } else {
        FRAME_SAMPLES as u32
    };
    let mut downmixed = false;
    static ONCE: AtomicBool = AtomicBool::new(false);

    for _ in 0..count {
        // SAFETY: `cmd` walks valid command headers in the command list.
        let magic = unsafe { ld_u32(cmd) };
        if magic != 0xcafe_babe {
            break;
        }
        // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
        let stride = unsafe { ld_i16(cmd + 6) };
        // SAFETY: `cmd` is a valid command header, so the byte at offset 4 is inside it.
        let enabled = unsafe { *(cmd as *const u8).add(4) };
        // SAFETY: `cmd` is a valid command header, so the byte at offset 5 is inside it.
        let id = unsafe { *(cmd as *const u8).add(5) };

        if enabled != 0 {
            if id == sink_id.wrapping_sub(CMD_DEVICE_SINK - CMD_DOWNMIX_6CH_TO_2CH) {
                downmixed = true;
            }
            if id == sink_id {
                // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
                let buffers = unsafe { ld_u64(cmd + 0x128) };
                // SAFETY: guest addresses come from the recompiled code; the guest address space is one identity-mapped region that stays mapped for the life of the process, and the access is atomic or unaligned-safe.
                let inputs = unsafe { ld_u32(cmd + 0x130) };
                if !ONCE.swap(true, Ordering::Relaxed) {
                    // SAFETY: the buffer is NUL-terminated: it is a fixed-size array we zero-initialised and the host callback filled, or a C string the caller passed.
                    let name = unsafe {
                        std::ffi::CStr::from_ptr((cmd + 0x20) as *const c_char).to_string_lossy()
                    };
                    host_log(&format!(
                        "audio: device sink '{name}', {inputs} inputs{}, {samples} samples at {rate} Hz",
                        if downmixed { " (downmixed to stereo)" } else { "" }
                    ));
                }
                if buffers != 0 && inputs != 0 && frames == FRAME_SAMPLES as u32 {
                    let n_ch = (inputs as usize).min(MAX_CHANNELS);
                    let mut ch_ptrs: Vec<&[i32]> = Vec::with_capacity(n_ch);
                    for k in 0..n_ch {
                        // SAFETY: `cmd + 0x134 + 2*k` holds `i16` buffer index.
                        let buf_idx = unsafe { ld_i16(cmd + 0x134 + 2 * (k as u64)) };
                        let ptr =
                            (buffers + (buf_idx as u64) * u64::from(frames) * 4) as *const i32;
                        // SAFETY: the range is guest or caller memory of the length passed, mapped for the life of the process; the C++ dispatcher is stopped while this call runs, so nothing else writes it.
                        ch_ptrs.push(unsafe { std::slice::from_raw_parts(ptr, FRAME_SAMPLES) });
                    }
                    let discrete = active_output_channels() == 6;
                    let mut ro = render_out().lock().unwrap_or_else(|e| e.into_inner());
                    mix_device_sink_into(&mut ro.mix, inputs, &ch_ptrs, discrete, downmixed);
                    ro.mixed = true;
                }
            }
        }
        cmd = (cmd as i64 + i64::from(stride)) as u64;
    }
}

#[no_mangle]
pub extern "C" fn acnh_audio_before_render_system_update() {
    let sync = pace_sync();
    {
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        if st.renders == st.renders_at_update && st.waited_logs < 5 {
            st.waited_logs += 1;
            host_log("audio: update waits for a render");
        }
        if st.renders == st.renders_at_update {
            let (guard, res) = sync
                .cv
                .wait_timeout_while(st, Duration::from_secs(1), |s| {
                    s.renders == s.renders_at_update
                })
                .unwrap_or_else(|e| e.into_inner());
            st = guard;
            if res.timed_out() && st.renders == st.renders_at_update {
                host_log("audio: no render for 1 s since the last update: updating anyway");
            }
        }
    }
    RENDER_SYSTEM_LOCK.lock();
}

#[no_mangle]
pub extern "C" fn acnh_audio_after_render_system_update() {
    {
        let sync = pace_sync();
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        st.renders_at_update = st.renders;
    }
    RENDER_SYSTEM_LOCK.unlock();
}

#[no_mangle]
pub extern "C" fn acnh_audio_before_execute_rendering() {
    RENDER_SYSTEM_LOCK.lock();
}

#[no_mangle]
pub extern "C" fn acnh_audio_after_execute_rendering() {
    {
        let sync = pace_sync();
        let mut st = sync.mu.lock().unwrap_or_else(|e| e.into_inner());
        st.renders += 1;
        sync.cv.notify_all();
    }
    RENDER_SYSTEM_LOCK.unlock();
}

static RENDER_NEXT_NS: AtomicU64 = AtomicU64::new(0);
static RENDER_FRAMES: AtomicU64 = AtomicU64::new(0);
static RENDER_BUSY_NS: AtomicU64 = AtomicU64::new(0);

/// # Safety
/// `out_impls` must be null or writable for `max_impls` `u64`s, and `out_t0` must be null or writable.
#[no_mangle]
pub unsafe extern "C" fn acnh_audio_render_tick_begin(
    out_impls: *mut u64,
    max_impls: usize,
    out_t0: *mut u64,
) -> usize {
    let mut next = RENDER_NEXT_NS.load(Ordering::Relaxed);
    if next == 0 {
        host_log("audio: render thread running");
        next = host_monotonic_ns();
    }
    next = next.wrapping_add(5_000_000);
    RENDER_NEXT_NS.store(next, Ordering::Relaxed);

    let wait = (next as i64).wrapping_sub(host_monotonic_ns() as i64);
    if wait > 0 {
        host_sleep_ns(wait);
    }

    if !out_t0.is_null() {
        // SAFETY: caller provides valid `out_t0` pointer.
        unsafe { *out_t0 = host_monotonic_ns() };
    }

    let list = renderers_mu().lock().unwrap_or_else(|e| e.into_inner());
    let count = list.len().min(max_impls);
    if !out_impls.is_null() && count > 0 {
        // SAFETY: caller provides `out_impls` buffer of at least `max_impls` entries.
        unsafe {
            std::ptr::copy_nonoverlapping(list.as_ptr(), out_impls, count);
        }
    }
    count
}

#[no_mangle]
pub extern "C" fn acnh_audio_render_tick_end(t0: u64, last_rc: u64) {
    let elapsed = host_monotonic_ns().saturating_sub(t0);
    let busy = RENDER_BUSY_NS.fetch_add(elapsed, Ordering::Relaxed) + elapsed;
    render_submit();
    let frames = RENDER_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if frames.is_multiple_of(2000) {
        RENDER_BUSY_NS.store(0, Ordering::Relaxed);
        host_log(&format!(
            "audio: {frames} frames rendered, {:.1} us each, last result {last_rc:x}",
            (busy as f64) / 1000.0 / 2000.0
        ));
    }
    host_signal_frame_event();
}

static REVERB_ENV_OFF: OnceLock<bool> = OnceLock::new();
static REVERB_CFG_GEN: AtomicU64 = AtomicU64::new(0);
static REVERB_CFG_OFF: AtomicBool = AtomicBool::new(false);
static REVERB_PROF_ON: OnceLock<bool> = OnceLock::new();
static REVERB_PROF_SEEN: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
pub extern "C" fn acnh_audio_reverb_should_bypass() -> bool {
    let env_off = *REVERB_ENV_OFF.get_or_init(|| {
        let off = std::env::var("NX_AUDIO_REVERB").is_ok_and(|v| v.starts_with('0'));
        if off {
            host_log("audio: I3DL2 reverb bypassed (NX_AUDIO_REVERB=0)");
        }
        off
    });
    let gen = config::config_generation();
    if gen != REVERB_CFG_GEN.load(Ordering::Relaxed) {
        REVERB_CFG_GEN.store(gen, Ordering::Relaxed);
        REVERB_CFG_OFF.store(
            !config::config_get_bool("audio.reverb", true),
            Ordering::Relaxed,
        );
    }
    env_off || REVERB_CFG_OFF.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn acnh_audio_reverb_should_benchmark(off: bool, enabled_arg: u64) -> bool {
    let prof = *REVERB_PROF_ON.get_or_init(|| std::env::var_os("NX_PROFILE_REVERB").is_some());
    if !prof || off || enabled_arg == 0 {
        return false;
    }
    REVERB_PROF_SEEN.fetch_add(1, Ordering::Relaxed) + 1 == 500
}

#[no_mangle]
pub extern "C" fn acnh_audio_reverb_log_benchmark(median_ns: u64, best_ns: u64) {
    host_log(&format!(
        "audio: reverb benchmark: median {median_ns} ns, best {best_ns} ns per call"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_ring_drops_oldest_when_full() {
        let mut ring = AudioMixRing::new();
        for i in 0..(MIX_RING_CAPACITY + 5) {
            ring.push((i % 1000) as i16);
        }
        assert_eq!(ring.len(), MIX_RING_CAPACITY);
        assert_eq!(ring.pop_front(), Some(5));
        assert_eq!(ring.len(), MIX_RING_CAPACITY - 1);
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.pop_front(), None);
    }

    #[test]
    fn upmix_stereo_to_6ch_layout() {
        let src = [100_i16, -200_i16, 300_i16, 100_i16];
        let mut dst = Vec::new();
        upmix_stereo_to_6ch(&src, &mut dst);
        assert_eq!(dst.len(), 12);
        // Frame 0: FL=100, FR=-200, FC=-50, LFE=0, RL=100, RR=-200
        assert_eq!(&dst[0..6], &[100, -200, -50, 0, 100, -200]);
        // Frame 1: FL=300, FR=100, FC=200, LFE=0, RL=300, RR=100
        assert_eq!(&dst[6..12], &[300, 100, 200, 0, 300, 100]);
    }

    #[test]
    fn decode_execution_mode_offset_handles_ldr_ldp_and_fallback() {
        // ldr w0, [x0, #0x2b8] -> imm12 = 0x2b8 / 4 = 0xae -> 0xB9400000 | (0xae << 10)
        let ldr = 0xB940_0000 | (0xAE << 10);
        assert_eq!(decode_execution_mode_offset(Some(ldr), None), 0x2b8);

        // ldp w8, w9, [x0, #0x48] -> imm7 = 0x48 / 4 = 0x12 -> 0x29400000 | (0x12 << 15) | (9 << 10) | 8
        let ldp = 0x2940_0000 | (0x12 << 15) | (9 << 10) | 8;
        assert_eq!(decode_execution_mode_offset(None, Some(ldp)), 0x48);

        // Fallback when neither matches
        assert_eq!(
            decode_execution_mode_offset(Some(0xD503_201F), None),
            DEFAULT_MODE_OFF
        );
        assert_eq!(decode_execution_mode_offset(None, None), DEFAULT_MODE_OFF);
    }

    #[test]
    fn decode_device_sink_id_from_synthetic_arm64() {
        // Build a synthetic GenerateDeviceSinkCommand that `bl`s into a generator with `mov w8, #20; strb w8, [x20, #5]`
        let mut code = [0u32; 16];
        // code[0]: bl +4 instructions (offset +16 bytes -> code[4])
        code[0] = 0x9400_0004;
        code[1] = 0xD65F_03C0; // ret
                               // code[4]: mov w8, #20
        code[4] = 0x5280_0008 | (20 << 5);
        // code[5]: strb w8, [x20, #5]
        code[5] = 0x3900_1688;
        // SAFETY: test-only: calls the crate's own exported function with pointers to locals that outlive the call, with the lengths we pass.
        let id = unsafe { decode_device_sink_id(code.as_ptr() as u64) };
        assert_eq!(id, 20);
    }

    #[test]
    fn mix_device_sink_folds_surround_to_stereo_and_keeps_discrete() {
        let ch0 = vec![1000_i32; FRAME_SAMPLES];
        let ch1 = vec![500_i32; FRAME_SAMPLES];
        let ch2 = vec![1000_i32; FRAME_SAMPLES];
        let ch3 = vec![200_i32; FRAME_SAMPLES];
        let ch4 = vec![1000_i32; FRAME_SAMPLES];
        let ch5 = vec![1000_i32; FRAME_SAMPLES];
        let ptrs: Vec<&[i32]> = vec![&ch0, &ch1, &ch2, &ch3, &ch4, &ch5];

        let mut stereo_mix = vec![0_i32; FRAME_SAMPLES * MAX_CHANNELS];
        mix_device_sink_into(&mut stereo_mix, 6, &ptrs, false, false);
        // L = 1000 + (1000 + 1000) * 707 / 1000 = 2414
        // R = 500 + (1000 + 1000) * 707 / 1000 = 1914
        assert_eq!(stereo_mix[0], 2414);
        assert_eq!(stereo_mix[1], 1914);

        let mut discrete_mix = vec![0_i32; FRAME_SAMPLES * MAX_CHANNELS];
        mix_device_sink_into(&mut discrete_mix, 6, &ptrs, true, false);
        assert_eq!(&discrete_mix[0..6], &[1000, 500, 1000, 200, 1000, 1000]);
    }

    #[test]
    fn audio_out_constants_and_buffer_info() {
        assert_eq!(acnh_audio_out_sample_rate(), 48_000);
        assert_eq!(acnh_audio_out_sample_format(), 2);
        assert_eq!(acnh_audio_list_audio_ins(), 0);
        assert_eq!(acnh_audio_open_audio_in(), AUDIO_IN_NOT_FOUND_ERR);

        let mut param = [1u8; 8];
        acnh_audio_out_init_parameter(param.as_mut_ptr() as u64);
        assert_eq!(param, [0u8; 8]);

        let mut buf = [0u64; 3];
        acnh_audio_out_set_buffer_info(buf.as_mut_ptr() as u64, 0x1234, 4096, 2048);
        assert_eq!(
            acnh_audio_out_buffer_data_pointer(buf.as_ptr() as u64),
            0x1234
        );
        assert_eq!(acnh_audio_out_buffer_data_size(buf.as_ptr() as u64), 2048);

        let mut dev_name = [0xAA_u8; 256];
        assert_eq!(
            acnh_audio_list_output_device_name(dev_name.as_mut_ptr() as u64, 1),
            1
        );
        assert!(dev_name.starts_with(OUTPUT_NAME));
        assert_eq!(
            acnh_audio_list_output_device_name(dev_name.as_mut_ptr() as u64, 0),
            0
        );
    }
}
