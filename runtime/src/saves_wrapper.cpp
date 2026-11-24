// Thin C++ wrapper forwarding nx::saves::*, nx::user_*, and acc:u0/acc:u1
// to the Rust implementation in crates/runtime/src/saves.rs.
#include "saves.h"
#include "users.h"

#include <algorithm>
#include <atomic>
#include <cstring>
#include <ctime>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#if defined(NX_HAVE_ZLIB)
#include <zlib.h>
#endif

#include <nx/runtime.h>

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#endif

#include "ipc.h"
#include "nextendo.h"
#include "platform.h"

#ifndef NX_RUST_RUNTIME
extern "C" {
struct AcnhSavesLocalTime {
    int32_t year;
    int32_t month;
    int32_t day;
    int32_t hour;
    int32_t minute;
    int32_t second;
};
struct AcnhSavesHost {
    size_t (*data_root)(char*, size_t);
    void (*log)(const char*);
    bool (*local_from_posix)(int64_t, AcnhSavesLocalTime*);
    bool (*zip_inflate)(const uint8_t*, size_t, uint8_t*, size_t, char*, size_t);
};
struct AcnhUserInfo {
    uint64_t lo;
    uint64_t hi;
    char nickname[32];
};
typedef void (*AcnhSavesStringCb)(const char*, size_t, void*);
typedef void (*AcnhSavesFoundCb)(const char*, size_t, const char*, size_t, void*);
typedef void (*AcnhSavesUserCb)(const AcnhUserInfo*, void*);

void acnh_saves_set_host(const AcnhSavesHost* host);
void acnh_saves_save_dir(AcnhSavesStringCb cb, void* ctx);
bool acnh_saves_has_save();
void acnh_saves_last_modified(AcnhSavesStringCb cb, void* ctx);
void acnh_saves_detect_emulator_saves(AcnhSavesFoundCb cb, void* ctx);
bool acnh_saves_looks_like_acnh_save(const char* dir, AcnhSavesStringCb why_cb, void* why_ctx);
bool acnh_saves_import_save(const char* from, AcnhSavesStringCb err_cb, void* err_ctx);
bool acnh_saves_export_save(const char* to, AcnhSavesStringCb written_cb, void* written_ctx, AcnhSavesStringCb err_cb,
                            void* err_ctx);
void acnh_saves_backup_now(bool automatic, AcnhSavesStringCb out_cb, void* out_ctx, AcnhSavesStringCb err_cb,
                           void* err_ctx);
void acnh_saves_backups_folder(AcnhSavesStringCb cb, void* ctx);
void acnh_saves_list_backups(AcnhSavesStringCb cb, void* ctx);
bool acnh_saves_restore_backup(const char* name, AcnhSavesStringCb err_cb, void* err_ctx);

void acnh_users_all(AcnhSavesUserCb cb, void* ctx);
int32_t acnh_users_count();
bool acnh_user_find(uint64_t lo, uint64_t hi, AcnhUserInfo* out);
bool acnh_user_create(AcnhUserInfo* out);
void acnh_user_boot(AcnhUserInfo* out);
void acnh_user_opened(uint64_t lo, uint64_t hi);
void acnh_user_last_opened(AcnhUserInfo* out);
void acnh_user_save_dir(uint64_t lo, uint64_t hi, AcnhSavesStringCb cb, void* ctx);

size_t acnh_account_preselected_user_blob(uint8_t* out, size_t cap);
}
#endif

namespace nx {

namespace {

size_t copy_string_to_buf(const std::string& s, char* buf, size_t cap) {
    if (buf && cap > 0) {
        size_t n = std::min(s.size(), cap - 1);
        if (n > 0) std::memcpy(buf, s.data(), n);
        buf[n] = '\0';
    }
    return s.size();
}

size_t host_data_root(char* buf, size_t cap) { return copy_string_to_buf(plat::data_root(), buf, cap); }

void host_log(const char* msg) {
    if (msg) log("%s", msg);
}

bool host_local_from_posix(int64_t posix, AcnhSavesLocalTime* out) {
    if (!out) return false;
    time_t tt = time_t(posix);
    std::tm tm{};
#if defined(_WIN32)
    localtime_s(&tm, &tt);
#else
    localtime_r(&tt, &tm);
#endif
    out->year = tm.tm_year + 1900;
    out->month = tm.tm_mon + 1;
    out->day = tm.tm_mday;
    out->hour = tm.tm_hour;
    out->minute = tm.tm_min;
    out->second = tm.tm_sec;
    return true;
}

bool host_zip_inflate(const uint8_t* in_ptr, size_t in_len, uint8_t* out_ptr, size_t out_len, char* err_buf,
                      size_t err_cap) {
#if defined(NX_HAVE_ZLIB)
    z_stream z{};
    if (inflateInit2(&z, -MAX_WBITS) != Z_OK) {
        copy_string_to_buf("zlib failed", err_buf, err_cap);
        return false;
    }
    z.next_in = const_cast<Bytef*>(reinterpret_cast<const Bytef*>(in_ptr));
    z.avail_in = uInt(in_len);
    z.next_out = reinterpret_cast<Bytef*>(out_ptr);
    z.avail_out = uInt(out_len);
    int r = inflate(&z, Z_FINISH);
    bool ok = r == Z_STREAM_END && z.total_out == out_len;
    inflateEnd(&z);
    if (!ok) copy_string_to_buf("damaged zip file (inflate)", err_buf, err_cap);
    return ok;
#else
    (void)in_ptr;
    (void)in_len;
    (void)out_ptr;
    (void)out_len;
    copy_string_to_buf("zip not supported in this build", err_buf, err_cap);
    return false;
#endif
}

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhSavesHost host{};
        host.data_root = host_data_root;
        host.log = host_log;
        host.local_from_posix = host_local_from_posix;
        host.zip_inflate = host_zip_inflate;
        acnh_saves_set_host(&host);
    });
}

void assign_string_cb(const char* ptr, size_t len, void* ctx) {
    if (!ctx) return;
    auto* out = static_cast<std::string*>(ctx);
    out->assign(ptr ? ptr : "", len);
}

void push_string_cb(const char* ptr, size_t len, void* ctx) {
    if (!ctx) return;
    auto* out = static_cast<std::vector<std::string>*>(ctx);
    out->emplace_back(ptr ? ptr : "", len);
}

void push_found_cb(const char* label, size_t label_len, const char* path, size_t path_len, void* ctx) {
    if (!ctx) return;
    auto* out = static_cast<std::vector<saves::Found>*>(ctx);
    out->push_back({std::string(label ? label : "", label_len), std::string(path ? path : "", path_len)});
}

UserInfo to_cpp_user(const AcnhUserInfo& u) {
    UserInfo out;
    out.lo = u.lo;
    out.hi = u.hi;
    out.nickname = std::string(u.nickname);
    return out;
}

void push_user_cb(const AcnhUserInfo* u, void* ctx) {
    if (!u || !ctx) return;
    auto* out = static_cast<std::vector<UserInfo>*>(ctx);
    out->push_back(to_cpp_user(*u));
}

// ---- IStorage / IStorageAccessor & acc:u0/acc:u1 ---------------------------------------------------------------

struct StorageService : Service {
    std::shared_ptr<std::vector<uint8_t>> blob;
    StorageService() : Service("IStorage") {}
};

void push_uid(IpcCtx& c, const UserInfo& u) {
    c.push<uint64_t>(u.lo);
    c.push<uint64_t>(u.hi);
}

void write_uid_list(IpcCtx& c) {
    Buffer out = c.out_buf(0);
    if (out.addr && out.size >= 16) {
        std::memset(reinterpret_cast<void*>(out.addr), 0, out.size);
        size_t i = 0;
        for (auto& u : users_all()) {
            if ((i + 1) * 16 > out.size) break;
            st<uint64_t>(out.addr + i * 16, u.lo);
            st<uint64_t>(out.addr + i * 16 + 8, u.hi);
            i++;
        }
    }
}

std::shared_ptr<Service> profile(UserInfo user) {
    auto s = std::make_shared<Service>("IProfile");
    auto base = [user](IpcCtx& c) {
        push_uid(c, user);
        c.push<uint64_t>(0);
        char name[0x20] = {};
        std::memcpy(name, user.nickname.c_str(), std::min<size_t>(user.nickname.size(), 0x1F));
        for (char ch : name) c.push<char>(ch);
    };
    s->cmds[0] = [base](IpcCtx& c) {
        Buffer out = c.out_buf(0);
        if (out.addr) std::memset(reinterpret_cast<void*>(out.addr), 0, out.size);
        base(c);
        return 0u;
    };
    s->cmds[1] = [base](IpcCtx& c) { base(c); return 0u; };
    s->cmds[10] = [](IpcCtx& c) { c.push<uint32_t>(0); return 0u; };
    s->cmds[11] = [](IpcCtx& c) { c.push<uint32_t>(0); return 0u; };
    return s;
}

std::shared_ptr<Service> async_context(std::function<uint32_t()> work) {
    struct State {
        std::shared_ptr<EventState> event = std::make_shared<EventState>();
        std::atomic<bool> done{false};
        std::atomic<uint32_t> result{0};
    };
    auto st = std::make_shared<State>();
    std::thread([st, work = std::move(work)] {
        st->result = work();
        st->done = true;
        st->event->signaled = true;
        notify_all();
    }).detach();
    auto s = std::make_shared<Service>("IAsyncContext");
    s->cmds[0] = [st](IpcCtx& c) {
        auto r = std::make_shared<KEvent>();
        r->state = st->event;
        c.out_copy.push_back(handle_add(r));
        return 0u;
    };
    s->cmds[1] = [](IpcCtx&) { return 0u; };
    s->cmds[2] = [st](IpcCtx& c) { c.push<uint8_t>(st->done ? 1 : 0); return 0u; };
    s->cmds[3] = [st](IpcCtx&) { return st->done ? st->result.load() : 0u; };
    return s;
}

constexpr uint32_t kNsaUnavailable = (280u << 9) | 124u;

std::shared_ptr<Service> manager_for_application() {
    auto s = std::make_shared<Service>("IManagerForApplication");
    s->cmds[0] = [](IpcCtx&) { return 0u; };
    s->cmds[1] = [](IpcCtx& c) {
        c.push<uint64_t>(nextendo::network_service_account_id());
        return 0u;
    };
    s->cmds[2] = [](IpcCtx& c) {
        c.push_object(async_context([] {
            if (!nextendo::online_services_active()) return kNsaUnavailable;
            if (!nextendo::ensure_online_identity()) {
                log("account: Nextendo token renewal failed: %s", nextendo::last_error().c_str());
                return kNsaUnavailable;
            }
            return 0u;
        }));
        return 0u;
    };
    s->cmds[3] = [](IpcCtx& c) {
        if (!nextendo::online_services_active()) return kNsaUnavailable;
        const std::string tok = nextendo::baas_id_token(nextendo::game_version_string());
        if (tok.empty()) return kNsaUnavailable;
        Buffer out = c.out_buf(0);
        if (!out.addr || out.size <= tok.size()) {
            log("account: id_token (%zu bytes) does not fit the game's buffer (%llu)", tok.size(),
                (unsigned long long)out.size);
            return kNsaUnavailable;
        }
        std::memset(reinterpret_cast<void*>(out.addr), 0, out.size);
        std::memcpy(reinterpret_cast<void*>(out.addr), tok.data(), tok.size());
        c.push<uint32_t>(uint32_t(tok.size()));
        return 0u;
    };
    s->cmds[4] = s->cmds[3];
    return s;
}

void account(Service& s) {
    s.cmds[0] = [](IpcCtx& c) {
        ensure_host();
        c.push<int32_t>(acnh_users_count());
        return 0u;
    };
    s.cmds[1] = [](IpcCtx& c) {
        ensure_host();
        c.push<uint8_t>(acnh_user_find(c.arg<uint64_t>(0), c.arg<uint64_t>(8), nullptr) ? 1 : 0);
        return 0u;
    };
    s.cmds[2] = [](IpcCtx& c) { write_uid_list(c); return 0u; };
    s.cmds[3] = [](IpcCtx& c) { write_uid_list(c); return 0u; };
    s.cmds[4] = [](IpcCtx& c) { push_uid(c, user_last_opened()); return 0u; };
    s.cmds[5] = [](IpcCtx& c) {
        UserInfo u;
        if (!user_find(c.arg<uint64_t>(0), c.arg<uint64_t>(8), &u)) {
            log("account: GetProfile of an unknown user");
            return 0x4E2u;
        }
        c.push_object(profile(u));
        return 0u;
    };
    s.cmds[50] = [](IpcCtx& c) { c.push<uint8_t>(1); return 0u; };
    s.cmds[51] = [](IpcCtx& c) { push_uid(c, user_boot()); return 0u; };
    s.cmds[100] = [](IpcCtx&) { return 0u; };
    s.cmds[101] = [](IpcCtx& c) { c.push_object(manager_for_application()); return 0u; };
    s.cmds[140] = [](IpcCtx&) { return 0u; };
    s.cmds[141] = [](IpcCtx& c) { write_uid_list(c); return 0u; };
    s.cmds[160] = [](IpcCtx&) { return 0u; };
    s.cmds[150] = [](IpcCtx& c) { c.push<uint8_t>(0); return 0u; };
}

ServiceRegistrar r_acc_u0("acc:u0", [] { auto s = std::make_shared<Service>("acc:u0"); account(*s); return s; });
ServiceRegistrar r_acc_u1("acc:u1", [] { auto s = std::make_shared<Service>("acc:u1"); account(*s); return s; });

}  // namespace

// ---- saves.h C++ API -------------------------------------------------------------------------------------------

namespace saves {

std::string save_dir() {
    ensure_host();
    std::string out;
    acnh_saves_save_dir(assign_string_cb, &out);
    return out;
}

bool has_save() {
    ensure_host();
    return acnh_saves_has_save();
}

std::string last_modified() {
    ensure_host();
    std::string out;
    acnh_saves_last_modified(assign_string_cb, &out);
    return out;
}

std::vector<Found> detect_emulator_saves() {
    ensure_host();
    std::vector<Found> out;
    acnh_saves_detect_emulator_saves(push_found_cb, &out);
    return out;
}

bool looks_like_acnh_save(const std::string& dir, std::string* why) {
    ensure_host();
    return acnh_saves_looks_like_acnh_save(dir.c_str(), why ? assign_string_cb : nullptr, why);
}

bool import_save(const std::string& from, std::string* error) {
    ensure_host();
    return acnh_saves_import_save(from.c_str(), error ? assign_string_cb : nullptr, error);
}

bool export_save(const std::string& to, std::string* written, std::string* error) {
    ensure_host();
    return acnh_saves_export_save(to.c_str(), written ? assign_string_cb : nullptr, written,
                                  error ? assign_string_cb : nullptr, error);
}

std::string backup_now(std::string* error, bool automatic) {
    ensure_host();
    std::string out;
    acnh_saves_backup_now(automatic, assign_string_cb, &out, error ? assign_string_cb : nullptr, error);
    return out;
}

std::string backups_folder() {
    ensure_host();
    std::string out;
    acnh_saves_backups_folder(assign_string_cb, &out);
    return out;
}

std::vector<std::string> list_backups() {
    ensure_host();
    std::vector<std::string> out;
    acnh_saves_list_backups(push_string_cb, &out);
    return out;
}

bool restore_backup(const std::string& name, std::string* error) {
    ensure_host();
    return acnh_saves_restore_backup(name.c_str(), error ? assign_string_cb : nullptr, error);
}

}  // namespace saves

// ---- users.h C++ API -------------------------------------------------------------------------------------------

std::vector<UserInfo> users_all() {
    ensure_host();
    std::vector<UserInfo> out;
    acnh_users_all(push_user_cb, &out);
    return out;
}

bool user_find(uint64_t lo, uint64_t hi, UserInfo* out) {
    ensure_host();
    AcnhUserInfo u{};
    if (!acnh_user_find(lo, hi, out ? &u : nullptr)) return false;
    if (out) *out = to_cpp_user(u);
    return true;
}

UserInfo user_create() {
    ensure_host();
    AcnhUserInfo u{};
    if (!acnh_user_create(&u)) return {};
    return to_cpp_user(u);
}

UserInfo user_boot() {
    ensure_host();
    AcnhUserInfo u{};
    acnh_user_boot(&u);
    return to_cpp_user(u);
}

void user_opened(uint64_t lo, uint64_t hi) {
    ensure_host();
    acnh_user_opened(lo, hi);
}

UserInfo user_last_opened() {
    ensure_host();
    AcnhUserInfo u{};
    acnh_user_last_opened(&u);
    return to_cpp_user(u);
}

std::string user_save_dir(uint64_t lo, uint64_t hi) {
    ensure_host();
    std::string out;
    acnh_user_save_dir(lo, hi, assign_string_cb, &out);
    return out;
}

// ---- ipc.h storage helpers -------------------------------------------------------------------------------------

std::shared_ptr<Service> make_storage(std::shared_ptr<std::vector<uint8_t>> blob) {
    auto s = std::make_shared<StorageService>();
    s->blob = blob;
    s->cmds[0] = [blob](IpcCtx& c) {
        auto a = std::make_shared<Service>("IStorageAccessor");
        a->cmds[0] = [blob](IpcCtx& c) { c.push<int64_t>(int64_t(blob->size())); return 0u; };
        a->cmds[10] = [blob](IpcCtx& c) {
            int64_t off = c.arg<int64_t>(0);
            Buffer in = c.in_buf(0);
            if (off < 0 || uint64_t(off) > blob->size()) return 0x3E880u;
            size_t n = std::min<size_t>(in.size, blob->size() - size_t(off));
            std::memcpy(blob->data() + off, reinterpret_cast<void*>(in.addr), n);
            return 0u;
        };
        a->cmds[11] = [blob](IpcCtx& c) {
            int64_t off = c.arg<int64_t>(0);
            Buffer out = c.out_buf(0);
            if (off < 0 || uint64_t(off) > blob->size()) return 0x3E880u;
            size_t n = std::min<size_t>(out.size, blob->size() - size_t(off));
            std::memcpy(reinterpret_cast<void*>(out.addr), blob->data() + off, n);
            return 0u;
        };
        c.push_object(a);
        return 0u;
    };
    return s;
}

std::shared_ptr<Service> make_storage(std::vector<uint8_t> data) {
    return make_storage(std::make_shared<std::vector<uint8_t>>(std::move(data)));
}

std::shared_ptr<std::vector<uint8_t>> storage_blob(const std::shared_ptr<Service>& s) {
    auto p = std::dynamic_pointer_cast<StorageService>(s);
    return p ? p->blob : nullptr;
}

std::shared_ptr<Service> preselected_user_storage() {
    ensure_host();
    std::vector<uint8_t> d(0x88, 0);
    acnh_account_preselected_user_blob(d.data(), d.size());
    return make_storage(std::move(d));
}

}  // namespace nx
