// macOS and iPad host, the OS half: memory (mmap at fixed addresses, vm_allocate on iOS), pthreads, time, files, the
// iPad's save sync and first-start download, the signal crash handler and main. The window (with its Metal layer for
// MoltenVK), input, audio and the main thread's event loop are the SDL host's (platform_sdl_wrapper.cpp,
// platform_sdl.h), shared with Linux and Windows.
//
// Cocoa owns the main thread: main() starts the game (nx::boot) on its own thread and then runs the SDL event loop.
//
// Data: NX_DATA_ROOT or games/<game>'s pc_data_root ("private/<game>"): analysis/{main,sdk}.bin, extract/romfs, user/.
// Settings: NX_<KEY> environment variables, as on Windows.
#include <TargetConditionals.h>
#include <dlfcn.h>
#include <pthread.h>
#include <cstdlib>
#include <mach/mach.h>
#if TARGET_OS_IPHONE
#include <mach/vm_map.h>
#else
#include <mach/mach_vm.h>
#endif
#include <mach/thread_switch.h>
#include <pthread/qos.h>
#include <sched.h>
#include <signal.h>
#include <sys/mman.h>
#include <time.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <cctype>
#include <cerrno>
#include <chrono>
#include <climits>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <filesystem>
#include <functional>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <SDL3/SDL.h>
#if TARGET_OS_IPHONE
#include <SDL3/SDL_main.h>  // the UIKit main, which runs ours as SDL_main
#endif

#include <nx/game.h>
#include <nx/runtime.h>

#if TARGET_OS_IPHONE
// The small .ipa's first-start download (runtime/src/appgame_ios.mm; issue #231).
extern "C" bool nx_appgame_first_start();      // blocks until the game is installed (true) or given up (false)
extern "C" bool nx_appgame_game_matches();      // the downloaded game is of this app's build and runtime
extern "C" const char* nx_appgame_game_dir();  // Library/Application Support/game
#endif

#include "kernel.h"
#include "config.h"
#include "bug_report.h"
#include "platform.h"
#include "platform_sdl.h"

namespace nx {
void boot();
void cpu_profile_register(const char* name);  // cpu_profile.cpp
}

namespace nx::plat {

// ---- memory ---------------------------------------------------------------------------------------------------

// No MAP_FIXED_NOREPLACE on Darwin: ask for the address as a hint and give it back if the kernel chose another.
// macOS 27 ignores mmap's address hint below 0x70_0000_0000 (the mapping lands up there), and MAP_FIXED would
// silently replace whatever is already mapped. mach_vm_allocate (vm_allocate on iOS, where mach_vm.h is refused)
// with VM_FLAGS_FIXED takes the exact range or fails when any of it is in use.
bool reserve(uint64_t addr, uint64_t size) {
#if TARGET_OS_IPHONE
    vm_address_t a = addr;
    if (vm_allocate(mach_task_self(), &a, size, VM_FLAGS_FIXED) != KERN_SUCCESS) {
        vm_address_t r = addr;  // name what is in the way (svcs and the guest layout are in runtime.h)
        vm_size_t rs = 0;
        vm_region_basic_info_data_64_t info;
        mach_msg_type_number_t n = VM_REGION_BASIC_INFO_COUNT_64;
        mach_port_t obj;
        if (vm_region_64(mach_task_self(), &r, &rs, VM_REGION_BASIC_INFO_64, (vm_region_info_t)&info, &n, &obj) ==
            KERN_SUCCESS)
            nx::log("memory: %llx (+%llx) overlaps a mapping at %llx (+%llx)", (unsigned long long)addr,
                    (unsigned long long)size, (unsigned long long)r, (unsigned long long)rs);
        return false;
    }
    if (mprotect(reinterpret_cast<void*>(addr), size, PROT_NONE) != 0) {
        vm_deallocate(mach_task_self(), addr, size);
        return false;
    }
    return true;
#else
    mach_vm_address_t a = addr;
    if (mach_vm_allocate(mach_task_self(), &a, size, VM_FLAGS_FIXED) != KERN_SUCCESS) {
        mach_vm_address_t r = addr;  // name what is in the way (svcs and the guest layout are in runtime.h)
        mach_vm_size_t rs = 0;
        vm_region_basic_info_data_64_t info;
        mach_msg_type_number_t n = VM_REGION_BASIC_INFO_COUNT_64;
        mach_port_t obj;
        if (mach_vm_region(mach_task_self(), &r, &rs, VM_REGION_BASIC_INFO_64, (vm_region_info_t)&info, &n, &obj) ==
            KERN_SUCCESS)
            nx::log("memory: %llx (+%llx) overlaps a mapping at %llx (+%llx)", (unsigned long long)addr,
                    (unsigned long long)size, (unsigned long long)r, (unsigned long long)rs);
        return false;
    }
    if (mprotect(reinterpret_cast<void*>(addr), size, PROT_NONE) != 0) {
        mach_vm_deallocate(mach_task_self(), addr, size);
        return false;
    }
    return true;
#endif
}

bool commit(uint64_t addr, uint64_t size) {
    uint64_t page = uint64_t(getpagesize()) - 1;  // 16 KiB on Apple silicon
    uint64_t a = addr & ~page, e = (addr + size + page) & ~page;
    return mprotect(reinterpret_cast<void*>(a), e - a, PROT_READ | PROT_WRITE) == 0;
}

// The guest frees 4 KiB pages; Apple silicon pages are 16 KiB, and mprotect would take the neighbours of a partly
// freed page with it (SIGBUS in Wonder when nn::os's VAMM unmapped single pages). Only whole host pages are
// released; the freed part of a shared one is zeroed, which is what the guest would see if it maps it again.
void decommit(uint64_t addr, uint64_t size) {
    uint64_t page = uint64_t(getpagesize());
    uint64_t lo = (addr + page - 1) & ~(page - 1), hi = (addr + size) & ~(page - 1);
    if (lo >= hi) {
        std::memset(reinterpret_cast<void*>(addr), 0, size);
        return;
    }
    if (addr < lo) std::memset(reinterpret_cast<void*>(addr), 0, lo - addr);
    if (hi < addr + size) std::memset(reinterpret_cast<void*>(hi), 0, addr + size - hi);
    madvise(reinterpret_cast<void*>(lo), hi - lo, MADV_FREE);
    mprotect(reinterpret_cast<void*>(lo), hi - lo, PROT_NONE);
}

// ---- threads and time -----------------------------------------------------------------------------------------

struct ThreadStart {
    ThreadFn fn;
    void* arg;
};

static void* thread_entry(void* p) {
    ThreadStart s = *static_cast<ThreadStart*>(p);
    delete static_cast<ThreadStart*>(p);
    // The game's threads (render, jobs, audio) are latency critical: default QoS lets macOS run them on the M1's
    // efficiency cores and stretch their sleeps and wakeups.
    pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
    s.fn(s.arg);
    return nullptr;
}

void start_thread(ThreadFn fn, void* arg, size_t stack_size) {
    pthread_attr_t a;
    pthread_attr_init(&a);
    pthread_attr_setstacksize(&a, (stack_size + 0x3FFF) & ~size_t(0x3FFF));
    pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    pthread_t t;
    pthread_create(&t, &a, thread_entry, new ThreadStart{fn, arg});
    pthread_attr_destroy(&a);
}

void exit_thread() { pthread_exit(nullptr); }
void exit_process(int code) { _exit(code); }

void sleep_ns(int64_t ns) {
    timespec ts{time_t(ns / 1000000000), long(ns % 1000000000)};
    nanosleep(&ts, nullptr);
}

// macOS sched_yield drops the caller to priority 0 for a while: measured 20-180 us per call on the M1, and Wonder's
// job system yields after every job item and in its barriers, so the main thread spent ~60% of its frame there and
// the game's governor fell to 30 fps (docs/nvn/DEBUG.md #44). thread_switch with no hint (2-4 us) gives the core to
// another ready thread without that penalty. NX_YIELD=sched: the old call.
void yield() {
    static const bool sched = [] {
        const char* e = std::getenv("NX_YIELD");
        return e && std::strcmp(e, "sched") == 0;
    }();
    if (sched)
        sched_yield();
    else
        thread_switch(MACH_PORT_NULL, SWITCH_OPTION_NONE, 0);
}
void set_thread_name(const char* name) { pthread_setname_np(name); }  // Darwin: the calling thread only

int64_t current_thread_id() {
    uint64_t tid = 0;
    pthread_threadid_np(nullptr, &tid);
    return int64_t(tid);
}

bool profile(const int64_t*, const uint64_t* const*, size_t, int, const std::string&) { return false; }  // use Instruments

uint64_t monotonic_ns() { return clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }

// ---- files and output -----------------------------------------------------------------------------------------

FILE* fopen_utf8(const std::string& path, const char* mode) { return std::fopen(path.c_str(), mode); }
int64_t ftell64(FILE* f) { return ftello(f); }
int fseek64(FILE* f, int64_t off, int origin) { return fseeko(f, off, origin); }

uint64_t read_at(FILE* f, int64_t off, void* buf, uint64_t size) {
    if (size > uint64_t(SSIZE_MAX)) return 0;  // the game passes -1 as "everything": fread read nothing
    int fd = fileno(f);
    uint64_t done = 0;
    while (done < size) {
        ssize_t n = pread(fd, static_cast<uint8_t*>(buf) + done, size_t(size - done), off + int64_t(done));
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        done += uint64_t(n);
    }
    return done;
}



void log_line(const char* line) { std::fprintf(stderr, "%s\n", line); }

std::string data_root() {
    const char* r = std::getenv("NX_DATA_ROOT");
    return r ? r : nx::game::kInfo.pc_data_root;
}

// The game inside the app (Contents/Resources/game on the Mac, ACNH.app/game on the iPad), or NX_GAME_ROOT from the
// launcher; the data root otherwise (a development build).
std::string game_root() {
    const char* base = SDL_GetBasePath();
    const auto is_file = [](const std::string& path) {
        std::error_code ec;
        return std::filesystem::is_regular_file(std::filesystem::u8path(path), ec);
    };
#if TARGET_OS_IPHONE
    // The game the app downloaded at first start, when the .ipa has none (appgame_ios.mm, issue #231).
    return choose_game_root(std::getenv("NX_GAME_ROOT"), true, base ? base : "", is_file, data_root(), nx_appgame_game_dir());
#else
    return choose_game_root(std::getenv("NX_GAME_ROOT"), true, base ? base : "", is_file, data_root());
#endif
}

std::string device_locale() { return ""; }

uint64_t wait_vblank() { return 0; }  // no display vblank to wait on here: hle_vi.cpp keeps its timed tick



}  // namespace nx::plat

// ---- per-thread CPU (NX_THREAD_CPU=<seconds>) ---------------------------------------------------------------------
// Every that many seconds, the threads that used the most CPU over the interval, by name, as a share of one core:
// what the process spends, where (the replay, the game's threads, Metal's, the audio...), to cut its heat.
namespace {
void thread_cpu_watch() {
    const char* e = std::getenv("NX_THREAD_CPU");
    const int every = e ? std::atoi(e) : 0;
    if (every <= 0) return;
    std::thread([every] {
        pthread_setname_np("thread cpu");
        std::map<uint64_t, double> last;
        for (;;) {
            std::this_thread::sleep_for(std::chrono::seconds(every));
            thread_act_array_t threads = nullptr;
            mach_msg_type_number_t n = 0;
            if (task_threads(mach_task_self(), &threads, &n) != KERN_SUCCESS) continue;
            std::vector<std::pair<double, std::string>> rows;
            std::map<uint64_t, double> now;
            double total = 0;
            for (mach_msg_type_number_t k = 0; k < n; k++) {
                thread_basic_info_data_t bi{};
                mach_msg_type_number_t c = THREAD_BASIC_INFO_COUNT;
                thread_identifier_info_data_t ii{};
                mach_msg_type_number_t ic = THREAD_IDENTIFIER_INFO_COUNT;
                if (thread_info(threads[k], THREAD_BASIC_INFO, thread_info_t(&bi), &c) == KERN_SUCCESS &&
                    thread_info(threads[k], THREAD_IDENTIFIER_INFO, thread_info_t(&ii), &ic) == KERN_SUCCESS) {
                    const double t = bi.user_time.seconds + bi.user_time.microseconds / 1e6 + bi.system_time.seconds +
                                     bi.system_time.microseconds / 1e6;
                    now[ii.thread_id] = t;
                    auto it = last.find(ii.thread_id);
                    const double d = it == last.end() ? 0 : t - it->second;
                    char name[64] = {};
                    if (pthread_t pt = pthread_from_mach_thread_np(threads[k])) pthread_getname_np(pt, name, sizeof name);
                    if (!name[0]) std::snprintf(name, sizeof name, "tid %llu", (unsigned long long)ii.thread_id);
                    rows.push_back({d, name});
                    total += d;
                }
                mach_port_deallocate(mach_task_self(), threads[k]);
            }
            vm_deallocate(mach_task_self(), vm_address_t(threads), n * sizeof(thread_act_t));
            last = std::move(now);
            std::sort(rows.begin(), rows.end(), [](auto& a, auto& b) { return a.first > b.first; });
            std::string out;
            for (size_t k = 0; k < rows.size() && k < 12; k++) {
                char b[96];
                std::snprintf(b, sizeof b, "%s%s %.0f%%", k ? ", " : "", rows[k].second.c_str(), 100.0 * rows[k].first / every);
                out += b;
            }
            nx::log("thread cpu: %.0f%% of a core over %d s: %s", 100.0 * total / every, every, out.c_str());
        }
    }).detach();
}
}  // namespace

#if TARGET_OS_IPHONE
// save-relay inside the player (crates/runtime, relay.rs; issue #172). The Rust half pulls the island before the game
// boots (it returns only when the game may boot) and pushes it when the game quits or the app goes to the background.
extern "C" void acnh_relay_before_game(const char* data_root);
extern "C" void acnh_relay_after_game(const char* data_root);   // synchronous: the game has quit (game thread)
extern "C" void acnh_relay_terminate(const char* data_root);    // main thread: waits at most ~3 s, never longer
extern "C" void acnh_relay_background(const char* data_root);   // returns at once: the push runs on a worker
extern "C" void acnh_relay_foreground(const char* data_root);   // returns at once: a waiting mark is written on a worker

// The Rust half's log lines go into the player's log (Documents/logs/nx.log).
extern "C" void nx_relay_log(const char* line) { nx::log("%s", line); }

// relay.ask lives in the player's own settings: this side is the only writer of config.ini (config_save rewrites the
// file from the entries it loaded, so a key written behind its back would be lost at the next save).
extern "C" bool nx_relay_ask_off() { return nx::config_get("relay.ask", "") == "0"; }
extern "C" bool nx_relay_remember_not_now() {
    nx::config_set("relay.ask", "0");
    return nx::config_save();
}

// The final push, once: the game thread's end, SDL_EVENT_QUIT and SDL_EVENT_TERMINATING can all get here. The Rust half
// (relay_ios.rs, FinalPush) lets the first one run it and returns at once for the others, so no thread waits on a push
// another runs. The game thread's end and SDL_EVENT_QUIT wait for the whole push; TERMINATING waits at most about 3 s,
// because iOS gives applicationWillTerminate: about 5 s and ends the process when it returns: a push not done by then
// is cut off, and the island is already marked waiting (before the push queues for its turn), so the next start sends it. The push itself
// does nothing before the game booted or when it started with sync off, and a running checkpoint is waited for (PUSH_LOCK).
static void relay_after_game() {
    if (const char* root = std::getenv("NX_DATA_ROOT")) acnh_relay_after_game(root);
}
static void relay_terminate() {
    if (const char* root = std::getenv("NX_DATA_ROOT")) acnh_relay_terminate(root);
}

// The Saves page's Sync now (platform.h, SaveSync): the background push on a worker (relay_ios.rs), and what the page
// shows of it. Offered wherever the player has a data root, the relay's island.
extern "C" bool acnh_relay_sync_now(const char* data_root);  // returns at once
extern "C" void acnh_relay_sync_status(const char* data_root, int* ready, int* outcome, uint64_t* last, char* service,
                                       size_t cap);

namespace nx::plat {
SaveSync save_sync_status() {
    SaveSync s;
    const char* root = std::getenv("NX_DATA_ROOT");
    if (!root) return s;
    s.offered = true;
    uint64_t last = 0;
    char service[64] = {};
    acnh_relay_sync_status(root, &s.ready, &s.outcome, &last, service, sizeof service);
    s.last_unix = int64_t(last);
    s.service = service;
    // The game started with sync off and Not now is what kept it off: the setup prompt can be brought back.
    s.can_set_up = s.ready == kSyncStartedOff && nx_relay_ask_off();
    return s;
}
bool save_sync_now() {
    const char* root = std::getenv("NX_DATA_ROOT");
    return root && acnh_relay_sync_now(root);
}
// Not now (relay.ask = 0) forgotten: the next start asks again. The player's own settings are the only writer of
// config.ini (see nx_relay_remember_not_now), so this goes through them too.
bool save_sync_set_up() {
    nx::config_set("relay.ask", "");
    return nx::config_save();
}
}  // namespace nx::plat

// The app went to the background. SDL's WILL_ENTER_BACKGROUND is UIKit's resign-active, which also fires for
// Control Center or a notification pulled over the game, so the push waits for DID_ENTER_BACKGROUND. The watch runs
// on the main thread, which must keep its run loop: acnh_relay_background begins the background task here
// (beginBackgroundTaskWithExpirationHandler:), pushes on a worker thread and returns at once. It does nothing until
// acnh_relay_before_game has returned.
static bool background_watch(void*, SDL_Event* e) {
    if (e->type == SDL_EVENT_DID_ENTER_BACKGROUND)
        if (const char* root = std::getenv("NX_DATA_ROOT")) acnh_relay_background(root);
    // Back to the front: a "playing here" mark the background push could not write back is written now (on a worker).
    if (e->type == SDL_EVENT_WILL_ENTER_FOREGROUND)
        if (const char* root = std::getenv("NX_DATA_ROOT")) acnh_relay_foreground(root);
    // iOS is ending the process (applicationWillTerminate: the app was swiped away while in the foreground or
    // suspended and killed in memory pressure after a wake): a quit that never reaches SDL_EVENT_QUIT. The push runs
    // on a worker under a background task and this thread waits a bounded time (relay_terminate), never the whole push.
    if (e->type == SDL_EVENT_TERMINATING) relay_terminate();
    return true;
}
#else
static void relay_after_game() {}
static void relay_terminate() {}
#endif

// ---- crash handler and entry ----------------------------------------------------------------------------------

namespace {

void crash_handler(int sig, siginfo_t* info, void* uc) {
    if ((sig == SIGSEGV || sig == SIGBUS) && nx::plat::write_watch_fault(info->si_addr)) return;  // write_watch.cpp
    const auto* m = static_cast<ucontext_t*>(uc)->uc_mcontext;
    Dl_info d{};
    uint64_t pc = m->__ss.__pc;
    dladdr(reinterpret_cast<void*>(pc), &d);
    nx::log("CRASH: signal %d at %s+%llx, data address %llx", sig, d.dli_fname ? d.dli_fname : "?",
            (unsigned long long)(pc - uint64_t(d.dli_fbase)), (unsigned long long)uint64_t(info->si_addr));
    nx::CPU& c = nx::current_cpu();
    nx::log("  guest lr %s  sp %llx", nx::describe(c.x[30]).c_str(), (unsigned long long)c.x[31]);
    for (int k = 0; k < 32; k += 4)
        nx::log("  x%-2d %016llx %016llx %016llx %016llx", k, (unsigned long long)c.x[k], (unsigned long long)c.x[k + 1],
                (unsigned long long)c.x[k + 2], (unsigned long long)c.x[k + 3]);
    nx::backtrace(c);
    nx::log_recent_files();
    signal(sig, SIG_DFL);
    raise(sig);
}

void game_thread() {
    try {
#if TARGET_OS_IPHONE
        // The small .ipa holds no game: offer to download it (Dropbox, in the background, resumable) before anything
        // else. Returns when the game is there or the person said Not now (the fatal below says what is missing). Also
        // when the game root is the downloaded game and it is of another build or runtime than this app's (the app was
        // updated; Library/Application Support/game outlives that): the matching one replaces it, whole or not at all.
        {
            std::error_code ec;
            const std::string root = nx::plat::game_root();
            const bool missing =
                !std::filesystem::is_regular_file(std::filesystem::u8path(root + "/analysis/main.bin"), ec);
            const bool mismatched = !missing && root == nx_appgame_game_dir() && !nx_appgame_game_matches();
            if (missing || mismatched) {
                const bool installed = nx_appgame_first_start();
                // Not now, or the download failed: say so in plain words here, before the save-sync prompts and the
                // loader's developer message (it names a tool). The next launch offers the download again.
                if (!installed)
                    nx::fatal(mismatched
                                  ? "the downloaded game is for another version of this app: open the app again to download the matching one"
                                  : "the game is not downloaded yet: open the app again to download it");
            }
        }
        // On this thread, so the main thread keeps the run loop the prompts need. Returns when the game may boot.
        if (const char* root = std::getenv("NX_DATA_ROOT")) acnh_relay_before_game(root);
#endif
        nx::boot();
    } catch (const nx::LongJmp& j) {
        nx::fatal("longjmp to buffer %llx with no setjmp frame", (unsigned long long)j.buf);
    } catch (const std::exception& e) {
        nx::fatal("host exception: %s", e.what());
    }
}

}  // namespace

int main(int argc, char** argv) {
    // acnh <game folder>: the prepared game folder (else NX_DATA_ROOT, else the setup's data folder)
    if (argc > 1) setenv("NX_DATA_ROOT", argv[1], 1);
#if TARGET_OS_IPHONE
    // SideStore starts the app with no environment and no arguments: the writable data root is the app's
    // Documents folder (visible in the Files app, ios/Info.plist.in), where config.ini, user/ and the logs live.
    if (!std::getenv("NX_DATA_ROOT"))
        if (const char* home = std::getenv("HOME")) {
            const std::string docs = std::string(home) + "/Documents";
            setenv("NX_DATA_ROOT", docs.c_str(), 1);
        }
    // The log goes with the saves, where the Files app shows it (the loader's default is one folder above Documents).
    if (!std::getenv("NX_LOG"))
        if (const char* root = std::getenv("NX_DATA_ROOT")) {
            const std::string logs = std::string(root) + "/logs";
            mkdir(logs.c_str(), 0755);  // the player opens the log with fopen: the folder must exist
            setenv("NX_LOG", (logs + "/nx.log").c_str(), 1);
        }
#endif
    nx::config_apply();  // config.ini -> NX_* defaults, before anything reads them (config.h)
    if (nx::report_mode()) return 0;
    struct sigaction sa{};
    sa.sa_sigaction = crash_handler;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGSEGV, &sa, nullptr);
    sigaction(SIGBUS, &sa, nullptr);
    sigaction(SIGILL, &sa, nullptr);
    sigaction(SIGFPE, &sa, nullptr);

    // MoltenVK encodes the Metal commands of a vkQueueSubmit inside the call. Its own dispatch queue (=0) took ~2.4 ms
    // of encoding per frame off the replay thread, but crashed MoltenVK 1.4.2 within minutes at 60 fps (signal 11 in
    // MVKAttachmentDescription, the render pass of a vkCmdBeginRendering, on its encoding thread; 5 of 5 runs, none of
    // 2 synchronous; the image views it named were all alive). Set before the Vulkan library loads; an
    // MVK_CONFIG_SYNCHRONOUS_QUEUE_SUBMITS in the environment wins.
    setenv("MVK_CONFIG_SYNCHRONOUS_QUEUE_SUBMITS", "1", 0);
    // Descriptor sets as Metal argument buffers (MoltenVK's default) cost the replay ~2.5 ms a frame at ~4000 sets a
    // frame: walking about the island's north at 60 fps on an M1 Air, replay 12.5 -> 10.1 ms and 56-59 -> 60 fps,
    // the picture the same (island, inventory, phone, map). Off; an MVK_CONFIG_USE_METAL_ARGUMENT_BUFFERS set wins.
    setenv("MVK_CONFIG_USE_METAL_ARGUMENT_BUFFERS", "0", 0);
    // A release package: the Vulkan loader and MoltenVK beside the player, before any
    // installed ones. A VK_DRIVER_FILES or SDL_VULKAN_LIBRARY in the environment wins.
    if (const char* base = SDL_GetBasePath()) {
        // Inside ACNHRecomp.app the libraries are in Contents/MacOS with the player, the ICD file in
        // Contents/Resources (SDL's base there); in a plain folder both are beside the player.
        // ACNH-Universal.app (the one contained app, crates/core/src/assemble.rs) keeps them in Contents/Frameworks.
        std::string dir = base;
        if (const size_t r = dir.rfind("/Contents/Resources/"); r != std::string::npos && r + 20 == dir.size()) {
            dir = dir.substr(0, r) + "/Contents/Frameworks/";
            if (access((dir + "libvulkan.1.dylib").c_str(), R_OK) != 0) dir = dir.substr(0, r) + "/Contents/MacOS/";
        }
        const std::string loader = dir + "libvulkan.1.dylib", icd = std::string(base) + "MoltenVK_icd.json";
        if (access(loader.c_str(), R_OK) == 0 && access(icd.c_str(), R_OK) == 0) {
            setenv("SDL_VULKAN_LIBRARY", loader.c_str(), 0);
            setenv("VK_DRIVER_FILES", icd.c_str(), 0);
        }
    }
    // One app id with the launcher: Wayland and the taskbars find the icon in io.github.acnhrecomp.desktop.
    if (!nx::plat::sdl_start()) return 1;  // SDL_Init, and this thread is the main one

    nx::plat::start_memory_guard();  // before anything can grow: a leak must never freeze the machine
#ifdef NX_GAME_DYNAMIC
    if (!nx::plat::load_game_or_explain()) return 1;  // game_folder.cpp
#endif
    thread_cpu_watch();              // NX_THREAD_CPU

    // The game on its own thread, with a big stack (recompiled calls nest deeply); the main thread keeps Cocoa's
    // event loop.
#if TARGET_OS_IPHONE
    SDL_AddEventWatch(background_watch, nullptr);
    nx::plat::sdl_set_quit_hook(relay_after_game);  // the whole push: no watchdog (relay_terminate is for TERMINATING)
#endif
    nx::plat::start_thread([](void*) { game_thread(); relay_after_game(); nx::plat::exit_process(0); }, nullptr, 64ull << 20);

    pthread_setname_np("events");        // the Cocoa main thread (NX_THREAD_CPU names it)
    nx::cpu_profile_register("events");  // NX_CPU_PROFILE (cpu_profile.cpp)
    nx::plat::sdl_main_loop();
}
