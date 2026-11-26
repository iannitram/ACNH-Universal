#pragma once

#include <cctype>
#include <string>

namespace nx::plat {

// The kind by the maker's USB vendor id (and, for 8BitDo, the product id), else the name: "switch" (Nintendo's own
// pads, 8BitDo in Switch mode), "xbox", "playstation", or "generic" (a Razer Kishi, ...). The settings screen's
// glyphs and the automatic confirm button (input.layout auto) follow it. The launcher (app/src/input.rs) and the
// Rust twin (crates/runtime/src/platform.rs) use the same rule; all three are held to tests/data/pad_kinds.tsv.
inline std::string pad_kind(int vendor, int product, const std::string& name = "") {
    std::string n;
    for (char ch : name) n += char(std::tolower(static_cast<unsigned char>(ch)));
    auto has = [&](const char* s) { return n.find(s) != std::string::npos; };
    if (vendor == 0x057e) return "switch";
    if (vendor == 0x045e) return "xbox";
    if (vendor == 0x054c) return "playstation";
    if (vendor == 0x17ef && has("legion")) return "xbox";
    if (vendor == 0x2dc8) {
        switch (product) {
            case 0x6001: case 0x6002: case 0x6006: case 0x6007:
            case 0x6101: case 0x6102: case 0x9006: case 0x9012: return "switch";
            case 0x2000: return "xbox";
            default: if (has("switch")) return "switch";
        }
    }
    if (has("nintendo") || has("pro controller") || has("joy-con")) return "switch";
    if (has("xbox")) return "xbox";
    if (has("dualsense") || has("dualshock") || has("playstation")) return "playstation";
    return "generic";
}

// Android controller_kind() rule: "" when no pad is connected, else pad_kind(vendor, product, name).
inline std::string android_controller_kind(bool connected, int vendor, int product, const std::string& name = "") {
    return connected ? pad_kind(vendor, product, name) : "";
}

}  // namespace nx::plat
