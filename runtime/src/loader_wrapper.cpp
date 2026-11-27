// Thin C++ wrapper forwarding loader, dispatch, HLE registry, logging, game_abi, and boot
// (`loader.cpp`, `game_abi.cpp`, `main.cpp`) to crates/runtime/src/loader.rs (`acnh_loader_*`).
#if defined(NX_GAME_ABI_ONLY)

#include <nx/cpu.h>
#ifdef _WIN32
#include <nx/runtime.h>
#include <cstddef>
#include <cstring>
#define NX_GAME_EXPORT __declspec(dllexport)
#else
#define NX_GAME_EXPORT __attribute__((visibility("default")))
#endif

extern "C" NX_GAME_EXPORT const char* nx_game_abi() { return NX_GAME_ABI; }

extern "C" NX_GAME_EXPORT [[noreturn]] void nx_game_longjmp(uint64_t buf, uint64_t val) {
    throw nx::LongJmp{buf, val};
}

#ifdef _WIN32
namespace {
struct Api {
    void (*call)(nx::CPU&, uint64_t);
    void (*svc)(nx::CPU&, uint32_t, uint64_t);
    uint64_t (*mrs)(nx::CPU&, uint32_t);
    void (*msr)(nx::CPU&, uint32_t, uint64_t);
    void (*unimpl)(nx::CPU&, uint64_t, const char*);
    void (*hook)(nx::CPU&, uint64_t);
    void (*trace_sp)(nx::CPU&, uint64_t);
} g_api;
}  // namespace

extern "C" NX_GAME_EXPORT void nx_game_bind(const void* api, size_t size) {
    std::memcpy(&g_api, api, size < sizeof g_api ? size : sizeof g_api);
}

namespace nx {
void call(CPU& c, uint64_t target) { g_api.call(c, target); }
void svc(CPU& c, uint32_t imm, uint64_t pc) { g_api.svc(c, imm, pc); }
uint64_t mrs(CPU& c, uint32_t sysreg) { return g_api.mrs(c, sysreg); }
void msr(CPU& c, uint32_t sysreg, uint64_t value) { g_api.msr(c, sysreg, value); }
[[noreturn]] void unimpl(CPU& c, uint64_t pc, const char* what) {
    g_api.unimpl(c, pc, what);
    __builtin_unreachable();
}
void trace_sp(CPU& c, uint64_t pc) { g_api.trace_sp(c, pc); }
}  // namespace nx
void nx_hook(nx::CPU& c, uint64_t pc) { g_api.hook(c, pc); }

namespace {
const char* g_variant_module = nullptr;
const uint32_t* g_variant_words = nullptr;
size_t g_variant_count = 0;
}  // namespace
extern "C" NX_GAME_EXPORT bool nx_variant_on = false;
extern "C" NX_GAME_EXPORT uint64_t nx_variant_lo = 0, nx_variant_hi = ~0ull;
nx::VariantRegistrar::VariantRegistrar(const char* module, const uint32_t* words, size_t count) {
    g_variant_module = module, g_variant_words = words, g_variant_count = count;
}
extern "C" NX_GAME_EXPORT size_t nx_game_variant(const char** module, const uint32_t** words) {
    *module = g_variant_module, *words = g_variant_words;
    return g_variant_module ? g_variant_count : 0;
}
extern "C" NX_GAME_EXPORT void nx_game_set_variant(bool on, uint64_t lo, uint64_t hi) {
    nx_variant_on = on, nx_variant_lo = lo, nx_variant_hi = hi;
}
#endif

#else  // !NX_GAME_ABI_ONLY

#include <atomic>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <string>

#include <nx/game.h>
#include <nx/runtime.h>

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#else
extern "C" {
struct AcnhCpu;
using AcnhHleFn = void (*)(AcnhCpu*);
struct AcnhFuncEntry {
    uint64_t offset;
    AcnhHleFn fn_ptr;
};
struct AcnhReloc {
    uint64_t offset;
    uint32_t reloc_type;
    uint32_t sym;
    int64_t addend;
};
struct AcnhSymbol {
    const char* name;
    uint64_t value;
    uint64_t size;
    uint8_t sym_type;
    uint8_t bind;
    uint8_t imported;
};
struct AcnhModuleInfo {
    const char* name;
    uint64_t base;
    uint64_t text, text_size, ro, ro_size, data, data_size, bss, bss_size;
    const AcnhFuncEntry* funcs;
    size_t func_count;
    const AcnhReloc* relocs;
    size_t reloc_count;
    const AcnhSymbol* symbols;
    size_t symbol_count;
    const uint64_t* init_array;
    size_t init_count;
    const uint64_t* fini_array;
    size_t fini_count;
};
struct AcnhLoadedModule {
    const AcnhModuleInfo* info;
    uint64_t base;
    uint64_t size;
};
struct AcnhLoaderLayout {
    uint64_t module_base;
    uint64_t thunk_base;
    uint64_t thunk_size;
};
struct AcnhLoaderHost {
    size_t (*data_root)(char* out, size_t cap);
    size_t (*game_root)(char* out, size_t cap);
    size_t (*setting)(const char* key, char* out, size_t cap);
    void (*log_line)(const char* line);
    void (*show_fatal)(const char* msg);
    void (*exit_process)(int32_t code);
    void (*backtrace)(AcnhCpu* cpu);
    bool (*commit)(uint64_t addr, uint64_t size);
    void (*vm_set)(uint64_t addr, uint64_t size, uint32_t state, uint32_t perm);
    void (*memory_init)();
    void (*kernel_init)();
    void (*set_tls_template)(uint64_t start, uint64_t end);
    uint32_t (*create_main_thread)(uint64_t stack_size);
    AcnhCpu* (*current_cpu)();
    uint64_t (*system_tick)();
    const AcnhModuleInfo* (*game_module)(const char* name);
    void (*game_longjmp)(uint64_t buf, uint64_t val);
    void (*game_set_variant)(bool on, uint64_t lo, uint64_t hi);
    void (*after_init)();
    void (*sync_variant)(bool on, uint64_t lo, uint64_t hi);
    void (*sync_trace)(bool trace);
};
struct AcnhVariantState {
    bool on;
    uint64_t lo;
    uint64_t hi;
};
uint32_t acnh_runtime_abi_version();
void acnh_loader_set_host(const AcnhLoaderHost* h);
void acnh_loader_set_layout(const AcnhLoaderLayout* lay);
void acnh_loader_set_trace(bool on);
bool acnh_loader_trace();
void acnh_loader_log(const char* msg);
void acnh_loader_log_flush();
size_t acnh_loader_log_file_path(char* out, size_t cap);
[[noreturn]] void acnh_loader_fatal(const char* msg);
void acnh_loader_register_hle(const char* name, AcnhHleFn fn_ptr);
AcnhHleFn acnh_loader_hle_lookup(const char* name);
uint64_t acnh_loader_hle_address(const char* name);
uint64_t acnh_loader_hle_address_or(const char* name, AcnhHleFn fallback);
const char* acnh_loader_hle_current_name();
void acnh_loader_register_hook(uint64_t offset, AcnhHleFn fn_ptr);
void acnh_loader_hook(AcnhCpu* c, uint64_t pc);
void acnh_loader_register_variant(const char* module, const uint32_t* words, size_t count);
AcnhVariantState acnh_loader_variant_state();
bool acnh_loader_module_at(uint64_t addr, AcnhLoadedModule* out);
size_t acnh_loader_describe(uint64_t addr, char* out, size_t cap);
uint64_t acnh_loader_find_symbol(const char* name);
void acnh_loader_call(AcnhCpu* c, uint64_t target);
void acnh_loader_trace_sp(AcnhCpu* c, uint64_t pc);
[[noreturn]] void acnh_loader_unimpl(AcnhCpu* c, uint64_t pc, const char* what);
uint64_t acnh_loader_mrs(AcnhCpu* c, uint32_t sysreg);
void acnh_loader_msr(AcnhCpu* c, uint32_t sysreg, uint64_t value);
void acnh_loader_load_modules(const AcnhModuleInfo* const* mods, size_t count, const char* data_dir, bool dynamic);
void acnh_loader_boot(bool dynamic, uint32_t expected_abi);
}
#endif

#include "kernel.h"
#include "platform.h"

#ifdef NX_GAME_DYNAMIC
namespace nx::plat {
const nx::ModuleInfo* game_module(const char* name);
[[noreturn]] void game_longjmp(uint64_t buf, uint64_t val);
}  // namespace nx::plat
#else
extern const nx::ModuleInfo nx_module_main;
extern const nx::ModuleInfo nx_module_sdk;
#ifdef NX_HAS_SUBSDK0
extern const nx::ModuleInfo nx_module_subsdk0;
#endif
#endif

extern "C" NX_GAME_API bool nx_variant_on = false;
extern "C" NX_GAME_API uint64_t nx_variant_lo = 0, nx_variant_hi = ~0ull;

namespace nx {

bool g_trace = false;
std::atomic<uint64_t> g_frame{0};

void backtrace(CPU& c);
void discord_presence_start();
void cpu_profile_register(const char* name);
namespace nextendo {
void load_account();
void presence_start();
}  // namespace nextendo
namespace save_unlink {
void run_if_requested();
}  // namespace save_unlink

namespace {

size_t copy_str(const std::string& s, char* out, size_t cap) {
    if (out && cap > 0) {
        size_t n = s.size() < cap - 1 ? s.size() : cap - 1;
        if (n > 0) std::memcpy(out, s.data(), n);
        out[n] = '\0';
    }
    return s.size();
}

size_t host_data_root(char* out, size_t cap) { return copy_str(plat::data_root(), out, cap); }
size_t host_game_root(char* out, size_t cap) { return copy_str(plat::game_root(), out, cap); }
size_t host_setting(const char* key, char* out, size_t cap) { return copy_str(plat::setting(key ? key : ""), out, cap); }
void host_log_line(const char* line) { plat::log_line(line ? line : ""); }
void host_show_fatal(const char* msg) { plat::show_fatal(msg ? msg : ""); }
void host_exit_process(int32_t code) { plat::exit_process(code); }
void host_backtrace(AcnhCpu* cpu) {
    if (cpu) backtrace(*reinterpret_cast<CPU*>(cpu));
}
bool host_commit(uint64_t addr, uint64_t size) { return commit(addr, size); }
void host_vm_set(uint64_t addr, uint64_t size, uint32_t state, uint32_t perm) { vm_set(addr, size, state, perm); }
void host_memory_init() { memory_init(); }
void host_kernel_init() { kernel_init(); }
void host_set_tls_template(uint64_t start, uint64_t end) { set_tls_template(start, end); }
uint32_t host_create_main_thread(uint64_t stack_size) { return create_main_thread(stack_size); }
AcnhCpu* host_current_cpu() { return reinterpret_cast<AcnhCpu*>(&current_cpu()); }
uint64_t host_system_tick() { return system_tick(); }

const AcnhModuleInfo* host_game_module(const char* name) {
    if (!name) return nullptr;
#ifdef NX_GAME_DYNAMIC
    return reinterpret_cast<const AcnhModuleInfo*>(plat::game_module(name));
#else
    if (std::strcmp(name, "main") == 0) return reinterpret_cast<const AcnhModuleInfo*>(&nx_module_main);
    if (std::strcmp(name, "sdk") == 0) return reinterpret_cast<const AcnhModuleInfo*>(&nx_module_sdk);
#ifdef NX_HAS_SUBSDK0
    if (std::strcmp(name, "subsdk0") == 0) return reinterpret_cast<const AcnhModuleInfo*>(&nx_module_subsdk0);
#endif
    return nullptr;
#endif
}

void host_game_longjmp(uint64_t buf, uint64_t val) {
#ifdef NX_GAME_DYNAMIC
    plat::game_longjmp(buf, val);
#else
    throw LongJmp{buf, uint32_t(val)};
#endif
}

void host_game_set_variant(bool on, uint64_t lo, uint64_t hi) {
#if defined(_WIN32) && defined(NX_GAME_DYNAMIC)
    plat::game_set_variant(on, lo, hi);
#else
    (void)on;
    (void)lo;
    (void)hi;
#endif
}

void host_after_init() {
    game::after_init();
    save_unlink::run_if_requested();
    discord_presence_start();
    nextendo::load_account();
    nextendo::presence_start();
    cpu_profile_register("main");
}

void host_sync_variant(bool on, uint64_t lo, uint64_t hi) {
    nx_variant_on = on;
    nx_variant_lo = lo;
    nx_variant_hi = hi;
}

void host_sync_trace(bool trace) { g_trace = trace; }

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhLoaderLayout lay{};
        lay.module_base = static_cast<uint64_t>(kModuleBase);
        lay.thunk_base = static_cast<uint64_t>(kThunkBase);
        lay.thunk_size = kThunkSize;
        acnh_loader_set_layout(&lay);

        AcnhLoaderHost h{};
        h.data_root = host_data_root;
        h.game_root = host_game_root;
        h.setting = host_setting;
        h.log_line = host_log_line;
        h.show_fatal = host_show_fatal;
        h.exit_process = host_exit_process;
        h.backtrace = host_backtrace;
        h.commit = host_commit;
        h.vm_set = host_vm_set;
        h.memory_init = host_memory_init;
        h.kernel_init = host_kernel_init;
        h.set_tls_template = host_set_tls_template;
        h.create_main_thread = host_create_main_thread;
        h.current_cpu = host_current_cpu;
        h.system_tick = host_system_tick;
        h.game_module = host_game_module;
        h.game_longjmp = host_game_longjmp;
        h.game_set_variant = host_game_set_variant;
        h.after_init = host_after_init;
        h.sync_variant = host_sync_variant;
        h.sync_trace = host_sync_trace;
        acnh_loader_set_host(&h);
    });
}

}  // namespace

std::string log_file_path() {
    ensure_host();
    char buf[4096];
    size_t n = acnh_loader_log_file_path(buf, sizeof(buf));
    return std::string(buf, n < sizeof(buf) ? n : sizeof(buf) - 1);
}

void log_flush() {
    ensure_host();
    acnh_loader_log_flush();
}

void log(const char* fmt, ...) {
    ensure_host();
    char buf[2048];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    acnh_loader_log(buf);
}

[[noreturn]] void fatal(const char* fmt, ...) {
    ensure_host();
    char buf[2048];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    acnh_loader_fatal(buf);
}

HleRegistrar::HleRegistrar(const char* name, HleFn fn) {
    ensure_host();
    acnh_loader_register_hle(name, reinterpret_cast<void (*)(AcnhCpu*)>(fn));
}

HookRegistrar::HookRegistrar(uint64_t offset, HleFn fn) {
    ensure_host();
    acnh_loader_register_hook(offset, reinterpret_cast<void (*)(AcnhCpu*)>(fn));
}

NX_GAME_API VariantRegistrar::VariantRegistrar(const char* module, const uint32_t* words, size_t count) {
    ensure_host();
    acnh_loader_register_variant(module, words, count);
}

HleFn hle_lookup(const char* name) {
    ensure_host();
    return reinterpret_cast<HleFn>(acnh_loader_hle_lookup(name));
}

uint64_t hle_address(const char* name) {
    ensure_host();
    return acnh_loader_hle_address(name);
}

uint64_t hle_address_or(const char* name, HleFn fallback) {
    ensure_host();
    return acnh_loader_hle_address_or(name, reinterpret_cast<void (*)(AcnhCpu*)>(fallback));
}

const char* hle_current_name() {
    ensure_host();
    return acnh_loader_hle_current_name();
}

const LoadedModule* module_at(uint64_t addr) {
    ensure_host();
    thread_local LoadedModule tl{};
    AcnhLoadedModule m{};
    if (!acnh_loader_module_at(addr, &m)) return nullptr;
    tl.info = reinterpret_cast<const ModuleInfo*>(m.info);
    tl.base = m.base;
    tl.size = m.size;
    return &tl;
}

std::string describe(uint64_t addr) {
    ensure_host();
    char buf[512];
    size_t n = acnh_loader_describe(addr, buf, sizeof(buf));
    return std::string(buf, n < sizeof(buf) ? n : sizeof(buf) - 1);
}

uint64_t find_symbol(const char* name) {
    ensure_host();
    return acnh_loader_find_symbol(name);
}

void load_modules(const ModuleInfo* const* mods, size_t count, const std::string& data_dir) {
    ensure_host();
#ifdef NX_GAME_DYNAMIC
    constexpr bool kDynamic = true;
#else
    constexpr bool kDynamic = false;
#endif
    acnh_loader_load_modules(reinterpret_cast<const AcnhModuleInfo* const*>(mods), count, data_dir.c_str(), kDynamic);
}

NX_GAME_API void call(CPU& c, uint64_t t) {
    acnh_loader_call(reinterpret_cast<AcnhCpu*>(&c), t);
}

NX_GAME_API void trace_sp(CPU& c, uint64_t pc) {
    acnh_loader_trace_sp(reinterpret_cast<AcnhCpu*>(&c), pc);
}

[[noreturn]] NX_GAME_API void unimpl(CPU& c, uint64_t pc, const char* what) {
    acnh_loader_unimpl(reinterpret_cast<AcnhCpu*>(&c), pc, what);
}

NX_GAME_API uint64_t mrs(CPU& c, uint32_t sysreg) {
    return acnh_loader_mrs(reinterpret_cast<AcnhCpu*>(&c), sysreg);
}

NX_GAME_API void msr(CPU& c, uint32_t sysreg, uint64_t value) {
    acnh_loader_msr(reinterpret_cast<AcnhCpu*>(&c), sysreg, value);
}

void boot() {
    ensure_host();
#ifdef NX_GAME_DYNAMIC
    constexpr bool kDynamic = true;
#else
    constexpr bool kDynamic = false;
#endif
    acnh_loader_boot(kDynamic, kRuntimeAbi);
}

}  // namespace nx

NX_GAME_API void nx_hook(nx::CPU& c, uint64_t pc) {
    acnh_loader_hook(reinterpret_cast<AcnhCpu*>(&c), pc);
}

#endif  // !NX_GAME_ABI_ONLY
