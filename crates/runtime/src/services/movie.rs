//! Split from the old `services.rs` (issue #356).

#[allow(unused_imports)]
use super::*;

// ---- 10. Movie Decoder (hle_movie.cpp) ------------------------------------------------------------------------------

pub(crate) const MOVIE_INPUTS: usize = 8;
pub(crate) const MOVIE_VIDEO_OUTPUTS: usize = 4;
pub(crate) const MOVIE_AUDIO_OUTPUTS: usize = 8;

pub(crate) struct MovieFrame {
    pub(crate) data: Vec<u8>,
    pub(crate) pts: i64,
    pub(crate) flags: u32,
}

pub(crate) struct MovieDecoder {
    pub(crate) codec_type: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) channels: i32,
    pub(crate) sample_rate: i32,
    pub(crate) started: bool,
    pub(crate) eos_sent: bool,
    pub(crate) in_event: u32,
    pub(crate) out_event: u32,
    pub(crate) err_event: u32,
    pub(crate) free_inputs: VecDeque<i32>,
    pub(crate) input_buffers: BTreeMap<i32, Vec<u8>>,
    pub(crate) free_outputs: VecDeque<i32>,
    pub(crate) active_outputs: BTreeMap<i32, MovieFrame>,
    pub(crate) decoded: VecDeque<MovieFrame>,
    pub(crate) current_frame: MovieFrame,
    pub(crate) play_start_ns: u64,
    pub(crate) first_pts: i64,
    pub(crate) codec_ctx: usize, // *mut c_void cast to usize for Send
}

impl MovieDecoder {
    pub(crate) fn is_audio(&self) -> bool {
        self.codec_type == MOVIE_CODEC_VORBIS
    }
    pub(crate) fn max_outputs(&self) -> usize {
        if self.is_audio() {
            MOVIE_AUDIO_OUTPUTS
        } else {
            MOVIE_VIDEO_OUTPUTS
        }
    }
}

pub(crate) static MOVIE_DECODERS: Mutex<BTreeMap<u64, MovieDecoder>> = Mutex::new(BTreeMap::new());

#[no_mangle]
pub extern "C" fn acnh_movie_create(self_addr: u64, codec_type: i32) {
    let dec = MovieDecoder {
        codec_type,
        width: 1920,
        height: 1080,
        channels: 2,
        sample_rate: 48000,
        started: false,
        eos_sent: false,
        in_event: 0,
        out_event: 0,
        err_event: 0,
        free_inputs: VecDeque::new(),
        input_buffers: BTreeMap::new(),
        free_outputs: VecDeque::new(),
        active_outputs: BTreeMap::new(),
        decoded: VecDeque::new(),
        current_frame: MovieFrame {
            data: Vec::new(),
            pts: 0,
            flags: 0,
        },
        play_start_ns: 0,
        first_pts: 0,
        codec_ctx: 0,
    };
    MOVIE_DECODERS.lock().unwrap().insert(self_addr, dec);
}

#[no_mangle]
pub extern "C" fn acnh_movie_destroy(self_addr: u64) {
    if let Some(d) = MOVIE_DECODERS.lock().unwrap().remove(&self_addr) {
        if d.codec_ctx != 0 {
            if let Some(Some(f)) = with_host(|h| h.movie_codec_close) {
                // SAFETY: f is a host callback installed through acnh_services_set_host; the C++ host keeps it valid for the process lifetime, and every argument is a plain value or a live local buffer sized as passed.
                unsafe { f(d.codec_ctx as *mut c_void, d.codec_type) };
            }
        }
    }
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_configure(
    self_addr: u64,
    width: i32,
    height: i32,
    channels: i32,
    sample_rate: i32,
    extra: *const u8,
    extra_size: usize,
) -> u32 {
    let codec_type = {
        let g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        d.codec_type
    };
    let ctx = with_host(|h| {
        h.movie_codec_open
            .map(|f| f(codec_type, channels.max(2), extra, extra_size))
    })
    .flatten()
    .unwrap_or(std::ptr::null_mut());

    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if width > 0 {
        d.width = width;
    }
    if height > 0 {
        d.height = height;
    }
    if channels > 0 {
        d.channels = channels;
    }
    if sample_rate > 0 {
        d.sample_rate = sample_rate;
    }
    d.codec_ctx = ctx as usize;
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_register_events(
    self_addr: u64,
    in_ev: u32,
    out_ev: u32,
    err_ev: u32,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    d.in_event = in_ev;
    d.out_event = out_ev;
    d.err_event = err_ev;
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_start(self_addr: u64) -> u32 {
    let in_ev = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        d.started = true;
        d.eos_sent = false;
        d.free_inputs = (0..MOVIE_INPUTS as i32).collect();
        let max_out = d.max_outputs() as i32;
        d.free_outputs = (0..max_out).collect();
        d.in_event
    };
    host_event_signal(in_ev, true);
    host_notify_all();
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_stop(self_addr: u64) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    d.started = false;
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_flush(self_addr: u64) -> u32 {
    let in_ev = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        d.decoded.clear();
        d.active_outputs.clear();
        d.free_inputs = (0..MOVIE_INPUTS as i32).collect();
        let max_out = d.max_outputs() as i32;
        d.free_outputs = (0..max_out).collect();
        d.eos_sent = false;
        d.play_start_ns = 0;
        if d.started {
            d.in_event
        } else {
            0
        }
    };
    if in_ev != 0 {
        host_event_signal(in_ev, true);
        host_notify_all();
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_acquire_input(
    self_addr: u64,
    out_idx: *mut i32,
    out_rem: *mut i32,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    let Some(idx) = d.free_inputs.pop_front() else {
        return MOVIE_ERR_NO_BUFFER;
    };
    d.input_buffers.entry(idx).or_default().clear();
    if !out_idx.is_null() {
        *out_idx = idx;
    }
    if !out_rem.is_null() {
        *out_rem = d.free_inputs.len() as i32;
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_send_input(
    self_addr: u64,
    idx: i32,
    data: *const u8,
    size: usize,
    pts: i64,
    flags: u32,
) -> u32 {
    let (codec_type, codec_ctx, is_audio) = {
        let g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        (d.codec_type, d.codec_ctx, d.is_audio())
    };
    let mut out_bytes = Vec::new();
    let mut out_w = 0i32;
    let mut out_h = 0i32;
    let mut out_ch = 0i32;
    let mut out_sr = 0i32;

    if (flags & MOVIE_FLAG_EOS) == 0 && size > 0 && !data.is_null() && codec_ctx != 0 {
        if is_audio {
            out_bytes.resize(0x8000, 0);
            let mut written = out_bytes.len();
            let ok = with_host(|h| {
                h.movie_codec_decode_audio
                    .map(|f| {
                        f(
                            codec_ctx as *mut c_void,
                            data,
                            size,
                            out_bytes.as_mut_ptr(),
                            &mut written,
                            &mut out_ch,
                            &mut out_sr,
                        )
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
            if ok {
                out_bytes.truncate(written);
            } else {
                out_bytes.clear();
            }
        } else {
            out_bytes.resize(1920 * 1080 * 3 / 2, 0);
            let mut written = out_bytes.len();
            let ok = with_host(|h| {
                h.movie_codec_decode_video
                    .map(|f| {
                        f(
                            codec_ctx as *mut c_void,
                            codec_type,
                            data,
                            size,
                            out_bytes.as_mut_ptr(),
                            &mut written,
                            &mut out_w,
                            &mut out_h,
                        )
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
            if ok {
                out_bytes.truncate(written);
            } else {
                out_bytes.clear();
            }
        }
    }

    let (in_ev, out_ev) = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        if out_w > 0 {
            d.width = out_w;
        }
        if out_h > 0 {
            d.height = out_h;
        }
        if out_ch > 0 {
            d.channels = out_ch;
        }
        if out_sr > 0 {
            d.sample_rate = out_sr;
        }
        if (flags & MOVIE_FLAG_EOS) != 0 {
            d.decoded.push_back(MovieFrame {
                data: Vec::new(),
                pts,
                flags: MOVIE_FLAG_EOS,
            });
        } else if !out_bytes.is_empty() {
            d.decoded.push_back(MovieFrame {
                data: out_bytes,
                pts,
                flags: 0,
            });
        }
        d.input_buffers.remove(&idx);
        d.free_inputs.push_back(idx);
        let sig_in = if d.started { d.in_event } else { 0 };
        let sig_out = if d.started && !d.decoded.is_empty() && !d.free_outputs.is_empty() {
            d.out_event
        } else {
            0
        };
        (sig_in, sig_out)
    };
    if in_ev != 0 {
        host_event_signal(in_ev, true);
    }
    if out_ev != 0 {
        host_event_signal(out_ev, true);
    }
    if in_ev != 0 || out_ev != 0 {
        host_notify_all();
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_acquire_output(
    self_addr: u64,
    out_idx: *mut i32,
    out_pts: *mut i64,
    out_flags: *mut u32,
    out_rem: *mut i32,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if d.eos_sent || d.free_outputs.is_empty() {
        return MOVIE_ERR_NO_OUTPUT_BUFFER;
    }
    if d.decoded.is_empty() && (d.is_audio() || d.current_frame.data.is_empty()) {
        return MOVIE_ERR_NO_OUTPUT_BUFFER;
    }
    let idx = d.free_outputs.pop_front().unwrap();
    let (pts, flags) =
        if !d.decoded.is_empty() && (d.decoded.front().unwrap().flags & MOVIE_FLAG_EOS) != 0 {
            if !d.is_audio() || d.active_outputs.is_empty() {
                let f = d.decoded.pop_front().unwrap();
                d.eos_sent = true;
                d.active_outputs.insert(
                    idx,
                    MovieFrame {
                        data: Vec::new(),
                        pts: f.pts,
                        flags: MOVIE_FLAG_EOS,
                    },
                );
                (f.pts, MOVIE_FLAG_EOS)
            } else {
                d.free_outputs.push_front(idx);
                return MOVIE_ERR_NO_OUTPUT_BUFFER;
            }
        } else if d.is_audio() {
            let f = d.decoded.pop_front().unwrap();
            let p = f.pts;
            d.active_outputs.insert(idx, f);
            (p, 0)
        } else {
            let p = d
                .decoded
                .front()
                .map(|f| f.pts)
                .unwrap_or(d.current_frame.pts);
            d.active_outputs.insert(
                idx,
                MovieFrame {
                    data: Vec::new(),
                    pts: p,
                    flags: 0,
                },
            );
            (p, 0)
        };
    if !out_idx.is_null() {
        *out_idx = idx;
    }
    if !out_pts.is_null() {
        *out_pts = pts;
    }
    if !out_flags.is_null() {
        *out_flags = flags;
    }
    if !out_rem.is_null() {
        *out_rem = d.free_outputs.len() as i32;
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_copy_output(
    self_addr: u64,
    idx: i32,
    dst: u64,
    cap: usize,
    out_written: *mut usize,
) -> u32 {
    let mut g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get_mut(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if !d.active_outputs.contains_key(&idx) {
        return MOVIE_ERR_INVALID;
    }
    let is_audio = d.is_audio();
    let n = if is_audio {
        let f = d.active_outputs.get(&idx).unwrap();
        let n = if cap > 0 {
            f.data.len().min(cap)
        } else {
            f.data.len()
        };
        if n > 0 && dst != 0 {
            std::ptr::copy_nonoverlapping(f.data.as_ptr(), dst as *mut u8, n);
        }
        n
    } else {
        let now = host_monotonic_ns();
        if d.play_start_ns == 0 {
            d.play_start_ns = now;
            if let Some(front) = d.decoded.front() {
                if (front.flags & MOVIE_FLAG_EOS) == 0 {
                    let f = d.decoded.pop_front().unwrap();
                    d.first_pts = f.pts;
                    d.current_frame = f;
                } else {
                    d.first_pts = d.current_frame.pts;
                }
            } else {
                d.first_pts = d.current_frame.pts;
            }
        } else {
            let elapsed_us = d.first_pts + ((now - d.play_start_ns) / 1000) as i64;
            while let Some(front) = d.decoded.front() {
                if (front.flags & MOVIE_FLAG_EOS) != 0 || front.pts > elapsed_us {
                    break;
                }
                d.current_frame = d.decoded.pop_front().unwrap();
            }
        }
        let f = &d.current_frame;
        let n = if cap > 0 {
            f.data.len().min(cap)
        } else {
            f.data.len()
        };
        if n > 0 && dst != 0 {
            std::ptr::copy_nonoverlapping(f.data.as_ptr(), dst as *mut u8, n);
        }
        n
    };
    if !out_written.is_null() {
        *out_written = n;
    }
    if n > 0 && dst != 0 && !is_audio {
        if let Some(Some(f)) = with_host(|h| h.nvn_textures_written) {
            f(dst, n);
        }
    }
    MOVIE_OK
}

#[no_mangle]
pub extern "C" fn acnh_movie_release_output(self_addr: u64, idx: i32) -> u32 {
    let out_ev = {
        let mut g = MOVIE_DECODERS.lock().unwrap();
        let Some(d) = g.get_mut(&self_addr) else {
            return MOVIE_ERR_INVALID;
        };
        if d.active_outputs.remove(&idx).is_some() {
            d.free_outputs.push_back(idx);
        }
        if !d.started {
            return MOVIE_OK;
        }
        let avail = if d.is_audio() {
            !d.eos_sent && !d.decoded.is_empty() && !d.free_outputs.is_empty()
        } else {
            !d.eos_sent
                && (!d.decoded.is_empty() || !d.current_frame.data.is_empty())
                && !d.free_outputs.is_empty()
        };
        if avail {
            d.out_event
        } else {
            0
        }
    };
    if out_ev != 0 {
        host_event_signal(out_ev, true);
        host_notify_all();
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_get_output_buffer_size(
    self_addr: u64,
    out_size: *mut i32,
    out_count: *mut i32,
) -> u32 {
    let g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    let w = if d.width > 0 { d.width } else { 1920 };
    let h = if d.height > 0 { d.height } else { 1080 };
    let sz = if d.is_audio() {
        0x2000 * d.channels.max(1)
    } else {
        w * h * 3 / 2
    };
    if !out_size.is_null() {
        *out_size = sz;
    }
    if !out_count.is_null() {
        *out_count = d.max_outputs() as i32;
    }
    MOVIE_OK
}

#[no_mangle]
///
/// # Safety
/// Called only by the C++ runtime. Every pointer argument must be null (where the function checks) or valid
/// for the reads and writes the function makes, and every `u64` address must lie in mapped guest memory.
pub unsafe extern "C" fn acnh_movie_get_output_format(
    self_addr: u64,
    out_is_audio: *mut bool,
    out_w: *mut i32,
    out_h: *mut i32,
    out_ch: *mut i32,
    out_sr: *mut i32,
) -> u32 {
    let g = MOVIE_DECODERS.lock().unwrap();
    let Some(d) = g.get(&self_addr) else {
        return MOVIE_ERR_INVALID;
    };
    if !out_is_audio.is_null() {
        *out_is_audio = d.is_audio();
    }
    if !out_w.is_null() {
        *out_w = d.width;
    }
    if !out_h.is_null() {
        *out_h = d.height;
    }
    if !out_ch.is_null() {
        *out_ch = d.channels;
    }
    if !out_sr.is_null() {
        *out_sr = d.sample_rate;
    }
    MOVIE_OK
}
