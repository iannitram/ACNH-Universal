// Thin C++ wrapper forwarding thermal, performance hints, window/picker state, volume, and
// platform helpers to crates/runtime/src/platform.rs (`acnh_platform_*`).
#include "platform.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

#include <nx/runtime.h>

#include <acnh_runtime.h>

#if defined(__ANDROID__)
#include <dirent.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <spawn.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <thread>
extern char** environ;
#endif

namespace nx::plat {

namespace {

void host_log(const char* msg) {
    if (msg) nx::log("%s", msg);
}

uint64_t host_monotonic_ns() { return monotonic_ns(); }

#if defined(__ANDROID__)

// ---- Android AThermal + ADPF + KGSL + RootShell OS glue ----------------------------------------

float android_thermal_headroom() {
    using Acquire = void* (*)();
    using Headroom = float (*)(void*, int);
    static std::mutex mx;
    static void* mgr = nullptr;
    static Headroom headroom = nullptr;
    static bool tried = false;
    static float last = -1.0f;
    static uint64_t at = 0;
    std::lock_guard lk(mx);
    if (!tried) {
        tried = true;
        if (void* lib = dlopen("libandroid.so", RTLD_NOW)) {
            auto acquire = reinterpret_cast<Acquire>(dlsym(lib, "AThermal_acquireManager"));
            headroom = reinterpret_cast<Headroom>(dlsym(lib, "AThermal_getThermalHeadroom"));
            if (acquire && headroom) mgr = acquire();
        }
    }
    uint64_t now = monotonic_ns();
    if (mgr && headroom && (!at || now - at >= 1'500'000'000ull)) {
        float h = headroom(mgr, 0);
        last = h == h ? h : -1.0f;
        at = now;
    }
    return last;
}

struct AdpfApi {
    using GetManager = void* (*)();
    using CreateSession = void* (*)(void*, const int32_t*, size_t, int64_t);
    using ReportActual = int (*)(void*, int64_t);
    using CloseSession = void (*)(void*);
    using UpdateTarget = int (*)(void*, int64_t);
    using PreferEfficiency = int (*)(void*, bool);
    using PreferredRate = int64_t (*)(void*);

    GetManager get_manager = nullptr;
    CreateSession create = nullptr;
    ReportActual report = nullptr;
    CloseSession close = nullptr;
    UpdateTarget update = nullptr;
    PreferEfficiency efficiency = nullptr;
    PreferredRate preferred_rate = nullptr;
    bool ok = false;
};

const AdpfApi& adpf_api() {
    static const AdpfApi a = [] {
        AdpfApi x;
        const char* e = std::getenv("NX_ADPF");
        if (e && e[0] == '0') return x;
        void* lib = dlopen("libandroid.so", RTLD_NOW);
        if (!lib) return x;
        x.get_manager = reinterpret_cast<AdpfApi::GetManager>(dlsym(lib, "APerformanceHint_getManager"));
        x.create = reinterpret_cast<AdpfApi::CreateSession>(dlsym(lib, "APerformanceHint_createSession"));
        x.report = reinterpret_cast<AdpfApi::ReportActual>(dlsym(lib, "APerformanceHint_reportActualWorkDuration"));
        x.close = reinterpret_cast<AdpfApi::CloseSession>(dlsym(lib, "APerformanceHint_closeSession"));
        x.update = reinterpret_cast<AdpfApi::UpdateTarget>(dlsym(lib, "APerformanceHint_updateTargetWorkDuration"));
        x.efficiency =
            reinterpret_cast<AdpfApi::PreferEfficiency>(dlsym(lib, "APerformanceHint_setPreferPowerEfficiency"));
        x.preferred_rate =
            reinterpret_cast<AdpfApi::PreferredRate>(dlsym(lib, "APerformanceHint_getPreferredUpdateRateNanos"));
        x.ok = x.get_manager && x.create && x.report && x.close;
        return x;
    }();
    return a;
}

void* host_adpf_create(const int32_t* tids, size_t count, int64_t target_ns) {
    const AdpfApi& a = adpf_api();
    if (!a.ok) return nullptr;
    void* mgr = a.get_manager();
    return mgr ? a.create(mgr, tids, count, target_ns) : nullptr;
}

void host_adpf_close(void* s) {
    if (adpf_api().close && s) adpf_api().close(s);
}

void host_adpf_update(void* s, int64_t target_ns) {
    if (adpf_api().update && s) adpf_api().update(s, target_ns);
}

void host_adpf_report(void* s, int64_t actual_ns) {
    if (adpf_api().report && s) adpf_api().report(s, actual_ns);
}

void host_adpf_efficiency(void* s, bool eff) {
    if (adpf_api().efficiency && s) adpf_api().efficiency(s, eff);
}

int64_t host_adpf_preferred_rate() {
    const AdpfApi& a = adpf_api();
    if (!a.preferred_rate || !a.get_manager) return 0;
    void* mgr = a.get_manager();
    return mgr ? a.preferred_rate(mgr) : 0;
}

int32_t host_kgsl_pwrctrl(bool enable_normal) {
    struct Property {
        unsigned int type;
        void* value;
        size_t sizebytes;
    };
    int fd = open("/dev/kgsl-3d0", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        nx::log("gpu turbo: /dev/kgsl-3d0 not opened (errno %d)", errno);
        return -1;
    }
    unsigned int enable = enable_normal ? 1 : 0;
    Property p{0xE, &enable, sizeof enable};
    int r = ioctl(fd, _IOW(0x09, 0x32, Property), &p);
    nx::log("gpu turbo: KGSL power control %s: %s (errno %d)",
            enable_normal ? "on (normal)" : "off (NX_GPU_TURBO=1)", r == 0 ? "done" : "refused",
            r == 0 ? 0 : errno);
    if (enable_normal) close(fd);
    return r;
}

struct RootShell {
    int fd = -1;
    pid_t pid = -1;
    std::string in;

    bool start() {
        int sv[2];
        if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv) != 0) return false;
        static const char kScript[] = "trap '' PIPE; R=:; while IFS= read -r l; do eval \"$l\"; done; eval \"$R\"";
        posix_spawn_file_actions_t fa;
        posix_spawn_file_actions_init(&fa);
        posix_spawn_file_actions_adddup2(&fa, sv[1], 0);
        posix_spawn_file_actions_adddup2(&fa, sv[1], 1);
        posix_spawn_file_actions_addopen(&fa, 2, "/dev/null", O_WRONLY, 0);
        char* argv[] = {const_cast<char*>("su"), const_cast<char*>("-c"), const_cast<char*>(kScript), nullptr};
        int rc = posix_spawnp(&pid, "su", &fa, nullptr, argv, environ);
        posix_spawn_file_actions_destroy(&fa);
        close(sv[1]);
        if (rc != 0) {
            close(sv[0]);
            pid = -1;
            return false;
        }
        fd = sv[0];
        return true;
    }

    bool fail() {
        if (fd >= 0) close(fd);
        fd = -1;
        if (pid > 0) waitpid(pid, nullptr, WNOHANG);
        return false;
    }

    bool run(const std::string& cmd, std::string* out, int timeout_ms) {
        if (fd < 0) return false;
        std::string line = cmd + "; echo @@\n";
        for (size_t at = 0; at < line.size();) {
            ssize_t w = send(fd, line.data() + at, line.size() - at, MSG_NOSIGNAL);
            if (w < 0 && errno == EINTR) continue;
            if (w <= 0) return fail();
            at += size_t(w);
        }
        const uint64_t until = monotonic_ns() + uint64_t(timeout_ms) * 1'000'000ull;
        for (;;) {
            size_t end = in.find("@@\n");
            if (end != std::string::npos) {
                if (out) *out = in.substr(0, end);
                in.erase(0, end + 3);
                return true;
            }
            uint64_t now = monotonic_ns();
            if (now >= until) return fail();
            pollfd pf{fd, POLLIN, 0};
            int r = poll(&pf, 1, int((until - now) / 1'000'000ull) + 1);
            if (r <= 0) continue;
            char buf[2048];
            ssize_t n = recv(fd, buf, sizeof buf, 0);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) return fail();
            in.append(buf, size_t(n));
        }
    }
};

std::mutex g_thermal_mx;
std::condition_variable g_thermal_cv;
bool g_thermal_paused = false;

void governor_main() {
    set_thread_name("thermal");
    RootShell sh;
    nx::log("thermal: asking for root (NX_ROOT_CLOCKS=1)");
    if (!sh.start()) {
        nx::log("thermal: su not available: ADPF only");
        return;
    }
    const char* cmd =
        "K=/sys/class/kgsl/kgsl-3d0; G=/sys/kernel/gpu/gpu_max_clock; "
        "[ -f $G ] && echo \"sgpu $(cat $G)\"; echo \"kgpu $(cat $K/max_gpuclk)\"; "
        "echo \"freqs $(cat $K/gpu_available_frequencies)\"; echo \"limit $(cat /sys/power/cpufreq_max_limit)\"; "
        "for p in /sys/devices/system/cpu/cpufreq/policy*; do echo \"policy ${p##*policy} $(cat $p/cpuinfo_max_freq) "
        "$(cat $p/scaling_max_freq) $(cat $p/scaling_available_frequencies)\"; done; "
        "for z in /sys/class/thermal/thermal_zone*; do echo \"zone ${z##*thermal_zone} $(cat $z/type)\"; done; "
        "echo \"id $(id -u)\"";
    std::string out;
    if (!sh.run(cmd, &out, 30'000)) {
        sh.fail();
        return;
    }
    AcnhThermalGovernor* gov = acnh_platform_governor_new();
    std::array<char, 4096> reads_buf{};
    if (!acnh_platform_governor_discover_parse(gov, out.c_str(), reads_buf.data(), reads_buf.size()) ||
        !sh.run("nxrd() { v=; read -r v < $2; echo \"$1 $v\"; }", nullptr, 3000)) {
        acnh_platform_governor_free(gov);
        sh.fail();
        nx::log("thermal: root refused or the clock files not found: ADPF only");
        return;
    }
    std::string reads = reads_buf.data();
    bool restored = false;
    for (uint64_t tick = 1;; tick++) {
        {
            std::unique_lock lk(g_thermal_mx);
            g_thermal_cv.wait_for(lk, std::chrono::seconds(1), [&] { return g_thermal_paused != restored; });
            if (g_thermal_paused && restored) continue;
            if (g_thermal_paused) {
                lk.unlock();
                sh.run("eval \"$R\"", nullptr, 3000);
                acnh_platform_governor_mark_restored(gov);
                restored = true;
                continue;
            }
        }
        if (restored) {
            restored = false;
            acnh_platform_governor_reset_on_resume(gov);
        }
        const int mode = thermal_mode();
        if (!sh.run(reads, &out, 3000)) {
            acnh_platform_governor_free(gov);
            nx::log("thermal: the root shell ended: clock control off");
            return;
        }
        acnh_platform_governor_sample_parse(gov, out.c_str());
        if (tick % 2 == 0) acnh_platform_governor_decide(gov, mode, thermal_headroom());
        std::array<char, 4096> restore_cmd{}, write_cmd{};
        acnh_platform_governor_build_apply(gov, mode, restore_cmd.data(), restore_cmd.size(), write_cmd.data(),
                                           write_cmd.size());
        if (restore_cmd[0]) sh.run(restore_cmd.data(), nullptr, 3000);
        if (write_cmd[0]) sh.run(write_cmd.data(), nullptr, 3000);
    }
}

void keeper_main(int cpu, int duty) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    sched_setaffinity(0, sizeof set, &set);
    sched_param sp{};
    sched_setscheduler(0, SCHED_IDLE, &sp);
    char name[16];
    std::snprintf(name, sizeof name, "keeper cpu%d", cpu);
    set_thread_name(name);
    const uint64_t spin = 2'000'000ull * uint64_t(duty) / 100, rest_us = uint64_t(2'000 * (100 - duty) / 100);
    for (;;) {
        uint64_t now = monotonic_ns();
        if (now - acnh_platform_perf_hint_last_frame_ns() > 1'000'000'000ull) {
            usleep(200'000);
            continue;
        }
        for (uint64_t until = now + spin; monotonic_ns() < until;) {
        }
        if (rest_us) usleep(useconds_t(rest_us));
    }
}

void clock_keeper_start() {
    const char* e = std::getenv("NX_CLOCK_KEEPER");
    if (!e || !*e || std::string(e) == "0") return;
    const char* d = std::getenv("NX_CLOCK_KEEPER_DUTY");
    int duty = d ? std::clamp(std::atoi(d), 1, 100) : 100;
    std::vector<int32_t> cpus;
    if (std::string(e) == "1") {
        if (DIR* dir = opendir("/sys/devices/system/cpu/cpufreq")) {
            while (dirent* de = readdir(dir))
                if (std::strncmp(de->d_name, "policy", 6) == 0) cpus.push_back(std::atoi(de->d_name + 6));
            closedir(dir);
        }
        std::sort(cpus.begin(), cpus.end());
    } else {
        std::array<int32_t, 64> buf{};
        size_t n = acnh_platform_parse_keeper_cpus(e, buf.data(), buf.size());
        cpus.assign(buf.begin(), buf.begin() + n);
    }
    for (int cpu : cpus) std::thread(keeper_main, cpu, duty).detach();
}

#endif  // __ANDROID__

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhPlatformHost h{};
        h.log = host_log;
        h.monotonic_ns = host_monotonic_ns;
#if defined(__ANDROID__)
        h.thermal_headroom = android_thermal_headroom;
        h.adpf_create_session = host_adpf_create;
        h.adpf_close_session = host_adpf_close;
        h.adpf_update_target = host_adpf_update;
        h.adpf_report_actual = host_adpf_report;
        h.adpf_set_efficiency = host_adpf_efficiency;
        h.adpf_preferred_rate_ns = host_adpf_preferred_rate;
        h.kgsl_set_pwrctrl = host_kgsl_pwrctrl;
#endif
        acnh_platform_set_host(&h);
    });
}

}  // namespace

// ---- Shared nx::plat::* forwards to Rust --------------------------------------------------------

bool headless() { return acnh_platform_headless(); }

bool make_dirs(const std::string& path) { return acnh_platform_make_dirs(path.c_str()); }

void set_master_volume(float v) { acnh_platform_set_master_volume(v); }

int display_refresh_hz() { return acnh_platform_display_refresh_hz(); }

#if !defined(__ANDROID__)
std::string setting(const std::string& key) {
    std::array<char, 512> buf{};
    size_t n = acnh_platform_setting(key.c_str(), buf.data(), buf.size());
    if (n < buf.size()) return std::string(buf.data(), n);
    std::string out(n + 1, '\0');
    acnh_platform_setting(key.c_str(), out.data(), out.size());
    out.resize(n);
    return out;
}

bool window_minimized() { return acnh_platform_window_minimized(); }

void window_size(uint32_t& w, uint32_t& h) { acnh_platform_window_size(&w, &h); }

bool pick_result(std::string& path) {
    std::array<char, 4096> buf{};
    size_t n = 0;
    if (!acnh_platform_pick_take(buf.data(), buf.size(), &n)) return false;
    if (n < buf.size()) {
        path.assign(buf.data(), n);
    } else {
        path.resize(n + 1);
        acnh_platform_pick_take(path.data(), path.size(), &n);
        path.resize(n);
    }
    return true;
}
#endif

int thermal_mode() {
#if defined(__ANDROID__)
    ensure_host();
    return acnh_platform_thermal_mode();
#else
    return 0;
#endif
}

double thermal_adpf_share(int mode) {
#if defined(__ANDROID__)
    return acnh_platform_thermal_adpf_share(mode);
#else
    (void)mode;
    return 0.0;
#endif
}

float thermal_headroom() {
#if defined(__ANDROID__)
    ensure_host();
    return acnh_platform_thermal_headroom();
#else
    return -1.0f;
#endif
}

void thermal_start() {
#if defined(__ANDROID__)
    ensure_host();
    const char* e = std::getenv("NX_ROOT_CLOCKS");
    if (e && e[0] == '1') std::thread(governor_main).detach();
#endif
}

void thermal_pause(bool paused) {
#if defined(__ANDROID__)
    {
        std::lock_guard lk(g_thermal_mx);
        g_thermal_paused = paused;
    }
    g_thermal_cv.notify_all();
#else
    (void)paused;
#endif
}

float dynamic_render_scale() {
#if defined(__ANDROID__)
    return acnh_platform_dynamic_render_scale();
#else
    return 1.0f;
#endif
}

void frame_period(int64_t ns) {
#if defined(__ANDROID__)
    acnh_platform_frame_period(ns);
#else
    (void)ns;
#endif
}

int64_t frame_period_ns() {
#if defined(__ANDROID__)
    return acnh_platform_frame_period_ns();
#else
    return 0;
#endif
}

void frame_replay_work(int64_t ns) {
#if defined(__ANDROID__)
    acnh_platform_frame_replay_work(ns);
#else
    (void)ns;
#endif
}

int64_t frame_replay_work_ns() {
#if defined(__ANDROID__)
    return acnh_platform_frame_replay_work_ns();
#else
    return 0;
#endif
}

void thermal_main_work(int64_t ns) {
#if defined(__ANDROID__)
    acnh_platform_thermal_main_work(ns);
#else
    (void)ns;
#endif
}

void thermal_frame(uint64_t interval_ns) {
#if defined(__ANDROID__)
    acnh_platform_thermal_frame(interval_ns);
#else
    (void)interval_ns;
#endif
}

void gpu_turbo() {
#if defined(__ANDROID__)
    ensure_host();
    acnh_platform_gpu_turbo();
#endif
}

void perf_hint_thread() {
#if defined(__ANDROID__)
    ensure_host();
    if (!adpf_api().ok) return;
    thread_local bool joined = false;
    if (joined) return;
    joined = true;
    acnh_platform_perf_hint_thread(int32_t(syscall(SYS_gettid)));
#endif
}

void perf_hint_frame(int64_t ns) {
#if defined(__ANDROID__)
    ensure_host();
    static std::once_flag keeper;
    std::call_once(keeper, clock_keeper_start);
    if (!adpf_api().ok) return;
    acnh_platform_perf_hint_frame(ns);
#else
    (void)ns;
#endif
}

}  // namespace nx::plat
