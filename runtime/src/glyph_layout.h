// The face-button layout the button-icon archive is drawn for (glyphs.cpp), from the input.layout setting and the
// kind of controller in hand (platform.h controller_kind: "xbox", "playstation", "switch", "generic" or "" for none).
#pragma once
#include <string>

namespace nx {

// Whether a pad is Nintendo's own, from SDL's name for its type ("switchpro", "joyconleft", ...) or its USB vendor id
// (0x057e: adapters and licensed pads that SDL types "standard"). The button mapping and the icon layout both use
// this, so the icons always match the buttons.
inline bool nintendo_pad_by(const std::string& type_name, unsigned vendor) {
    return type_name.rfind("switch", 0) == 0 || type_name.rfind("joycon", 0) == 0 || vendor == 0x057e;
}

// "xbox": the game's A on the bottom button; "console": on the right one, as on a Switch. An explicit setting is
// kept; "auto" (the default) follows the host's input.layout auto: xbox when a pad that is not Nintendo's is in
// hand, console for Nintendo's own pads or none.
inline std::string glyph_layout(const std::string& setting, const std::string& kind) {
    if (setting == "xbox") return "xbox";
    if (setting == "console") return "console";
    return kind == "xbox" || kind == "playstation" || kind == "generic" ? "xbox" : "console";
}

}  // namespace nx
