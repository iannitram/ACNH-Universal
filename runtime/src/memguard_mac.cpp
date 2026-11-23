// Memory guard (macOS): the process's physical footprint (what Activity Monitor shows, GPU memory included on Apple
// silicon) is checked every 250 ms; past the limit the process ends at once, so a leak can never push the machine
// into swap until it freezes (it did: 85 GB, texture images never freed). The limit is NX_MEM_LIMIT_GB (default 8, half of a 16 GB Mac);
// the footprint is logged every 30 s so growth shows before it matters.
#include <TargetConditionals.h>
#include <mach/mach.h>
#if TARGET_OS_IPHONE
#include <os/proc.h>
#endif
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <thread>

#include <nx/runtime.h>

#include "platform.h"

namespace nx::plat {

namespace {

uint64_t footprint() {
    task_vm_info_data_t info{};
    mach_msg_type_number_t count = TASK_VM_INFO_COUNT;
    if (task_info(mach_task_self(), TASK_VM_INFO, reinterpret_cast<task_info_t>(&info), &count) != KERN_SUCCESS) return 0;
    return info.phys_footprint;
}

}  // namespace

void start_memory_guard() {
    const char* e = std::getenv("NX_MEM_LIMIT_GB");
#if TARGET_OS_IPHONE
    // iOS and iPadOS kill an app (jetsam) past the footprint it may still grow: the one it has at start plus what
    // os_proc_available_memory() reports, less a margin, so a run that nears the limit ends with a log line.
    const uint64_t start = footprint();
    const uint64_t available = os_proc_available_memory();
    const uint64_t margin = 256ull << 20;
    // 0 means the process has no such limit (the iOS Simulator, Mac Catalyst): guard like the Mac does (8 GB).
    uint64_t limit = available == 0 ? 8ull << 30 : start + (available > margin ? available - margin : 0);
    double gb = e ? std::atof(e) : 0.0;
    if (gb > 0) limit = uint64_t(gb * double(1ull << 30));
    else gb = double(limit) / double(1ull << 30);
    log("memory: %.2f GB at start, iOS limit %.1f GB (%llu MB available, %llu MB margin)", double(start) / double(1ull << 30),
        gb, (unsigned long long)(available >> 20), (unsigned long long)(margin >> 20));
#else
    double gb = e ? std::atof(e) : 8.0;
    if (gb <= 0) gb = 8.0;
    const uint64_t limit = uint64_t(gb * double(1ull << 30));
#endif
    std::thread([limit, gb] {
        uint64_t ticks = 0, peak = 0;
        for (;;) {
            std::this_thread::sleep_for(std::chrono::milliseconds(250));
            uint64_t f = footprint();
            if (f > peak) peak = f;
            if (++ticks % 120 == 0)
                log("memory: %.2f GB (peak %.2f GB, limit %.1f GB)", double(f) / double(1ull << 30),
                    double(peak) / double(1ull << 30), gb);
            if (f > limit) {
                char msg[160];
                std::snprintf(msg, sizeof msg, "memory: %.2f GB is over the %.1f GB limit (NX_MEM_LIMIT_GB): stopping",
                              double(f) / double(1ull << 30), gb);
                log("%s", msg);
                std::fprintf(stderr, "%s\n", msg);
                _exit(3);
            }
        }
    }).detach();
}

uint64_t memguard_overlap_ns(uint64_t, uint64_t) { return 0; }

}  // namespace nx::plat
