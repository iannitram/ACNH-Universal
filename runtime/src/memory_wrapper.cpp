// Thin C++ wrapper forwarding nx::memory_*, nx::vm_*, nx::kernel_alloc, nx::plat::write_watch_*,
// and nx::plat::start_memory_guard / memguard_overlap_ns to crates/runtime/src/memory.rs.
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>

#include <nx/runtime.h>

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#endif

#include "platform.h"

#if defined(_WIN32)
#include <windows.h>
#include <psapi.h>
#else
#include <sys/mman.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <TargetConditionals.h>
#include <mach/mach.h>
#if TARGET_OS_IPHONE
#include <os/proc.h>
#endif
#endif
#endif

#ifndef NX_RUST_RUNTIME
extern "C" {
struct AcnhVmInfo {
    uint64_t addr;
    uint64_t size;
    uint32_t state;
    uint32_t perm;
};
struct AcnhMemoryBounds {
    uint64_t address_space_base;
    uint64_t address_space_size;
    uint64_t module_base;
    uint64_t module_size;
    uint64_t heap_base;
    uint64_t heap_max;
    uint64_t alias_base;
    uint64_t alias_size;
    uint64_t stack_base;
    uint64_t stack_size;
    uint64_t kernel_base;
    uint64_t kernel_size;
    uint64_t thunk_base;
    uint64_t thunk_size;
    bool compact_layout;
};
struct AcnhMemoryHost {
    bool (*reserve)(uint64_t addr, uint64_t size);
    bool (*commit)(uint64_t addr, uint64_t size);
    void (*decommit)(uint64_t addr, uint64_t size);
    void (*log)(const char* msg);
    void (*fatal)(const char* msg);
    bool (*mprotect_ro)(uint64_t addr, uint64_t size);
    bool (*mprotect_rw)(uint64_t addr, uint64_t size);
    uint64_t (*page_size)();
    uint64_t (*monotonic_ns)();
    void (*log_flush)();
    uint64_t (*memguard_footprint)(bool use_smaps);
    uint64_t (*memguard_total_ram_bytes)();
    uint64_t (*memguard_ios_available_bytes)();
    void (*memguard_terminate)(int32_t code);
    void (*memguard_stderr)(const char* msg);
};
void acnh_memory_set_host(const AcnhMemoryHost* host);
void acnh_memory_init(const AcnhMemoryBounds* bounds);
bool acnh_memory_commit(uint64_t addr, uint64_t size);
void acnh_memory_decommit(uint64_t addr, uint64_t size);
uint64_t acnh_memory_kernel_alloc(uint64_t size, uint64_t align);
void acnh_memory_vm_set(uint64_t addr, uint64_t size, uint32_t state, uint32_t perm);
AcnhVmInfo acnh_memory_vm_query(uint64_t addr);
bool acnh_write_watch_add(uint64_t addr, uint64_t size);
void acnh_write_watch_remove(uint64_t addr);
bool acnh_write_watch_take(uint64_t addr, uint64_t* out_lo, uint64_t* out_hi);
bool acnh_write_watch_fault(uint64_t at);
uint64_t acnh_memguard_overlap_ns(uint64_t from, uint64_t to);
double acnh_memguard_limit_gb(const char* env);
uint64_t acnh_memguard_footprint();
void acnh_memguard_start(uint32_t platform);
}
#endif

namespace {

bool host_reserve(uint64_t addr, uint64_t size) { return nx::plat::reserve(addr, size); }
bool host_commit(uint64_t addr, uint64_t size) { return nx::plat::commit(addr, size); }
void host_decommit(uint64_t addr, uint64_t size) { nx::plat::decommit(addr, size); }
void host_log(const char* msg) { nx::log("%s", msg ? msg : ""); }
void host_fatal(const char* msg) { nx::fatal("%s", msg ? msg : ""); }

bool host_mprotect_ro(uint64_t addr, uint64_t size) {
#if !defined(_WIN32)
    return mprotect(reinterpret_cast<void*>(addr), size_t(size), PROT_READ) == 0;
#else
    (void)addr;
    (void)size;
    return false;
#endif
}

bool host_mprotect_rw(uint64_t addr, uint64_t size) {
#if !defined(_WIN32)
    return mprotect(reinterpret_cast<void*>(addr), size_t(size), PROT_READ | PROT_WRITE) == 0;
#else
    (void)addr;
    (void)size;
    return false;
#endif
}

uint64_t host_page_size() {
#if !defined(_WIN32)
    return uint64_t(sysconf(_SC_PAGESIZE));
#else
    return 4096;
#endif
}

uint64_t host_monotonic_ns() { return nx::plat::monotonic_ns(); }

void host_log_flush() {
#if defined(__ANDROID__)
    nx::log_flush();
#endif
}

uint64_t host_memguard_footprint(bool use_smaps) {
#if defined(_WIN32)
    (void)use_smaps;
    PROCESS_MEMORY_COUNTERS_EX pmc{};
    pmc.cb = sizeof pmc;
    if (!GetProcessMemoryInfo(GetCurrentProcess(), reinterpret_cast<PROCESS_MEMORY_COUNTERS*>(&pmc), sizeof pmc))
        return 0;
    return uint64_t(pmc.PrivateUsage);
#elif defined(__APPLE__)
    (void)use_smaps;
    task_vm_info_data_t info{};
    mach_msg_type_number_t count = TASK_VM_INFO_COUNT;
    if (task_info(mach_task_self(), TASK_VM_INFO, reinterpret_cast<task_info_t>(&info), &count) != KERN_SUCCESS)
        return 0;
    return info.phys_footprint;
#else
    if (use_smaps) {
        FILE* f = std::fopen("/proc/self/smaps_rollup", "r");
        if (!f) return 0;
        char line[256];
        uint64_t kb = 0;
        while (std::fgets(line, sizeof line, f)) {
            if (std::strncmp(line, "Pss:", 4) == 0) {
                kb = std::strtoull(line + 4, nullptr, 10);
                break;
            }
        }
        std::fclose(f);
        return kb << 10;
    }
    FILE* f = std::fopen("/proc/self/statm", "r");
    if (!f) return 0;
    unsigned long long size = 0, resident = 0;
    if (std::fscanf(f, "%llu %llu", &size, &resident) != 2) resident = 0;
    std::fclose(f);
    return uint64_t(resident) * uint64_t(sysconf(_SC_PAGESIZE));
#endif
}

uint64_t host_memguard_total_ram_bytes() {
#if defined(_WIN32)
    MEMORYSTATUSEX ms{};
    ms.dwLength = sizeof ms;
    return GlobalMemoryStatusEx(&ms) ? uint64_t(ms.ullTotalPhys) : 0;
#else
    return uint64_t(sysconf(_SC_PHYS_PAGES)) * uint64_t(sysconf(_SC_PAGESIZE));
#endif
}

uint64_t host_memguard_ios_available_bytes() {
#if defined(__APPLE__) && TARGET_OS_IPHONE
    return uint64_t(os_proc_available_memory());
#else
    return 0;
#endif
}

void host_memguard_terminate(int32_t code) {
#if defined(_WIN32)
    TerminateProcess(GetCurrentProcess(), UINT(code));
#else
    _exit(code);
#endif
}

void host_memguard_stderr(const char* msg) {
    std::fprintf(stderr, "%s\n", msg ? msg : "");
    std::fflush(nullptr);
}

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhMemoryHost host{};
        host.reserve = host_reserve;
        host.commit = host_commit;
        host.decommit = host_decommit;
        host.log = host_log;
        host.fatal = host_fatal;
        host.mprotect_ro = host_mprotect_ro;
        host.mprotect_rw = host_mprotect_rw;
        host.page_size = host_page_size;
        host.monotonic_ns = host_monotonic_ns;
        host.log_flush = host_log_flush;
        host.memguard_footprint = host_memguard_footprint;
        host.memguard_total_ram_bytes = host_memguard_total_ram_bytes;
        host.memguard_ios_available_bytes = host_memguard_ios_available_bytes;
        host.memguard_terminate = host_memguard_terminate;
        host.memguard_stderr = host_memguard_stderr;
        acnh_memory_set_host(&host);
    });
}

constexpr uint32_t memguard_platform_id() {
#if defined(__ANDROID__)
    return 1;
#elif defined(__APPLE__) && TARGET_OS_IPHONE
    return 3;
#elif defined(__APPLE__)
    return 2;
#elif defined(_WIN32)
    return 4;
#else
    return 0;
#endif
}

}  // namespace

namespace nx {

void memory_init() {
    ensure_host();
    AcnhMemoryBounds b{};
    b.address_space_base = uint64_t(kAddressSpaceBase);
    b.address_space_size = kAddressSpaceSize;
    b.module_base = uint64_t(kModuleBase);
    b.module_size = kModuleSize;
    b.heap_base = uint64_t(kHeapBase);
    b.heap_max = kHeapMax;
    b.alias_base = uint64_t(kAliasBase);
    b.alias_size = kAliasSize;
    b.stack_base = uint64_t(kStackBase);
    b.stack_size = kStackSize;
    b.kernel_base = uint64_t(kKernelBase);
    b.kernel_size = kKernelSize;
    b.thunk_base = uint64_t(kThunkBase);
    b.thunk_size = kThunkSize;
#if defined(NX_COMPACT_LAYOUT)
    b.compact_layout = true;
#else
    b.compact_layout = false;
#endif
    acnh_memory_init(&b);
}

bool commit(uint64_t addr, uint64_t size) {
    ensure_host();
    return acnh_memory_commit(addr, size);
}

void decommit(uint64_t addr, uint64_t size) {
    ensure_host();
    acnh_memory_decommit(addr, size);
}

uint64_t kernel_alloc(uint64_t size, uint64_t align) {
    ensure_host();
    return acnh_memory_kernel_alloc(size, align);
}

void vm_set(uint64_t addr, uint64_t size, uint32_t state, uint32_t perm) {
    ensure_host();
    acnh_memory_vm_set(addr, size, state, perm);
}

VmInfo vm_query(uint64_t addr) {
    ensure_host();
    AcnhVmInfo info = acnh_memory_vm_query(addr);
    return {info.addr, info.size, info.state, info.perm};
}

namespace plat {

bool write_watch_add(uint64_t addr, uint64_t size) {
    ensure_host();
    return acnh_write_watch_add(addr, size);
}

void write_watch_remove(uint64_t addr) {
    ensure_host();
    acnh_write_watch_remove(addr);
}

bool write_watch_take(uint64_t addr, uint64_t* lo, uint64_t* hi) {
    ensure_host();
    return acnh_write_watch_take(addr, lo, hi);
}

bool write_watch_fault(void* at) {
    ensure_host();
    return acnh_write_watch_fault(reinterpret_cast<uint64_t>(at));
}

void start_memory_guard() {
    ensure_host();
    acnh_memguard_start(memguard_platform_id());
}

uint64_t memguard_overlap_ns(uint64_t from, uint64_t to) {
    ensure_host();
    return acnh_memguard_overlap_ns(from, to);
}

#if defined(_WIN32)
uint64_t memory_footprint() {
    ensure_host();
    return acnh_memguard_footprint();
}

double memory_limit_gb(const char* env) {
    ensure_host();
    return acnh_memguard_limit_gb(env);
}
#endif

}  // namespace plat
}  // namespace nx
