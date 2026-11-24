// The Windows memory guard's helpers (memory_wrapper.cpp -> crates/runtime/src/memory.rs): the limit parsing,
// and that the footprint it reads follows this process's own commits. Built by CMake with -DNX_TESTS=ON (ctest).
#include <windows.h>

#include <cstdio>
#include <vector>

#include "platform.h"

namespace nx {
void log(const char*, ...) {}
[[noreturn]] void fatal(const char*, ...) { std::abort(); }
namespace plat {
bool reserve(uint64_t, uint64_t) { return false; }
bool commit(uint64_t, uint64_t) { return false; }
void decommit(uint64_t, uint64_t) {}
uint64_t monotonic_ns() { return 0; }
double memory_limit_gb(const char* env);
uint64_t memory_footprint();
}  // namespace plat
}  // namespace nx

int main() {
    using namespace nx::plat;
    int failed = 0;
    auto check = [&](bool ok, const char* what) {
        if (!ok) std::printf("FAIL: %s\n", what), failed++;
    };
    // Unset: half the RAM, at least 8 GB.
    MEMORYSTATUSEX ms{};
    ms.dwLength = sizeof ms;
    GlobalMemoryStatusEx(&ms);
    const double half = double(ms.ullTotalPhys) / double(1ull << 30) / 2.0, dflt = half > 8.0 ? half : 8.0;
    check(memory_limit_gb(nullptr) == dflt, "unset limit is half the RAM, at least 8 GB");
    check(memory_limit_gb("") == dflt, "empty limit is the default");
    check(memory_limit_gb("0") == dflt, "zero limit is the default");
    check(memory_limit_gb("-2") == dflt, "negative limit is the default");
    check(memory_limit_gb("5.5") == 5.5, "5.5 GB");
    check(memory_limit_gb("12") == 12.0, "12 GB");

    const uint64_t before = memory_footprint();
    check(before > 0, "footprint readable");
    std::vector<char> block(size_t(256) << 20, 1);  // 256 MB, touched
    const uint64_t after = memory_footprint();
    check(after >= before + (uint64_t(200) << 20), "a 256 MB commit shows in the footprint");
    std::printf("memguard_test: footprint %.1f MB -> %.1f MB (%d)\n", double(before) / 1048576.0,
                double(after) / 1048576.0, int(block[12345]));
    std::printf("memguard_test: %s\n", failed ? "FAILED" : "all passed");
    return failed ? 1 : 0;
}
