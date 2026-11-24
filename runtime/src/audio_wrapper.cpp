// Thin C++ wrapper forwarding nn::audio HLE (AudioOut, AudioIn, AudioRenderer,
// device enumeration, and I3DL2 reverb bypass) to crates/runtime/src/audio.rs.
#include <algorithm>
#include <chrono>
#include <cstring>
#include <ctime>
#include <memory>
#include <mutex>
#include <vector>

#include <nx/runtime.h>

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#endif

#include "kernel.h"
#include "platform.h"

#ifndef NX_RUST_RUNTIME
extern "C" {
struct AcnhAudioHost {
    bool (*audio_open)(void (*done)(void*), int32_t channels);
    bool (*audio_submit)(const void* data, size_t bytes, void* ctx);
    uint32_t (*audio_queued)();
    uint64_t (*monotonic_ns)();
    void (*sleep_ns)(int64_t ns);
    void (*log)(const char* msg);
    void (*signal_out_event)();
    void (*clear_out_event)();
    void (*signal_frame_event)();
};
void acnh_audio_set_host(const AcnhAudioHost* host);
uint64_t acnh_audio_active_output_channel_count();
int32_t acnh_audio_list_output_device_name(uint64_t names_ptr, int32_t count);
int32_t acnh_audio_get_active_output_device_name(uint64_t name_ptr);
void acnh_audio_out_init_parameter(uint64_t param_ptr);
uint64_t acnh_audio_out_open(uint64_t out_ptr, uint64_t param_ptr);
uint64_t acnh_audio_out_start();
void acnh_audio_out_stop();
void acnh_audio_out_close();
uint64_t acnh_audio_out_sample_rate();
uint64_t acnh_audio_out_channel_count();
uint64_t acnh_audio_out_sample_format();
void acnh_audio_out_set_volume(float vol);
uint64_t acnh_audio_out_buffer_data_size(uint64_t buf_ptr);
void acnh_audio_out_set_buffer_info(uint64_t buf_ptr, uint64_t data, uint64_t capacity, uint64_t size);
uint64_t acnh_audio_out_buffer_data_pointer(uint64_t buf_ptr);
uint64_t acnh_audio_out_append_buffer(uint64_t buf_ptr);
uint64_t acnh_audio_out_get_released_buffer();
uint64_t acnh_audio_list_audio_ins();
uint64_t acnh_audio_open_audio_in();
bool acnh_audio_renderer_opened(uint64_t rc, uint64_t handle, uint64_t vtable_sym, uint64_t sdk_base,
                                uint64_t get_exec_mode_sym, uint64_t sys_exec_sym, uint64_t impl_exec_sym,
                                uint64_t min_addr, uint64_t max_addr);
uint64_t acnh_audio_execute_rendering_addr();
void acnh_audio_capture_device_sink(uint64_t proc_addr, uint64_t gen_device_sink_sym);
void acnh_audio_before_render_system_update();
void acnh_audio_after_render_system_update();
void acnh_audio_before_execute_rendering();
void acnh_audio_after_execute_rendering();
size_t acnh_audio_render_tick_begin(uint64_t* out_impls, size_t max_impls, uint64_t* out_t0);
void acnh_audio_render_tick_end(uint64_t t0, uint64_t last_rc);
bool acnh_audio_reverb_should_bypass();
bool acnh_audio_reverb_should_benchmark(bool off, uint64_t enabled_arg);
void acnh_audio_reverb_log_benchmark(uint64_t median_ns, uint64_t best_ns);
}
#endif

namespace nx {

namespace {

std::shared_ptr<EventState> g_out_event = std::make_shared<EventState>();
std::shared_ptr<EventState> g_audio_frame = std::make_shared<EventState>();

bool host_audio_open(void (*done)(void*), int32_t channels) { return plat::audio_open(done, channels); }
bool host_audio_submit(const void* data, size_t bytes, void* ctx) { return plat::audio_submit(data, bytes, ctx); }
uint32_t host_audio_queued() { return plat::audio_queued(); }
uint64_t host_monotonic_ns() { return plat::monotonic_ns(); }
void host_sleep_ns(int64_t ns) { plat::sleep_ns(ns); }
void host_log(const char* msg) {
    if (msg) log("%s", msg);
}
void host_signal_out_event() {
    g_out_event->signaled = true;
    notify_all();
}
void host_clear_out_event() { g_out_event->signaled = false; }
void host_signal_frame_event() {
    g_audio_frame->signaled = true;
    notify_all();
}

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhAudioHost host{};
        host.audio_open = host_audio_open;
        host.audio_submit = host_audio_submit;
        host.audio_queued = host_audio_queued;
        host.monotonic_ns = host_monotonic_ns;
        host.sleep_ns = host_sleep_ns;
        host.log = host_log;
        host.signal_out_event = host_signal_out_event;
        host.clear_out_event = host_clear_out_event;
        host.signal_frame_event = host_signal_frame_event;
        acnh_audio_set_host(&host);
    });
}

constexpr const char* kRendererVtableSym =
    "_ZTVN2nn2sf6detail38ObjectImplFactoryWithStatefulAllocatorINS0_13InterfaceInfoINS_5audio6detail14IAudioRenderer"
    "EE4_tABINS7_5_tO2NINS4_6server24AudioRendererManagerImpl17AudioRendererImplEE4typeES6_E4typeENS0_24StatefulAllocat"
    "ionPolicyINS0_16ExpHeapAllocatorEEEE6ObjectE";
constexpr const char* kExecuteRenderingSym =
    "_ZN2nn5audio6server24AudioRendererManagerImpl17AudioRendererImpl29ExecuteAudioRendererRenderingEv";

uint64_t sdk_base() {
    static uint64_t base = [] {
        const LoadedModule* m = module_at(find_symbol("nvnBootstrapLoader"));
        return m ? m->base : 0;
    }();
    return base;
}

void InitializeAudioOutParameter(CPU& c) { acnh_audio_out_init_parameter(c.x[0]); }

void OpenDefaultAudioOut(CPU& c) {
    ensure_host();
    uint64_t out = c.x[0], sysev = c.x[1];
    if (sysev) {
        auto w = std::make_shared<KEvent>();
        auto r = std::make_shared<KEvent>();
        w->state = r->state = g_out_event;
        uint32_t hr = handle_add(r), hw = handle_add(w);
        uint64_t attach = find_symbol("_ZN2nn2os17AttachSystemEventEPNS0_15SystemEventTypeEjbjbNS0_14EventClearModeE");
        uint64_t saved[8];
        for (int k = 0; k < 8; k++) saved[k] = c.x[k];
        c.x[0] = sysev;
        c.x[1] = hr;
        c.x[2] = 1;
        c.x[3] = hw;
        c.x[4] = 1;
        c.x[5] = 1;  // manual clear
        call(c, attach);
        for (int k = 0; k < 8; k++) c.x[k] = saved[k];
    }
    c.x[0] = acnh_audio_out_open(out, c.x[2]);
}

void OpenDefaultAudioOutNoEvent(CPU& c) {
    c.x[2] = c.x[1];
    c.x[1] = 0;
    OpenDefaultAudioOut(c);
}

void StartAudioOut(CPU& c) {
    ensure_host();
    c.x[0] = acnh_audio_out_start();
}

void StopAudioOut(CPU&) {
    ensure_host();
    acnh_audio_out_stop();
}

void CloseAudioOut(CPU&) {
    ensure_host();
    acnh_audio_out_close();
}

void GetAudioOutSampleRate(CPU& c) { c.x[0] = acnh_audio_out_sample_rate(); }
void GetAudioOutChannelCount(CPU& c) { c.x[0] = acnh_audio_out_channel_count(); }
void GetAudioOutSampleFormat(CPU& c) { c.x[0] = acnh_audio_out_sample_format(); }
void SetAudioOutVolume(CPU& c) { acnh_audio_out_set_volume(c.v[0].f32[0]); }
void GetAudioOutBufferDataSize(CPU& c) { c.x[0] = acnh_audio_out_buffer_data_size(c.x[0]); }
void SetAudioOutBufferInfo(CPU& c) { acnh_audio_out_set_buffer_info(c.x[0], c.x[1], c.x[2], c.x[3]); }
void GetAudioOutBufferDataPointer(CPU& c) { c.x[0] = acnh_audio_out_buffer_data_pointer(c.x[0]); }

void AppendAudioOutBuffer(CPU& c) {
    ensure_host();
    c.x[0] = acnh_audio_out_append_buffer(c.x[1]);
}

void GetReleasedAudioOutBuffer(CPU& c) {
    ensure_host();
    c.x[0] = acnh_audio_out_get_released_buffer();
}

void ListAudioIns(CPU& c) { c.x[0] = acnh_audio_list_audio_ins(); }
void audio_in_fail(CPU& c) { c.x[0] = acnh_audio_open_audio_in(); }
void zero(CPU& c) { c.x[0] = 0; }

void CreateAudioRendererManagerByHipc(CPU& c) {
    static uint64_t dfc = find_symbol("_ZN2nn5audio31CreateAudioRendererManagerByDfcEv");
    if (!dfc) fatal("CreateAudioRendererManagerByDfc not found");
    log("audio: AudioRenderer manager in process (DFC) instead of audren:u");
    call(c, dfc);
}

void audio_render_thread(CPU& c) {
    ensure_host();
    uint64_t impls[16];
    for (;;) {
        uint64_t t0 = 0;
        size_t n = acnh_audio_render_tick_begin(impls, 16, &t0);
        uint64_t last_rc = 0;
        uint64_t exec = acnh_audio_execute_rendering_addr();
        for (size_t i = 0; i < n; i++) {
            uint64_t saved[31];
            std::memcpy(saved, c.x, sizeof saved);
            c.x[0] = impls[i];
            acnh_audio_before_execute_rendering();
            call(c, exec);
            last_rc = c.x[0];
            acnh_audio_after_execute_rendering();
            std::memcpy(c.x, saved, sizeof saved);
        }
        acnh_audio_render_tick_end(t0, last_rc);
    }
}

void start_render_thread(CPU& c) {
    uint64_t create = find_symbol("_ZN2nn2os12CreateThreadEPNS0_10ThreadTypeEPFvPvES3_S3_mi");
    uint64_t start = find_symbol("_ZN2nn2os11StartThreadEPNS0_10ThreadTypeE");
    if (!create || !start) {
        log("audio: nn::os::CreateThread/StartThread not found, no render thread");
        return;
    }
    uint64_t thread = kernel_alloc(0x1000), stack = kernel_alloc(0x40000);
    uint64_t saved[31];
    std::memcpy(saved, c.x, sizeof saved);
    c.x[0] = thread, c.x[1] = hle_address("__audio_render_thread"), c.x[2] = 0, c.x[3] = stack, c.x[4] = 0x40000;
    c.x[5] = 16;  // high priority, like the audio system's own threads
    call(c, create);
    uint64_t rc = c.x[0];
    c.x[0] = thread;
    if (rc == 0) call(c, start);
    std::memcpy(c.x, saved, sizeof saved);
    if (rc) log("audio: CreateThread failed (%llx)", (unsigned long long)rc);
}

void OpenAudioRenderer(CPU& c) {
    ensure_host();
    static uint64_t real = find_symbol(
        "_ZN2nn5audio17OpenAudioRendererEPNS0_19AudioRendererHandleEPNS_2os11SystemEventERKNS0_22AudioRendererParameterEPvm");
    uint64_t handle = c.x[0], sysev = c.x[1];
    call(c, real);
    if (c.x[0] == 0 && ld<uint32_t>(sysev + 0x14) == 0) {
        auto r = std::make_shared<KEvent>(), w = std::make_shared<KEvent>();
        r->state = w->state = g_audio_frame;
        st<uint32_t>(sysev + 0x14, handle_add(r));
        st<uint32_t>(sysev + 0x18, handle_add(w));
    }
    uint64_t rc = c.x[0];
    if (acnh_audio_renderer_opened(
            rc, handle, find_symbol(kRendererVtableSym), sdk_base(),
            find_symbol("_ZNK2nn5audio6server17AudioRenderSystem16GetExecutionModeEv"),
            find_symbol("_ZN2nn5audio6server17AudioRenderSystem29ExecuteAudioRendererRenderingEv"),
            find_symbol(kExecuteRenderingSym), kModuleBase, kKernelBase + kKernelSize)) {
        start_render_thread(c);
    }
    c.x[0] = rc;
}

void AudioRenderSystemUpdate(CPU& c) {
    ensure_host();
    static uint64_t real = find_symbol("_ZN2nn5audio6server17AudioRenderSystem6UpdateEPvmS3_mPKvm");
    acnh_audio_before_render_system_update();
    call(c, real);
    acnh_audio_after_render_system_update();
}

void CommandListProcessorProcess(CPU& c) {
    ensure_host();
    static uint64_t real = find_symbol("_ZN2nn5audio3dsp20CommandListProcessor7ProcessEv");
    static uint64_t gen_sink = find_symbol("_ZN2nn5audio6server16CommandGenerator25GenerateDeviceSinkCommandEiPNS1_12SinkInfoBaseE");
    uint64_t proc = c.x[0];
    call(c, real);
    acnh_audio_capture_device_sink(proc, gen_sink);
}

void AcquireAudioDeviceNotificationForOutput(CPU& c) {
    auto w = std::make_shared<KEvent>(), r = std::make_shared<KEvent>();
    r->state = w->state = std::make_shared<EventState>();
    uint32_t hr = handle_add(r), hw = handle_add(w);
    uint64_t attach = find_symbol("_ZN2nn2os17AttachSystemEventEPNS0_15SystemEventTypeEjbjbNS0_14EventClearModeE");
    uint64_t saved[8];
    for (int k = 0; k < 8; k++) saved[k] = c.x[k];
    c.x[1] = hr, c.x[2] = 1, c.x[3] = hw, c.x[4] = 1, c.x[5] = 1;
    call(c, attach);
    for (int k = 0; k < 8; k++) c.x[k] = saved[k];
    c.x[0] = 0;
}

void ListAudioOutputDeviceName(CPU& c) {
    c.x[0] = uint64_t(acnh_audio_list_output_device_name(c.x[0], int32_t(c.x[1])));
}

void GetActiveAudioOutputDeviceName(CPU& c) {
    c.x[0] = uint64_t(acnh_audio_get_active_output_device_name(c.x[0]));
}

void two(CPU& c) { c.x[0] = acnh_audio_active_output_channel_count(); }

void ApplyI3dl2ReverbEffect(CPU& c) {
    ensure_host();
    static const uint64_t real = find_symbol(
        "_ZN2nn5audio22ApplyI3dl2ReverbEffectERKNS0_20I3dl2ReverbParameterEPNS0_16I3dl2ReverbStateEbPPKiPPii");
    const bool off = acnh_audio_reverb_should_bypass();
    if (!real) return;
    if (off) c.x[2] = 0;
    if (acnh_audio_reverb_should_benchmark(off, c.x[2])) {
        auto now = [] {
#if defined(_WIN32)
            return uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(
                                std::chrono::steady_clock::now().time_since_epoch())
                                .count());
#else
            timespec ts;
            clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ts);
            return uint64_t(ts.tv_sec) * 1000000000ull + uint64_t(ts.tv_nsec);
#endif
        };
        const CPU saved = c;
        std::vector<uint64_t> t(3000);
        for (auto& v : t) {
            c = saved;
            const uint64_t t0 = now();
            call(c, real);
            v = now() - t0;
        }
        std::sort(t.begin(), t.end());
        acnh_audio_reverb_log_benchmark(t[t.size() / 2], t[0]);
        c = saved;
    }
    call(c, real);
}

}  // namespace

NX_HLE("_ZN2nn5audio39AcquireAudioDeviceNotificationForOutputEPNS_2os11SystemEventE", AcquireAudioDeviceNotificationForOutput);
NX_HLE("_ZN2nn5audio25ListAudioOutputDeviceNameEPNS0_15AudioDeviceNameEi", ListAudioOutputDeviceName);
NX_HLE("_ZN2nn5audio30GetActiveAudioOutputDeviceNameEPNS0_15AudioDeviceNameE", GetActiveAudioOutputDeviceName);
NX_HLE("_ZN2nn5audio26SetAudioDeviceOutputVolumeEPKNS0_15AudioDeviceNameEf", zero);
NX_HLE("_ZN2nn5audio41GetActiveAudioDeviceChannelCountForOutputEv", two);
NX_HLE("_ZN2nn5audio27InitializeAudioOutParameterEPNS0_17AudioOutParameterE", InitializeAudioOutParameter);
NX_HLE("_ZN2nn5audio19OpenDefaultAudioOutEPNS0_8AudioOutEPNS_2os11SystemEventERKNS0_17AudioOutParameterE", OpenDefaultAudioOut);
NX_HLE("_ZN2nn5audio19OpenDefaultAudioOutEPNS0_8AudioOutERKNS0_17AudioOutParameterE", OpenDefaultAudioOutNoEvent);
NX_HLE("_ZN2nn5audio21GetAudioOutSampleRateEPKNS0_8AudioOutE", GetAudioOutSampleRate);
NX_HLE("_ZN2nn5audio23GetAudioOutChannelCountEPKNS0_8AudioOutE", GetAudioOutChannelCount);
NX_HLE("_ZN2nn5audio23GetAudioOutSampleFormatEPKNS0_8AudioOutE", GetAudioOutSampleFormat);
NX_HLE("_ZN2nn5audio17SetAudioOutVolumeEPNS0_8AudioOutEf", SetAudioOutVolume);
NX_HLE("_ZN2nn5audio25GetAudioOutBufferDataSizeEPKNS0_14AudioOutBufferE", GetAudioOutBufferDataSize);
NX_HLE("_ZN2nn5audio13StartAudioOutEPNS0_8AudioOutE", StartAudioOut);
NX_HLE("_ZN2nn5audio12StopAudioOutEPNS0_8AudioOutE", StopAudioOut);
NX_HLE("_ZN2nn5audio13CloseAudioOutEPNS0_8AudioOutE", CloseAudioOut);
NX_HLE("_ZN2nn5audio21SetAudioOutBufferInfoEPNS0_14AudioOutBufferEPvmm", SetAudioOutBufferInfo);
NX_HLE("_ZN2nn5audio28GetAudioOutBufferDataPointerEPKNS0_14AudioOutBufferE", GetAudioOutBufferDataPointer);
NX_HLE("_ZN2nn5audio20AppendAudioOutBufferEPNS0_8AudioOutEPNS0_14AudioOutBufferE", AppendAudioOutBuffer);
NX_HLE("_ZN2nn5audio25GetReleasedAudioOutBufferEPNS0_8AudioOutE", GetReleasedAudioOutBuffer);
NX_HLE("_ZN2nn5audio12ListAudioInsEPNS0_11AudioInInfoEi", ListAudioIns);
NX_HLE("_ZN2nn5audio11OpenAudioInEPNS0_7AudioInEPKcRKNS0_16AudioInParameterE", audio_in_fail);
NX_HLE("_ZN2nn5audio26InitializeAudioInParameterEPNS0_16AudioInParameterE", zero);
NX_HLE("_ZN2nn5audio32CreateAudioRendererManagerByHipcEv", CreateAudioRendererManagerByHipc);
NX_HLE("_ZN2nn5audio17OpenAudioRendererEPNS0_19AudioRendererHandleEPNS_2os11SystemEventERKNS0_22AudioRendererParameterEPvm", OpenAudioRenderer);
NX_HLE("__audio_render_thread", audio_render_thread);
NX_HLE("_ZN2nn5audio3dsp20CommandListProcessor7ProcessEv", CommandListProcessorProcess);
NX_HLE("_ZN2nn5audio6server17AudioRenderSystem6UpdateEPvmS3_mPKvm", AudioRenderSystemUpdate);
NX_HLE("_ZN2nn5audio22ApplyI3dl2ReverbEffectERKNS0_20I3dl2ReverbParameterEPNS0_16I3dl2ReverbStateEbPPKiPPii",
       ApplyI3dl2ReverbEffect);

}  // namespace nx
