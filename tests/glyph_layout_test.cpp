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
    if (failures == 0) std::printf("glyph_layout: ok\n");
    return failures ? 1 : 0;
}
