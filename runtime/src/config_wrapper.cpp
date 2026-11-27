// Thin C++ wrapper forwarding nx::config_* calls to the Rust implementation in crates/runtime/src/config.rs.
#include "config.h"

#include <algorithm>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

#include <nx/runtime.h>

#include <acnh_runtime.h>

#include "game_clock.h"
#include "platform.h"

#ifndef NX_VERSION_STRING
#define NX_VERSION_STRING "dev"
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

size_t host_device_locale(char* buf, size_t cap) { return copy_string_to_buf(plat::device_locale(), buf, cap); }

void host_log(const char* msg) {
    if (msg) log("%s", msg);
}

void host_mods_fetch() { mods_fetch(); }

void host_game_clock_apply_config() { game_clock_apply_config(); }

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhConfigHost host{};
        host.data_root = host_data_root;
        host.device_locale = host_device_locale;
        host.version_string = NX_VERSION_STRING;
        host.log = host_log;
        host.mods_fetch = host_mods_fetch;
        host.game_clock_apply_config = host_game_clock_apply_config;
        acnh_config_set_host(&host);
    });
}

void string_cb(const char* data, size_t len, void* ctx) {
    auto* out = static_cast<std::string*>(ctx);
    if (data && len > 0) out->assign(data, len);
    else out->clear();
}

void entry_cb(const char* k, size_t klen, const char* v, size_t vlen, void* ctx) {
    auto* out = static_cast<std::vector<ConfigEntry>*>(ctx);
    out->push_back({k ? std::string(k, klen) : std::string(), v ? std::string(v, vlen) : std::string()});
}

void pair_cb(const char* k, size_t klen, const char* v, size_t vlen, void* ctx) {
    auto* out = static_cast<std::vector<std::pair<std::string, std::string>>*>(ctx);
    out->push_back({k ? std::string(k, klen) : std::string(), v ? std::string(v, vlen) : std::string()});
}

}  // namespace

void config_load() {
    ensure_host();
    acnh_config_load();
}

void config_apply() {
    ensure_host();
    acnh_config_apply();
}

bool config_save() {
    ensure_host();
    return acnh_config_save();
}

std::string config_get(const std::string& key, const std::string& fallback) {
    ensure_host();
    std::string out;
    acnh_config_get(key.c_str(), fallback.c_str(), string_cb, &out);
    return out;
}

int config_get_int(const std::string& key, int fallback) {
    ensure_host();
    return acnh_config_get_int(key.c_str(), fallback);
}

int64_t config_get_int64(const std::string& key, int64_t fallback) {
    ensure_host();
    return acnh_config_get_int64(key.c_str(), fallback);
}

float config_get_float(const std::string& key, float fallback) {
    ensure_host();
    return acnh_config_get_float(key.c_str(), fallback);
}

bool config_get_bool(const std::string& key, bool fallback) {
    ensure_host();
    return acnh_config_get_bool(key.c_str(), fallback);
}

void config_set(const std::string& key, const std::string& value) {
    ensure_host();
    acnh_config_set(key.c_str(), value.c_str());
}

std::vector<ConfigEntry> config_all() {
    ensure_host();
    std::vector<ConfigEntry> out;
    acnh_config_all(entry_cb, &out);
    return out;
}

uint64_t config_generation() {
    ensure_host();
    return acnh_config_generation();
}

const char* config_env_override(const char* name) {
    ensure_host();
    if (!name) return nullptr;
    const char* e = std::getenv(name);
    if (!e || !*e) return nullptr;
    return acnh_config_is_env_from_config(name) ? nullptr : e;
}

bool config_show_fps() {
    ensure_host();
    return acnh_config_show_fps();
}

bool config_is_handheld() {
    ensure_host();
    return acnh_config_is_handheld();
}

std::vector<std::pair<std::string, std::string>> config_preset(const std::string& name) {
    ensure_host();
    std::vector<std::pair<std::string, std::string>> out;
    acnh_config_preset(name.c_str(), pair_cb, &out);
    return out;
}

bool config_needs_restart(const std::string& key) {
    ensure_host();
    return acnh_config_needs_restart(key.c_str());
}

}  // namespace nx
