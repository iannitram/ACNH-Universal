#pragma once
#include <cstdint>

namespace nx::compact {
struct Choice { uint64_t address = 0, gap_size = 0; };
// Compare one free gap against the current choice. Retried placements skip rejected addresses.
inline void consider_gap(uint64_t start, uint64_t end, uint64_t page, uint64_t required,
                         const uint64_t* rejected, unsigned count, Choice& best) {
    if (!page || start > UINT64_MAX - (page - 1)) return;
    const uint64_t aligned = (start + page - 1) & ~(page - 1);
    if (end <= aligned || end - aligned < required || end - aligned <= best.gap_size) return;
    const uint64_t candidates[] = {aligned,
        (aligned + (end - aligned - required) / 2) & ~(page - 1),
        (end - required) & ~(page - 1)};
    for (uint64_t candidate : candidates) {
        bool tried = false;
        for (unsigned i = 0; i < count; ++i) tried |= rejected[i] == candidate;
        if (!tried) { best = {candidate, end - aligned}; return; }
    }
}
}  // namespace nx::compact
