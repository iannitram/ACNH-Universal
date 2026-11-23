// pipeline_cache_blob_valid (runtime/src/nvn/nvn_pipeline_cache_blob.h): the VkPipelineCacheHeaderVersionOne check.
// Exit code 0: every case passed.  clang++ -std=c++20 -Iruntime/src/nvn tests/pipeline_cache_blob_test.cpp
#include <cstdio>
#include <cstring>
#include <vector>

#include "nvn_pipeline_cache_blob.h"

using nx::nvn::pipeline_cache_blob_valid;

static int g_failed = 0;
static void check(bool ok, const char* what) {
    if (!ok) std::printf("FAIL: %s\n", what), g_failed++;
}

static const uint8_t kUuid[16] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};

static std::vector<uint8_t> blob(uint32_t size = 32, uint32_t ver = 1, size_t total = 64) {
    std::vector<uint8_t> b(total, 0xAB);
    uint32_t vendor = 0x13b5, dev = 0x70;
    if (total >= 32) {
        std::memcpy(&b[0], &size, 4), std::memcpy(&b[4], &ver, 4);
        std::memcpy(&b[8], &vendor, 4), std::memcpy(&b[12], &dev, 4);
        std::memcpy(&b[16], kUuid, 16);
    }
    return b;
}

static bool valid(const std::vector<uint8_t>& b, uint32_t vendor = 0x13b5, uint32_t dev = 0x70,
                  const uint8_t* uuid = kUuid) {
    return pipeline_cache_blob_valid(b.data(), b.size(), vendor, dev, uuid);
}

int main() {
    check(valid(blob()), "valid header plus payload");
    check(valid(blob(32, 1, 32)), "valid, header only");
    check(!valid(blob(32, 1, 31)), "too short");
    check(!pipeline_cache_blob_valid(nullptr, 0, 0x13b5, 0x70, kUuid), "empty");
    check(!valid(blob(65)), "headerSize larger than the blob");
    check(!valid(blob(16)), "headerSize below 32");
    check(!valid(blob(32, 2)), "wrong version");
    check(!valid(blob(), 0x5143), "wrong vendor");
    check(!valid(blob(), 0x13b5, 0x71), "wrong device");
    uint8_t other[16];
    std::memcpy(other, kUuid, 16), other[15] ^= 1;
    check(!valid(blob(), 0x13b5, 0x70, other), "wrong UUID");
    check(valid(blob(32, 1, nx::nvn::kPipelineCacheMaxBytes)), "exactly at the cap");
    check(!valid(blob(32, 1, nx::nvn::kPipelineCacheMaxBytes + 1)), "over the cap");
    if (!g_failed) std::printf("pipeline_cache_blob_test: all passed\n");
    return g_failed ? 1 : 0;
}
