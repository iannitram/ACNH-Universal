// Per-submit upload reuse (docs/nvn/DEBUG.md #30) and cross-submit persistent buffer cache. A draw's buffers are copied
// from guest memory into the upload ring when it is replayed; the same camera block, scene constants and vertex buffers
// were copied again for every draw that bound them (~130 KB per draw, 50-70 MB per frame). Within one submit guest
// memory only changes through inline uniform updates (UpdateUniformBuffer, which invalidates what it overlaps) and
// compute results (ranges a dispatch writes are read from their written copy, nvn_written.cpp, and invalidated here):
// a copy made once can serve every later draw of the submit that binds the same bytes.
//
// Cross-submit persistent VkBuffer cache + dirty-range delta patching (adapted from Timqt/d2r-android
// nvn_vulkan_delta.inc / pending_ranges.c, MIT): buffers whose content is unchanged across submits are served from a
// persistent host-visible VkBuffer without re-copying, and retired entries with partial edits patch only dirty 256-byte
// chunks. NX_PERSISTENT_DEDUP=0 disables the cross-submit cache.
#include <algorithm>
#include <array>
#include <atomic>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <map>
#include <memory>
#include <memory_resource>
#include <mutex>
#include <unordered_map>
#include <vector>

#include "nvn_draw_state.h"

namespace nx::nvn::replay {

using namespace nx::vk;

namespace {

struct Entry {
    uint64_t size = 0, padded = 0;
    Upload upload;
};
// By host address; several sizes of one address are rare, so the latest one is kept. Its nodes come from an arena
// emptied with it at each submit (~2,000 inserts a frame were as many mallocs and frees: ~3% of the replay thread on a
// Snapdragon 865).
// Per replaying thread (docs/PARALLEL_REPLAY.md): a worker shares copies within its own segments.
struct Copies {
    std::pmr::monotonic_buffer_resource arena{1 << 20};
    std::pmr::map<const uint8_t*, Entry> map{&arena};
};
PerThread<Copies> g_cp;
PerThread<uint64_t> g_saved, g_copied, g_data, g_copies_n, g_big_pads;  // g_data: bytes of guest data among g_copied

// In front of the map (a tree walk per bound buffer of every draw, and most are hits): a slot per address hash with a
// copy of that address's entry in (*g_cp).map. A slot of the current generation always equals the map's entry: both are
// written together, dedup_invalidate clears the slot of each entry it erases, dedup_reset starts a new generation.
struct Front {
    const uint8_t* p = nullptr;
    uint64_t generation = 0;
    Entry entry;
};
PerThread<TlsArray<Front, 1024>> g_front;
PerThread<uint64_t> g_generation{1};
Front& front_of(const uint8_t* p) { return (*g_front)[(uintptr_t(p) >> 4 ^ uintptr_t(p) >> 14) & 1023]; }

bool persistent_enabled() {
    static const bool on = [] {
        const char* e = std::getenv("NX_PERSISTENT_DEDUP");
        return !(e && e[0] == '0');
    }();
    return on && !draw_slow(kSlowUploads) && vk_device() != VK_NULL_HANDLE && vk_physical() != VK_NULL_HANDLE;
}

uint64_t fast_hash(const uint8_t* p, uint64_t size) {
    uint64_t h = 0x9E3779B97F4A7C15ull ^ size;
    const uint64_t step = size > 1024 ? size / 64 : 8;
    for (uint64_t i = 0; i + 8 <= size; i += step) {
        uint64_t v = 0;
        std::memcpy(&v, p + i, 8);
        h = (h ^ v) * 0x100000001B3ull, h ^= h >> 31;
    }
    if (size >= 8) {
        uint64_t v = 0;
        std::memcpy(&v, p + size - 8, 8);
        h = (h ^ v) * 0x100000001B3ull, h ^= h >> 31;
    } else {
        for (uint64_t i = 0; i < size; i++) h = (h ^ p[i]) * 0x100000001B3ull, h ^= h >> 31;
    }
    return h;
}

struct PersistentSlab {
    VkBuffer buffer = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    uint8_t* ptr = nullptr;
    uint64_t size = 0;
    uint64_t used = 0;
};

struct PersistentEntry {
    uint64_t size = 0;
    uint64_t padded = 0;
    Upload upload{};
    std::vector<uint8_t> shadow;
    uint64_t last_serial = 0;
    bool valid = true;
};

struct Candidate {
    uint64_t size = 0;
    uint64_t padded = 0;
    uint64_t hash = 0;
    uint64_t epoch = 0;
};

struct PersistentCache {
    static constexpr uint64_t kSlabBytes = 16ull << 20;
    static constexpr size_t kMaxSlabs = 2;
    static constexpr size_t kMaxCandidates = 4096;
    static constexpr uint64_t kMaxEntryBytes = 4ull << 20;

    std::mutex mx;
    std::vector<PersistentSlab> slabs;
    std::map<const uint8_t*, PersistentEntry> entries;
    std::unordered_map<const uint8_t*, Candidate> candidates;
    std::atomic<uint64_t> epoch{1};
    std::atomic<uint64_t> saved_bytes{0};
    std::atomic<uint64_t> patched_bytes{0};

    Upload alloc_slab(uint64_t padded, uint64_t align) {
        const uint64_t a = std::max<uint64_t>(align, 256);
        for (PersistentSlab& s : slabs) {
            uint64_t off = (s.used + a - 1) & ~(a - 1);
            if (off + padded <= s.size) {
                s.used = off + padded;
                return {s.buffer, off, s.ptr + off};
            }
        }
        if (slabs.size() >= kMaxSlabs) return {};
        PersistentSlab s;
        s.size = std::max(kSlabBytes, padded);
        VkBufferCreateInfo bi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
        bi.size = s.size;
        bi.usage = kUploadUsage;
        if (p_vkCreateBuffer(vk_device(), &bi, nullptr, &s.buffer) != VK_SUCCESS) return {};
        VkMemoryRequirements req{};
        p_vkGetBufferMemoryRequirements(vk_device(), s.buffer, &req);
        VkMemoryAllocateInfo ai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
        ai.allocationSize = req.size;
        ai.memoryTypeIndex = upload_memory_type(req.memoryTypeBits);
        if (p_vkAllocateMemory(vk_device(), &ai, nullptr, &s.memory) != VK_SUCCESS) {
            p_vkDestroyBuffer(vk_device(), s.buffer, nullptr);
            return {};
        }
        p_vkBindBufferMemory(vk_device(), s.buffer, s.memory, 0);
        void* mapped = nullptr;
        if (p_vkMapMemory(vk_device(), s.memory, 0, VK_WHOLE_SIZE, 0, &mapped) != VK_SUCCESS || !mapped) {
            p_vkFreeMemory(vk_device(), s.memory, nullptr);
            p_vkDestroyBuffer(vk_device(), s.buffer, nullptr);
            return {};
        }
        s.ptr = static_cast<uint8_t*>(mapped);
        uint64_t off = 0;
        s.used = padded;
        slabs.push_back(s);
        return {s.buffer, off, s.ptr};
    }

    bool try_lookup_or_promote(const uint8_t* p, uint64_t size, uint64_t padded, uint64_t align, Upload& out) {
        if (size > kMaxEntryBytes || padded > kMaxEntryBytes) return false;
        const uint64_t cur_epoch = epoch.load(std::memory_order_relaxed);
        const bool async = async_gpu();
        const uint64_t completed = async ? frame_completed() : UINT64_MAX;
        const uint64_t cur_serial = async ? (frame_submitted() + 1) : 0;

        std::lock_guard lk(mx);
        if (auto it = entries.find(p); it != entries.end()) {
            PersistentEntry& e = it->second;
            if (e.size == size && e.padded == padded && e.upload.offset % align == 0) {
                if (e.valid && std::memcmp(e.shadow.data(), p, size) == 0) {
                    e.last_serial = cur_serial;
                    saved_bytes.fetch_add(padded, std::memory_order_relaxed);
                    out = e.upload;
                    return true;
                }
                if (!async || completed >= e.last_serial) {
                    constexpr uint64_t kChunk = 256;
                    uint64_t patched = 0;
                    for (uint64_t off = 0; off < size; off += kChunk) {
                        uint64_t len = std::min(kChunk, size - off);
                        if (!e.valid || std::memcmp(e.shadow.data() + off, p + off, len) != 0) {
                            std::memcpy(e.upload.ptr + off, p + off, len);
                            std::memcpy(e.shadow.data() + off, p + off, len);
                            patched += len;
                        }
                    }
                    e.valid = true;
                    e.last_serial = cur_serial;
                    patched_bytes.fetch_add(patched, std::memory_order_relaxed);
                    if (padded > patched) saved_bytes.fetch_add(padded - patched, std::memory_order_relaxed);
                    out = e.upload;
                    return true;
                }
            }
            return false;
        }

        const uint64_t h = fast_hash(p, size);
        auto cit = candidates.find(p);
        if (cit == candidates.end()) {
            if (candidates.size() >= kMaxCandidates) candidates.clear();
            candidates[p] = {size, padded, h, cur_epoch};
            return false;
        }
        Candidate& c = cit->second;
        if (c.size != size || c.padded != padded || c.hash != h) {
            c = {size, padded, h, cur_epoch};
            return false;
        }
        if (c.epoch == cur_epoch) return false;

        Upload u = alloc_slab(padded, align);
        if (!u.buffer) return false;
        std::memcpy(u.ptr, p, size);
        if (padded > size) std::memset(u.ptr + size, 0, padded - size);
        PersistentEntry e;
        e.size = size;
        e.padded = padded;
        e.upload = u;
        e.shadow.assign(p, p + size);
        e.last_serial = cur_serial;
        e.valid = true;
        entries[p] = std::move(e);
        candidates.erase(cit);
        out = u;
        return true;
    }

    void invalidate(const uint8_t* p, uint64_t size) {
        std::lock_guard lk(mx);
        auto it = entries.lower_bound(p + size);
        while (it != entries.begin()) {
            --it;
            if (it->first + it->second.size <= p) {
                if (p - it->first > (64ll << 20)) break;
                continue;
            }
            it->second.valid = false;
        }
        candidates.erase(p);
    }
};

PersistentCache g_persistent;

}  // namespace

PerThread<uint64_t> g_upload_epoch{1};

Upload dedup_upload(const uint8_t* p, uint64_t size, uint64_t padded, uint64_t align) {
    write_back_before_read(p, size, "dedup");
    if (!p || !size) {
        Upload u = upload_alloc(padded, align);
        std::memset(u.ptr, 0, padded);
        return u;
    }
    static const bool off = std::getenv("NX_NO_DEDUP") != nullptr;  // bisecting: a fresh copy every time
    Front& f = front_of(p);
    const Entry* e = nullptr;
    if (!off && f.generation == (*g_generation) && f.p == p) {
        e = &f.entry;  // the map's entry for p
    } else if (auto it = off ? (*g_cp).map.end() : (*g_cp).map.find(p); it != (*g_cp).map.end()) {
        f = {p, (*g_generation), it->second};
        e = &f.entry;
    }
    if (e && e->size == size && e->padded == padded && e->upload.offset % align == 0) {
        (*g_saved) += padded;
        return e->upload;
    }
    Upload u;
    if (!off && persistent_enabled() && g_persistent.try_lookup_or_promote(p, size, padded, align, u)) {
        (*g_cp).map[p] = {size, padded, u};
        f = {p, (*g_generation), {size, padded, u}};
        return u;
    }
    u = upload_alloc(padded, align);
    std::memcpy(u.ptr, p, size);
    if (padded > size) std::memset(u.ptr + size, 0, padded - size);
    (*g_cp).map[p] = {size, padded, u};
    f = {p, (*g_generation), {size, padded, u}};
    (*g_copied) += padded, (*g_data) += size, (*g_copies_n)++, (*g_big_pads) += padded >= 0x10000;
    return u;
}

void dedup_reset() {
    // The map is made again after the arena's release: MSVC's std::map allocates its head node from that arena.
    Copies& c = *g_cp;
    std::destroy_at(&c.map);
    c.arena.release();
    std::construct_at(&c.map, &c.arena);
    (*g_generation)++, (*g_upload_epoch)++;
    g_persistent.epoch.fetch_add(1, std::memory_order_relaxed);
}

void dedup_invalidate(const uint8_t* p, uint64_t size) {
    (*g_upload_epoch)++;
    g_persistent.invalidate(p, size);
    // Entries starting before p may still reach into it: look back over the few that can.
    auto it = (*g_cp).map.lower_bound(p + size);
    while (it != (*g_cp).map.begin()) {
        --it;
        if (it->first + it->second.size <= p) {
            if (p - it->first > (64ll << 20)) break;  // nothing larger than 64 MB is copied
            continue;
        }
        if (Front& f = front_of(it->first); f.p == it->first) f.generation = 0;
        it = (*g_cp).map.erase(it);
    }
}

// c[0], the NVN driver's constants, as far as shaders read them (docs/nvn/TRANSLATOR.md): buffer descriptors
// {u64 GPU address; u32 size} of what is bound, which compute shaders search to find the buffer an address falls in
// (they wrote garbage with a zero table), and the maximum point size.
std::array<uint8_t, 0x1000> driver_constants(ShaderStage stage, int st) {
    std::array<uint8_t, 0x1000> c0{};
    auto put = [&](uint32_t at, const Range& r) {
        if (at + 12 > c0.size()) return;
        std::memcpy(c0.data() + at, &r.address, 8);
        uint32_t size = uint32_t(std::min<uint64_t>(r.size, 0xFFFFFFFFu));
        std::memcpy(c0.data() + at + 8, &size, 4);
    };
    if (stage == ShaderStage::Compute) {
        for (int n = 0; n < 16; n++) put(0x210 + 0x10 * n, (*g_state).uniforms[st][n]);
        for (int n = 0; n < 16; n++) put(0x310 + 0x10 * n, (*g_state).storage[st][n]);
    } else {
        int hw = stage == ShaderStage::Fragment ? 4 : 0;  // Maxwell stage numbers: vertex 0, fragment 4
        for (int n = 0; n < 16; n++) put(0x110 + 0x100 * hw + 0x10 * n, (*g_state).storage[st][n]);
        float max_point = 64.0f;
        std::memcpy(c0.data() + 0x610, &max_point, 4);
    }
    return c0;
}

void dedup_log() {
    const uint64_t psaved = g_persistent.saved_bytes.exchange(0, std::memory_order_relaxed);
    const uint64_t ppatched = g_persistent.patched_bytes.exchange(0, std::memory_order_relaxed);
    if ((*g_copied) + (*g_saved) + psaved + ppatched)
        log("nvn: uploads copied %.1f MB (%.1f MB of data, %llu copies, %llu padded to 64 KB), reused %.1f MB, "
            "persistent saved %.1f MB (patched %.1f MB)",
            double((*g_copied)) / 1e6, double((*g_data)) / 1e6, (unsigned long long)(*g_copies_n), (unsigned long long)(*g_big_pads),
            double((*g_saved)) / 1e6, double(psaved) / 1e6, double(ppatched) / 1e6);
    (*g_copied) = (*g_saved) = (*g_data) = (*g_copies_n) = (*g_big_pads) = 0;
}

}  // namespace nx::nvn::replay
