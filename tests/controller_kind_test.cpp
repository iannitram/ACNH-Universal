// The Android controller_kind rule (runtime/src/controller_kind.h): Microsoft -> xbox, Sony -> playstation,
// Nintendo -> switch, any other connected pad -> generic, no pad -> "".
//   clang++ -std=c++20 -Iruntime/src tests/controller_kind_test.cpp  (or through CMake -DNX_TESTS=ON, ctest)
#include <cstdio>
#include <string>

#include "controller_kind.h"

using nx::plat::android_controller_kind;

namespace {

int g_failed = 0;

void expect(const std::string& got, const std::string& want, int line) {
    if (got != want) {
        std::printf("FAIL line %d: got \"%s\", expected \"%s\"\n", line, got.c_str(), want.c_str());
        g_failed++;
    }
}

}  // namespace

#define EXPECT(got, want) expect((got), (want), __LINE__)

int main() {
    // Vendor IDs match SDL hosts: Microsoft (0x045E), Sony (0x054C), Nintendo (0x057E).
    EXPECT(android_controller_kind(true, 0x045E), "xbox");
    EXPECT(android_controller_kind(true, 0x054C), "playstation");
    EXPECT(android_controller_kind(true, 0x057E), "switch");

    // Any other connected pad (including vendor 0 Bluetooth pads) is generic.
    EXPECT(android_controller_kind(true, 0x2DC8, "8BitDo Pro 2"), "generic");
    EXPECT(android_controller_kind(true, 0x1234, "USB Gamepad"), "generic");
    EXPECT(android_controller_kind(true, 0, "Bluetooth Gamepad"), "generic");

    // Name fallback when vendor ID is absent.
    EXPECT(android_controller_kind(true, 0, "Xbox Wireless Controller"), "xbox");
    EXPECT(android_controller_kind(true, 0, "DualSense Wireless Controller"), "playstation");
    EXPECT(android_controller_kind(true, 0, "Pro Controller"), "switch");

    // No connected pad -> "".
    EXPECT(android_controller_kind(false, 0x045E), "");
    EXPECT(android_controller_kind(false, 0x054C), "");
    EXPECT(android_controller_kind(false, 0x057E), "");
    EXPECT(android_controller_kind(false, 0), "");

    std::printf("controller_kind_test: %s\n", g_failed ? "FAILED" : "all passed");
    return g_failed ? 1 : 0;
}
