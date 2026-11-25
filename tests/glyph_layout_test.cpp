// glyph_layout (runtime/src/glyph_layout.h): input.layout auto follows the pad in hand; explicit settings stay.
#include <cstdio>
#include <string>

#include "glyph_layout.h"

static int failures = 0;
static void expect(const char* setting, const char* kind, const char* want) {
    std::string got = nx::glyph_layout(setting, kind);
    if (got != want) {
        std::printf("FAIL: layout %s, kind '%s': %s, wanted %s\n", setting, kind, got.c_str(), want);
        failures++;
    }
}

int main() {
    for (const char* kind : {"xbox", "playstation", "generic"}) expect("auto", kind, "xbox");
    for (const char* kind : {"switch", ""}) expect("auto", kind, "console");
    expect("", "xbox", "xbox");  // unknown values count as auto
    expect("", "switch", "console");
    for (const char* kind : {"xbox", "switch", ""}) {
        expect("xbox", kind, "xbox");
        expect("console", kind, "console");
    }
    // Nintendo's vendor id counts even when SDL types the pad "standard" (a GameCube adapter, 057e:0337).
    if (!nx::nintendo_pad_by("standard", 0x057e) || !nx::nintendo_pad_by("switchpro", 0) ||
        !nx::nintendo_pad_by("joyconleft", 0) || nx::nintendo_pad_by("standard", 0x045e) ||
        nx::nintendo_pad_by("ps5", 0x054c)) {
        std::printf("FAIL: nintendo_pad_by\n");
        failures++;
    }
    if (failures == 0) std::printf("glyph_layout: ok\n");
    return failures ? 1 : 0;
}
