// Is a saved pipeline-cache blob safe to hand to vkCreatePipelineCache? Checks the VkPipelineCacheHeaderVersionOne
// layout from the Vulkan spec (headerSize, headerVersion, vendorID, deviceID, pipelineCacheUUID) and a size cap.
// Mali and Adreno drivers do not all survive a truncated or stale blob. No Vulkan header: the layout is read by hand.
// Header validation idea from Timqt/d2r-android nvn_vulkan_pipeline_cache.inc (MIT); written fresh here.
#pragma once
#include <cstddef>
#include <cstdint>
#include <cstring>

namespace nx::nvn {

constexpr size_t kPipelineCacheMaxBytes = size_t(64) << 20;

inline bool pipeline_cache_blob_valid(const uint8_t* data, size_t n, uint32_t vendor, uint32_t device,
                                      const uint8_t uuid[16]) {
    if (!data || n < 32 || n > kPipelineCacheMaxBytes) return false;
    uint32_t header_size, version, v, d;
    std::memcpy(&header_size, data, 4);
    std::memcpy(&version, data + 4, 4);
    std::memcpy(&v, data + 8, 4);
    std::memcpy(&d, data + 12, 4);
    if (header_size < 32 || header_size > n || version != 1) return false;  // VK_PIPELINE_CACHE_HEADER_VERSION_ONE
    return v == vendor && d == device && std::memcmp(data + 16, uuid, 16) == 0;
}

}  // namespace nx::nvn
