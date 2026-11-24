// See touch_input.h.
#include "touch_input.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <mutex>
#include <vector>

#include <nx/game.h>
#include <nx/runtime.h>

#include "config.h"
#include "kernel.h"
#include "touch_layout.h"

namespace nx::plat {

namespace {

struct Finger {
    int64_t id;
    float x0, y0, x, y;  // in screen heights of the safe area, from its left and top edges
    bool left;           // the stick's
    uint64_t button;
    bool run = false;      // the stick: holding the run button
    uint64_t past_ns = 0;  // the stick: when it went past the run distance, 0 while inside it
};

std::mutex g_lock;
std::vector<Finger> g_fingers;
float g_w = 1, g_h = 1;
touch::Insets g_insets;
int g_controllers = 0;

touch::Region region() { return touch::safe_region(g_w, g_h, g_insets); }

// The button the stick holds to run: Wonder's Y (dash), ACNH's B (as the Android host).
uint64_t run_button() {
    static const uint64_t run = std::strcmp(nx::game::kInfo.id, "acnh") == 0 ? kB : kY;
    return run;
}

bool has_layout(float aspect) {
    size_t n = 0;
    touch_buttons(n, aspect);
    return n > 0;
}

bool visible_locked() { return touch::controls_visible(touch_enabled(), g_controllers); }

}  // namespace

void touch_input_screen(float width, float height, touch::Insets insets) {
    std::lock_guard lk(g_lock);
    if (width > 0 && height > 0) g_w = width, g_h = height;
    g_insets = insets;
}

void touch_input_set_controllers(int connected) {
    std::lock_guard lk(g_lock);
    if (connected > 0 && g_controllers <= 0) g_fingers.clear();  // a controller joins: nothing stays held
    if ((connected > 0) != (g_controllers > 0))
        nx::log("input: %s: touch controls %s", connected > 0 ? "a controller is connected" : "no controller connected",
                connected > 0 ? "hidden" : "shown");
    g_controllers = connected;
}

bool touch_input_active() {
    std::lock_guard lk(g_lock);
    return visible_locked() && has_layout(region().aspect());
}

bool touch_input_down(int64_t id, float fx, float fy) {
    std::lock_guard lk(g_lock);
    if (!visible_locked()) {
        g_fingers.clear();
        return false;
    }
    const touch::Region r = region();
    const float aspect = r.aspect();
    if (!has_layout(aspect)) return false;
    float lx, ly;
    touch::window_to_layout(r, g_w, g_h, fx, fy, lx, ly);
    std::erase_if(g_fingers, [&](const Finger& f) { return f.id == id; });  // a lost end: this id starts again
    const uint64_t b = touch_hit(lx, ly, aspect);
    const bool left = !b && (touch_mirrored() ? lx > 0.5f : lx < 0.5f);
    if (!b && !left) return false;
    g_fingers.push_back({id, lx * aspect, ly, lx * aspect, ly, left, b});
    return true;
}

void touch_input_move(int64_t id, float fx, float fy) {
    std::lock_guard lk(g_lock);
    const touch::Region r = region();
    float lx, ly;
    touch::window_to_layout(r, g_w, g_h, fx, fy, lx, ly);
    for (Finger& f : g_fingers)
        if (f.id == id) f.x = lx * r.aspect(), f.y = ly;
}

void touch_input_up(int64_t id) {
    std::lock_guard lk(g_lock);
    std::erase_if(g_fingers, [&](const Finger& f) { return f.id == id; });
}

void touch_input_clear() {
    std::lock_guard lk(g_lock);
    g_fingers.clear();
}

void touch_input_pad(PadState& p, uint64_t now_ns) {
    std::lock_guard lk(g_lock);
    if (g_fingers.empty()) return;
    if (!visible_locked()) {
        g_fingers.clear();
        return;
    }
    // The stick pushed past its ring also holds the run button; the Android host explains why only past the ring, after a
    // moment, and back inside it to stop (platform_android_input.cpp read_pad).
    constexpr float kRing = 0.12f, kRunOn = 1.3f, kRunOff = 1.1f;
    constexpr uint64_t kRunDelayNs = 250'000'000;
    const bool can_run = config_get_bool("input.touch_run", true) && !(keyboard_state_known() && keyboard_typing());
    for (Finger& f : g_fingers) {
        if (f.left) {
            const float dx = (f.x - f.x0) / kRing, dy = (f.y - f.y0) / kRing;
            p.lx = int32_t(std::clamp(dx, -1.f, 1.f) * kStickMax);
            p.ly = int32_t(std::clamp(-dy, -1.f, 1.f) * kStickMax);
            const float d = std::hypot(dx, dy);
            if (!can_run || (f.run ? d < kRunOff : d <= kRunOn)) {
                f.run = false;
                f.past_ns = 0;
            } else if (!f.run) {
                if (!f.past_ns) f.past_ns = now_ns;
                f.run = now_ns - f.past_ns >= kRunDelayNs;
            }
            if (f.run) p.buttons |= run_button();
        } else {
            p.buttons |= f.button;
        }
    }
}

size_t touch_input_overlay(OverlayShape* out, size_t max) {
    std::lock_guard lk(g_lock);
    if (!visible_locked()) return 0;
    const touch::Region reg = region();
    const float aspect = reg.aspect();
    size_t n = 0, k = 0;
    const TouchButton* b = touch_buttons(n, aspect);
    if (!n) return 0;
    const float shade = touch_opacity_scale();
    const Finger* stick = nullptr;
    uint64_t held = 0;
    for (const Finger& f : g_fingers) {
        if (f.left) stick = &f;
        held |= f.button | (f.run ? run_button() : 0);
    }
    // A button's letters in the glyphs present.frag draws: "ABXYRLZ+-"; nothing for the d-pad and gear.
    auto label_code = [](const char* t) {
        static const char kGlyphs[] = "ABXYRLZ+-";
        uint32_t code = 0;
        for (int c = 0; c < 2 && t[c]; c++) {
            const char* at = std::strchr(kGlyphs, t[c]);
            if (!at) return 0u;
            code |= uint32_t(at - kGlyphs + 1) << (8 * c);
        }
        return code;
    };
    auto put = [&](float lx, float ly, float lr, float alpha, uint32_t label) {
        OverlayShape s{};
        touch::layout_to_window(reg, g_w, g_h, lx, ly, lr, s.x, s.y, s.r);
        s.alpha = alpha, s.label = label;
        out[k++] = s;
    };
    for (size_t i = 0; i < n && k < max; i++)
        put(b[i].x, b[i].y, b[i].r, std::min(1.0f, (held & b[i].button ? 0.5f : 0.2f) * shade), label_code(touch_label(i)));
    if (stick && k + 2 <= max) {
        const float r = 0.12f;
        float dx = stick->x - stick->x0, dy = stick->y - stick->y0;
        const float len = std::hypot(dx, dy), s = len > r ? r / len : 1.f;
        put(stick->x0 / aspect, stick->y0, r, std::min(1.0f, (stick->run ? 0.3f : 0.14f) * shade), 0);
        put((stick->x0 + dx * s) / aspect, stick->y0 + dy * s, 0.055f, std::min(1.0f, 0.45f * shade), 0);
    } else if (k < max) {
        float sx, sy, sr;
        touch_stick_rest(aspect, touch_mirrored(), sx, sy, sr);
        put(sx, sy, sr, std::min(1.0f, 0.08f * shade), 0);
    }
    return k;
}

}  // namespace nx::plat
