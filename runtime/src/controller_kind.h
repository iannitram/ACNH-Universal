#pragma once

#include <cctype>
#include <string>

namespace nx::plat {

// The kind by the maker's USB vendor id, else the name: "switch" (Nintendo's own pads), "xbox", "playstation", or
// "generic" (a Razer Kishi, an 8BitDo in X-input mode, ...). The settings screen's glyphs and the automatic confirm
// button (input.layout auto) follow it.
inline std::string pad_kind(int vendor, const std::string& name = "") {
    std::string n;
    for (char ch : name) n += char(std::tolower(static_cast<unsigned char>(ch)));
    auto has = [&](const char* s) { return n.find(s) != std::string::npos; };
    if (vendor == 0x057e || has("nintendo") || has("pro controller") || has("joy-con")) return "switch";
    if (vendor == 0x045e || has("xbox")) return "xbox";
    if (vendor == 0x054c || has("dualsense") || has("dualshock") || has("playstation")) return "playstation";
    return "generic";
}

// Android controller_kind() rule: "" when no pad is connected, else pad_kind(vendor, name).
inline std::string android_controller_kind(bool connected, int vendor, const std::string& name = "") {
    return connected ? pad_kind(vendor, name) : "";
}

}  // namespace nx::plat
