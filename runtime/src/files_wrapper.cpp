// Thin C++ wrapper forwarding nn::fs / nn::aoc HLE, mods_fetch, and game_folder
// helpers to the Rust implementation in crates/runtime/src/files.rs.
#include "config.h"
#include "platform.h"

#include <algorithm>
#include <cstring>
#include <mutex>
#include <string>

#include <nx/runtime.h>

#if defined(NX_GAME_DYNAMIC) && !defined(__ANDROID__)
#include <SDL3/SDL.h>
#include <nx/game.h>
#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <dlfcn.h>
#include <unistd.h>
#endif
#endif

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#endif

#ifndef NX_RUST_RUNTIME
extern "C" {
typedef void (*AcnhFilesStringCb)(const char*, size_t, void*);
typedef void (*AcnhFilesDirEntryCb)(const char*, size_t, bool, int64_t, void*);
typedef void (*AcnhFilesBytesCb)(const uint8_t*, size_t, void*);

struct AcnhFilesHost {
    size_t (*data_root)(char*, size_t);
    size_t (*game_root)(char*, size_t);
    void (*log)(const char*);
    int64_t (*current_thread_id)();
    uint64_t (*monotonic_ns)();
    bool (*trace_enabled)();
    void (*discord_presence_file)(const char*);
    size_t (*glyph_override)(const char*, char*, size_t);
    bool (*read_asset)(const char*, AcnhFilesBytesCb, void*);
};

void acnh_files_set_host(const AcnhFilesHost* host);
void acnh_files_log_recent();
void acnh_files_add_virtual_file(const char* path, const uint8_t* data, size_t len);
void acnh_files_read_game_file(const char* relative_to_romfs, AcnhFilesBytesCb cb, void* ctx);
uint32_t acnh_files_mount_rom(const char* name);
uint32_t acnh_files_ensure_save_data(uint64_t uid_lo, uint64_t uid_hi);
uint32_t acnh_files_mount_save_data(const char* name, uint64_t uid_lo, uint64_t uid_hi);
uint32_t acnh_files_unmount(const char* name);
uint32_t acnh_files_commit(const char* names);
uint32_t acnh_files_mount_device_save_data(const char* name);
uint32_t acnh_files_get_device_save_data_size(int64_t* out_size, int64_t* out_journal);
uint32_t acnh_files_extend_device_save_data(int64_t size, int64_t journal);
uint32_t acnh_files_mount_cache_storage(const char* name, int32_t index);
uint32_t acnh_files_create_file(const char* path, int64_t size);
uint32_t acnh_files_delete_file(const char* path);
uint32_t acnh_files_create_directory(const char* path);
uint32_t acnh_files_get_entry_type(const char* path, uint32_t* out_type);
uint32_t acnh_files_open_file(const char* path, int32_t mode, uint64_t* out_handle);
void acnh_files_close_file(uint64_t handle);
uint32_t acnh_files_get_file_size(uint64_t handle, int64_t* out_size);
uint32_t acnh_files_read_file(uint64_t handle, int64_t off, uint8_t* buf, uint64_t size, uint64_t* out_read);
uint32_t acnh_files_write_file(uint64_t handle, int64_t off, const uint8_t* buf, uint64_t size);
uint32_t acnh_files_flush_file(uint64_t handle);
uint32_t acnh_files_set_file_size(uint64_t handle, int64_t size);
uint32_t acnh_files_open_directory(const char* path, int32_t mode, uint64_t* out_handle);
uint32_t acnh_files_read_directory(uint64_t handle, int64_t max_entries, int64_t* out_count, AcnhFilesDirEntryCb cb,
                                   void* ctx);
uint32_t acnh_files_get_directory_entry_count(uint64_t handle, int64_t* out_count);
void acnh_files_close_directory(uint64_t handle);
uint32_t acnh_files_clean_directory_recursively(const char* path);
uint32_t acnh_files_rename_directory(const char* from, const char* to);
uint32_t acnh_files_delete_directory_recursively(const char* path);
uint64_t acnh_files_count_aoc();
int32_t acnh_files_list_aoc(int32_t* out, int32_t offset, int32_t count);
uint32_t acnh_files_mount_aoc(const char* name, int32_t index);
void acnh_files_mods_fetch();
void acnh_files_game_library_path(const char* folder, const char* game_id, AcnhFilesStringCb cb, void* ctx);
void acnh_files_format_load_error(const char* why, AcnhFilesStringCb cb, void* ctx);
}
#endif

namespace nx {

void discord_presence_file(const std::string& guest_path);
std::string glyph_override(const std::string& romfs_path);

namespace {

constexpr uint32_t kOk = 0;

size_t copy_string_to_buf(const std::string& s, char* buf, size_t cap) {
    if (buf && cap > 0) {
        size_t n = std::min(s.size(), cap - 1);
        if (n > 0) std::memcpy(buf, s.data(), n);
        buf[n] = '\0';
    }
    return s.size();
}

size_t host_data_root(char* buf, size_t cap) { return copy_string_to_buf(plat::data_root(), buf, cap); }
size_t host_game_root(char* buf, size_t cap) { return copy_string_to_buf(plat::game_root(), buf, cap); }

void host_log(const char* msg) {
    if (msg) log("%s", msg);
}

int64_t host_current_thread_id() { return plat::current_thread_id(); }
uint64_t host_monotonic_ns() { return plat::monotonic_ns(); }
bool host_trace_enabled() { return g_trace; }

void host_discord_presence_file(const char* path) {
    if (path) discord_presence_file(path);
}

size_t host_glyph_override(const char* rel, char* buf, size_t cap) {
    if (!rel) return 0;
    std::string g = glyph_override(rel);
    if (g.empty()) return 0;
    return copy_string_to_buf(g, buf, cap);
}

bool host_read_asset(const char* rel, void (*cb)(const uint8_t*, size_t, void*), void* ctx) {
#if defined(__ANDROID__)
    if (!rel || !cb) return false;
    std::string out;
    if (!plat::read_asset(rel, out)) return false;
    cb(reinterpret_cast<const uint8_t*>(out.data()), out.size(), ctx);
    return true;
#else
    (void)rel;
    (void)cb;
    (void)ctx;
    return false;
#endif
}

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhFilesHost host{};
        host.data_root = host_data_root;
        host.game_root = host_game_root;
        host.log = host_log;
        host.current_thread_id = host_current_thread_id;
        host.monotonic_ns = host_monotonic_ns;
        host.trace_enabled = host_trace_enabled;
        host.discord_presence_file = host_discord_presence_file;
        host.glyph_override = host_glyph_override;
        host.read_asset = host_read_asset;
        acnh_files_set_host(&host);
    });
}

std::string cstr(uint64_t p) { return p ? std::string(reinterpret_cast<const char*>(p)) : std::string(); }

void string_cb(const char* data, size_t len, void* ctx) {
    auto* out = static_cast<std::string*>(ctx);
    if (data && len > 0) out->assign(data, len);
    else out->clear();
}

void bytes_cb(const uint8_t* data, size_t len, void* ctx) {
    auto* out = static_cast<std::string*>(ctx);
    if (data && len > 0) out->assign(reinterpret_cast<const char*>(data), len);
    else out->clear();
}

// ---- HLE functions ---------------------------------------------------------------------------------------------

void QueryMountRomCacheSize(CPU& c) {
    ensure_host();
    st<uint64_t>(c.x[0], 0x1000);
    c.x[0] = kOk;
}

void MountRom(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_mount_rom(cstr(c.x[0]).c_str());
}

void EnsureSaveData(CPU& c) {
    ensure_host();
    uint64_t lo = ld<uint64_t>(c.x[0]);
    uint64_t hi = ld<uint64_t>(c.x[0] + 8);
    c.x[0] = acnh_files_ensure_save_data(lo, hi);
}

void MountSaveData(CPU& c) {
    ensure_host();
    uint64_t lo = ld<uint64_t>(c.x[1]);
    uint64_t hi = ld<uint64_t>(c.x[1] + 8);
    c.x[0] = acnh_files_mount_save_data(cstr(c.x[0]).c_str(), lo, hi);
}

void Unmount(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_unmount(cstr(c.x[0]).c_str());
}

void Commit(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_commit(cstr(c.x[0]).c_str());
}

void CommitMultiple(CPU& c) {
    ensure_host();
    std::string names;
    for (int k = 0; k < int(c.x[1]) && k < 8; k++) names += (k ? ", " : "") + cstr(ld<uint64_t>(c.x[0] + 8ull * k));
    c.x[0] = acnh_files_commit(names.c_str());
}

void MountDeviceSaveData(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_mount_device_save_data(cstr(c.x[0]).c_str());
}

void GetDeviceSaveDataSize(CPU& c) {
    ensure_host();
    int64_t size = 0, journal = 0;
    uint32_t rc = acnh_files_get_device_save_data_size(&size, &journal);
    if (c.x[0]) st<int64_t>(c.x[0], size);
    if (c.x[1]) st<int64_t>(c.x[1], journal);
    c.x[0] = rc;
}

void ExtendDeviceSaveData(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_extend_device_save_data(int64_t(c.x[0]), int64_t(c.x[1]));
}

void GetSaveDataRestoreFlag(CPU& c) {
    if (c.x[0]) st<uint8_t>(c.x[0], 0);
    c.x[0] = kOk;
}

void SetPriorityOnCurrentThread(CPU&) {}
void EnableGlobalFileDataCache(CPU& c) { c.x[0] = kOk; }
void DisableGlobalFileDataCache(CPU& c) { c.x[0] = kOk; }

void CreateFile(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_create_file(cstr(c.x[0]).c_str(), int64_t(c.x[1]));
}

void DeleteFile(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_delete_file(cstr(c.x[0]).c_str());
}

void CreateDirectory(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_create_directory(cstr(c.x[0]).c_str());
}

void GetEntryType(CPU& c) {
    ensure_host();
    uint32_t et = 0;
    uint32_t rc = acnh_files_get_entry_type(cstr(c.x[1]).c_str(), &et);
    if (rc == kOk && c.x[0]) st<uint32_t>(c.x[0], et);
    c.x[0] = rc;
}

void OpenFile(CPU& c) {
    ensure_host();
    uint64_t h = 0;
    uint32_t rc = acnh_files_open_file(cstr(c.x[1]).c_str(), int32_t(c.x[2]), &h);
    if (rc == kOk && c.x[0]) st<uint64_t>(c.x[0], h);
    c.x[0] = rc;
}

void CloseFile(CPU& c) {
    ensure_host();
    acnh_files_close_file(c.x[0]);
}

void GetFileSize(CPU& c) {
    ensure_host();
    int64_t sz = 0;
    uint32_t rc = acnh_files_get_file_size(c.x[1], &sz);
    if (rc == kOk && c.x[0]) st<int64_t>(c.x[0], sz);
    c.x[0] = rc;
}

void ReadFileOut(CPU& c) {
    ensure_host();
    uint64_t read_n = 0;
    uint32_t rc = acnh_files_read_file(c.x[1], int64_t(c.x[2]), reinterpret_cast<uint8_t*>(c.x[3]), c.x[4], &read_n);
    if (rc == kOk && c.x[0]) st<uint64_t>(c.x[0], read_n);
    c.x[0] = rc;
}

void ReadFile(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_read_file(c.x[0], int64_t(c.x[1]), reinterpret_cast<uint8_t*>(c.x[2]), c.x[3], nullptr);
}

void WriteFile(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_write_file(c.x[0], int64_t(c.x[1]), reinterpret_cast<const uint8_t*>(c.x[2]), c.x[3]);
}

void FlushFile(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_flush_file(c.x[0]);
}

void OpenDirectory(CPU& c) {
    ensure_host();
    uint64_t h = 0;
    uint32_t rc = acnh_files_open_directory(cstr(c.x[1]).c_str(), int32_t(c.x[2]), &h);
    if (rc == kOk && c.x[0]) st<uint64_t>(c.x[0], h);
    c.x[0] = rc;
}

struct DirWriteCtx {
    uint64_t base;
    int64_t index;
};

void dir_entry_cb(const char* name, size_t len, bool is_dir, int64_t size, void* ctx) {
    auto* w = static_cast<DirWriteCtx*>(ctx);
    uint64_t out = w->base + uint64_t(w->index) * 0x310;
    std::memset(reinterpret_cast<void*>(out), 0, 0x310);
    if (name && len > 0) std::memcpy(reinterpret_cast<void*>(out), name, std::min<size_t>(len, 0x300));
    st<int8_t>(out + 0x304, is_dir ? 0 : 1);
    st<int64_t>(out + 0x308, size);
    w->index++;
}

void ReadDirectory(CPU& c) {
    ensure_host();
    DirWriteCtx ctx{c.x[1], 0};
    int64_t n = 0;
    uint32_t rc = acnh_files_read_directory(c.x[2], int64_t(c.x[3]), &n, dir_entry_cb, &ctx);
    if (rc == kOk && c.x[0]) st<int64_t>(c.x[0], n);
    c.x[0] = rc;
}

void GetDirectoryEntryCount(CPU& c) {
    ensure_host();
    int64_t n = 0;
    uint32_t rc = acnh_files_get_directory_entry_count(c.x[1], &n);
    if (rc == kOk && c.x[0]) st<int64_t>(c.x[0], n);
    c.x[0] = rc;
}

void CloseDirectory(CPU& c) {
    ensure_host();
    acnh_files_close_directory(c.x[0]);
}

void OpenCacheStorageList(CPU& c) {
    st<uint64_t>(c.x[0], 1);
    c.x[0] = kOk;
}
void ReadCacheStorageList(CPU& c) {
    st<int32_t>(c.x[0], 0);
    c.x[0] = kOk;
}
void CloseCacheStorageList(CPU&) {}
void CreateCacheStorage(CPU& c) { c.x[0] = kOk; }
void DeleteCacheStorage(CPU& c) { c.x[0] = kOk; }

void MountCacheStorage(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_mount_cache_storage(cstr(c.x[0]).c_str(), int32_t(c.x[1]));
}

void CountAddOnContent(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_count_aoc();
}

void ListAddOnContent(CPU& c) {
    ensure_host();
    c.x[0] = uint64_t(acnh_files_list_aoc(reinterpret_cast<int32_t*>(c.x[0]), int32_t(c.x[1]), int32_t(c.x[2])));
}

void QueryMountAddOnContentCacheSize(CPU& c) {
    st<uint64_t>(c.x[0], 0x1000);
    c.x[0] = kOk;
}

void MountAddOnContent(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_mount_aoc(cstr(c.x[0]).c_str(), int32_t(c.x[1]));
}

void IsSaveDataExisting(CPU& c) { c.x[0] = 1; }

void SetFileSize(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_set_file_size(c.x[0], int64_t(c.x[1]));
}

void CleanDirectoryRecursively(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_clean_directory_recursively(cstr(c.x[0]).c_str());
}

void RenameDirectory(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_rename_directory(cstr(c.x[0]).c_str(), cstr(c.x[1]).c_str());
}

void DeleteDirectoryRecursively(CPU& c) {
    ensure_host();
    c.x[0] = acnh_files_delete_directory_recursively(cstr(c.x[0]).c_str());
}

}  // namespace

NX_HLE("_ZN2nn2fs22QueryMountRomCacheSizeEPm", QueryMountRomCacheSize);
NX_HLE("_ZN2nn2fs19MountDeviceSaveDataEPKc", MountDeviceSaveData);
NX_HLE("_ZN2nn2fs21GetDeviceSaveDataSizeEPlS1_", GetDeviceSaveDataSize);
NX_HLE("_ZN2nn2fs20ExtendDeviceSaveDataEll", ExtendDeviceSaveData);
NX_HLE("_ZN2nn2fs22GetSaveDataRestoreFlagEPbPKc", GetSaveDataRestoreFlag);
NX_HLE("_ZN2nn2fs6CommitEPKPKci", CommitMultiple);
NX_HLE("_ZN2nn2fs6CommitEPKcRKNS0_12CommitOptionE", Commit);
NX_HLE("_ZN2nn2fs25CleanDirectoryRecursivelyEPKc", CleanDirectoryRecursively);
NX_HLE("_ZN2nn2fs15RenameDirectoryEPKcS2_", RenameDirectory);
NX_HLE("_ZN2nn2fs26SetPriorityOnCurrentThreadENS0_8PriorityE", SetPriorityOnCurrentThread);
NX_HLE("_ZN2nn2fs8MountRomEPKcPvm", MountRom);
NX_HLE("_ZN2nn2fs14EnsureSaveDataERKNS_7account3UidE", EnsureSaveData);
NX_HLE("_ZN2nn2fs13MountSaveDataEPKcRKNS_7account3UidE", MountSaveData);
NX_HLE("_ZN2nn2fs7UnmountEPKc", Unmount);
NX_HLE("_ZN2nn2fs10DeleteFileEPKc", DeleteFile);
NX_HLE("_ZN2nn2fs10CreateFileEPKcl", CreateFile);
NX_HLE("_ZN2nn2fs8OpenFileEPNS0_10FileHandleEPKci", OpenFile);
NX_HLE("_ZN2nn2fs11GetFileSizeEPlNS0_10FileHandleE", GetFileSize);
NX_HLE("_ZN2nn2fs15CreateDirectoryEPKc", CreateDirectory);
NX_HLE("_ZN2nn2fs12GetEntryTypeEPNS0_18DirectoryEntryTypeEPKc", GetEntryType);
NX_HLE("_ZN2nn2fs9CloseFileENS0_10FileHandleE", CloseFile);
NX_HLE("_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvm", ReadFileOut);
NX_HLE("_ZN2nn2fs8ReadFileENS0_10FileHandleElPvm", ReadFile);
NX_HLE("_ZN2nn2fs9WriteFileENS0_10FileHandleElPKvmRKNS0_11WriteOptionE", WriteFile);
NX_HLE("_ZN2nn2fs9FlushFileENS0_10FileHandleE", FlushFile);
NX_HLE("_ZN2nn2fs25EnableGlobalFileDataCacheEPvm", EnableGlobalFileDataCache);
NX_HLE("_ZN2nn2fs26DisableGlobalFileDataCacheEv", DisableGlobalFileDataCache);
NX_HLE("_ZN2nn2fs6CommitEPKc", Commit);
NX_HLE("_ZN2nn2fs13OpenDirectoryEPNS0_15DirectoryHandleEPKci", OpenDirectory);
NX_HLE("_ZN2nn2fs13ReadDirectoryEPlPNS0_14DirectoryEntryENS0_15DirectoryHandleEl", ReadDirectory);
NX_HLE("_ZN2nn2fs14CloseDirectoryENS0_15DirectoryHandleE", CloseDirectory);
NX_HLE("_ZN2nn2fs22GetDirectoryEntryCountEPlNS0_15DirectoryHandleE", GetDirectoryEntryCount);
NX_HLE("_ZN2nn2fs20OpenCacheStorageListEPNS0_22CacheStorageListHandleE", OpenCacheStorageList);
NX_HLE("_ZN2nn2fs20ReadCacheStorageListEPiPNS0_16CacheStorageInfoENS0_22CacheStorageListHandleEi", ReadCacheStorageList);
NX_HLE("_ZN2nn2fs21CloseCacheStorageListENS0_22CacheStorageListHandleE", CloseCacheStorageList);
NX_HLE("_ZN2nn2fs18CreateCacheStorageEill", CreateCacheStorage);
NX_HLE("_ZN2nn2fs18DeleteCacheStorageEi", DeleteCacheStorage);
NX_HLE("_ZN2nn2fs17MountCacheStorageEPKci", MountCacheStorage);
NX_HLE("_ZN2nn2fs8MountRomEPKc", MountRom);
NX_HLE("_ZN2nn3aoc17CountAddOnContentEv", CountAddOnContent);
NX_HLE("_ZN2nn3aoc16ListAddOnContentEPiii", ListAddOnContent);
NX_HLE("_ZN2nn2fs31QueryMountAddOnContentCacheSizeEPmi", QueryMountAddOnContentCacheSize);
NX_HLE("_ZN2nn2fs17MountAddOnContentEPKciPvm", MountAddOnContent);
NX_HLE("_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvmRKNS0_10ReadOptionE", ReadFileOut);
NX_HLE("_ZN2nn2fs18IsSaveDataExistingERKNS_7account3UidE", IsSaveDataExisting);
NX_HLE("_ZN2nn2fs11SetFileSizeENS0_10FileHandleEl", SetFileSize);
NX_HLE("_ZN2nn2fs26DeleteDirectoryRecursivelyEPKc", DeleteDirectoryRecursively);

void log_recent_files() {
    ensure_host();
    acnh_files_log_recent();
}

void add_virtual_file(const std::string& path, std::string data) {
    ensure_host();
    acnh_files_add_virtual_file(path.c_str(), reinterpret_cast<const uint8_t*>(data.data()), data.size());
}

std::string read_game_file(const std::string& relative_to_romfs) {
    ensure_host();
    std::string out;
    acnh_files_read_game_file(relative_to_romfs.c_str(), bytes_cb, &out);
    return out;
}

void mods_fetch() {
    ensure_host();
    acnh_files_mods_fetch();
}

}  // namespace nx

#if defined(NX_GAME_DYNAMIC) && !defined(__ANDROID__)
#ifdef _WIN32
void nx_hook(nx::CPU& c, uint64_t pc);  // loader.cpp
#endif

namespace nx::plat {

namespace {
#ifdef _WIN32
HMODULE g_game_lib = nullptr;
void* sym(const char* name) { return reinterpret_cast<void*>(GetProcAddress(g_game_lib, name)); }
#else
void* g_game_lib = nullptr;
void* sym(const char* name) { return dlsym(g_game_lib, name); }
#endif
using LongJmpFn = void (*)(uint64_t, uint64_t);
LongJmpFn g_game_longjmp = nullptr;
}  // namespace

std::string load_game(const std::string& folder) {
    ensure_host();
    std::string lib;
    acnh_files_game_library_path(folder.c_str(), nx::game::kInfo.id, string_cb, &lib);
#ifdef _WIN32
    int n = MultiByteToWideChar(CP_UTF8, 0, lib.c_str(), -1, nullptr, 0);
    std::wstring w(size_t(n), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, lib.c_str(), -1, w.data(), n);
    if (GetFileAttributesW(w.c_str()) == INVALID_FILE_ATTRIBUTES) return "missing " + lib;
    g_game_lib = LoadLibraryExW(w.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    if (!g_game_lib) return "LoadLibrary failed (" + std::to_string(GetLastError()) + "): " + lib;
#else
    if (access(lib.c_str(), R_OK) != 0) return "missing " + lib;
    g_game_lib = dlopen(lib.c_str(), RTLD_NOW | RTLD_LOCAL);
    if (!g_game_lib) return std::string("dlopen: ") + dlerror();
#endif
    auto abi = reinterpret_cast<const char* (*)()>(sym("nx_game_abi"));
    auto jmp = reinterpret_cast<LongJmpFn>(sym("nx_game_longjmp"));
    if (!abi || !jmp || !sym("nx_module_main") || !sym("nx_module_sdk")) return "not a game library: " + lib;
    if (std::strcmp(abi(), NX_GAME_ABI) != 0) return "abi " + std::string(abi()) + " (this ACNHRecomp: " + NX_GAME_ABI + ")";
#ifdef _WIN32
    struct Api {
        void (*call)(nx::CPU&, uint64_t);
        void (*svc)(nx::CPU&, uint32_t, uint64_t);
        uint64_t (*mrs)(nx::CPU&, uint32_t);
        void (*msr)(nx::CPU&, uint32_t, uint64_t);
        void (*unimpl)(nx::CPU&, uint64_t, const char*);
        void (*hook)(nx::CPU&, uint64_t);
        void (*trace_sp)(nx::CPU&, uint64_t);
    } api = {&nx::call, &nx::svc, &nx::mrs, &nx::msr, &nx::unimpl, &nx_hook, &nx::trace_sp};
    auto bind = reinterpret_cast<void (*)(const void*, size_t)>(sym("nx_game_bind"));
    if (!bind) return "not a game library (no nx_game_bind): " + lib;
    bind(&api, sizeof api);
    if (auto variant = reinterpret_cast<size_t (*)(const char**, const uint32_t**)>(sym("nx_game_variant"))) {
        const char* module = nullptr;
        const uint32_t* words = nullptr;
        if (size_t wn = variant(&module, &words); wn && module && words) nx::VariantRegistrar(module, words, wn);
    }
#endif
    g_game_longjmp = jmp;
    return "";
}

void game_set_variant(bool on, uint64_t lo, uint64_t hi) {
#ifdef _WIN32
    if (auto set = reinterpret_cast<void (*)(bool, uint64_t, uint64_t)>(g_game_lib ? sym("nx_game_set_variant") : nullptr))
        set(on, lo, hi);
#else
    (void)on, (void)lo, (void)hi;
#endif
}

const nx::ModuleInfo* game_module(const char* name) {
    std::string s = std::string("nx_module_") + name;
    return g_game_lib ? static_cast<const nx::ModuleInfo*>(sym(s.c_str())) : nullptr;
}

[[noreturn]] void game_longjmp(uint64_t buf, uint64_t val) {
    g_game_longjmp(buf, val);
    std::abort();
}

bool load_game_or_explain() {
    std::string why = load_game(data_root());
    if (why.empty()) return true;
    std::string msg;
    acnh_files_format_load_error(why.c_str(), string_cb, &msg);
    nx::log("platform: %s", why.c_str());
    const SDL_MessageBoxButtonData buttons[] = {{SDL_MESSAGEBOX_BUTTON_RETURNKEY_DEFAULT, 1, "Discord"},
                                                {SDL_MESSAGEBOX_BUTTON_ESCAPEKEY_DEFAULT, 0, "Close"}};
    const SDL_MessageBoxData box = {SDL_MESSAGEBOX_ERROR, nullptr, nx::game::kInfo.title, msg.c_str(), 2, buttons, nullptr};
    int pressed = 0;
    if (SDL_ShowMessageBox(&box, &pressed) && pressed == 1) SDL_OpenURL("https://discord.gg/VJW9Dtdu7a");
    return false;
}

}  // namespace nx::plat
#endif
