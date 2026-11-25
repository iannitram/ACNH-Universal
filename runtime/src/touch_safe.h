// Pure helpers of the on-screen touch controls on iPhone and iPad (touch_input.cpp), kept free of SDL and the
// runtime so tests/touch_safe_test.cpp checks them on the Mac.
//
// The layout (touch_layout.cpp) is drawn for a whole screen; on a phone with a notch or a home indicator it is laid
// out in the safe area instead, and a touch or a drawn circle is mapped between the window and that area.
#pragma once

#include <algorithm>

namespace nx::plat::touch {

// The window's unsafe edges (the notch, the rounded corners, the home indicator), in the window's own units.
struct Insets {
    float left = 0, top = 0, right = 0, bottom = 0;
};

// The safe area as a rectangle of the window, in the same units.
struct Region {
    float x = 0, y = 0, w = 1, h = 1;
    float aspect() const { return h > 0 ? w / h : 16.0f / 9.0f; }
};

// The window (W x H) less its insets. Negative insets count as none; insets that leave less than a quarter of an
// edge (a bad report) are ignored, so the controls never end up squeezed into a sliver.
inline Region safe_region(float W, float H, Insets in) {
    in.left = std::max(in.left, 0.0f), in.top = std::max(in.top, 0.0f);
    in.right = std::max(in.right, 0.0f), in.bottom = std::max(in.bottom, 0.0f);
    Region r{in.left, in.top, W - in.left - in.right, H - in.top - in.bottom};
    if (!(W > 0 && H > 0) || r.w < W * 0.25f || r.h < H * 0.25f) return {0, 0, W > 0 ? W : 1, H > 0 ? H : 1};
    return r;
}

// A point of the window (fx, fy: 0..1 across it, as SDL reports a finger) to the layout's coordinates: 0..1 across
// the safe area (below 0 or above 1 where the finger is in an inset).
inline void window_to_layout(Region r, float W, float H, float fx, float fy, float& lx, float& ly) {
    lx = r.w > 0 ? (fx * W - r.x) / r.w : fx;
    ly = r.h > 0 ? (fy * H - r.y) / r.h : fy;
}

// A circle of the layout (x, y: 0..1 across the safe area, radius: a fraction of its height) to the whole window,
// which is what the present pass draws over (x, y: 0..1 across the window, radius: a fraction of its height).
inline void layout_to_window(Region r, float W, float H, float lx, float ly, float lr, float& wx, float& wy, float& wr) {
    wx = W > 0 ? (r.x + lx * r.w) / W : lx;
    wy = H > 0 ? (r.y + ly * r.h) / H : ly;
    wr = H > 0 ? lr * r.h / H : lr;
}

// The controls are drawn and answer touches only while no game controller is connected (one waiting to join counts,
// as does one that has joined): the touch controls hide the moment a pad is plugged in and come back when the last one
// leaves. `setting_on` is Settings > Controls > On-screen controls (touch_enabled()).
inline bool controls_visible(bool setting_on, int controllers_connected) {
    return setting_on && controllers_connected <= 0;
}

}  // namespace nx::plat::touch
