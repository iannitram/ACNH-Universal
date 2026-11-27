// Thin C++ wrapper forwarding the Horizon kernel, synchronization, IPC, and diagnostics
// to crates/runtime/src/kernel.rs.
#include <algorithm>
#include <atomic>
#include <cstdarg>
#include <cstdlib>
#include <cstdio>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

#include <nx/game.h>
#include <nx/runtime.h>

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#endif

#include "ipc.h"
#include "kernel.h"
#include "kernel_state.h"
#include "platform.h"

#ifndef NX_RUST_RUNTIME
extern "C" {
struct alignas(16) AcnhV128 {
    uint64_t u64[2];
};
struct AcnhCpu {
    uint64_t x[32];
    AcnhV128 v[32];
    uint8_t nf, zf, cf, vf;
    uint64_t fpcr, fpsr;
    uint64_t tpidr, tpidrro;
    uint64_t excl_addr;
    uint64_t excl_val, excl_val_hi;
};
struct AcnhRegionBounds {
    uint64_t module_base;
    uint64_t heap_base;
    uint64_t heap_max;
    uint64_t alias_base;
    uint64_t alias_size;
    uint64_t stack_base;
    uint64_t stack_size;
    uint64_t guest_shift;
    bool compact_layout;
};
struct AcnhIpcBuffer {
    uint64_t addr;
    uint64_t size;
};
struct AcnhIpcDispatch {
    AcnhCpu* cpu;
    uint64_t service;
    uint32_t cmd;
    uint32_t msg_type;
    uint64_t pid;
    const uint8_t* in_ptr;
    size_t in_size;
    const uint32_t* in_copy;
    size_t in_copy_len;
    const uint32_t* in_move;
    size_t in_move_len;
    const AcnhIpcBuffer* a_bufs;
    size_t a_len;
    const AcnhIpcBuffer* b_bufs;
    size_t b_len;
    const AcnhIpcBuffer* x_bufs;
    size_t x_len;
    const AcnhIpcBuffer* c_bufs;
    size_t c_len;
    const uint64_t* in_services;
    size_t in_services_len;
    uint8_t* out_buf;
    size_t out_cap;
    size_t* out_len;
    uint32_t* out_copy;
    size_t out_copy_cap;
    size_t* out_copy_len;
    uint32_t* out_move;
    size_t out_move_cap;
    size_t* out_move_len;
    uint64_t* out_services;
    size_t out_services_cap;
    size_t* out_services_len;
};
struct AcnhKernelHost {
    uint64_t (*monotonic_ns)();
    void (*sleep_ns)(int64_t ns);
    void (*yield_thread)();
    int64_t (*current_thread_id)();
    void (*set_thread_name)(const char* name);
    void (*start_thread)(void (*entry)(void*), void* arg, size_t stack_size);
    void (*exit_thread)();
    void (*exit_process)(int32_t code);
    bool (*window_minimized)();
    size_t (*data_root)(char* buf, size_t cap);
    bool (*profile_samples)(const int64_t* tids, const uint64_t* const* lrs, size_t count, int32_t seconds,
                            const char* out_path);
    void (*log)(const char* msg);
    void (*fatal)(const char* msg);
    size_t (*describe)(uint64_t addr, char* buf, size_t cap);
    uint64_t (*kernel_alloc)(uint64_t size, uint64_t align);
    bool (*commit)(uint64_t addr, uint64_t size);
    void (*decommit)(uint64_t addr, uint64_t size);
    void (*vm_set)(uint64_t addr, uint64_t size, uint32_t state, uint32_t perm);
    void (*vm_query)(uint64_t addr, uint64_t* out_addr, uint64_t* out_size, uint32_t* out_state, uint32_t* out_perm);
    void (*call_guest)(AcnhCpu* cpu, uint64_t target);
    void (*cpu_profile_register)(const char* name);
    void (*init_tls_block)(uint64_t block);
    uint64_t (*get_info)(uint32_t info_type, uint32_t handle, uint64_t sub, uint32_t* out_rc);
    uint32_t (*set_thread_core_mask)(uint32_t handle, int32_t ideal, uint64_t mask, int32_t* out_core);
    uint32_t (*map_transfer_memory)(uint32_t handle, uint64_t addr, uint64_t size);
    uint32_t (*unmap_transfer_memory)(uint32_t handle, uint64_t addr, uint64_t size);
    bool (*is_hades)();
    bool (*get_trace)();
    uint64_t (*get_frame)();
    uint64_t (*get_guest_base)();
    void (*get_region_bounds)(AcnhRegionBounds* out);
    int32_t (*obj_signaled)(uint32_t handle);
    void (*obj_consume)(uint32_t handle);
    uint32_t (*event_signal)(uint32_t handle);
    uint32_t (*event_clear)(uint32_t handle, bool reset_signal);
    void (*on_thread_created)(uint32_t handle, uint64_t id, uint64_t entry, uint64_t tls, int32_t prio, int32_t core);
    void (*on_thread_exited)(uint32_t handle);
    void (*on_event_created)(uint32_t w_handle, uint32_t r_handle);
    void (*on_transfer_memory_created)(uint32_t handle, uint64_t addr, uint64_t size);
    void (*on_handle_close)(uint32_t handle);
    uint64_t (*service_make)(const char* name);
    size_t (*service_name)(uint64_t service, char* buf, size_t cap);
    uint32_t (*service_dispatch)(AcnhIpcDispatch* dispatch);
    void (*service_drop)(uint64_t service);
    uint32_t (*shared_mem_create)(const char* name, uint64_t size, uint64_t host_addr);
    uint32_t (*shared_mem_map)(uint32_t handle, uint64_t addr, uint64_t size);
};
void acnh_kernel_set_host(const AcnhKernelHost* host);
void acnh_kernel_init();
uint32_t acnh_kernel_alloc_handle();
void acnh_kernel_handle_close(uint32_t h);
void acnh_kernel_notify_all();
uint64_t acnh_kernel_heap_size();
uint64_t acnh_kernel_system_tick();
void acnh_kernel_hle_get_system_tick(AcnhCpu* c);
uint32_t acnh_kernel_create_main_thread(uint64_t stack_size);
AcnhCpu* acnh_kernel_current_cpu();
void acnh_kernel_backtrace(const AcnhCpu* c);
void acnh_kernel_svc(AcnhCpu* c, uint32_t imm, uint64_t pc);
void acnh_kernel_svc_profile_mark(int32_t role);
void acnh_kernel_svc_profile_report(uint64_t frames, int32_t role);
void acnh_kernel_svc_frame_take(uint64_t* total, uint64_t* top_ns, uint32_t* top_imm, uint64_t* top_pc);
void acnh_kernel_counters(uint64_t* tick_calls, uint64_t* yield_calls, uint64_t* yield_naps, uint64_t* sleep_calls);
uint32_t acnh_kernel_ipc_connect(const char* port, uint64_t* out_handle);
uint32_t acnh_kernel_ipc_request(AcnhCpu* c, uint32_t handle, uint64_t buf, uint64_t size);
uint32_t acnh_kernel_make_shared_memory(const char* name, uint64_t size);
uint32_t acnh_kernel_map_shared_memory(uint64_t addr, uint64_t size, uint64_t host_addr, uint64_t shm_size);
}
#endif

namespace nx {

static_assert(sizeof(AcnhCpu) == sizeof(CPU), "AcnhCpu must match nx::CPU");

void cpu_profile_register(const char* name);  // cpu_profile.cpp

namespace kstate {
std::recursive_mutex g_k;
std::unordered_map<uint32_t, std::shared_ptr<KObject>> g_handles;
thread_local KThread* t_self = nullptr;
}  // namespace kstate

namespace {

std::mutex g_obj_lock;

std::unordered_map<std::string, ServiceFactory>& factories() {
    static std::unordered_map<std::string, ServiceFactory> f;
    return f;
}

struct SmService : Service {
    SmService() : Service("sm:") {
        cmds[0] = [](IpcCtx&) { return 0u; };
        cmds[1] = [](IpcCtx& c) {
            char name[9] = {};
            std::memcpy(name, c.in, std::min<size_t>(8, c.in_size));
            log("sm: GetService(%s)", name);
            c.push_object(make_service(name));
            return 0u;
        };
    }
};

struct ServiceHolder {
    std::shared_ptr<Service> svc;
};

uint64_t box_service(std::shared_ptr<Service> svc) {
    if (!svc) return 0;
    return reinterpret_cast<uint64_t>(new ServiceHolder{std::move(svc)});
}

std::shared_ptr<Service> get_service(uint64_t token) {
    if (!token) return nullptr;
    return reinterpret_cast<ServiceHolder*>(token)->svc;
}

uint64_t host_monotonic_ns() { return plat::monotonic_ns(); }
void host_sleep_ns(int64_t ns) { plat::sleep_ns(ns); }
void host_yield_thread() { plat::yield(); }
int64_t host_current_thread_id() { return plat::current_thread_id(); }
void host_set_thread_name(const char* name) { plat::set_thread_name(name); }
void host_start_thread(void (*entry)(void*), void* arg, size_t stack_size) {
    plat::start_thread(entry, arg, stack_size);
}
void host_exit_thread() { plat::exit_thread(); }
void host_exit_process(int32_t code) { plat::exit_process(code); }
bool host_window_minimized() { return plat::window_minimized(); }
size_t host_data_root(char* buf, size_t cap) {
    std::string r = plat::data_root();
    size_t n = std::min(r.size(), cap ? cap - 1 : 0);
    if (buf && cap) {
        std::memcpy(buf, r.data(), n);
        buf[n] = '\0';
    }
    return n;
}
bool host_profile_samples(const int64_t* tids, const uint64_t* const* lrs, size_t count, int32_t seconds,
                          const char* out_path) {
    return plat::profile(tids, lrs, count, seconds, out_path ? out_path : "");
}
void host_log(const char* msg) {
    if (msg) log("%s", msg);
}
void host_fatal(const char* msg) { fatal("%s", msg ? msg : "fatal"); }
size_t host_describe(uint64_t addr, char* buf, size_t cap) {
    std::string s = describe(addr);
    size_t n = std::min(s.size(), cap ? cap - 1 : 0);
    if (buf && cap) {
        std::memcpy(buf, s.data(), n);
        buf[n] = '\0';
    }
    return n;
}
uint64_t host_kernel_alloc(uint64_t size, uint64_t align) { return kernel_alloc(size, align); }
bool host_commit(uint64_t addr, uint64_t size) { return commit(addr, size); }
void host_decommit(uint64_t addr, uint64_t size) { decommit(addr, size); }
void host_vm_set(uint64_t addr, uint64_t size, uint32_t state, uint32_t perm) { vm_set(addr, size, state, perm); }
void host_vm_query(uint64_t addr, uint64_t* out_addr, uint64_t* out_size, uint32_t* out_state, uint32_t* out_perm) {
    VmInfo v = vm_query(addr);
    if (out_addr) *out_addr = v.addr;
    if (out_size) *out_size = v.size;
    if (out_state) *out_state = v.state;
    if (out_perm) *out_perm = v.perm;
}
void host_call_guest(AcnhCpu* cpu, uint64_t target) { call(*reinterpret_cast<CPU*>(cpu), target); }
void host_cpu_profile_register(const char* name) { cpu_profile_register(name); }
void host_init_tls_block(uint64_t block) { init_tls_block(block); }
uint64_t host_get_info(uint32_t info_type, uint32_t handle, uint64_t sub, uint32_t* out_rc) {
    uint32_t rc = 0;
    uint64_t res = get_info(info_type, handle, sub, rc);
    if (out_rc) *out_rc = rc;
    return res;
}
uint32_t host_set_thread_core_mask(uint32_t handle, int32_t ideal, uint64_t mask, int32_t* out_core) {
    uint32_t rc = set_thread_core_mask(handle, ideal, mask);
    if (auto t = std::dynamic_pointer_cast<KThread>(handle_get(handle))) {
        if (out_core) *out_core = t->core;
    }
    return rc;
}
uint32_t host_map_transfer_memory(uint32_t handle, uint64_t addr, uint64_t size) {
    return map_transfer_memory(handle, addr, size);
}
uint32_t host_unmap_transfer_memory(uint32_t handle, uint64_t addr, uint64_t size) {
    return unmap_transfer_memory(handle, addr, size);
}
bool host_is_hades() { return std::strcmp(game::kInfo.id, "hades") == 0; }
bool host_get_trace() { return g_trace; }
uint64_t host_get_frame() { return g_frame.load(); }
uint64_t host_get_guest_base() {
#if defined(NX_COMPACT_LAYOUT)
    return NX_BASE;
#else
    return 0;
#endif
}
void host_get_region_bounds(AcnhRegionBounds* out) {
    if (!out) return;
    out->module_base = kModuleBase;
    out->heap_base = kHeapBase;
    out->heap_max = kHeapMax;
    out->alias_base = kAliasBase;
    out->alias_size = kAliasSize;
    out->stack_base = kStackBase;
    out->stack_size = kStackSize;
#if defined(NX_COMPACT_LAYOUT)
    out->guest_shift = 0;
    out->compact_layout = true;
#else
    out->guest_shift = kGuestShift;
    out->compact_layout = false;
#endif
}

int32_t host_obj_signaled(uint32_t handle) {
    auto o = handle_get(handle);
    if (!o) return -1;
    return o->signaled() ? 1 : 0;
}
void host_obj_consume(uint32_t handle) {
    if (auto o = handle_get(handle)) o->consume();
}
uint32_t host_event_signal(uint32_t handle) {
    auto e = std::dynamic_pointer_cast<KEvent>(handle_get(handle));
    if (!e) return 0xE401;
    e->state->signaled = true;
    return 0;
}
uint32_t host_event_clear(uint32_t handle, bool reset_signal) {
    auto e = std::dynamic_pointer_cast<KEvent>(handle_get(handle));
    if (!e) return 0xE401;
    uint32_t rc = (reset_signal && !e->state->signaled) ? 0xFA01 : 0;
    e->state->signaled = false;
    return rc;
}
void host_on_thread_created(uint32_t handle, uint64_t id, uint64_t entry, uint64_t tls, int32_t prio, int32_t core) {
    auto t = std::make_shared<KThread>();
    t->self = t;
    t->handle = handle;
    t->id = id;
    t->entry = entry;
    t->tls = tls;
    t->prio = prio;
    t->core = core;
    std::lock_guard lk(g_obj_lock);
    kstate::g_handles[handle] = t;
    if (id == 0x50 && !kstate::t_self) {
        t->started = true;
        kstate::t_self = t.get();
    }
}
void host_on_thread_exited(uint32_t handle) {
    if (auto t = std::dynamic_pointer_cast<KThread>(handle_get(handle))) {
        t->exited = true;
    }
}
void host_on_event_created(uint32_t w_handle, uint32_t r_handle) {
    auto st = std::make_shared<EventState>();
    auto w = std::make_shared<KEvent>();
    auto r = std::make_shared<KEvent>();
    w->state = r->state = st;
    std::lock_guard lk(g_obj_lock);
    kstate::g_handles[w_handle] = w;
    kstate::g_handles[r_handle] = r;
}
void host_on_transfer_memory_created(uint32_t handle, uint64_t addr, uint64_t size) {
    auto tm = std::make_shared<KTransferMemory>();
    tm->addr = addr;
    tm->size = size;
    std::lock_guard lk(g_obj_lock);
    kstate::g_handles[handle] = tm;
}
void host_on_handle_close(uint32_t handle) {
    std::lock_guard lk(g_obj_lock);
    kstate::g_handles.erase(handle);
}

uint64_t host_service_make(const char* name) {
    std::string p = name ? name : "";
    auto svc = (p == "sm:") ? std::make_shared<SmService>() : make_service(p);
    return box_service(std::move(svc));
}
size_t host_service_name(uint64_t service, char* buf, size_t cap) {
    auto svc = get_service(service);
    std::string n = svc ? svc->name : "";
    size_t len = std::min(n.size(), cap ? cap - 1 : 0);
    if (buf && cap) {
        std::memcpy(buf, n.data(), len);
        buf[len] = '\0';
    }
    return len;
}
uint32_t host_service_dispatch(AcnhIpcDispatch* d) {
    auto svc = get_service(d->service);
    if (!svc) return 0xE401;
    IpcCtx ctx;
    ctx.cpu = reinterpret_cast<CPU*>(d->cpu);
    ctx.cmd = d->cmd;
    ctx.type = d->msg_type;
    ctx.pid = d->pid;
    ctx.in = d->in_ptr;
    ctx.in_size = d->in_size;
    ctx.in_copy.assign(d->in_copy, d->in_copy + d->in_copy_len);
    ctx.in_move.assign(d->in_move, d->in_move + d->in_move_len);
    for (size_t i = 0; i < d->a_len; i++) ctx.a.push_back({d->a_bufs[i].addr, d->a_bufs[i].size});
    for (size_t i = 0; i < d->b_len; i++) ctx.b.push_back({d->b_bufs[i].addr, d->b_bufs[i].size});
    for (size_t i = 0; i < d->x_len; i++) ctx.x.push_back({d->x_bufs[i].addr, d->x_bufs[i].size});
    for (size_t i = 0; i < d->c_len; i++) ctx.c.push_back({d->c_bufs[i].addr, d->c_bufs[i].size});
    for (size_t i = 0; i < d->in_services_len; i++) ctx.in_services.push_back(get_service(d->in_services[i]));

    uint32_t rc = svc->handle(ctx);

    size_t out_n = std::min(ctx.out.size(), d->out_cap);
    if (ctx.out.size() > d->out_cap) {
        char msg[96];
        std::snprintf(msg, sizeof(msg), "ipc: service reply of %zu bytes truncated to %zu", ctx.out.size(), d->out_cap);
        host_log(msg);
    }
    if (out_n) std::memcpy(d->out_buf, ctx.out.data(), out_n);
    *d->out_len = out_n;

    size_t copy_n = std::min(ctx.out_copy.size(), d->out_copy_cap);
    if (copy_n) std::memcpy(d->out_copy, ctx.out_copy.data(), copy_n * sizeof(uint32_t));
    *d->out_copy_len = copy_n;

    size_t move_n = std::min(ctx.out_move.size(), d->out_move_cap);
    if (move_n) std::memcpy(d->out_move, ctx.out_move.data(), move_n * sizeof(uint32_t));
    *d->out_move_len = move_n;

    size_t svc_n = std::min(ctx.out_objects.size(), d->out_services_cap);
    for (size_t i = 0; i < svc_n; i++) d->out_services[i] = box_service(std::move(ctx.out_objects[i]));
    *d->out_services_len = svc_n;

    return rc;
}
void host_service_drop(uint64_t service) { delete reinterpret_cast<ServiceHolder*>(service); }

uint32_t host_shared_mem_create(const char* name, uint64_t size, uint64_t host_addr) {
    auto s = std::make_shared<KSharedMemory>();
    s->name = name ? name : "";
    s->size = size;
    s->host = host_addr;
    return handle_add(std::move(s));
}
uint32_t host_shared_mem_map(uint32_t handle, uint64_t addr, uint64_t size) {
    auto s = std::dynamic_pointer_cast<KSharedMemory>(handle_get(handle));
    if (!s) return 0xE401;
    return map_shared_memory(*s, addr, size);
}

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhKernelHost host{};
        host.monotonic_ns = host_monotonic_ns;
        host.sleep_ns = host_sleep_ns;
        host.yield_thread = host_yield_thread;
        host.current_thread_id = host_current_thread_id;
        host.set_thread_name = host_set_thread_name;
        host.start_thread = host_start_thread;
        host.exit_thread = host_exit_thread;
        host.exit_process = host_exit_process;
        host.window_minimized = host_window_minimized;
        host.data_root = host_data_root;
        host.profile_samples = host_profile_samples;
        host.log = host_log;
        host.fatal = host_fatal;
        host.describe = host_describe;
        host.kernel_alloc = host_kernel_alloc;
        host.commit = host_commit;
        host.decommit = host_decommit;
        host.vm_set = host_vm_set;
        host.vm_query = host_vm_query;
        host.call_guest = host_call_guest;
        host.cpu_profile_register = host_cpu_profile_register;
        host.init_tls_block = host_init_tls_block;
        host.get_info = host_get_info;
        host.set_thread_core_mask = host_set_thread_core_mask;
        host.map_transfer_memory = host_map_transfer_memory;
        host.unmap_transfer_memory = host_unmap_transfer_memory;
        host.is_hades = host_is_hades;
        host.get_trace = host_get_trace;
        host.get_frame = host_get_frame;
        host.get_guest_base = host_get_guest_base;
        host.get_region_bounds = host_get_region_bounds;
        host.obj_signaled = host_obj_signaled;
        host.obj_consume = host_obj_consume;
        host.event_signal = host_event_signal;
        host.event_clear = host_event_clear;
        host.on_thread_created = host_on_thread_created;
        host.on_thread_exited = host_on_thread_exited;
        host.on_event_created = host_on_event_created;
        host.on_transfer_memory_created = host_on_transfer_memory_created;
        host.on_handle_close = host_on_handle_close;
        host.service_make = host_service_make;
        host.service_name = host_service_name;
        host.service_dispatch = host_service_dispatch;
        host.service_drop = host_service_drop;
        host.shared_mem_create = host_shared_mem_create;
        host.shared_mem_map = host_shared_mem_map;
        acnh_kernel_set_host(&host);
    });
}

void hle_get_system_tick(CPU& c) {
    ensure_host();
    acnh_kernel_hle_get_system_tick(reinterpret_cast<AcnhCpu*>(&c));
}

}  // namespace

// The SVC counters live in Rust; the one reader (the CPU profile line in nvn_queue.cpp) asks for them when it prints.
void kernel_counters(uint64_t& tick_calls, uint64_t& yield_calls, uint64_t& yield_naps, uint64_t& sleep_calls) {
    ensure_host();
    acnh_kernel_counters(&tick_calls, &yield_calls, &yield_naps, &sleep_calls);
}

NX_HLE("_ZN2nn2os13GetSystemTickEv", hle_get_system_tick);

uint32_t handle_add(std::shared_ptr<KObject> o) {
    ensure_host();
    uint32_t h = acnh_kernel_alloc_handle();
    std::lock_guard lk(g_obj_lock);
    kstate::g_handles[h] = std::move(o);
    return h;
}

std::shared_ptr<KObject> handle_get(uint32_t h) {
    std::lock_guard lk(g_obj_lock);
    if (h == kstate::kPseudoCurrentThread && kstate::t_self) return kstate::t_self->self.lock();
    auto it = kstate::g_handles.find(h);
    return it == kstate::g_handles.end() ? nullptr : it->second;
}

void handle_close(uint32_t h) {
    ensure_host();
    acnh_kernel_handle_close(h);
}

void notify_all() {
    ensure_host();
    acnh_kernel_notify_all();
}

KThread* self_thread() { return kstate::t_self; }

void backtrace(CPU& c) {
    ensure_host();
    acnh_kernel_backtrace(reinterpret_cast<const AcnhCpu*>(&c));
}

uint64_t heap_size() {
    ensure_host();
    return acnh_kernel_heap_size();
}

CPU& current_cpu() {
    ensure_host();
    return *reinterpret_cast<CPU*>(acnh_kernel_current_cpu());
}

uint64_t system_tick() {
    ensure_host();
    return acnh_kernel_system_tick();
}

uint32_t create_main_thread(uint64_t stack_size) {
    ensure_host();
    return acnh_kernel_create_main_thread(stack_size);
}

void kernel_init() {
    ensure_host();
    acnh_kernel_init();
}

NX_GAME_API void svc(CPU& c, uint32_t imm, uint64_t pc) {
    ensure_host();
    acnh_kernel_svc(reinterpret_cast<AcnhCpu*>(&c), imm, pc);
}

void svc_profile_mark(int role) {
    ensure_host();
    acnh_kernel_svc_profile_mark(role);
}

void svc_profile_report(uint64_t frames, int role) {
    ensure_host();
    acnh_kernel_svc_profile_report(frames, role);
}

// NX_FRAME_PROFILE, as kernel.rs reads it; nvn_queue.cpp asks for it (the C++ kernel defined it before the Rust port).
bool frame_profile_on() {
    static const bool on = std::getenv("NX_FRAME_PROFILE") != nullptr;
    return on;
}

void svc_frame_take(uint64_t& total, uint64_t& top_ns, uint32_t& top_imm, uint64_t& top_pc) {
    ensure_host();
    acnh_kernel_svc_frame_take(&total, &top_ns, &top_imm, &top_pc);
}

void register_service(const std::string& name, ServiceFactory f) { factories()[name] = std::move(f); }

std::shared_ptr<Service> make_service(const std::string& name) {
    auto it = factories().find(name);
    if (it != factories().end()) return it->second();
    log("ipc: no implementation for service '%s', using a stub", name.c_str());
    return std::make_shared<Service>(name);
}

uint32_t Service::handle(IpcCtx& ctx) {
    auto it = cmds.find(ctx.cmd);
    if (it != cmds.end()) return it->second(ctx);
    static std::mutex m;
    static std::map<std::pair<std::string, uint32_t>, uint64_t> calls;
    uint64_t n;
    {
        std::lock_guard lk(m);
        n = ++calls[{name, ctx.cmd}];
    }
    if (n <= 3 || n % 10000 == 0)
        log("ipc: %s cmd %u not implemented (stub ok)%s", name.c_str(), ctx.cmd,
            n == 3 ? "; further calls logged every 10000th" : n > 3 ? " (10000 more)" : "");
    return 0;
}

uint32_t ipc_connect(const char* port, uint64_t* out_handle) {
    ensure_host();
    return acnh_kernel_ipc_connect(port, out_handle);
}

uint32_t ipc_request(CPU& c, uint32_t handle, uint64_t buf, uint64_t size) {
    ensure_host();
    return acnh_kernel_ipc_request(reinterpret_cast<AcnhCpu*>(&c), handle, buf, size);
}

std::shared_ptr<KSharedMemory> make_shared_memory(const std::string& name, uint64_t size) {
    ensure_host();
    uint32_t h = acnh_kernel_make_shared_memory(name.c_str(), size);
    return std::dynamic_pointer_cast<KSharedMemory>(handle_get(h));
}

uint32_t map_shared_memory(KSharedMemory& s, uint64_t addr, uint64_t size) {
    ensure_host();
    uint32_t rc = acnh_kernel_map_shared_memory(addr, size, s.host, s.size);
    s.host = addr;
    return rc;
}

}  // namespace nx
