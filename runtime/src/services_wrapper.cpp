// Thin C++ wrapper forwarding the Services group (svc_am, svc_set, svc_misc, svc_process, svc_hid,
// svc_nfp, hle_ro, hle_vi, hle_hid_system_nfc, hle_net, hle_web, hle_pl, hle_album, hle_jpeg,
// hle_nfp, hle_bcat, hle_movie, hle_friends, hle_ngc, hle_ldn) to crates/runtime/src/services.rs.
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <deque>
#include <initializer_list>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

#if !defined(NX_NO_VPX) && __has_include(<vpx/vpx_decoder.h>)
#include <vpx/vp8dx.h>
#include <vpx/vpx_decoder.h>
#define NX_SERVICES_HAVE_VPX 1
#endif
#if !defined(NX_NO_VORBIS) && __has_include(<vorbis/codec.h>)
#include <vorbis/codec.h>
#define NX_SERVICES_HAVE_VORBIS 1
#endif

#include <nx/game.h>
#include <nx/runtime.h>

#include <acnh_runtime.h>

#include "config.h"
#include "game_clock.h"
#include "hle_hostnet.h"
#include "hle_ldn.h"
#include "ipc.h"
#include "kernel.h"
#include "language.h"
#include "nextendo.h"
#include "nfp_host.h"
#include "platform.h"
#include "save_storage.h"
#include "users.h"

namespace nx {

namespace nvn {
void wait_submits_so_far(int mode);
void worker_finish();
bool cpu_read_after_write_backs(const uint8_t* addr, uint64_t size);
void album_capture(uint64_t texture, const std::string& stem);
void screenshot_save_rgba(const uint8_t* rgba, uint32_t width, uint32_t height, const std::string& stem);
uint64_t guest_call(CPU& c, uint64_t fn, std::initializer_list<uint64_t> args);
uint64_t scratch(CPU& c, size_t size);
}  // namespace nvn
namespace ui {
void ui_open_online();
}  // namespace ui
uint64_t texture_at_memory(uint64_t addr);
void textures_written(uint64_t addr, uint64_t size);  // nvn_objects.cpp: uint64_t, not size_t (they differ on macOS)

namespace {

// ---- Host Event & Service Registry ----------------------------------------------------------------------------------

std::mutex g_ev_lock;
std::unordered_map<uint32_t, std::shared_ptr<EventState>> g_events;
uint32_t g_next_ev_id = 1;

uint32_t register_event(std::shared_ptr<EventState> st) {
    std::lock_guard lk(g_ev_lock);
    uint32_t id = g_next_ev_id++;
    g_events[id] = std::move(st);
    return id;
}

std::shared_ptr<EventState> find_event(uint32_t id) {
    std::lock_guard lk(g_ev_lock);
    auto it = g_events.find(id);
    return it == g_events.end() ? nullptr : it->second;
}

uint32_t event_handle_from_state(std::shared_ptr<EventState> st) {
    auto e = std::make_shared<KEvent>();
    e->state = std::move(st);
    return handle_add(e);
}

uint32_t event_handle_from_id(uint32_t id) {
    auto st = find_event(id);
    if (!st) st = std::make_shared<EventState>();
    return event_handle_from_state(std::move(st));
}

void attach_os_system_event(CPU& c, uint64_t guest_ev, std::shared_ptr<EventState> st) {
    auto r = std::make_shared<KEvent>(), w = std::make_shared<KEvent>();
    r->state = w->state = std::move(st);
    uint32_t hr = handle_add(r), hw = handle_add(w);
    static const uint64_t attach =
        find_symbol("_ZN2nn2os17AttachSystemEventEPNS0_15SystemEventTypeEjbjbNS0_14EventClearModeE");
    uint64_t saved[8];
    for (int k = 0; k < 8; k++) saved[k] = c.x[k];
    c.x[0] = guest_ev, c.x[1] = hr, c.x[2] = 1, c.x[3] = hw, c.x[4] = 1, c.x[5] = 1;
    if (attach) call(c, attach);
    for (int k = 0; k < 8; k++) c.x[k] = saved[k];
}

// ---- Host Storage Registry for preselected_user_storage -------------------------------------------------------------

std::mutex g_stor_lock;
std::unordered_map<uint32_t, std::shared_ptr<Service>> g_storages;
uint32_t g_next_stor_id = 1;

uint32_t register_storage(std::shared_ptr<Service> s) {
    std::lock_guard lk(g_stor_lock);
    uint32_t id = g_next_stor_id++;
    g_storages[id] = std::move(s);
    return id;
}

std::shared_ptr<Service> take_storage(uint32_t id) {
    std::lock_guard lk(g_stor_lock);
    auto it = g_storages.find(id);
    if (it == g_storages.end()) return nullptr;
    auto s = std::move(it->second);
    g_storages.erase(it);
    return s;
}

// ---- Host Callbacks -------------------------------------------------------------------------------------------------

void host_log(const char* msg) {
    if (msg) log("%s", msg);
}
bool host_is_handheld() { return config_is_handheld(); }
bool host_config_bool(const char* k, bool def) { return config_get_bool(k ? k : "", def); }
uint64_t host_desired_lang() { return desired_language(); }
uint64_t host_lang_for_enum(uint32_t e) { return pack_language_code(language_code_for_enum(e)); }

uint64_t host_monotonic_ns() { return plat::monotonic_ns(); }
void host_sleep_ns(int64_t ns) { plat::sleep_ns(ns); }
uint64_t host_wait_vblank() { return plat::wait_vblank(); }
void* host_create_window() { return plat::create_window(); }
void host_release_window(void* w) { plat::release_window(w); }
size_t host_copy_str(const std::string& s, char* out, size_t cap) {
    if (!out || cap == 0) return 0;
    size_t n = std::min(s.size(), cap - 1);
    std::memcpy(out, s.data(), n);
    out[n] = 0;
    return n;
}
size_t host_game_root(char* out, size_t cap) { return host_copy_str(plat::game_root(), out, cap); }
size_t host_data_root(char* out, size_t cap) { return host_copy_str(plat::data_root(), out, cap); }
void host_rumble(float l, float h, uint32_t ms) { plat::rumble(l, h, ms); }
void host_wifi_multicast(bool on) {
#if defined(__ANDROID__)
    plat::wifi_multicast(on);
#else
    (void)on;
#endif
}

uint64_t host_kernel_alloc(uint64_t sz) { return kernel_alloc(sz); }
void host_mem_commit(uint64_t a, uint64_t s) { commit(a, s); }
void host_mem_decommit(uint64_t a, uint64_t s) { decommit(a, s); }
void host_vm_set(uint64_t a, uint64_t s, uint32_t t, uint32_t p) { vm_set(a, s, t, p); }
uint64_t host_heap_size() { return heap_size(); }
void host_get_mem_layout(uint64_t* out, size_t len) {
    if (!out || len < 8) return;
    out[0] = kAliasBase;
    out[1] = kAliasSize;
    out[2] = kHeapBase;
    out[3] = kHeapMax;
    out[4] = kAddressSpaceBase;
    out[5] = kAddressSpaceSize;
    out[6] = kStackBase;
    out[7] = kStackSize;
}
void host_get_game_info(uint64_t* sys_res, uint64_t* prog_id) {
    if (sys_res) *sys_res = game::kInfo.system_resource_size;
    if (prog_id) *prog_id = game::kInfo.program_id;
}

bool host_set_thread_core(uint32_t handle, int32_t core) {
    auto t = std::dynamic_pointer_cast<KThread>(handle_get(handle));
    if (!t) return false;
    t->core = core;
    return true;
}
bool host_get_transfer_mem(uint32_t handle, uint64_t* addr, uint64_t* size) {
    auto tm = std::dynamic_pointer_cast<KTransferMemory>(handle_get(handle));
    if (!tm) return false;
    if (addr) *addr = tm->addr;
    if (size) *size = tm->size;
    return true;
}

uint32_t host_event_create(bool signaled) {
    auto st = std::make_shared<EventState>();
    st->signaled = signaled;
    return register_event(std::move(st));
}
void host_event_signal(uint32_t id, bool signaled) {
    if (auto st = find_event(id)) st->signaled = signaled;
}
void host_notify_all() { notify_all(); }
void host_attach_system_event(void* cpu, uint64_t guest_ev, uint32_t ev_id) {
    if (!cpu) return;
    auto st = find_event(ev_id);
    if (!st) st = std::make_shared<EventState>();
    attach_os_system_event(*static_cast<CPU*>(cpu), guest_ev, std::move(st));
}

uint32_t host_user_create_storage(uint32_t ui, uint8_t* uid_out, size_t cap) {
    UserInfo u;
    if (ui == 1 || ui == 5) {
        u = user_create();
        log("am: playerSelect UserCreator -> %s", u.valid() ? u.nickname.c_str() : "no room (8 users)");
    } else if (ui == 0) {
        u = user_last_opened();
        log("am: playerSelect UserSelector -> %s", u.nickname.c_str());
    }
    if (u.valid() && uid_out && cap >= 16) {
        std::memcpy(uid_out, &u.lo, 8);
        std::memcpy(uid_out + 8, &u.hi, 8);
        user_opened(u.lo, u.hi);
        return 1;
    }
    return 0;
}
uint32_t host_preselected_user_storage() { return register_storage(preselected_user_storage()); }

void host_nvn_wait_finish() { nvn::wait_submits_so_far(0); }
void host_nvn_worker_finish() { nvn::worker_finish(); }
bool host_nvn_rawb(const uint8_t* p, uint64_t sz) { return nvn::cpu_read_after_write_backs(p, sz); }
void host_nvn_album_capture(uint64_t tex, const char* stem) { nvn::album_capture(tex, stem ? stem : ""); }
void host_nvn_screenshot_rgba(const uint8_t* rgba, uint32_t w, uint32_t h, const char* stem) {
    nvn::screenshot_save_rgba(rgba, w, h, stem ? stem : "");
}
uint64_t host_nvn_tex_at_mem(uint64_t addr) { return texture_at_memory(addr); }
void host_nvn_tex_written(uint64_t addr, size_t sz) { textures_written(addr, sz); }

size_t host_save_island_name(char* out, size_t cap) {
    return host_copy_str(save_storage::island_display_name(save_storage::device_dir(plat::data_root())), out, cap);
}

bool host_nfp_has_tag() { return nfp_host::has_tag(); }
uint32_t host_nfp_state() { return uint32_t(nfp_host::device_state()); }
void host_nfp_init() { nfp_host::on_initialize(); }
void host_nfp_fini() { nfp_host::on_finalize(); }
void host_nfp_start() { nfp_host::on_start_detection(); }
void host_nfp_stop() { nfp_host::on_stop_detection(); }
void host_nfp_mount() { nfp_host::on_mount(); }
void host_nfp_unmount() { nfp_host::on_unmount(); }
bool host_nfp_has_app() { return nfp_host::tag_has_application_area(); }
bool host_nfp_open_app(uint32_t id) { return nfp_host::on_open_application_area(id); }
bool host_nfp_read_app(void* out, uint32_t sz) { return nfp_host::read_application_area(out, sz); }
void host_nfp_write_app(const void* data, uint32_t sz) { nfp_host::write_application_area(data, sz); }
void host_nfp_fill_info(uint32_t kind, void* out) {
    switch (kind) {
        case 0: nfp_host::fill_tag_info(out); break;
        case 1: nfp_host::fill_model_info(out); break;
        case 2: nfp_host::fill_register_info(out); break;
        case 3: nfp_host::fill_register_info_private(out); break;
        case 4: nfp_host::fill_nfp_data(out); break;
        case 5: nfp_host::fill_common_info(out); break;
        case 6: nfp_host::fill_admin_info(out); break;
        default: break;
    }
}
size_t host_nfp_tag_path(char* out, size_t cap) { return host_copy_str(nfp_host::tag_path(), out, cap); }
void host_nfp_bind_ev(void* cpu, uint64_t ev, int32_t slot) {
    if (cpu) nfp_host::bind_system_event(*static_cast<CPU*>(cpu), ev, slot);
}
void host_nfp_sig_avail() { nfp_host::signal_availability(); }
void host_nfp_ensure(void* cpu) {
    if (cpu) nfp_host::ensure_nfc_services_ready(*static_cast<CPU*>(cpu));
}

bool host_nextendo_online() { return nextendo::online_services_active(); }
bool host_nextendo_is() { return nextendo::is_nextendo(); }
bool host_nextendo_signed() { return nextendo::signed_in(); }
bool host_nextendo_ver_ok() { return nextendo::version_ok_for_nextendo(); }
uint64_t host_nextendo_nsa() { return nextendo::account().nsa; }
bool host_nextendo_ldn_wanted() { return nextendo::ldn_wanted(); }
size_t host_nextendo_friends_count() { return nextendo::friends_cached().size(); }
bool host_nextendo_friends_get(size_t idx, AcnhFriendInfo* out) {
    auto list = nextendo::friends_cached();
    if (idx >= list.size() || !out) return false;
    const auto& f = list[idx];
    std::memset(out, 0, sizeof *out);
    out->nsa = f.nsa;
    out->pid = f.pid;
    out->status = f.status;
    out->favorite = f.favorite ? 1 : 0;
    host_copy_str(f.app_id, out->app_id, sizeof out->app_id);
    host_copy_str(f.name, out->name, sizeof out->name);
    return true;
}
void host_nextendo_friends_refresh() { nextendo::friends_refresh_blocking(); }
void host_nextendo_presence(bool p) { nextendo::presence_set_playing(p); }
void host_time_refresh() { nx_time_refresh_shared_clocks(); }
void host_ui_open_online() { ui::ui_open_online(); }

bool host_hostnet_active() { return hostnet_active(); }
uint32_t host_hostnet_local_ip() { return hostnet_local_ip(); }

void host_ldn_start() { link_host_start(); }
void host_ldn_stop() { link_stop(); }
int32_t host_ldn_scan(uint8_t* out, int32_t max, const uint8_t* filter) { return link_scan(out, max, filter); }
int32_t host_ldn_join(const uint8_t* bssid, const uint8_t* me, uint8_t* out_info) {
    uint8_t node[0x40]{};
    if (me) std::memcpy(node, me, sizeof node);
    return link_join(bssid, node, out_info);
}
uint32_t host_ldn_local_addr() { return local_address(); }
size_t host_ldn_island_name(const uint8_t* info, char* out, size_t cap) {
    return host_copy_str(ldn_island_name(info), out, cap);
}
void host_vsock_reset() { vsock_reset(); }

struct MovieCodecState {
#ifdef NX_SERVICES_HAVE_VPX
    vpx_codec_ctx_t vpx{};
    bool vpx_open = false;
#endif
#ifdef NX_SERVICES_HAVE_VORBIS
    vorbis_info vi{};
    vorbis_comment vc{};
    vorbis_dsp_state vd{};
    vorbis_block vb{};
    bool vorbis_open = false;
    int header_step = 0;
#endif
};

void* host_movie_open(int32_t codec_type, int32_t /*channels*/, const uint8_t* extra, size_t extra_size) {
    auto* st = new MovieCodecState();
#ifdef NX_SERVICES_HAVE_VPX
    if (codec_type == 6 || codec_type == 7) {
        const vpx_codec_iface_t* iface = codec_type == 6 ? vpx_codec_vp8_dx() : vpx_codec_vp9_dx();
        vpx_codec_dec_cfg_t cfg{4, 0, 0};
        if (vpx_codec_dec_init(&st->vpx, iface, &cfg, 0) == VPX_CODEC_OK) st->vpx_open = true;
    }
#else
    (void)codec_type;
#endif
#ifdef NX_SERVICES_HAVE_VORBIS
    if (codec_type == 11) {
        vorbis_info_init(&st->vi);
        vorbis_comment_init(&st->vc);
        if (extra && extra_size > 2 && extra[0] == 2) {
            size_t l1 = extra[1], l2 = extra[2];
            if (3 + l1 + l2 < extra_size) {
                const uint8_t* p1 = extra + 3;
                const uint8_t* p2 = p1 + l1;
                const uint8_t* p3 = p2 + l2;
                size_t l3 = extra_size - (3 + l1 + l2);
                const uint8_t* pkts[3] = {p1, p2, p3};
                size_t lens[3] = {l1, l2, l3};
                for (int i = 0; i < 3; i++) {
                    ogg_packet op{};
                    op.packet = const_cast<unsigned char*>(pkts[i]);
                    op.bytes = long(lens[i]);
                    op.b_o_s = (i == 0) ? 1 : 0;
                    vorbis_synthesis_headerin(&st->vi, &st->vc, &op);
                }
                if (vorbis_synthesis_init(&st->vd, &st->vi) == 0) {
                    vorbis_block_init(&st->vd, &st->vb);
                    st->vorbis_open = true;
                    st->header_step = 3;
                }
            }
        }
    }
#else
    (void)extra;
    (void)extra_size;
#endif
    return st;
}

void host_movie_close(void* ptr, int32_t /*codec_type*/) {
    auto* st = static_cast<MovieCodecState*>(ptr);
    if (!st) return;
#ifdef NX_SERVICES_HAVE_VPX
    if (st->vpx_open) vpx_codec_destroy(&st->vpx);
#endif
#ifdef NX_SERVICES_HAVE_VORBIS
    if (st->vorbis_open) {
        vorbis_block_clear(&st->vb);
        vorbis_dsp_clear(&st->vd);
    }
    vorbis_comment_clear(&st->vc);
    vorbis_info_clear(&st->vi);
#endif
    delete st;
}

bool host_movie_decode_video(void* ptr, int32_t /*codec_type*/, const uint8_t* data, size_t size, uint8_t* out,
                             size_t* out_written, int32_t* out_w, int32_t* out_h) {
    auto* st = static_cast<MovieCodecState*>(ptr);
    if (!st || !data || !out || !out_written) return false;
#ifdef NX_SERVICES_HAVE_VPX
    if (!st->vpx_open) return false;
    if (vpx_codec_decode(&st->vpx, data, unsigned(size), nullptr, 0) != VPX_CODEC_OK) return false;
    vpx_codec_iter_t iter = nullptr;
    vpx_image_t* img = vpx_codec_get_frame(&st->vpx, &iter);
    if (!img) return false;
    int w = int(img->d_w), h = int(img->d_h);
    size_t need = size_t(w) * size_t(h) * 3 / 2;
    if (*out_written < need) return false;
    uint8_t* dst = out;
    for (int y = 0; y < h; y++) {
        std::memcpy(dst, img->planes[VPX_PLANE_Y] + y * img->stride[VPX_PLANE_Y], size_t(w));
        dst += w;
    }
    for (int y = 0; y < h / 2; y++) {
        const uint8_t* u = img->planes[VPX_PLANE_U] + y * img->stride[VPX_PLANE_U];
        const uint8_t* v = img->planes[VPX_PLANE_V] + y * img->stride[VPX_PLANE_V];
        for (int x = 0; x < w / 2; x++) {
            *dst++ = u[x];
            *dst++ = v[x];
        }
    }
    *out_written = need;
    if (out_w) *out_w = w;
    if (out_h) *out_h = h;
    return true;
#else
    (void)size;
    (void)out_w;
    (void)out_h;
    return false;
#endif
}

bool host_movie_decode_audio(void* ptr, const uint8_t* data, size_t size, uint8_t* out, size_t* out_written,
                             int32_t* out_ch, int32_t* out_sr) {
    auto* st = static_cast<MovieCodecState*>(ptr);
    if (!st || !data || !out || !out_written) return false;
#ifdef NX_SERVICES_HAVE_VORBIS
    ogg_packet op{};
    op.packet = const_cast<unsigned char*>(data);
    op.bytes = long(size);
    if (!st->vorbis_open) {
        if (st->header_step == 0) op.b_o_s = 1;
        if (vorbis_synthesis_headerin(&st->vi, &st->vc, &op) == 0) {
            st->header_step++;
            if (st->header_step == 3 && vorbis_synthesis_init(&st->vd, &st->vi) == 0) {
                vorbis_block_init(&st->vd, &st->vb);
                st->vorbis_open = true;
            }
        }
        return false;
    }
    if (vorbis_synthesis(&st->vb, &op) != 0) return false;
    vorbis_synthesis_blockin(&st->vd, &st->vb);
    float** pcm = nullptr;
    int samples = vorbis_synthesis_pcmout(&st->vd, &pcm);
    if (samples <= 0) return false;
    int ch = st->vi.channels;
    size_t max_samples = *out_written / (size_t(ch) * 2);
    int take = std::min(samples, int(max_samples));
    auto* dst = reinterpret_cast<int16_t*>(out);
    for (int i = 0; i < take; i++) {
        for (int c = 0; c < ch; c++) {
            float v = std::clamp(pcm[c][i], -1.0f, 1.0f);
            dst[i * ch + c] = int16_t(v * 32767.0f);
        }
    }
    vorbis_synthesis_read(&st->vd, take);
    *out_written = size_t(take) * size_t(ch) * 2;
    if (out_ch) *out_ch = ch;
    if (out_sr) *out_sr = int32_t(st->vi.rate);
    return true;
#else
    (void)size;
    (void)out_ch;
    (void)out_sr;
    return false;
#endif
}

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhServicesHost h{};
        h.log = host_log;
        h.config_is_handheld = host_is_handheld;
        h.config_get_bool = host_config_bool;
        h.desired_language = host_desired_lang;
        h.language_code_for_enum = host_lang_for_enum;
        h.monotonic_ns = host_monotonic_ns;
        h.sleep_ns = host_sleep_ns;
        h.wait_vblank = host_wait_vblank;
        h.create_window = host_create_window;
        h.release_window = host_release_window;
        h.game_root = host_game_root;
        h.data_root = host_data_root;
        h.rumble = host_rumble;
        h.wifi_multicast = host_wifi_multicast;
        h.kernel_alloc = host_kernel_alloc;
        h.mem_commit = host_mem_commit;
        h.mem_decommit = host_mem_decommit;
        h.vm_set = host_vm_set;
        h.heap_size = host_heap_size;
        h.get_mem_layout = host_get_mem_layout;
        h.get_game_info = host_get_game_info;
        h.set_thread_core = host_set_thread_core;
        h.get_transfer_memory = host_get_transfer_mem;
        h.event_create = host_event_create;
        h.event_signal = host_event_signal;
        h.notify_all = host_notify_all;
        h.attach_system_event = host_attach_system_event;
        h.user_create_storage = host_user_create_storage;
        h.preselected_user_storage = host_preselected_user_storage;
        h.nvn_wait_finish = host_nvn_wait_finish;
        h.nvn_worker_finish = host_nvn_worker_finish;
        h.nvn_cpu_read_after_write_backs = host_nvn_rawb;
        h.nvn_album_capture = host_nvn_album_capture;
        h.nvn_screenshot_save_rgba = host_nvn_screenshot_rgba;
        h.nvn_texture_at_memory = host_nvn_tex_at_mem;
        h.nvn_textures_written = host_nvn_tex_written;
        h.save_island_display_name = host_save_island_name;
        h.nfp_has_tag = host_nfp_has_tag;
        h.nfp_device_state = host_nfp_state;
        h.nfp_on_initialize = host_nfp_init;
        h.nfp_on_finalize = host_nfp_fini;
        h.nfp_on_start_detection = host_nfp_start;
        h.nfp_on_stop_detection = host_nfp_stop;
        h.nfp_on_mount = host_nfp_mount;
        h.nfp_on_unmount = host_nfp_unmount;
        h.nfp_tag_has_app_area = host_nfp_has_app;
        h.nfp_on_open_app_area = host_nfp_open_app;
        h.nfp_read_app_area = host_nfp_read_app;
        h.nfp_write_app_area = host_nfp_write_app;
        h.nfp_fill_info = host_nfp_fill_info;
        h.nfp_tag_path = host_nfp_tag_path;
        h.nfp_bind_system_event = host_nfp_bind_ev;
        h.nfp_signal_availability = host_nfp_sig_avail;
        h.nfp_ensure_ready = host_nfp_ensure;
        h.nextendo_online_active = host_nextendo_online;
        h.nextendo_is_nextendo = host_nextendo_is;
        h.nextendo_signed_in = host_nextendo_signed;
        h.nextendo_version_ok = host_nextendo_ver_ok;
        h.nextendo_account_nsa = host_nextendo_nsa;
        h.nextendo_ldn_wanted = host_nextendo_ldn_wanted;
        h.nextendo_friends_count = host_nextendo_friends_count;
        h.nextendo_friends_get = host_nextendo_friends_get;
        h.nextendo_friends_refresh = host_nextendo_friends_refresh;
        h.nextendo_presence_playing = host_nextendo_presence;
        h.time_refresh_clocks = host_time_refresh;
        h.ui_open_online = host_ui_open_online;
        h.hostnet_active = host_hostnet_active;
        h.hostnet_local_ip = host_hostnet_local_ip;
        h.ldn_link_host_start = host_ldn_start;
        h.ldn_link_stop = host_ldn_stop;
        h.ldn_link_scan = host_ldn_scan;
        h.ldn_link_join = host_ldn_join;
        h.ldn_local_address = host_ldn_local_addr;
        h.ldn_island_name = host_ldn_island_name;
        h.vsock_reset = host_vsock_reset;
        h.movie_codec_open = host_movie_open;
        h.movie_codec_close = host_movie_close;
        h.movie_codec_decode_video = host_movie_decode_video;
        h.movie_codec_decode_audio = host_movie_decode_audio;
        acnh_services_set_host(&h);
    });
}

// ---- IPC Service Registrars (am, set, misc, hid, nfp) ---------------------------------------------------------------

std::shared_ptr<Service> make_svc(const char* name, std::function<void(Service&)> setup = {}) {
    ensure_host();
    auto s = std::make_shared<Service>(name);
    if (setup) setup(*s);
    return s;
}

std::shared_ptr<Service> make_set_service(std::string name) {
    return make_svc(name.c_str(), [](Service& s) {
        s.cmds[0] = [](IpcCtx& c) { c.push<uint64_t>(acnh_svc_set_desired_language()); return 0u; };
        s.cmds[1] = [](IpcCtx& c) {
            Buffer out = c.out_buf(0);
            c.push<int32_t>(acnh_svc_set_push_available_languages(out.addr, out.size));
            return 0u;
        };
        s.cmds[2] = [](IpcCtx& c) { c.push<uint64_t>(acnh_svc_set_language_code_for_enum(c.arg<uint32_t>(0))); return 0u; };
        s.cmds[3] = [](IpcCtx& c) { c.push<int32_t>(acnh_svc_set_language_count()); return 0u; };
        s.cmds[4] = [](IpcCtx& c) { c.push<int32_t>(acnh_svc_set_region_code()); return 0u; };
        s.cmds[5] = s.cmds[1];
        s.cmds[6] = s.cmds[3];
    });
}

ServiceRegistrar r_set("set", [] { return make_set_service("set"); });
ServiceRegistrar r_set_sys("set:sys", [] { return make_set_service("set:sys"); });
ServiceRegistrar r_set_u("set:u", [] { return make_set_service("set:u"); });

std::shared_ptr<Service> parental_control() {
    return make_svc("IParentalControlService", [](Service& s) {
        s.cmds[1403] = [](IpcCtx& c) { c.push<uint8_t>(0); return 0u; };
        s.cmds[1001] = [](IpcCtx&) { return 0u; };
    });
}
void mount_pctl(Service& s) {
    s.cmds[0] = [](IpcCtx& c) { c.push_object(parental_control()); return 0u; };
    s.cmds[1] = [](IpcCtx& c) { c.push_object(parental_control()); return 0u; };
}
ServiceRegistrar r_pctl("pctl", [] { return make_svc("pctl", mount_pctl); });
ServiceRegistrar r_pctl_s("pctl:s", [] { return make_svc("pctl:s", mount_pctl); });
ServiceRegistrar r_pctl_a("pctl:a", [] { return make_svc("pctl:a", mount_pctl); });
ServiceRegistrar r_pctl_r("pctl:r", [] { return make_svc("pctl:r", mount_pctl); });
ServiceRegistrar r_lm("lm", [] {
    return make_svc("lm", [](Service& s) {
        s.cmds[0] = [](IpcCtx& c) { c.push_object(make_svc("ILogger")); return 0u; };
    });
});
ServiceRegistrar r_apm("apm", [] {
    return make_svc("apm", [](Service& s) {
        s.cmds[0] = [](IpcCtx& c) { c.push_object(make_svc("ISession")); return 0u; };
        s.cmds[1] = [](IpcCtx& c) { c.push<uint32_t>(acnh_svc_apm_performance_mode()); return 0u; };
    });
});
ServiceRegistrar r_aoc("aoc:u", [] {
    return make_svc("aoc:u", [](Service& s) {
        s.cmds[2] = [](IpcCtx& c) { c.push<uint32_t>(0); return 0u; };
        s.cmds[3] = [](IpcCtx& c) { c.push<uint32_t>(0); return 0u; };
        s.cmds[7] = [](IpcCtx& c) { c.push<uint32_t>(0); return 0u; };
        s.cmds[8] = [](IpcCtx& c) {
            c.out_copy.push_back(event_handle_from_state(std::make_shared<EventState>()));
            return 0u;
        };
    });
});

ServiceRegistrar r_hid("hid", [] {
    return make_svc("hid", [](Service& s) {
        s.cmds[206] = [](IpcCtx& c) {
            const Buffer h = c.in_buf(0), v = c.in_buf(1);
            return acnh_svc_hid_send_vibration(h.addr, h.size, v.addr, v.size);
        };
    });
});

std::shared_ptr<Service> am_library_applet(uint32_t applet_id, uint32_t /*mode*/) {
    struct State {
        uint32_t id;
        std::shared_ptr<EventState> done = std::make_shared<EventState>();
        std::shared_ptr<EventState> out_ready = std::make_shared<EventState>();
        std::shared_ptr<EventState> inter_ready = std::make_shared<EventState>();
        std::vector<std::vector<uint8_t>> in;
        std::deque<std::shared_ptr<Service>> out;
        bool started = false;
    };
    auto st = std::make_shared<State>();
    st->id = applet_id;
    return make_svc("ILibraryAppletAccessor", [st](Service& s) {
        s.cmds[0] = [st](IpcCtx& c) { c.out_copy.push_back(event_handle_from_state(st->done)); return 0u; };
        s.cmds[1] = [st](IpcCtx& c) { c.push<uint8_t>(st->started); return 0u; };
        s.cmds[10] = [st](IpcCtx&) {
            st->started = true;
            std::vector<uint8_t> out;
            if (st->id == 0x10 && !st->in.empty()) {
                out.resize(0x18);
                size_t n = acnh_svc_am_player_select(st->in.back().data(), st->in.back().size(), out.data(), out.size());
                out.resize(n);
            }
            st->out.push_back(make_storage(std::move(out)));
            st->out_ready->signaled = true;
            st->done->signaled = true;
            return 0u;
        };
        s.cmds[20] = [](IpcCtx&) { return 0u; };
        s.cmds[25] = [](IpcCtx&) { return 0u; };
        s.cmds[30] = [](IpcCtx&) { return 0u; };
        s.cmds[50] = [](IpcCtx&) { return 0u; };
        s.cmds[100] = [st](IpcCtx& c) {
            if (!c.in_services.empty())
                if (auto b = storage_blob(c.in_services[0])) st->in.push_back(*b);
            return 0u;
        };
        s.cmds[101] = [st](IpcCtx& c) {
            if (st->out.empty()) return 0x2A80u;
            c.push_object(st->out.front());
            st->out.pop_front();
            if (st->out.empty()) st->out_ready->signaled = false;
            return 0u;
        };
        s.cmds[102] = [](IpcCtx&) { return 0u; };
        s.cmds[103] = [](IpcCtx&) { return 0u; };
        s.cmds[104] = [](IpcCtx&) { return 0x2A80u; };
        s.cmds[105] = [st](IpcCtx& c) { c.out_copy.push_back(event_handle_from_state(st->out_ready)); return 0u; };
        s.cmds[106] = [st](IpcCtx& c) { c.out_copy.push_back(event_handle_from_state(st->inter_ready)); return 0u; };
        s.cmds[110] = [](IpcCtx& c) { c.push<uint8_t>(0); return 0u; };
        s.cmds[150] = [](IpcCtx&) { return 0u; };
    });
}

ServiceRegistrar reg_applet_oe("appletOE", [] {
    return make_svc("appletOE", [](Service& s) {
        s.cmds[0] = [](IpcCtx& c) {
            c.push_object(make_svc("IApplicationProxy", [](Service& p) {
                p.cmds[0] = [](IpcCtx& c) {
                    c.push_object(make_svc("ICommonStateGetter", [](Service& g) {
                        g.cmds[0] = [](IpcCtx& c) {
                            c.out_copy.push_back(event_handle_from_id(acnh_svc_am_get_message_event()));
                            return 0u;
                        };
                        g.cmds[1] = [](IpcCtx& c) {
                            uint32_t msg = 0;
                            uint32_t rc = acnh_svc_am_receive_message(&msg);
                            if (rc == 0) c.push<uint32_t>(msg);
                            return rc;
                        };
                        g.cmds[5] = [](IpcCtx& c) { c.push<uint8_t>(acnh_svc_am_get_operation_mode()); return 0u; };
                        g.cmds[6] = [](IpcCtx& c) { c.push<uint32_t>(acnh_svc_apm_performance_mode()); return 0u; };
                        g.cmds[9] = [](IpcCtx& c) { c.push<uint8_t>(1); return 0u; };
                        g.cmds[60] = [](IpcCtx& c) {
                            uint32_t w = 0, h = 0;
                            acnh_svc_am_get_default_display_resolution(&w, &h);
                            c.push<uint32_t>(w);
                            c.push<uint32_t>(h);
                            return 0u;
                        };
                        g.cmds[61] = [](IpcCtx& c) {
                            c.out_copy.push_back(event_handle_from_state(std::make_shared<EventState>()));
                            return 0u;
                        };
                        g.cmds[66] = [](IpcCtx&) { return 0u; };
                    }));
                    return 0u;
                };
                p.cmds[1] = [](IpcCtx& c) {
                    c.push_object(make_svc("ISelfController", [](Service& sc) {
                        sc.cmds[0] = [](IpcCtx&) { return 0u; };
                        sc.cmds[9] = [](IpcCtx& c) {
                            auto e = std::make_shared<EventState>();
                            e->signaled = true;
                            c.out_copy.push_back(event_handle_from_state(std::move(e)));
                            return 0u;
                        };
                        sc.cmds[40] = [](IpcCtx& c) { c.push<uint64_t>(1); return 0u; };
                        sc.cmds[62] = [](IpcCtx&) { return 0u; };
                        for (uint32_t cmd : {19u, 50u, 71u, 110u})
                            sc.cmds[cmd] = [](IpcCtx& c) {
                                c.out_copy.push_back(event_handle_from_state(std::make_shared<EventState>()));
                                return 0u;
                            };
                    }));
                    return 0u;
                };
                p.cmds[2] = [](IpcCtx& c) {
                    c.push_object(make_svc("IWindowController", [](Service& w) {
                        w.cmds[1] = [](IpcCtx& c) { c.push<uint64_t>(0x51); return 0u; };
                    }));
                    return 0u;
                };
                p.cmds[3] = [](IpcCtx& c) { c.push_object(make_svc("IAudioController")); return 0u; };
                p.cmds[4] = [](IpcCtx& c) { c.push_object(make_svc("IDisplayController")); return 0u; };
                p.cmds[10] = [](IpcCtx& c) { c.push_object(make_svc("IProcessWindingController")); return 0u; };
                p.cmds[11] = [](IpcCtx& c) {
                    c.push_object(make_svc("ILibraryAppletCreator", [](Service& lac) {
                        lac.cmds[0] = [](IpcCtx& c) {
                            c.push_object(am_library_applet(c.arg<uint32_t>(0), c.arg<uint32_t>(4)));
                            return 0u;
                        };
                        lac.cmds[1] = [](IpcCtx&) { return 0u; };
                        lac.cmds[2] = [](IpcCtx& c) { c.push<uint8_t>(0); return 0u; };
                        lac.cmds[10] = [](IpcCtx& c) {
                            int64_t n = c.arg<int64_t>(0);
                            c.push_object(make_storage(std::vector<uint8_t>(n > 0 && n < (1 << 24) ? size_t(n) : 0, 0)));
                            return 0u;
                        };
                        lac.cmds[11] = [](IpcCtx& c) {
                            c.push_object(make_storage(std::vector<uint8_t>()));
                            return 0u;
                        };
                    }));
                    return 0u;
                };
                p.cmds[20] = [](IpcCtx& c) {
                    c.push_object(make_svc("IApplicationFunctions", [](Service& af) {
                        af.cmds[1] = [](IpcCtx& c) {
                            uint32_t id = acnh_svc_am_pop_launch_parameter(c.arg<uint32_t>(0));
                            auto s = take_storage(id);
                            if (!s) return 0x2A80u;
                            c.push_object(std::move(s));
                            return 0u;
                        };
                        af.cmds[20] = [](IpcCtx& c) { c.push<uint64_t>(0); return 0u; };
                        af.cmds[21] = [](IpcCtx& c) { c.push<uint64_t>(acnh_svc_set_desired_language()); return 0u; };
                        af.cmds[40] = [](IpcCtx& c) { c.push<uint8_t>(0); return 0u; };
                        af.cmds[50] = [](IpcCtx& c) { c.push<uint64_t>(0x1234); c.push<uint64_t>(0x5678); return 0u; };
                        af.cmds[130] = [](IpcCtx& c) {
                            c.out_copy.push_back(event_handle_from_state(std::make_shared<EventState>()));
                            return 0u;
                        };
                        af.cmds[210] = [](IpcCtx& c) {
                            static std::shared_ptr<EventState> ev = std::make_shared<EventState>();
                            c.out_copy.push_back(event_handle_from_state(ev));
                            return 0u;
                        };
                    }));
                    return 0u;
                };
                p.cmds[1000] = [](IpcCtx& c) { c.push_object(make_svc("IDebugFunctions")); return 0u; };
            }));
            return 0u;
        };
    });
});

uint32_t nfp_ipc_push_event(IpcCtx& c, int slot) {
    auto state = std::make_shared<EventState>();
    auto ev = std::make_shared<KEvent>();
    ev->state = state;
    nfp_host::set_event_slot(slot, state);
    c.out_copy.push_back(handle_add(ev));
    if (slot == 0) nfp_host::sync_activate_event_after_attach();
    if (slot == 2) nfp_host::signal_availability();
    return 0u;
}

ServiceRegistrar r_nfp_user("nfp:user", [] {
    return make_svc("nfp:user", [](Service& mgr) {
        mgr.cmds[0] = [](IpcCtx& c) {
            acnh_nfp_initialize(true);
            c.push_object(make_svc("nfp:IUser", [](Service& s) {
                s.cmds[0] = [](IpcCtx&) { return acnh_nfp_initialize(true); };
                s.cmds[1] = [](IpcCtx&) { return acnh_nfp_finalize(true); };
                s.cmds[2] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    Buffer out = c.out_buf(0);
                    if (!out.addr || out.size < 8) return 115u | (10u << 9);
                    std::memset(reinterpret_cast<void*>(out.addr), 0, out.size);
                    st<uint64_t>(out.addr, nfp_host::kVirtualDeviceHandle);
                    c.push<uint32_t>(1);
                    return 0u;
                };
                s.cmds[3] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    nfp_host::on_start_detection();
                    return 0u;
                };
                s.cmds[4] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    nfp_host::on_stop_detection();
                    return 0u;
                };
                s.cmds[5] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    return acnh_nfp_mount(c.arg<uint32_t>(8), c.arg<uint32_t>(12), true);
                };
                s.cmds[6] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    return acnh_nfp_unmount();
                };
                s.cmds[7] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    return acnh_nfp_open_app_area(c.arg<uint32_t>(8));
                };
                s.cmds[8] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    Buffer out = c.out_buf(0);
                    uint32_t sz = uint32_t(std::min<uint64_t>(out.size, nfp_host::kApplicationAreaSize));
                    uint32_t rc = acnh_nfp_get_app_area(out.addr, 0, sz);
                    if (rc == 0) c.push<uint32_t>(sz);
                    return rc;
                };
                s.cmds[9] = [](IpcCtx&) { return 0u; };
                s.cmds[10] = [](IpcCtx&) { return 0u; };
                s.cmds[11] = [](IpcCtx&) { return 0u; };
                s.cmds[12] = [](IpcCtx&) { return 115u | (128u << 9); };
                s.cmds[13] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    return acnh_nfp_fill_info_checked(0, c.out_buf(0).addr, true);
                };
                s.cmds[14] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    return acnh_nfp_fill_info_checked(2, c.out_buf(0).addr, false);
                };
                s.cmds[15] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    return acnh_nfp_fill_info_checked(5, c.out_buf(0).addr, false);
                };
                s.cmds[16] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    return acnh_nfp_fill_info_checked(1, c.out_buf(0).addr, false);
                };
                s.cmds[17] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    return acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0)) ? nfp_ipc_push_event(c, 0) : (115u | (64u << 9));
                };
                s.cmds[18] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    return acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0)) ? nfp_ipc_push_event(c, 1) : (115u | (64u << 9));
                };
                s.cmds[19] = [](IpcCtx& c) { c.push<uint32_t>(acnh_nfp_get_state(true)); return 0u; };
                s.cmds[20] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) {
                        c.push<uint32_t>(5);
                        return 115u | (64u << 9);
                    }
                    c.push<uint32_t>(acnh_nfp_get_device_state(true));
                    return 0u;
                };
                s.cmds[21] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    c.push<uint32_t>(nfp_host::kVirtualNpadId);
                    return 0u;
                };
                s.cmds[22] = [](IpcCtx& c) { c.push<uint32_t>(nfp_host::kApplicationAreaSize); return 0u; };
                s.cmds[23] = [](IpcCtx& c) { return nfp_ipc_push_event(c, 2); };
            }));
            return 0u;
        };
    });
});

ServiceRegistrar r_nfc_user("nfc:user", [] {
    return make_svc("nfc:user", [](Service& mgr) {
        mgr.cmds[0] = [](IpcCtx& c) {
            acnh_nfp_initialize(true);
            c.push_object(make_svc("nfc:IUser", [](Service& s) {
                s.cmds[400] = [](IpcCtx&) {
                    acnh_svc_nfc_ipc_set_init(true);
                    nfp_host::on_initialize();
                    return 0u;
                };
                s.cmds[401] = [](IpcCtx&) { acnh_svc_nfc_ipc_set_init(false); return 0u; };
                s.cmds[402] = [](IpcCtx& c) { c.push<uint32_t>(acnh_svc_nfc_ipc_is_init() ? 1u : 0u); return 0u; };
                s.cmds[403] = [](IpcCtx& c) { c.push<uint8_t>(1); return 0u; };
                s.cmds[404] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    Buffer out = c.out_buf(0);
                    if (!out.addr || out.size < 8) return 115u | (10u << 9);
                    std::memset(reinterpret_cast<void*>(out.addr), 0, out.size);
                    st<uint64_t>(out.addr, nfp_host::kVirtualDeviceHandle);
                    c.push<uint32_t>(1);
                    return 0u;
                };
                s.cmds[405] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    c.push<uint32_t>(acnh_nfp_get_device_state(true));
                    return 0u;
                };
                s.cmds[406] = [](IpcCtx& c) {
                    if (!acnh_svc_nfc_ipc_is_init() || !acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0)))
                        return 115u | (64u << 9);
                    c.push<uint32_t>(nfp_host::kVirtualNpadId);
                    return 0u;
                };
                s.cmds[407] = [](IpcCtx& c) { return nfp_ipc_push_event(c, 2); };
                s.cmds[408] = [](IpcCtx& c) {
                    acnh_nfp_initialize(true);
                    if (!acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0))) return 115u | (64u << 9);
                    nfp_host::on_start_detection();
                    return 0u;
                };
                s.cmds[409] = [](IpcCtx&) {
                    if (!acnh_svc_nfc_ipc_is_init()) return 115u | (64u << 9);
                    nfp_host::on_stop_detection();
                    return 0u;
                };
                s.cmds[410] = [](IpcCtx& c) {
                    if (!acnh_svc_nfc_ipc_is_init() || !acnh_svc_nfp_ipc_handle_ok(c.arg<uint64_t>(0)))
                        return 115u | (64u << 9);
                    return acnh_nfp_fill_info_checked(0, c.out_buf(0).addr, true);
                };
                s.cmds[411] = [](IpcCtx& c) { return nfp_ipc_push_event(c, 0); };
                s.cmds[412] = [](IpcCtx& c) { return nfp_ipc_push_event(c, 1); };
            }));
            return 0u;
        };
    });
});

// ---- NX_HLE Wrappers ------------------------------------------------------------------------------------------------

void hle_ok(CPU& c) { c.x[0] = 0; }
void hle_no(CPU& c) { c.x[0] = 0; }
void hle_one(CPU& c) { c.x[0] = 1; }

// bcat
void hle_bcat_not_found(CPU& c) { c.x[0] = 122 | (30u << 9); }
void hle_bcat_no_net(CPU& c) { c.x[0] = 122 | (2u << 9); }
void hle_bcat_dir_read(CPU& c) { c.x[0] = acnh_hle_bcat_directory_read(c.x[1]); }
void hle_bcat_file_read(CPU& c) { c.x[0] = acnh_hle_bcat_file_read(c.x[1]); }
void hle_bcat_get_digest(CPU& c) { acnh_hle_bcat_get_digest(c.x[8]); }

NX_HLE("_ZN2nn4bcat10InitializeEv", hle_ok);
NX_HLE("_ZN2nn4bcat25MountDeliveryCacheStorageEv", hle_ok);
NX_HLE("_ZN2nn4bcat27UnmountDeliveryCacheStorageEv", hle_ok);
NX_HLE("_ZN2nn4bcat24RequestSyncDeliveryCacheEPNS0_21DeliveryCacheProgressE", hle_bcat_no_net);
NX_HLE("_ZN2nn4bcat22DeliveryCacheDirectory4OpenERKNS0_13DirectoryNameE", hle_bcat_not_found);
NX_HLE("_ZN2nn4bcat22DeliveryCacheDirectory4ReadEPiPNS0_27DeliveryCacheDirectoryEntryEi", hle_bcat_dir_read);
NX_HLE("_ZN2nn4bcat17DeliveryCacheFile4OpenERKNS0_13DirectoryNameERKNS0_8FileNameE", hle_bcat_not_found);
NX_HLE("_ZN2nn4bcat17DeliveryCacheFile4ReadEPmlPvm", hle_bcat_file_read);
NX_HLE("_ZN2nn4bcat17DeliveryCacheFile9GetDigestEv", hle_bcat_get_digest);

// ngc
void hle_ngc_check(CPU& c) { c.x[0] = acnh_hle_ngc_check_words(c.x[1], c.x[4]); }
void hle_ngc_mask(CPU& c) { c.x[0] = acnh_hle_ngc_mask_words(c.x[1]); }
NX_HLE("_ZN2nn3ngc15ProfanityFilter10InitializeEPvmb", hle_ok);
NX_HLE("_ZN2nn3ngc15ProfanityFilter11SetMaskModeENS1_8MaskModeE", hle_ok);
NX_HLE("_ZN2nn3ngc15ProfanityFilter19CheckProfanityWordsEPjjPPKDsm", hle_ngc_check);
NX_HLE("_ZN2nn3ngc15ProfanityFilter24MaskProfanityWordsInTextEPiPDsj", hle_ngc_mask);

// ro
void hle_ro_buf_size(CPU& c) { c.x[0] = acnh_hle_ro_get_buffer_size(c.x[0]); }
void hle_ro_load(CPU& c) { c.x[0] = acnh_hle_ro_load_module(); }
NX_HLE("_ZN2nn2ro10InitializeEv", hle_ok);
NX_HLE("_ZN2nn2ro8FinalizeEv", hle_ok);
NX_HLE("_ZN2nn2ro18RegisterModuleInfoEPNS0_16RegistrationInfoEPKv", hle_ok);
NX_HLE("_ZN2nn2ro20UnregisterModuleInfoEPNS0_16RegistrationInfoE", hle_ok);
NX_HLE("_ZN2nn2ro13GetBufferSizeEPmPKv", hle_ro_buf_size);
NX_HLE("_ZN2nn2ro10LoadModuleEPNS0_6ModuleEPKvPvmib", hle_ro_load);
NX_HLE("_ZN2nn2ro12UnloadModuleEPNS0_6ModuleE", hle_ok);

// web
void hle_web_share(CPU& c) { c.x[0] = acnh_hle_web_show_share_page(c.x[0]); }
NX_HLE("_ZN2nn3web13ShowSharePageEPNS0_20SharePageReturnValueERKNS0_16ShowSharePageArgE", hle_web_share);

// jpeg
void hle_jpeg_set_pixel_data(CPU& c) {
    ensure_host();
    static const uint64_t real =
        find_symbol("_ZN2nn5image11JpegEncoder12SetPixelDataEPKvNS0_11PixelFormatERKNS0_9DimensionEi");
    acnh_hle_jpeg_set_pixel_data(c.x[1], c.x[3], int32_t(c.x[4]));
    call(c, real);
}
NX_HLE("_ZN2nn5image11JpegEncoder12SetPixelDataEPKvNS0_11PixelFormatERKNS0_9DimensionEi", hle_jpeg_set_pixel_data);

// album
void hle_album_save(CPU& c) {
    ensure_host();
    const std::time_t t = std::time(nullptr);
    char stamp[32];
    std::strftime(stamp, sizeof stamp, "%Y-%m-%d_%H-%M-%S", std::localtime(&t));
    c.x[0] = acnh_hle_album_save_screenshot(c.x[0], c.x[1], uint32_t(c.x[2]), stamp);
}
NX_HLE("_ZN2nn5album14SaveScreenshotEPKvmNS0_9ImageSizeENS0_17AlbumReportOptionE", hle_album_save);

// pl
void hle_pl_addr(CPU& c) { ensure_host(); c.x[0] = acnh_hle_pl_get_shared_font_address(uint32_t(c.x[0])); }
void hle_pl_size(CPU& c) { ensure_host(); c.x[0] = acnh_hle_pl_get_shared_font_size(uint32_t(c.x[0])); }
void hle_pl_get(CPU& c) { ensure_host(); c.x[0] = acnh_hle_pl_get_shared_font(c.x[0], c.x[1], int32_t(c.x[2])); }
NX_HLE("_ZN2nn2pl13GetSharedFontEPiPNS0_14SharedFontDataEiNS_8settings12LanguageCodeE", hle_pl_get);
NX_HLE("_ZN2nn2pl21RequestSharedFontLoadENS0_14SharedFontTypeE", hle_ok);
NX_HLE("_ZN2nn2pl22GetSharedFontLoadStateENS0_14SharedFontTypeE", hle_one);
NX_HLE("_ZN2nn2pl20GetSharedFontAddressENS0_14SharedFontTypeE", hle_pl_addr);
NX_HLE("_ZN2nn2pl17GetSharedFontSizeENS0_14SharedFontTypeE", hle_pl_size);

// vi / nv
void hle_vi_open_display(CPU& c) { st<uint64_t>(c.x[0], kKernelBase - 0x1000); c.x[0] = 0; }
void hle_vi_create_layer(CPU& c) {
    void* w = plat::create_window();
    log("vi: layer created -> native window %p", w);
    plat::release_window(w);
    st<uint64_t>(c.x[0], kKernelBase - 0x2000);
    c.x[0] = 0;
}
void hle_vi_native_window(CPU& c) { st<uint64_t>(c.x[0], uint64_t(plat::create_window())); c.x[0] = 0; }
void hle_vi_vsync_event(CPU& c) {
    ensure_host();
    uint32_t ev = acnh_vi_get_vsync_event_and_start();
    auto st = find_event(ev);
    if (!st) st = std::make_shared<EventState>();
    attach_os_system_event(c, c.x[0], std::move(st));
    c.x[0] = 0;
}
void hle_nv_noop(CPU&) {}
NX_HLE("_ZN2nn2vi10InitializeEv", hle_ok);
NX_HLE("_ZN2nn2vi18OpenDefaultDisplayEPPNS0_7DisplayE", hle_vi_open_display);
NX_HLE("_ZN2nn2vi11CreateLayerEPPNS0_5LayerEPNS0_7DisplayE", hle_vi_create_layer);
NX_HLE("_ZN2nn2vi15GetNativeWindowEPPvPNS0_5LayerE", hle_vi_native_window);
NX_HLE("_ZN2nn2vi20GetDisplayVsyncEventEPNS_2os15SystemEventTypeEPNS0_7DisplayE", hle_vi_vsync_event);
NX_HLE("_ZN2nv20SetGraphicsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_", hle_nv_noop);
NX_HLE("_ZN2nv28SetGraphicsDevtoolsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_", hle_nv_noop);
NX_HLE("_ZN2nv18InitializeGraphicsEPvm", hle_nv_noop);

// net
void hle_net_avail(CPU& c) { ensure_host(); c.x[0] = acnh_net_is_network_available(); }
void hle_net_accepted(CPU& c) { ensure_host(); c.x[0] = acnh_net_is_any_internet_request_accepted(); }
void hle_net_proxy(CPU& c) { ensure_host(); c.x[0] = acnh_net_get_current_proxy_setting(c.x[0]); }
void hle_net_err_res(CPU& c) { ensure_host(); c.x[0] = acnh_net_handle_request_error(); }
void hle_net_resolve(CPU& c) { ensure_host(); c.x[0] = acnh_net_resolve_ex(c.x[0], c.x[1]); }
void hle_net_primary_ip(CPU& c) { ensure_host(); c.x[0] = acnh_net_get_current_primary_ip(c.x[0]); }
void hle_net_ip_cfg(CPU& c) {
    ensure_host();
    c.x[0] = acnh_net_get_current_ip_config_info(c.x[0], c.x[1], c.x[2], c.x[3], c.x[4]);
}
void hle_net_client_id(CPU& c) { c.x[0] = 1; }
void hle_net_htonl(CPU& c) { c.x[0] = bswap(uint32_t(c.x[0])); }
void hle_net_htons(CPU& c) { c.x[0] = bswap(uint16_t(c.x[0])); }
void hle_net_getaddrinfo(CPU& c) {
    if (hostnet_active()) return hostnet_GetAddrInfo(c);
    c.x[0] = 4;
}
void hle_net_freeaddrinfo(CPU& c) {
    if (hostnet_active()) return hostnet_FreeAddrInfo(c);
    c.x[0] = 0;
}
void hle_net_gethostbyname(CPU& c) {
    if (hostnet_active()) return hostnet_GetHostByName(c);
    c.x[0] = 0;
}
void hle_err_show(CPU& c) {
    const uint64_t code = c.x[0];
    if (code <= 0xffffffffull && (code & 0x1ff) == 115) {
        log("nn::err::%s(%#llx) nfp error, virtual amiibo %s (lr %s)", hle_current_name(), (unsigned long long)code,
            nfp_host::has_tag() ? "loaded" : "not loaded", describe(c.x[30]).c_str());
        return;
    }
    if (std::strstr(hle_current_name(), "ErrorResultVariant") && code > 0xffffffffull) {
        log("nn::err::%s variant %08x %08x %08x %08x (lr %s)", hle_current_name(), ld<uint32_t>(code),
            ld<uint32_t>(code + 4), ld<uint32_t>(code + 8), ld<uint32_t>(code + 12), describe(c.x[30]).c_str());
        return;
    }
    log("nn::err::%s(%#llx)%s", hle_current_name(), (unsigned long long)code,
        nextendo::is_nextendo() ? (nextendo::signed_in() ? " [Nextendo signed in]" : " [Nextendo not signed in]") : "");
}

NX_HLE("_ZN2nn4nifm10InitializeEv", hle_ok);
NX_HLE("_ZN2nn4nifm20SubmitNetworkRequestEv", hle_ok);
NX_HLE("_ZN2nn4nifm27SubmitNetworkRequestAndWaitEv", hle_ok);
NX_HLE("_ZN2nn4nifm22IsNetworkRequestOnHoldEv", hle_no);
NX_HLE("_ZN2nn4nifm20CancelNetworkRequestEv", hle_ok);
NX_HLE("_ZN2nn4nifm18IsNetworkAvailableEv", hle_net_avail);
NX_HLE("_ZN2nn4nifm31HandleNetworkRequestErrorResultEv", hle_net_err_res);
NX_HLE("_ZN2nn4nifm22GetCurrentProxySettingEPNS0_12ProxySettingE", hle_net_proxy);
NX_HLE("_ZN2nn4nifm19SetLocalNetworkModeEb", hle_ok);
NX_HLE("_ZN2nn4nifm11GetClientIdEv", hle_net_client_id);
NX_HLE("_ZN2nn4nifm30IsAnyForegroundRequestAcceptedEv", hle_net_accepted);
NX_HLE("_ZN2nn4nifm28IsAnyInternetRequestAcceptedENS0_8ClientIdE", hle_net_accepted);
NX_HLE("_ZN2nn4nifm26GetCurrentPrimaryIpAddressEP7in_addr", hle_net_primary_ip);
NX_HLE("_ZN2nn4nifm26GetCurrentPrimaryIpAddressEPNS_6socket6InAddrE", hle_net_primary_ip);
NX_HLE("_ZN2nn4nifm22GetCurrentIpConfigInfoEP7in_addrS2_S2_S2_S2_", hle_net_ip_cfg);
NX_HLE("_ZN2nn4nifm22GetCurrentIpConfigInfoEPNS_6socket6InAddrES3_S3_S3_S3_", hle_net_ip_cfg);
NX_HLE("_ZN2nn6socket10InitializeERKNS0_6ConfigE", hle_ok);
NX_HLE("_ZN2nn6socket10InitializeEPvmmi", hle_ok);
NX_HLE("_ZN2nn6socket8FinalizeEv", hle_ok);
NX_HLE("_ZN2nn6socket11GetAddrInfoEPKcS2_PKNS0_8AddrInfoEPPS3_", hle_net_getaddrinfo);
NX_HLE("_ZN2nn6socket11GetAddrInfoEPKcS2_PK8addrinfoPPS3_", hle_net_getaddrinfo);
NX_HLE("_ZN2nn6socket12FreeAddrInfoEPNS0_8AddrInfoE", hle_net_freeaddrinfo);
NX_HLE("_ZN2nn6socket12FreeAddrInfoEP8addrinfo", hle_net_freeaddrinfo);
NX_HLE("nnsocketGetHostByName", hle_net_gethostbyname);
NX_HLE("nnsocketGetHostByNameWithOptions", hle_net_gethostbyname);
NX_HLE("nnsocketResolverSetOption", hle_ok);
NX_HLE("nnsocketResolverGetOption", hle_ok);
NX_HLE("_ZN2nn6socket9InetHtonlEj", hle_net_htonl);
NX_HLE("_ZN2nn6socket9InetHtonsEt", hle_net_htons);
NX_HLE("_ZN2nn3nsd9ResolveExEPNS0_4FqdnERKS1_", hle_net_resolve);
NX_HLE("_ZN2nn3err9ShowErrorENS_6ResultE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS0_9ErrorCodeE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorERKNS0_18ErrorResultVariantE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS_6ResultERKNS0_15ResultBacktraceE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS_6ResultERKNS0_12ErrorContextE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS0_9ErrorCodeERKNS0_12ErrorContextE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS_6ResultERKNS0_16CallerIdentifierE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS_6ResultERKNS0_15ResultBacktraceERKNS0_16CallerIdentifierE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS_6ResultERKNS0_12ErrorContextERKNS0_16CallerIdentifierE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS0_9ErrorCodeERKNS0_16CallerIdentifierE", hle_err_show);
NX_HLE("_ZN2nn3err9ShowErrorENS0_9ErrorCodeERKNS0_12ErrorContextERKNS0_16CallerIdentifierE", hle_err_show);
NX_HLE("_ZN2nn3err20ShowApplicationErrorERKNS0_19ApplicationErrorArgE", hle_err_show);
NX_HLE("_ZN2nn3err15ShowSystemErrorERKNS0_14SystemErrorArgE", hle_err_show);

// friends
void hle_friends_empty(CPU& c) { if (c.x[0]) st<int32_t>(c.x[0], 0); c.x[0] = 0; }
void hle_friends_reg_key(CPU& c) { if (c.x[0]) std::memset(reinterpret_cast<void*>(c.x[0]), 0, 0x40); c.x[0] = 0; }
void hle_friends_async_ev(CPU& c) {
    auto e = std::make_shared<EventState>();
    e->signaled = true;
    attach_os_system_event(c, c.x[1], std::move(e));
    c.x[0] = 0;
}
void hle_friends_get_list(CPU& c) {
    ensure_host();
    c.x[0] = acnh_hle_friends_get_list(c.x[0], c.x[1], c.x[2], int32_t(c.x[3]), int32_t(c.x[4]), c.x[5]);
}
void hle_friends_update(CPU& c) {
    ensure_host();
    c.x[0] = acnh_hle_friends_update_info(c.x[0], c.x[1], c.x[2], int32_t(c.x[3]));
}
void hle_friends_ensure(CPU& c) { ensure_host(); c.x[0] = acnh_hle_friends_ensure_available(); }
void hle_friends_online_ok(CPU& c) { ensure_host(); c.x[0] = acnh_hle_friends_online_ok(); }
void hle_friends_open(CPU& c) { ensure_host(); c.x[0] = acnh_hle_friends_declare_playing(true); }
void hle_friends_close(CPU& c) { ensure_host(); c.x[0] = acnh_hle_friends_declare_playing(false); }

NX_HLE("_ZN2nn7friends10InitializeEv", hle_ok);
NX_HLE("_ZN2nn7friends13GetFriendListEPiPNS0_6FriendERKNS_7account3UidEiiRKNS0_12FriendFilterE", hle_friends_get_list);
NX_HLE("_ZN2nn7friends18GetBlockedUserListEPiPNS_7account23NetworkServiceAccountIdERKNS2_3UidEii", hle_friends_empty);
NX_HLE("_ZN2nn7friends29GetPlayHistoryRegistrationKeyEPNS0_26PlayHistoryRegistrationKeyERKNS_7account3UidEb",
       hle_friends_reg_key);
NX_HLE("_ZN2nn7friends14AddPlayHistoryERKNS_7account3UidERKNS0_26PlayHistoryRegistrationKeyERKNS0_15InAppScreenNameESA_",
       hle_ok);
NX_HLE("_ZN2nn7friends28DeclareOpenOnlinePlaySessionERKNS_7account3UidE", hle_friends_open);
NX_HLE("_ZN2nn7friends29DeclareCloseOnlinePlaySessionERKNS_7account3UidE", hle_friends_close);
NX_HLE("_ZN2nn7friends16UpdateFriendInfoEPNS0_6FriendERKNS_7account3UidEPKNS3_23NetworkServiceAccountIdEi",
       hle_friends_update);
NX_HLE("_ZN2nn7friends25EnsureFriendListAvailableEPNS0_12AsyncContextERKNS_7account3UidE", hle_friends_ensure);
NX_HLE("_ZN2nn7friends12AsyncContext14GetSystemEventEPNS_2os11SystemEventE", hle_friends_async_ev);
NX_HLE("_ZN2nn7friends18ShowUserDetailInfoERKNS_7account3UidERKNS1_23NetworkServiceAccountIdERKNS0_15InAppScreenNameESA_",
       hle_friends_online_ok);
NX_HLE("_ZN2nn7friends25StartSendingFriendRequestERKNS_7account3UidERKNS1_23NetworkServiceAccountIdERKNS0_15InAppScreenNameESA_",
       hle_friends_online_ok);

// nfp / nfc / hid::system NFC
void hle_nfp_init(CPU& c) { ensure_host(); nfp_host::ensure_nfc_services_ready(c); c.x[0] = acnh_nfp_initialize(false); }
void hle_nfp_fini(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_finalize(false); }
void hle_nfp_init_sys(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_initialize(false); }
void hle_nfp_state(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_get_state(false); }
void hle_nfp_list(CPU& c) {
    ensure_host();
    nfp_host::ensure_nfc_services_ready(c);
    c.x[0] = acnh_nfp_list_devices(c.x[0], c.x[1], int32_t(c.x[2]));
}
void hle_nfp_dev_state(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_get_device_state(false); }
void hle_nfp_dev_state_out(CPU& c) {
    ensure_host();
    if (c.x[0]) st<uint32_t>(c.x[0], acnh_nfp_get_device_state(false));
    c.x[0] = 0;
}
void hle_nfp_attach_avail(CPU& c) { nfp_host::bind_system_event(c, c.x[0], 2); nfp_host::signal_availability(); c.x[0] = 0; }
void hle_nfp_attach_act(CPU& c) { nfp_host::bind_system_event(c, c.x[0], 0); nfp_host::sync_activate_event_after_attach(); c.x[0] = 0; }
void hle_nfp_attach_deact(CPU& c) { nfp_host::bind_system_event(c, c.x[0], 1); c.x[0] = 0; }
void hle_nfp_npad(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_get_npad_id(c.x[0], c.x[1]); }
void hle_nfp_start_det(CPU& c) { ensure_host(); acnh_nfp_initialize(false); nfp_host::on_start_detection(); c.x[0] = 0; }
void hle_nfp_stop_det(CPU& c) { nfp_host::on_stop_detection(); c.x[0] = 0; }
void hle_nfp_tag_info(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_fill_info_checked(0, c.x[0], true); }
void hle_nfp_mount_full(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_mount(uint32_t(c.x[1]), uint32_t(c.x[2]), true); }
void hle_nfp_mount_short(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_mount(uint32_t(c.x[1]), 0, false); }
void hle_nfp_unmount(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_unmount(); }
void hle_nfp_model(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_fill_info_checked(1, c.x[0], false); }
void hle_nfp_reg(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_fill_info_checked(2, c.x[0], false); }
void hle_nfp_reg_priv(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_fill_info_checked(3, c.x[0], false); }
void hle_nfp_all(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_fill_info_checked(4, c.x[0], false); }
void hle_nfp_set_all(CPU& c) { ensure_host(); c.x[0] = (nfp_host::device_state() == nfp_host::DeviceState::TagMounted) ? 0u : (115u | (73u << 9)); }
void hle_nfp_common(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_fill_info_checked(5, c.x[0], false); }
void hle_nfp_admin(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_fill_info_checked(6, c.x[0], false); }
void hle_nfp_exists_app(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_exists_app_area(c.x[0]); }
void hle_nfp_open_app(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_open_app_area(uint32_t(c.x[1])); }
void hle_nfp_get_app(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_get_app_area(c.x[0], 0, uint32_t(c.x[1])); }
void hle_nfp_get_app_sz(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_get_app_area(c.x[0], c.x[1], uint32_t(c.x[3])); }
void hle_nfp_set_app(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_set_app_area(c.x[0], uint32_t(c.x[1])); }
void hle_nfp_set_app_sz(CPU& c) { ensure_host(); c.x[0] = acnh_nfp_set_app_area(c.x[1], uint32_t(c.x[2])); }
void hle_nfp_app_size(CPU& c) { if (c.x[0]) st<uint32_t>(c.x[0], nfp_host::kApplicationAreaSize); c.x[0] = 0; }
void hle_nfp_check_err(CPU& c) { c.x[0] = c.x[0] ? ld<uint32_t>(c.x[0]) : 0; }

NX_HLE("_ZN2nn3nfp10InitializeEv", hle_nfp_init);
NX_HLE("_ZN2nn3nfp8FinalizeEv", hle_nfp_fini);
NX_HLE("_ZN2nn3nfp16InitializeSystemEv", hle_nfp_init_sys);
NX_HLE("_ZN2nn3nfp14FinalizeSystemEv", hle_ok);
NX_HLE("_ZN2nn3nfp8GetStateEv", hle_nfp_state);
NX_HLE("_ZN2nn3nfp11ListDevicesEPNS0_12DeviceHandleEPii", hle_nfp_list);
NX_HLE("_ZN2nn3nfp14GetDeviceStateERKNS0_12DeviceHandleE", hle_nfp_dev_state);
NX_HLE("_ZN2nn3nfp14GetDeviceStateEPNS0_11DeviceStateERKNS0_12DeviceHandleE", hle_nfp_dev_state_out);
NX_HLE("_ZN2nn3nfp29AttachAvailabilityChangeEventEPNS_2os15SystemEventTypeE", hle_nfp_attach_avail);
NX_HLE("_ZN2nn3nfp19AttachActivateEventEPNS_2os15SystemEventTypeERKNS0_12DeviceHandleE", hle_nfp_attach_act);
NX_HLE("_ZN2nn3nfp21AttachDeactivateEventEPNS_2os15SystemEventTypeERKNS0_12DeviceHandleE", hle_nfp_attach_deact);
NX_HLE("_ZN2nn3nfp9GetNpadIdEPjRKNS0_12DeviceHandleE", hle_nfp_npad);
NX_HLE("_ZN2nn3nfp14StartDetectionERKNS0_12DeviceHandleE", hle_nfp_start_det);
NX_HLE("_ZN2nn3nfp13StopDetectionERKNS0_12DeviceHandleE", hle_nfp_stop_det);
NX_HLE("_ZN2nn3nfp10GetTagInfoEPNS0_7TagInfoERKNS0_12DeviceHandleE", hle_nfp_tag_info);
NX_HLE("_ZN2nn3nfp5MountERKNS0_12DeviceHandleENS0_9ModelTypeENS0_11MountTargetE", hle_nfp_mount_full);
NX_HLE("_ZN2nn3nfp5MountERKNS0_12DeviceHandleENS0_9ModelTypeE", hle_nfp_mount_short);
NX_HLE("_ZN2nn3nfp7UnmountERKNS0_12DeviceHandleE", hle_nfp_unmount);
NX_HLE("_ZN2nn3nfp12GetModelInfoEPNS0_9ModelInfoERKNS0_12DeviceHandleE", hle_nfp_model);
NX_HLE("_ZN2nn3nfp15GetRegisterInfoEPNS0_12RegisterInfoERKNS0_12DeviceHandleE", hle_nfp_reg);
NX_HLE("_ZN2nn3nfp15GetRegisterInfoEPNS0_19RegisterInfoPrivateERKNS0_12DeviceHandleE", hle_nfp_reg_priv);
NX_HLE("_ZN2nn3nfp6GetAllEPNS0_7NfpDataERKNS0_12DeviceHandleE", hle_nfp_all);
NX_HLE("_ZN2nn3nfp6SetAllERKNS0_12DeviceHandleERKNS0_7NfpDataE", hle_nfp_set_all);
NX_HLE("_ZN2nn3nfp13GetCommonInfoEPNS0_10CommonInfoERKNS0_12DeviceHandleE", hle_nfp_common);
NX_HLE("_ZN2nn3nfp12GetAdminInfoEPNS0_9AdminInfoERKNS0_12DeviceHandleE", hle_nfp_admin);
NX_HLE("_ZN2nn3nfp21ExistsApplicationAreaEPbRKNS0_12DeviceHandleE", hle_nfp_exists_app);
NX_HLE("_ZN2nn3nfp19OpenApplicationAreaERKNS0_12DeviceHandleEj", hle_nfp_open_app);
NX_HLE("_ZN2nn3nfp18GetApplicationAreaEPvjRKNS0_12DeviceHandleE", hle_nfp_get_app);
NX_HLE("_ZN2nn3nfp18GetApplicationAreaEPvPmRKNS0_12DeviceHandleEm", hle_nfp_get_app_sz);
NX_HLE("_ZN2nn3nfp18SetApplicationAreaEPKvjRKNS0_12DeviceHandleE", hle_nfp_set_app);
NX_HLE("_ZN2nn3nfp18SetApplicationAreaERKNS0_12DeviceHandleEPKvm", hle_nfp_set_app_sz);
NX_HLE("_ZN2nn3nfp5FlushERKNS0_12DeviceHandleE", hle_nfp_set_all);
NX_HLE("_ZN2nn3nfp7RestoreERKNS0_12DeviceHandleE", hle_nfp_set_all);
NX_HLE("_ZN2nn3nfp21GetApplicationAreaSizeEPjRKNS0_12DeviceHandleE", hle_nfp_app_size);
NX_HLE("_ZN2nn3nfc10InitializeEv", hle_nfp_init);
NX_HLE("_ZN2nn3nfc8FinalizeEv", hle_nfp_fini);
NX_HLE("_ZN2nn3nfc16InitializeSystemEv", hle_nfp_init_sys);
NX_HLE("_ZN2nn3nfc14FinalizeSystemEv", hle_ok);
NX_HLE("_ZN2nn3nfc8GetStateEv", hle_nfp_state);
NX_HLE("_ZN2nn3nfc11ListDevicesEPNS0_12DeviceHandleEPii", hle_nfp_list);
NX_HLE("_ZN2nn3nfc14GetDeviceStateERKNS0_12DeviceHandleE", hle_nfp_dev_state);
NX_HLE("_ZN2nn3nfc29AttachAvailabilityChangeEventEPNS_2os15SystemEventTypeE", hle_nfp_attach_avail);
NX_HLE("_ZN2nn3nfc19AttachActivateEventEPNS_2os15SystemEventTypeERKNS0_12DeviceHandleE", hle_nfp_attach_act);
NX_HLE("_ZN2nn3nfc21AttachDeactivateEventEPNS_2os15SystemEventTypeERKNS0_12DeviceHandleE", hle_nfp_attach_deact);
NX_HLE("_ZN2nn3nfc9GetNpadIdEPjRKNS0_12DeviceHandleE", hle_nfp_npad);
NX_HLE("_ZN2nn3nfc14StartDetectionERKNS0_12DeviceHandleENS0_11NfcProtocolE", hle_nfp_start_det);
NX_HLE("_ZN2nn3nfc14StartDetectionERKNS0_12DeviceHandleE", hle_nfp_start_det);
NX_HLE("_ZN2nn3nfc13StopDetectionERKNS0_12DeviceHandleE", hle_nfp_stop_det);
NX_HLE("_ZN2nn3nfc10GetTagInfoEPNS0_7TagInfoERKNS0_12DeviceHandleE", hle_nfp_tag_info);
NX_HLE("_ZN2nn3nfc12IsNfcEnabledEv", hle_one);
NX_HLE("_ZN2nn3nfp6detail10CheckErrorERKNS_6ResultE", hle_nfp_check_err);
NX_HLE("_ZN2nn3nfc6detail10CheckErrorERKNS_6ResultE", hle_nfp_check_err);

void hle_hid_nfc_abs(CPU& c) { c.x[0] = acnh_hid_nfc_get_abstracted_pad_id(c.x[0], uint32_t(c.x[1])); }
void hle_hid_nfc_npads(CPU& c) { nfp_host::ensure_nfc_services_ready(c); c.x[0] = acnh_hid_nfc_get_npads(c.x[0], int32_t(c.x[1])); }
void hle_hid_nfc_detail_npads(CPU& c) { c.x[0] = acnh_hid_nfc_detail_get_npads(c.x[0], c.x[1], int32_t(c.x[2])); }
void hle_hid_nfc_detail_act(CPU& c) { if (c.x[0]) st<uint32_t>(c.x[0], nfp_host::kVirtualNpadId); c.x[0] = 0; }
void hle_hid_nfc_detail_is_act(CPU& c) { if (c.x[0]) st<uint8_t>(c.x[0], 1); c.x[0] = 0; }
void hle_hid_nfc_info(CPU& c) {
    if (c.x[0]) {
        std::memset(reinterpret_cast<void*>(c.x[0]), 0, 0x40);
        if (nfp_host::has_tag()) st<uint8_t>(c.x[0], 1);
    }
    c.x[0] = 0;
}
void hle_hid_nfc_bind_act(CPU& c) { nfp_host::bind_system_event(c, c.x[1], 0); c.x[0] = 0; }
void hle_hid_nfc_bind_upd(CPU& c) { nfp_host::bind_system_event(c, c.x[1], 2); nfp_host::signal_availability(); c.x[0] = 0; }

NX_HLE("_ZN2nn8settings6system12IsNfcEnabledEv", hle_one);
NX_HLE("_ZN2nn8settings6system13SetNfcEnabledEb", hle_ok);
NX_HLE("_ZN2nn3hid6system32GetAbstractedPadIdForNpadWithNfcEPNS1_15AbstractedPadIdEj", hle_hid_nfc_abs);
NX_HLE("_ZN2nn3hid6system6detail32GetAbstractedPadIdForNpadWithNfcEPNS1_15AbstractedPadIdEj", hle_hid_nfc_abs);
NX_HLE("_ZN2nn3hid6system15GetNpadsWithNfcEPji", hle_hid_nfc_npads);
NX_HLE("_ZN2nn3hid6system6detail15GetNpadsWithNfcEPiPji", hle_hid_nfc_detail_npads);
NX_HLE("_ZN2nn3hid6system17StartNfcDiscoveryERKNS1_21NfcDiscoveryParameterENS1_15AbstractedPadIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system6detail17StartNfcDiscoveryERKNS1_21NfcDiscoveryParameterENS1_15AbstractedPadIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system16StopNfcDiscoveryENS1_15AbstractedPadIdE", hle_nfp_stop_det);
NX_HLE("_ZN2nn3hid6system6detail16StopNfcDiscoveryENS1_15AbstractedPadIdE", hle_nfp_stop_det);
NX_HLE("_ZN2nn3hid6system11ActivateNfcEj", hle_ok);
NX_HLE("_ZN2nn3hid6system11ActivateNfcEjNS_6applet20AppletResourceUserIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system6detail11ActivateNfcERjNS_6applet20AppletResourceUserIdE", hle_hid_nfc_detail_act);
NX_HLE("_ZN2nn3hid6system13DeactivateNfcEj", hle_ok);
NX_HLE("_ZN2nn3hid6system13DeactivateNfcEjNS_6applet20AppletResourceUserIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system6detail13DeactivateNfcERjNS_6applet20AppletResourceUserIdE", hle_hid_nfc_detail_act);
NX_HLE("_ZN2nn3hid6system14IsNfcActivatedEj", hle_one);
NX_HLE("_ZN2nn3hid6system6detail14IsNfcActivatedEPbRj", hle_hid_nfc_detail_is_act);
NX_HLE("_ZN2nn3hid6system19CheckNfcDevicePowerENS1_15AbstractedPadIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system6detail19CheckNfcDevicePowerENS1_15AbstractedPadIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system10GetNfcInfoEPNS1_7NfcInfoENS1_15AbstractedPadIdE", hle_hid_nfc_info);
NX_HLE("_ZN2nn3hid6system6detail10GetNfcInfoEPNS1_7NfcInfoENS1_15AbstractedPadIdE", hle_hid_nfc_info);
NX_HLE("_ZN2nn3hid6system11SetNfcEventEPNS_2os15SystemEventTypeES4_NS1_15AbstractedPadIdE", hle_hid_nfc_bind_upd);
NX_HLE("_ZN2nn3hid6system6detail11SetNfcEventEPNS_2os15SystemEventTypeES5_NS1_15AbstractedPadIdE", hle_hid_nfc_bind_upd);
NX_HLE("_ZN2nn3hid6system20BindNfcActivateEventEjPNS_2os15SystemEventTypeENS2_14EventClearModeE", hle_hid_nfc_bind_act);
NX_HLE("_ZN2nn3hid6system6detail20BindNfcActivateEventERjPNS_2os15SystemEventTypeENS4_14EventClearModeE",
       hle_hid_nfc_bind_act);
NX_HLE("_ZN2nn3hid6system24BindNfcDeviceUpdateEventEPNS_2os15SystemEventTypeENS2_14EventClearModeE",
       hle_hid_nfc_bind_upd);
NX_HLE("_ZN2nn3hid6system6detail24BindNfcDeviceUpdateEventEPNS_2os15SystemEventTypeENS3_14EventClearModeE",
       hle_hid_nfc_bind_upd);
NX_HLE("_ZN2nn3hid6system14SendNfcRawDataERKNS1_20NfcPassThruParameterENS1_15AbstractedPadIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system6detail14SendNfcRawDataERKNS1_20NfcPassThruParameterENS1_15AbstractedPadIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system19GetMcuVersionForNfcEPNS1_20McuVersionDataForNfcENS1_15AbstractedPadIdE", hle_ok);
NX_HLE("_ZN2nn3hid6system6detail19GetMcuVersionForNfcEPNS1_20McuVersionDataForNfcENS1_15AbstractedPadIdE", hle_ok);

// ldn
void hle_ldn_disabled(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_disabled(); }
void hle_ldn_init(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_initialize(); }
void hle_ldn_fini(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_finalize(); }
void hle_ldn_state(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_get_state(); }
void hle_ldn_attach(CPU& c) {
    ensure_host();
    uint32_t ev = host_event_create(false);
    acnh_ldn_set_state_event(ev);
    host_attach_system_event(&c, c.x[0], ev);
}
void hle_ldn_open_ap(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_open_access_point(); }
void hle_ldn_close_ap(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_close_access_point(); }
void hle_ldn_open_st(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_open_station(); }
void hle_ldn_close_st(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_close_station(); }
void hle_ldn_create_net(CPU& c) {
    ensure_host();
    c.x[0] = acnh_ldn_create_network(reinterpret_cast<const uint8_t*>(c.x[0]), reinterpret_cast<const uint8_t*>(c.x[1]),
                                     nullptr, reinterpret_cast<const uint8_t*>(c.x[2]));
}
void hle_ldn_create_net_priv(CPU& c) {
    ensure_host();
    c.x[0] = acnh_ldn_create_network(reinterpret_cast<const uint8_t*>(c.x[0]), reinterpret_cast<const uint8_t*>(c.x[1]),
                                     reinterpret_cast<const uint8_t*>(c.x[2]), reinterpret_cast<const uint8_t*>(c.x[3]));
}
void hle_ldn_destroy_net(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_destroy_network(); }
void hle_ldn_set_adv(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_set_advertise_data(reinterpret_cast<const uint8_t*>(c.x[0]), c.x[1]); }
void hle_ldn_set_policy(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_set_station_accept_policy(uint32_t(c.x[0])); }
void hle_ldn_scan(CPU& c) {
    int found = c.x[0] && c.x[3]
                    ? link_scan(reinterpret_cast<uint8_t*>(c.x[0]), int(c.x[2]), reinterpret_cast<const uint8_t*>(c.x[3]))
                    : 0;
    if (c.x[1]) st<int32_t>(c.x[1], found);
    c.x[0] = 0;
}
void hle_ldn_connect(CPU& c) {
    ensure_host();
    c.x[0] = acnh_ldn_connect(reinterpret_cast<const uint8_t*>(c.x[0]), reinterpret_cast<const uint8_t*>(c.x[2]),
                              uint16_t(c.x[3]));
}
void hle_ldn_disconnect(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_disconnect(); }
void hle_ldn_net_info(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_get_network_info(reinterpret_cast<uint8_t*>(c.x[0])); }
void hle_ldn_net_cfg(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_get_network_config(reinterpret_cast<uint8_t*>(c.x[0])); }
void hle_ldn_sec_param(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_get_security_parameter(reinterpret_cast<uint8_t*>(c.x[0])); }
void hle_ldn_ipv4(CPU& c) {
    if (c.x[0]) st<uint32_t>(c.x[0], acnh_ldn_own_ip());
    if (c.x[1]) st<uint32_t>(c.x[1], 0xFFFFFF00u);
    c.x[0] = 0;
}
void hle_ldn_bcast(CPU& c) {
    uint32_t ip = uint32_t(c.x[0]), mask = uint32_t(c.x[1]);
    c.x[0] = (ip & mask) | ~mask;
}
void hle_ldn_disc_reason(CPU& c) { ensure_host(); c.x[0] = acnh_ldn_get_disconnect_reason(); }

NX_HLE("_ZN2nn3ldn10InitializeEv", hle_ldn_init);
NX_HLE("_ZN2nn3ldn16InitializeSystemEv", hle_ldn_init);
NX_HLE("_ZN2nn3ldn16InitializeSystemEi", hle_ldn_init);
NX_HLE("_ZN2nn3ldn17InitializeMonitorEv", hle_ldn_disabled);
NX_HLE("_ZN2nn3ldn8FinalizeEv", hle_ldn_fini);
NX_HLE("_ZN2nn3ldn14FinalizeSystemEv", hle_ldn_fini);
NX_HLE("_ZN2nn3ldn15FinalizeMonitorEv", hle_ldn_fini);
NX_HLE("_ZN2nn3ldn8GetStateEv", hle_ldn_state);
NX_HLE("_ZN2nn3ldn22AttachStateChangeEventEPNS_2os15SystemEventTypeE", hle_ldn_attach);
NX_HLE("_ZN2nn3ldn15OpenAccessPointEv", hle_ldn_open_ap);
NX_HLE("_ZN2nn3ldn16CloseAccessPointEv", hle_ldn_close_ap);
NX_HLE("_ZN2nn3ldn11OpenStationEv", hle_ldn_open_st);
NX_HLE("_ZN2nn3ldn12CloseStationEv", hle_ldn_close_st);
NX_HLE("_ZN2nn3ldn13CreateNetworkERKNS0_13NetworkConfigERKNS0_14SecurityConfigERKNS0_10UserConfigE", hle_ldn_create_net);
NX_HLE("_ZN2nn3ldn20CreateNetworkPrivateERKNS0_13NetworkConfigERKNS0_14SecurityConfigERKNS0_17SecurityParameterERKNS0_"
       "10UserConfigEiPKNS0_12AddressEntryE",
       hle_ldn_create_net_priv);
NX_HLE("_ZN2nn3ldn14DestroyNetworkEv", hle_ldn_destroy_net);
NX_HLE("_ZN2nn3ldn16SetAdvertiseDataEPKvm", hle_ldn_set_adv);
NX_HLE("_ZN2nn3ldn22SetStationAcceptPolicyENS0_12AcceptPolicyE", hle_ldn_set_policy);
NX_HLE("_ZN2nn3ldn4ScanEPNS0_11NetworkInfoEPiiRKNS0_10ScanFilterEi", hle_ldn_scan);
NX_HLE("_ZN2nn3ldn11ScanPrivateEPNS0_11NetworkInfoEPiiRKNS0_10ScanFilterEi", hle_ldn_scan);
NX_HLE("_ZN2nn3ldn7ConnectERKNS0_11NetworkInfoERKNS0_14SecurityConfigERKNS0_10UserConfigEiNS0_13ConnectOptionE",
       hle_ldn_connect);
NX_HLE("_ZN2nn3ldn14ConnectPrivateERKNS0_13NetworkConfigERKNS0_14SecurityConfigERKNS0_17SecurityParameterERKNS0_"
       "10UserConfigEiNS0_13ConnectOptionE",
       hle_ldn_connect);
NX_HLE("_ZN2nn3ldn10DisconnectEv", hle_ldn_disconnect);
NX_HLE("_ZN2nn3ldn14GetNetworkInfoEPNS0_11NetworkInfoE", hle_ldn_net_info);
NX_HLE("_ZN2nn3ldn16GetNetworkConfigEPNS0_13NetworkConfigE", hle_ldn_net_cfg);
NX_HLE("_ZN2nn3ldn20GetSecurityParameterEPNS0_17SecurityParameterE", hle_ldn_sec_param);
NX_HLE("_ZN2nn3ldn14GetIpv4AddressEPNS0_11Ipv4AddressEPNS0_10SubnetMaskE", hle_ldn_ipv4);
NX_HLE("_ZN2nn3ldn20MakeBroadcastAddressENS0_11Ipv4AddressENS0_10SubnetMaskE", hle_ldn_bcast);
NX_HLE("_ZN2nn3ldn19GetDisconnectReasonEv", hle_ldn_disc_reason);
NX_HLE("_ZN2nn3ldn6RejectENS0_11Ipv4AddressE", hle_ok);

// movie
void hle_movie_dtor(CPU& c) { acnh_movie_destroy(c.x[0]); c.x[0] = 0; }
void hle_movie_dtor_del(CPU& c) { acnh_movie_destroy(c.x[0]); c.x[0] = 0; }
void hle_movie_configure(CPU& c) {
    ensure_host();
    uint64_t md = c.x[1];
    static const uint64_t find_i32 = find_symbol("_ZNK5movie9MediaData7FindIntEPKcPi");
    static const uint64_t find_buf = find_symbol("_ZNK5movie9MediaData8FindDataEPKcPPKvPm");
    auto get_i32 = [&](const char* k) -> int32_t {
        if (!find_i32 || !md) return 0;
        uint64_t ks = nvn::scratch(c, 32), out = nvn::scratch(c, 8);
        std::strncpy(reinterpret_cast<char*>(ks), k, 31);
        if (!nvn::guest_call(c, find_i32, {md, ks, out})) return 0;
        return ld<int32_t>(out);
    };
    int32_t w = get_i32("width"), h = get_i32("height"), ch = get_i32("channel-count"), sr = get_i32("sample-rate");
    const uint8_t* extra = nullptr;
    size_t extra_sz = 0;
    if (find_buf && md) {
        uint64_t ks = nvn::scratch(c, 32), ptr = nvn::scratch(c, 8), sz = nvn::scratch(c, 8);
        std::strcpy(reinterpret_cast<char*>(ks), "csd-0");
        if (nvn::guest_call(c, find_buf, {md, ks, ptr, sz})) {
            extra = reinterpret_cast<const uint8_t*>(ld<uint64_t>(ptr));
            extra_sz = size_t(ld<uint64_t>(sz));
        }
    }
    c.x[0] = acnh_movie_configure(c.x[0], w, h, ch, sr, extra, extra_sz);
}
void hle_movie_reg_events(CPU& c) {
    ensure_host();
    uint32_t in_ev = host_event_create(false), out_ev = host_event_create(false), err_ev = host_event_create(false);
    if (c.x[1]) st<uint32_t>(c.x[1], event_handle_from_id(in_ev));
    if (c.x[2]) st<uint32_t>(c.x[2], event_handle_from_id(out_ev));
    if (c.x[3]) st<uint32_t>(c.x[3], event_handle_from_id(err_ev));
    c.x[0] = acnh_movie_register_events(c.x[0], in_ev, out_ev, err_ev);
}
void hle_movie_get_out_fmt(CPU& c) {
    ensure_host();
    bool is_audio = false;
    int32_t w = 0, h = 0, ch = 0, sr = 0;
    uint32_t rc = acnh_movie_get_output_format(c.x[0], &is_audio, &w, &h, &ch, &sr);
    uint64_t md = c.x[1];
    static const uint64_t set_i32 = find_symbol("_ZN5movie9MediaData6SetIntEPKci");
    if (rc == 0 && md && set_i32) {
        auto put = [&](const char* k, int32_t v) {
            uint64_t ks = nvn::scratch(c, 32);
            std::strncpy(reinterpret_cast<char*>(ks), k, 31);
            nvn::guest_call(c, set_i32, {md, ks, uint64_t(uint32_t(v))});
        };
        if (is_audio) {
            put("channel-count", ch);
            put("sample-rate", sr);
        } else {
            put("width", w);
            put("height", h);
            put("stride", w);
            put("slice-height", h);
            put("color-format", 4);
        }
    }
    c.x[0] = rc;
}
void hle_movie_start(CPU& c) { c.x[0] = acnh_movie_start(c.x[0]); }
void hle_movie_stop(CPU& c) { c.x[0] = acnh_movie_stop(c.x[0]); }
void hle_movie_flush(CPU& c) { c.x[0] = acnh_movie_flush(c.x[0]); }
void hle_movie_acq_in(CPU& c) {
    int32_t idx = 0, rem = 0;
    uint32_t rc = acnh_movie_acquire_input(c.x[0], &idx, &rem);
    if (rc == 0) {
        if (c.x[1]) st<int32_t>(c.x[1], idx);
        if (c.x[2]) st<int32_t>(c.x[2], rem);
    }
    c.x[0] = rc;
}
void hle_movie_get_in(CPU& c) {
    static const uint64_t set_range = find_symbol("_ZN5movie6Buffer8SetRangeEii");
    if (c.x[2] && set_range) nvn::guest_call(c, set_range, {c.x[2], 0, 0});
    c.x[0] = 0;
}
void hle_movie_send_in(CPU& c) {
    static const uint64_t base = find_symbol("_ZNK5movie6Buffer4BaseEv");
    static const uint64_t size = find_symbol("_ZNK5movie6Buffer4SizeEv");
    uint64_t buf = c.x[2];
    const uint8_t* ptr = base && buf ? reinterpret_cast<const uint8_t*>(nvn::guest_call(c, base, {buf})) : nullptr;
    size_t sz = size && buf ? size_t(nvn::guest_call(c, size, {buf})) : 0;
    c.x[0] = acnh_movie_send_input(c.x[0], int32_t(c.x[1]), ptr, sz, int64_t(c.x[3]), uint32_t(c.x[4]));
}
void hle_movie_acq_out(CPU& c) {
    int32_t idx = 0, rem = 0;
    int64_t pts = 0;
    uint32_t flags = 0;
    uint32_t rc = acnh_movie_acquire_output(c.x[0], &idx, &pts, &flags, &rem);
    if (rc == 0) {
        if (c.x[1]) st<int32_t>(c.x[1], idx);
        if (c.x[2]) st<int64_t>(c.x[2], pts);
        if (c.x[3]) st<uint32_t>(c.x[3], flags);
        if (c.x[4]) st<int32_t>(c.x[4], rem);
    }
    c.x[0] = rc;
}
void hle_movie_get_out(CPU& c) {
    static const uint64_t base = find_symbol("_ZNK5movie6Buffer4BaseEv");
    static const uint64_t cap_fn = find_symbol("_ZNK5movie6Buffer8CapacityEv");
    static const uint64_t set_range = find_symbol("_ZN5movie6Buffer8SetRangeEii");
    uint64_t buf = c.x[2];
    uint64_t dst = base && buf ? nvn::guest_call(c, base, {buf}) : 0;
    size_t cap = cap_fn && buf ? size_t(nvn::guest_call(c, cap_fn, {buf})) : 0;
    size_t written = 0;
    uint32_t rc = acnh_movie_copy_output(c.x[0], int32_t(c.x[1]), dst, cap, &written);
    if (rc == 0 && set_range && buf) nvn::guest_call(c, set_range, {buf, 0, written});
    c.x[0] = rc;
}
void hle_movie_get_out_sz(CPU& c) {
    int32_t sz = 0, cnt = 0;
    uint32_t rc = acnh_movie_get_output_buffer_size(c.x[0], &sz, &cnt);
    if (rc == 0) {
        if (c.x[1]) st<int32_t>(c.x[1], sz);
        if (c.x[2]) st<int32_t>(c.x[2], cnt);
    }
    c.x[0] = rc;
}
void hle_movie_rel_out(CPU& c) { c.x[0] = acnh_movie_release_output(c.x[0], int32_t(c.x[1])); }
void hle_movie_get_out_fmt_wrap(CPU& c) {
    uint64_t self = c.x[0], md = c.x[3];
    hle_movie_get_out(c);
    if (c.x[0] == 0 && md) {
        uint64_t saved = c.x[1];
        c.x[0] = self, c.x[1] = md;
        hle_movie_get_out_fmt(c);
        c.x[1] = saved;
    }
}
void hle_movie_get_out_buf_fmt(CPU& c) { c.x[1] = c.x[2]; hle_movie_get_out_fmt(c); }

uint64_t movie_vtable() {
    static uint64_t vt = 0;
    static std::once_flag once;
    std::call_once(once, [] {
        uint64_t mem = kernel_alloc(0x1000);
        st<uint64_t>(mem, 0);
        st<uint64_t>(mem + 8, 0);
        struct Slot {
            const char* name;
            HleFn fn;
        };
        const Slot slots[] = {
            {"movie::Decoder::~Decoder", hle_movie_dtor},
            {"movie::Decoder::~Decoder(deleting)", hle_movie_dtor_del},
            {"movie::Decoder::Configure", hle_movie_configure},
            {"movie::Decoder::RegisterClientEvents", hle_movie_reg_events},
            {"movie::Decoder::GetOutputFormat", hle_movie_get_out_fmt},
            {"movie::Decoder::Start", hle_movie_start},
            {"movie::Decoder::Stop", hle_movie_stop},
            {"movie::Decoder::Reset", hle_movie_flush},
            {"movie::Decoder::Flush", hle_movie_flush},
            {"movie::Decoder::AcquireInputBufferIndex", hle_movie_acq_in},
            {"movie::Decoder::GetInputBuffer", hle_movie_get_in},
            {"movie::Decoder::SendInputBufferForDecode", hle_movie_send_in},
            {"movie::Decoder::AcquireOutputBufferIndex", hle_movie_acq_out},
            {"movie::Decoder::GetOutputBuffer", hle_movie_get_out},
            {"movie::Decoder::GetOutputBufferSize", hle_movie_get_out_sz},
            {"movie::Decoder::ReleaseOutputBufferIndex", hle_movie_rel_out},
            {"movie::Decoder::GetLastError", hle_ok},
            {"movie::Decoder::GetOutputBuffer(format)", hle_movie_get_out_fmt_wrap},
            {"movie::Decoder::GetOutputBufferFormat", hle_movie_get_out_buf_fmt},
        };
        int k = 0;
        for (const auto& s : slots) st<uint64_t>(mem + 16 + 8 * k++, hle_address_or(s.name, s.fn));
        vt = mem + 16;
    });
    return vt;
}

void hle_movie_ctor(CPU& c) {
    ensure_host();
    int type = int(c.x[1]);
#ifdef NX_SERVICES_HAVE_VORBIS
    bool ours = type == 6 || type == 7 || type == 11;
#else
    bool ours = type == 6 || type == 7;
#endif
    if (!ours) {
        static const uint64_t real =
            find_symbol("_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeEi");
        call(c, real);
        return;
    }
    st<uint64_t>(c.x[0], movie_vtable());
    st<uint64_t>(c.x[0] + 8, 0);
    acnh_movie_create(c.x[0], type);
}
NX_HLE("_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeEi", hle_movie_ctor);

}  // namespace

// ---- Exported C++ Symbols Called by Other Runtime Files -------------------------------------------------------------

uint64_t get_info(uint32_t type, uint32_t handle, uint64_t sub, uint32_t& rc) {
    ensure_host();
    return acnh_svc_get_info(type, handle, sub, &rc);
}
uint32_t set_thread_core_mask(uint32_t handle, int32_t ideal, uint64_t mask) {
    ensure_host();
    return acnh_svc_set_thread_core_mask(handle, ideal, mask);
}
uint32_t map_transfer_memory(uint32_t handle, uint64_t addr, uint64_t size) {
    ensure_host();
    return acnh_svc_map_transfer_memory(handle, addr, size);
}
uint32_t unmap_transfer_memory(uint32_t handle, uint64_t addr, uint64_t size) {
    ensure_host();
    return acnh_svc_unmap_transfer_memory(handle, addr, size);
}
void set_tls_template(uint64_t start, uint64_t end) { acnh_svc_set_tls_template(start, end); }
void init_tls_block(uint64_t block) { acnh_svc_init_tls_block(block); }

uint64_t display_period_ns() { ensure_host(); return acnh_vi_display_period_ns(); }
uint64_t tick_period_ns() { ensure_host(); return acnh_vi_tick_period_ns(); }
void set_present_interval(int interval) { acnh_vi_set_present_interval(interval); }
void vblank_hold() { acnh_vi_vblank_hold(); }

std::string ldn_visited_island() {
    ensure_host();
    char buf[128];
    size_t n = acnh_ldn_visited_island(buf, sizeof buf);
    return n > 0 ? std::string(buf, n) : std::string();
}
bool ldn_enabled() { return acnh_ldn_enabled(); }
bool ldn_net_up() { return acnh_ldn_net_up(); }
uint32_t ldn_own_ip() { return acnh_ldn_own_ip(); }
uint32_t ldn_broadcast_ip() { return acnh_ldn_broadcast_ip(); }
bool ldn_info_valid(uint8_t* info) { return acnh_ldn_info_valid(info); }
bool ldn_link_info(uint8_t* out) { return acnh_ldn_link_info(out); }
int ldn_link_add_node(const uint8_t* station, uint32_t own_ip) { return acnh_ldn_link_add_node(station, own_ip); }
void ldn_link_remove_node(int n) { acnh_ldn_link_remove_node(n); }
int ldn_peer_ips(uint32_t* out) { return acnh_ldn_peer_ips(out); }
void ldn_link_set_info(const uint8_t* info) { acnh_ldn_link_set_info(info); }
void ldn_link_lost() { acnh_ldn_link_lost(); }

}  // namespace nx
