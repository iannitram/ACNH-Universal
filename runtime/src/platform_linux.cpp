// Linux host (x86-64), the OS half: memory (mmap at fixed addresses), pthreads, time, files, the signal crash handler
// and main. The window, input, audio and the main thread's event loop are the SDL host's (platform_sdl_wrapper.cpp,
// platform_sdl.h), shared with Windows and macOS: main() starts the game (nx::boot) on its own thread and then runs
// that event loop.
//
// Data: NX_DATA_ROOT or games/<game>'s pc_data_root ("private/<game>"): analysis/{main,sdk}.bin, extract/romfs, user/.
// Settings: NX_<KEY> environment variables, as on Windows and macOS.
#include <dlfcn.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <cctype>
#include <cerrno>
#include <climits>
#include <condition_variable>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <filesystem>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <SDL3/SDL.h>

#include <nx/game.h>
#include <nx/runtime.h>

#include "kernel.h"
#include "config.h"
#include "bug_report.h"
#include "platform.h"
#include "platform_sdl.h"

namespace nx {
void boot();
}

namespace nx::plat {

// ---- memory ---------------------------------------------------------------------------------------------------

bool reserve(uint64_t addr, uint64_t size) {
    void* p = mmap(reinterpret_cast<void*>(addr), size, PROT_NONE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE | MAP_FIXED_NOREPLACE, -1, 0);
    if (p == MAP_FAILED) return false;
    if (uint64_t(p) != addr) {  // kernels before 4.17 take MAP_FIXED_NOREPLACE as a hint
        munmap(p, size);
        return false;
    }
    return true;
}

bool commit(uint64_t addr, uint64_t size) {
    uint64_t page = uint64_t(getpagesize()) - 1;
    uint64_t a = addr & ~page, e = (addr + size + page) & ~page;
    return mprotect(reinterpret_cast<void*>(a), e - a, PROT_READ | PROT_WRITE) == 0;
}

// As on macOS: only whole host pages are released (MADV_DONTNEED gives zero pages back on the next touch); the freed
// part of a page shared with live data is zeroed instead. With 4 KiB host pages that is the guest's own granule.
void decommit(uint64_t addr, uint64_t size) {
    uint64_t page = uint64_t(getpagesize());
    uint64_t lo = (addr + page - 1) & ~(page - 1), hi = (addr + size) & ~(page - 1);
    if (lo >= hi) {
        std::memset(reinterpret_cast<void*>(addr), 0, size);
        return;
    }
    if (addr < lo) std::memset(reinterpret_cast<void*>(addr), 0, lo - addr);
    if (hi < addr + size) std::memset(reinterpret_cast<void*>(hi), 0, addr + size - hi);
    madvise(reinterpret_cast<void*>(lo), hi - lo, MADV_DONTNEED);
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
    s.fn(s.arg);
    return nullptr;
}

void start_thread(ThreadFn fn, void* arg, size_t stack_size) {
    pthread_attr_t a;
    pthread_attr_init(&a);
    size_t page = size_t(getpagesize()) - 1;
    pthread_attr_setstacksize(&a, std::max<size_t>((stack_size + page) & ~page, PTHREAD_STACK_MIN));
    pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    pthread_t t;
    pthread_create(&t, &a, thread_entry, new ThreadStart{fn, arg});
    pthread_attr_destroy(&a);
}

void exit_thread() { pthread_exit(nullptr); }
void exit_process(int code) {
    std::fflush(nullptr);
    _exit(code);
}

void sleep_ns(int64_t ns) {
    timespec ts{time_t(ns / 1000000000), long(ns % 1000000000)};
    while (clock_nanosleep(CLOCK_MONOTONIC, 0, &ts, &ts) == EINTR) {
    }
}

void yield() { sched_yield(); }

void set_thread_name(const char* name) {
    char n[16];  // the kernel keeps 15 characters
    std::snprintf(n, sizeof n, "%s", name);
    pthread_setname_np(pthread_self(), n);
}

int64_t current_thread_id() { return int64_t(syscall(SYS_gettid)); }

bool profile(const int64_t*, const uint64_t* const*, size_t, int, const std::string&) { return false; }  // use perf

uint64_t monotonic_ns() {
    timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return uint64_t(ts.tv_sec) * 1000000000ull + uint64_t(ts.tv_nsec);
}

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



std::string device_locale() { return ""; }

uint64_t wait_vblank() { return 0; }  // no display vblank to wait on here: hle_vi.cpp keeps its timed tick
}  // namespace nx::plat

// ---- crash handler and entry ----------------------------------------------------------------------------------

namespace {


void crash_handler(int sig, siginfo_t* info, void* uc) {
    if ((sig == SIGSEGV || sig == SIGBUS) && nx::plat::write_watch_fault(info->si_addr)) return;  // write_watch.cpp
    const auto& m = static_cast<ucontext_t*>(uc)->uc_mcontext;
    Dl_info d{};
    uint64_t pc = uint64_t(m.gregs[REG_RIP]);
    dladdr(reinterpret_cast<void*>(pc), &d);
    nx::log("CRASH: signal %d at %s+%llx (%s), data address %llx", sig, d.dli_fname ? d.dli_fname : "?",
            (unsigned long long)(pc - uint64_t(d.dli_fbase)), d.dli_sname ? d.dli_sname : "?",
            (unsigned long long)uint64_t(info->si_addr));
    nx::CPU& c = nx::current_cpu();
    nx::log("  guest lr %s  sp %llx", nx::describe(c.x[30]).c_str(), (unsigned long long)c.x[31]);
    for (int k = 0; k < 32; k += 4)
        nx::log("  x%-2d %016llx %016llx %016llx %016llx", k, (unsigned long long)c.x[k], (unsigned long long)c.x[k + 1],
                (unsigned long long)c.x[k + 2], (unsigned long long)c.x[k + 3]);
    nx::backtrace(c);
    nx::log_recent_files();
    std::fflush(nullptr);
    signal(sig, SIG_DFL);
    raise(sig);
}

void game_thread() {
    try {
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
    nx::config_apply();  // config.ini -> NX_* defaults, before anything reads them (config.h)
    if (nx::report_mode()) return 0;

    // The crash handler runs on its own stack: a guest stack overflow would leave it none.
    static std::vector<uint8_t> alt(1 << 20);
    stack_t ss{};
    ss.ss_sp = alt.data();
    ss.ss_size = alt.size();
    sigaltstack(&ss, nullptr);
    struct sigaction sa{};
    sa.sa_sigaction = crash_handler;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGSEGV, &sa, nullptr);
    sigaction(SIGBUS, &sa, nullptr);
    sigaction(SIGILL, &sa, nullptr);
    sigaction(SIGFPE, &sa, nullptr);

    // On Wayland SDL draws window decorations through libdecor, whose GTK plugin aborts the process on some GNOME
    // setups (a gsettings schema without the "antialiasing" key: GLib-GIO-ERROR in gtk_init_check). XWayland
    // decorates the window itself, so prefer X11 when it is there; SDL_VIDEO_DRIVER still decides when set.
    if (!std::getenv("SDL_VIDEO_DRIVER") && std::getenv("DISPLAY")) SDL_SetHint(SDL_HINT_VIDEO_DRIVER, "x11,wayland");
    if (!nx::plat::sdl_start()) return 1;  // SDL_Init, and this thread is the main one

    nx::plat::start_memory_guard();  // before anything can grow: a leak must never freeze the machine

#ifdef NX_GAME_DYNAMIC
    if (!nx::plat::load_game_or_explain()) return 1;  // game_folder.cpp
#endif

    // The game on its own thread, with a big stack (recompiled calls nest deeply); the main thread keeps the event
    // loop.
    nx::plat::start_thread([](void*) { game_thread(); nx::plat::exit_process(0); }, nullptr, 64ull << 20);

    nx::plat::sdl_main_loop();
}
