// Thin C++ wrapper forwarding nx::game_clock_*, nn::time HLE, and time:u/a/s IPC
// to the Rust implementation in crates/runtime/src/time.rs.
#include "game_clock.h"

#include <cstring>
#include <ctime>
#include <memory>
#include <mutex>

#include <nx/runtime.h>

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#endif

#include "ipc.h"
#include "nextendo.h"
#include "platform.h"

#ifndef NX_RUST_RUNTIME
extern "C" {
struct AcnhCalendarTime {
    int16_t year;
    int8_t month;
    int8_t day;
    int8_t hour;
    int8_t minute;
    int8_t second;
    uint8_t _pad;
};
struct AcnhCalendarAdditionalInfo {
    int32_t day_of_week;
    int32_t day_of_year;
    uint8_t tz_name[8];
    int32_t is_dst;
    int32_t utc_offset;
};
struct AcnhTimeHost {
    bool (*online_services_active)();
    uint64_t (*monotonic_ns)();
    uint64_t (*steady_ticks)();
    bool (*local_from_posix)(int64_t posix, AcnhCalendarTime* out_cal, AcnhCalendarAdditionalInfo* out_add);
    bool (*posix_from_local)(const AcnhCalendarTime* cal, int64_t* out_posix);
    int32_t (*utc_offset_west)();
};
void acnh_time_set_host(const AcnhTimeHost* host);
int64_t acnh_time_real_unix_seconds();
int64_t acnh_time_unix_seconds();
int64_t acnh_time_network_unix_seconds();
int64_t acnh_time_steady_seconds();
void acnh_time_apply_config();
void acnh_time_set_offset(int64_t offset);
void acnh_time_save();
void acnh_time_init_shared_memory(uint8_t* shm_ptr, size_t shm_size);
void acnh_time_refresh_shared_clocks();
void acnh_time_gettimeofday(int64_t* out_sec, int64_t* out_usec);
uint64_t acnh_time_clock_us();
void acnh_time_device_location_name(char* out, size_t cap);
void acnh_time_to_calendar(int64_t posix_time, AcnhCalendarTime* out_cal, AcnhCalendarAdditionalInfo* out_add);
int64_t acnh_time_to_posix(const AcnhCalendarTime* cal);
void acnh_time_to_calendar_ipc(int64_t posix_time, AcnhCalendarTime* out_cal, AcnhCalendarAdditionalInfo* out_add);
int64_t acnh_time_to_posix_ipc(const AcnhCalendarTime* cal);
}
#endif

namespace nx {

namespace {

constexpr uint64_t kSourceLo = 0x4E58525443524543ull, kSourceHi = 0x484144455354494Dull;

std::tm local_tm(int64_t t) {
    time_t tt = time_t(t);
    std::tm tm{};
#if defined(_WIN32)
    localtime_s(&tm, &tt);
#else
    localtime_r(&tt, &tm);
#endif
    return tm;
}

void utc_tm(time_t t, std::tm& out) {
#if defined(_WIN32)
    gmtime_s(&out, &t);
#else
    gmtime_r(&t, &out);
#endif
}

int64_t utc_from_tm(std::tm& tm) {
#if defined(_WIN32)
    return int64_t(_mkgmtime(&tm));
#else
    return int64_t(timegm(&tm));
#endif
}

long gmt_offset(const std::tm& tm, int64_t t) {
#if defined(_WIN32)
    std::tm local = tm;
    return long(int64_t(_mkgmtime(&local)) - t);
#else
    (void)t;
    return tm.tm_gmtoff;
#endif
}

bool host_online_services_active() { return nextendo::online_services_active(); }
uint64_t host_monotonic_ns() { return plat::monotonic_ns(); }
uint64_t host_steady_ticks() { return system_tick(); }

bool host_local_from_posix(int64_t posix, AcnhCalendarTime* out_cal, AcnhCalendarAdditionalInfo* out_add) {
    std::tm tm = local_tm(posix);
    if (out_cal) {
        out_cal->year = int16_t(tm.tm_year + 1900);
        out_cal->month = int8_t(tm.tm_mon + 1);
        out_cal->day = int8_t(tm.tm_mday);
        out_cal->hour = int8_t(tm.tm_hour);
        out_cal->minute = int8_t(tm.tm_min);
        out_cal->second = int8_t(tm.tm_sec);
        out_cal->_pad = 0;
    }
    if (out_add) {
        out_add->day_of_week = tm.tm_wday;
        out_add->day_of_year = tm.tm_yday;
        std::memset(out_add->tz_name, 0, sizeof(out_add->tz_name));
#if !defined(_WIN32)
        if (tm.tm_zone) std::strncpy(reinterpret_cast<char*>(out_add->tz_name), tm.tm_zone, sizeof(out_add->tz_name) - 1);
#endif
        out_add->is_dst = tm.tm_isdst > 0 ? 1 : 0;
        out_add->utc_offset = int32_t(gmt_offset(tm, posix));
    }
    return true;
}

bool host_posix_from_local(const AcnhCalendarTime* cal, int64_t* out_posix) {
    if (!cal || !out_posix) return false;
    std::tm tm{};
    tm.tm_year = cal->year - 1900;
    tm.tm_mon = cal->month - 1;
    tm.tm_mday = cal->day;
    tm.tm_hour = cal->hour;
    tm.tm_min = cal->minute;
    tm.tm_sec = cal->second;
    tm.tm_isdst = -1;
    *out_posix = int64_t(std::mktime(&tm));
    return true;
}

int32_t host_utc_offset_west() {
    time_t now = std::time(nullptr);
    std::tm lt{}, gt{};
#if defined(_WIN32)
    localtime_s(&lt, &now);
#else
    localtime_r(&now, &lt);
#endif
    utc_tm(now, gt);
    lt.tm_isdst = 0;
    return int32_t(utc_from_tm(gt) - utc_from_tm(lt));
}

void ensure_host() {
    static std::once_flag once;
    std::call_once(once, [] {
        AcnhTimeHost host{};
        host.online_services_active = host_online_services_active;
        host.monotonic_ns = host_monotonic_ns;
        host.steady_ticks = host_steady_ticks;
        host.local_from_posix = host_local_from_posix;
        host.posix_from_local = host_posix_from_local;
        host.utc_offset_west = host_utc_offset_west;
        acnh_time_set_host(&host);
    });
}

// ---- HLE functions (nn::time & libc time) ----------------------------------------------------------------------

void current_time(CPU& c) {
    ensure_host();
    st<int64_t>(c.x[0], acnh_time_unix_seconds());
    c.x[0] = 0;
}

void ok(CPU& c) { c.x[0] = 0; }

void network_time(CPU& c) {
    ensure_host();
    st<int64_t>(c.x[0], acnh_time_network_unix_seconds());
    c.x[0] = 0;
}

bool sdk_libc(CPU& c, const char* name) {
    if (nextendo::online_services_active()) return false;
    const uint64_t fn = find_symbol(name);
    if (!fn) return false;
    call(c, fn);
    return true;
}

void gettimeofday_(CPU& c) {
    if (sdk_libc(c, "gettimeofday")) return;
    ensure_host();
    int64_t sec = 0, usec = 0;
    acnh_time_gettimeofday(&sec, &usec);
    if (c.x[0]) {
        st<int64_t>(c.x[0], sec);
        st<int64_t>(c.x[0] + 8, usec);
    }
    c.x[0] = 0;
}

void time_(CPU& c) {
    if (sdk_libc(c, "time")) return;
    ensure_host();
    const int64_t t = acnh_time_unix_seconds();
    if (c.x[0]) st<int64_t>(c.x[0], t);
    c.x[0] = uint64_t(t);
}

void clock_(CPU& c) {
    if (sdk_libc(c, "clock")) return;
    ensure_host();
    c.x[0] = acnh_time_clock_us();
}

void network_accurate(CPU& c) { c.x[0] = nextendo::online_services_active() ? 1 : 0; }

void GetDeviceLocationName(CPU& c) {
    ensure_host();
    char name[0x24] = {};
    acnh_time_device_location_name(name, sizeof(name));
    std::memcpy(reinterpret_cast<void*>(c.x[0]), name, sizeof(name));
}

void LoadTimeZoneRule(CPU& c) { c.x[0] = 0; }

void ToCalendarTime(CPU& c) {
    ensure_host();
    int64_t t = ld<int64_t>(c.x[2]);
    AcnhCalendarTime cal{};
    AcnhCalendarAdditionalInfo add{};
    acnh_time_to_calendar(t, &cal, c.x[1] ? &add : nullptr);
    std::memcpy(reinterpret_cast<void*>(c.x[0]), &cal, sizeof(cal));
    if (c.x[1]) std::memcpy(reinterpret_cast<void*>(c.x[1]), &add, sizeof(add));
    c.x[0] = 0;
}

void ToPosixTime(CPU& c) {
    ensure_host();
    uint64_t count = c.x[0], out = c.x[1], cal_ptr = c.x[3];
    int max = int(c.x[2]);
    AcnhCalendarTime cal{};
    std::memcpy(&cal, reinterpret_cast<const void*>(cal_ptr), sizeof(cal));
    int64_t t = acnh_time_to_posix(&cal);
    if (max >= 1 && out) st<int64_t>(out, t);
    if (count) st<int32_t>(count, max >= 1 ? 1 : 0);
    c.x[0] = 0;
}

// ---- IPC services (time:u / time:a / time:s) -------------------------------------------------------------------

std::shared_ptr<KSharedMemory> time_shared_memory() {
    static std::shared_ptr<KSharedMemory> shm = [] {
        ensure_host();
        auto s = make_shared_memory("time", 0x1000);
        acnh_time_init_shared_memory(reinterpret_cast<uint8_t*>(s->host), 0x1000);
        return s;
    }();
    return shm;
}

std::shared_ptr<Service> system_clock(bool network = false) {
    auto s = std::make_shared<Service>("ISystemClock");
    s->cmds[0] = [network](IpcCtx& c) {
        ensure_host();
        c.push<int64_t>(network ? acnh_time_network_unix_seconds() : acnh_time_unix_seconds());
        return 0u;
    };
    s->cmds[2] = [network](IpcCtx& c) {
        ensure_host();
        const int64_t now = network ? acnh_time_network_unix_seconds() : acnh_time_unix_seconds();
        c.push<int64_t>(now - acnh_time_steady_seconds());
        c.push<int64_t>(0);
        c.push<uint64_t>(kSourceLo);
        c.push<uint64_t>(kSourceHi);
        return 0u;
    };
    return s;
}

std::shared_ptr<Service> steady_clock() {
    auto s = std::make_shared<Service>("ISteadyClock");
    s->cmds[0] = [](IpcCtx& c) {
        ensure_host();
        c.push<int64_t>(acnh_time_steady_seconds());
        c.push<uint64_t>(kSourceLo);
        c.push<uint64_t>(kSourceHi);
        return 0u;
    };
    return s;
}

void push_calendar(IpcCtx& c, int64_t t) {
    ensure_host();
    AcnhCalendarTime cal{};
    AcnhCalendarAdditionalInfo add{};
    acnh_time_to_calendar_ipc(t, &cal, &add);
    c.push(cal);
    c.push(add);
}

std::shared_ptr<Service> time_zone() {
    auto s = std::make_shared<Service>("ITimeZoneService");
    s->cmds[0] = [](IpcCtx& c) {
        ensure_host();
        char name[0x24] = {};
        acnh_time_device_location_name(name, sizeof(name));
        for (char ch : name) c.push<char>(ch);
        return 0u;
    };
    s->cmds[100] = [](IpcCtx& c) { push_calendar(c, c.arg<int64_t>(0)); return 0u; };
    s->cmds[101] = [](IpcCtx& c) { push_calendar(c, c.arg<int64_t>(0)); return 0u; };
    s->cmds[201] = [](IpcCtx& c) {
        ensure_host();
        AcnhCalendarTime cal{};
        cal.year = c.arg<int16_t>(0);
        cal.month = c.arg<int8_t>(2);
        cal.day = c.arg<int8_t>(3);
        cal.hour = c.arg<int8_t>(4);
        cal.minute = c.arg<int8_t>(5);
        cal.second = c.arg<int8_t>(6);
        int64_t t = acnh_time_to_posix_ipc(&cal);
        Buffer out = c.out_buf(0);
        if (out.addr && out.size >= 8) st<int64_t>(out.addr, t);
        c.push<uint32_t>(1);
        return 0u;
    };
    return s;
}

void time_static(Service& s) {
    s.cmds[0] = [](IpcCtx& c) { c.push_object(system_clock()); return 0u; };
    s.cmds[1] = [](IpcCtx& c) { c.push_object(system_clock(true)); return 0u; };
    s.cmds[2] = [](IpcCtx& c) { c.push_object(steady_clock()); return 0u; };
    s.cmds[3] = [](IpcCtx& c) { c.push_object(time_zone()); return 0u; };
    s.cmds[4] = [](IpcCtx& c) { c.push_object(system_clock()); return 0u; };
    s.cmds[5] = [](IpcCtx& c) { c.push_object(system_clock(true)); return 0u; };
    s.cmds[20] = [](IpcCtx& c) {
        c.out_copy.push_back(handle_add(time_shared_memory()));
        return 0u;
    };
    s.cmds[100] = [](IpcCtx& c) { c.push<uint8_t>(0); return 0u; };
    s.cmds[300] = [](IpcCtx& c) {
        ensure_host();
        c.push<int64_t>(acnh_time_unix_seconds());
        return 0u;
    };
}

ServiceRegistrar r_time_u("time:u", [] { auto s = std::make_shared<Service>("time:u"); time_static(*s); return s; });
ServiceRegistrar r_time_a("time:a", [] { auto s = std::make_shared<Service>("time:a"); time_static(*s); return s; });
ServiceRegistrar r_time_s("time:s", [] { auto s = std::make_shared<Service>("time:s"); time_static(*s); return s; });

}  // namespace

// ---- game_clock.h C++ API --------------------------------------------------------------------------------------

int64_t game_clock_real_unix_seconds() {
    ensure_host();
    return acnh_time_real_unix_seconds();
}

int64_t game_clock_unix_seconds() {
    ensure_host();
    return acnh_time_unix_seconds();
}

void game_clock_apply_config() {
    ensure_host();
    acnh_time_apply_config();
}

void game_clock_set_offset(int64_t offset) {
    ensure_host();
    acnh_time_set_offset(offset);
}

void game_clock_save() {
    ensure_host();
    acnh_time_save();
}

void nx_time_refresh_shared_clocks() {
    ensure_host();
    acnh_time_refresh_shared_clocks();
}

NX_HLE("_ZN2nn4time23StandardUserSystemClock14GetCurrentTimeEPNS0_9PosixTimeE", current_time);
NX_HLE("gettimeofday", gettimeofday_);
NX_HLE("time", time_);
NX_HLE("clock", clock_);
NX_HLE("_ZN2nn4time26StandardNetworkSystemClock14GetCurrentTimeEPNS0_9PosixTimeE", network_time);
NX_HLE("_ZN2nn4time25AdjustableUserSystemClock14GetCurrentTimeEPNS0_9PosixTimeE", current_time);
NX_HLE("_ZN2nn4time25AdjustableUserSystemClock6AdjustEv", ok);
NX_HLE("_ZN2nn4time26StandardNetworkSystemClock20IsAccuracySufficientEv", network_accurate);
NX_HLE("_ZN2nn4time21GetDeviceLocationNameEPNS0_12LocationNameE", GetDeviceLocationName);
NX_HLE("_ZN2nn4time16LoadTimeZoneRuleEPNS0_12TimeZoneRuleERKNS0_12LocationNameE", LoadTimeZoneRule);
NX_HLE("_ZN2nn4time14ToCalendarTimeEPNS0_12CalendarTimeEPNS0_22CalendarAdditionalInfoERKNS0_9PosixTimeERKNS0_12TimeZoneRuleE",
       ToCalendarTime);
NX_HLE("_ZN2nn4time14ToCalendarTimeEPNS0_12CalendarTimeEPNS0_22CalendarAdditionalInfoERKNS0_9PosixTimeE", ToCalendarTime);
NX_HLE("_ZN2nn4time11ToPosixTimeEPiPNS0_9PosixTimeEiRKNS0_12CalendarTimeERKNS0_12TimeZoneRuleE", ToPosixTime);

}  // namespace nx
