// Adapted from fedebuonco/nxrt runtime/yield_backoff.{c,h} (c41d913), Unlicense
// Progress-aware back-off for svcSleepThread(0) yield loops: a thread that yields over and over without touching its
// callee-saved registers (x19..x28) is polling a flag, so it may sleep; one whose registers change is making progress
// (a job join) and never sleeps.
#pragma once

#include <cstdint>

namespace nx {

// Nanoseconds to sleep for a run of consecutive yields that began run_ns ago: 0 until run_ns reaches start_ns, then
// run_ns/8 clamped to 10 us..cap_ns.
constexpr uint64_t yield_backoff_sleep_ns(uint64_t run_ns, uint64_t start_ns, uint64_t cap_ns) {
    if (run_ns < start_ns) return 0;
    uint64_t s = run_ns / 8;
    if (s < 10'000) s = 10'000;
    if (s > cap_ns) s = cap_ns;
    return s;
}

// True if any of x19..x28 differs between the two snapshots.
constexpr bool yield_backoff_progress(const uint64_t* prev, const uint64_t* now) {
    for (int i = 0; i < 10; i++)
        if (prev[i] != now[i]) return true;
    return false;
}

// Checked by the normal runtime build (ported from nxrt tests/test_yield.c).
static_assert(yield_backoff_sleep_ns(999, 1000, 250'000) == 0, "below start: no sleep");
static_assert(yield_backoff_sleep_ns(1000, 1000, 250'000) == 10'000, "at start: clamped to 10 us");
static_assert(yield_backoff_sleep_ns(800'000, 1000, 250'000) == 100'000, "run/8 in the middle range");
static_assert(yield_backoff_sleep_ns(100'000'000, 1000, 250'000) == 250'000, "capped at cap_ns");
namespace yield_backoff_check {
constexpr uint64_t a[10] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10};
constexpr uint64_t b[10] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 11};
static_assert(!yield_backoff_progress(a, a), "equal registers: no progress");
static_assert(yield_backoff_progress(a, b), "one register differs: progress");
}  // namespace yield_backoff_check

}  // namespace nx
