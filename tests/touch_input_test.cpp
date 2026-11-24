// The iPhone and iPad touch controls' behaviour (runtime/src/touch_input.cpp on the ACNH layout of touch_layout.cpp):
// a finger on a button holds it, the left half's stick and its run delay and hysteresis, the controls going away while
// a controller is connected, and the shapes the present pass draws. Config, log and the game's info are stubbed; no SDL.
//   clang++ -std=c++20 -Iruntime/src -Iruntime/include tests/touch_input_test.cpp runtime/src/touch_input.cpp
//           runtime/src/touch_layout.cpp  (or through CMake -DNX_TESTS=ON, ctest)
#include <cassert>
#include <cmath>
#include <cstdarg>
#include <cstdio>
#include <map>
#include <string>

#include <nx/game.h>
#include <nx/runtime.h>

#include "config.h"
#include "platform.h"
#include "touch_input.h"
#include "touch_layout.h"

// ---- stubs ----
static std::map<std::string, std::string> g_cfg;
namespace nx {
std::string config_get(const std::string& k, const std::string& fb) {
    auto it = g_cfg.find(k);
    return it == g_cfg.end() ? fb : it->second;
}
int config_get_int(const std::string& k, int fb) {
    auto it = g_cfg.find(k);
    return it == g_cfg.end() ? fb : std::atoi(it->second.c_str());
}
bool config_get_bool(const std::string& k, bool fb) {
    auto it = g_cfg.find(k);
    return it == g_cfg.end() ? fb : it->second != "0" && it->second != "false";
}
void config_set(const std::string& k, const std::string& v) { g_cfg[k] = v; }
void log(const char*, ...) {}
namespace game {
const Info kInfo = {"acnh", "ACNH", 0, "", "", 0};
}
}  // namespace nx
namespace nx::plat {
bool keyboard_typing() { return false; }
bool keyboard_state_known() { return false; }
}  // namespace nx::plat

using namespace nx::plat;

static bool near(float a, float b) { return std::fabs(a - b) < 1e-3f; }
static PadState pad(uint64_t now_ns) {
    PadState p;
    touch_input_pad(p, now_ns);
    return p;
}

int main() {
    // A 844 x 390 point phone in landscape, notch left and right (47), home indicator (21).
    const float W = 844, H = 390;
    const touch::Insets ins{47, 0, 47, 21};
    touch_input_screen(W, H, ins);
    const touch::Region reg = touch::safe_region(W, H, ins);
    const float aspect = reg.aspect();
    assert(near(touch_input_layout_aspect(), aspect));
    assert(touch_input_active());

    size_t n = 0;
    const TouchButton* b = touch_buttons(n, aspect);
    assert(n > 0);
    // Window-fraction position of a layout point.
    auto win = [&](float lx, float ly, float& fx, float& fy) {
        float r;
        touch::layout_to_window(reg, W, H, lx, ly, 0.0f, fx, fy, r);
    };
    size_t A = n;
    for (size_t i = 0; i < n; i++)
        if (b[i].button == kA) A = i;
    assert(A < n);

    // A finger on the A button holds it, and lets go.
    float fx, fy;
    win(b[A].x, b[A].y, fx, fy);
    assert(touch_input_down(1, fx, fy));
    assert(pad(0).buttons & kA);
    touch_input_up(1);
    assert(!(pad(0).buttons & kA));

    // Held buttons are released when a controller connects, and stay released after it leaves.
    assert(touch_input_down(1, fx, fy));
    assert(pad(0).buttons & kA);
    touch_input_set_controllers(1);
    assert(!touch_input_active());
    assert(!(pad(0).buttons & kA));
    touch_input_set_controllers(0);
    assert(touch_input_active());
    assert(!(pad(0).buttons & kA));

    // With a controller: no finger is taken, and no shape is drawn.
    OverlayShape shapes[64];
    assert(touch_input_overlay(shapes, 64) > 0);
    touch_input_set_controllers(2);
    assert(!touch_input_down(2, fx, fy));
    assert(touch_input_overlay(shapes, 64) == 0);
    touch_input_set_controllers(0);

    // The setting off: hidden and not taking fingers.
    g_cfg["input.touch_controls"] = "0";
    assert(!touch_input_active() && !touch_input_down(3, fx, fy) && touch_input_overlay(shapes, 64) == 0);
    g_cfg.erase("input.touch_controls");

    // A finger in the safe area's left half, off the buttons, is the stick; the window's left strip (the notch) maps
    // outside the layout and the safe area's own left edge is its 0.
    float sx, sy;
    win(0.15f, 0.5f, sx, sy);
    assert(!touch_hit(0.15f, 0.5f, aspect));
    assert(touch_input_down(4, sx, sy));
    PadState p = pad(0);
    assert(p.lx == 0 && p.ly == 0);
    // Right by half the ring (0.06 screen heights): half deflection, no run.
    const float ring = 0.12f;
    auto move_to = [&](float dx_heights, float dy_heights) {
        float mx, my;
        win(0.15f + dx_heights / aspect, 0.5f + dy_heights, mx, my);
        touch_input_move(4, mx, my);
    };
    move_to(0.06f, 0);
    p = pad(0);
    assert(near(float(p.lx) / kStickMax, 0.5f) && p.ly == 0);
    // Up on the screen is positive ly.
    move_to(0, -0.06f);
    p = pad(0);
    assert(near(float(p.ly) / kStickMax, 0.5f));
    // Past the ring clamps the axis.
    move_to(0.3f, 0);
    p = pad(0);
    assert(p.lx == kStickMax);

    // Run (ACNH's B): not before 250 ms past 1.3 rings, after it yes; held until below 1.1 rings, then off.
    const uint64_t ms = 1'000'000;
    move_to(ring * 1.5f, 0);
    assert(!(pad(1000 * ms).buttons & kB));
    assert(!(pad(1200 * ms).buttons & kB));
    assert(pad(1250 * ms).buttons & kB);
    move_to(ring * 1.2f, 0);  // between the two limits: still running
    assert(pad(1300 * ms).buttons & kB);
    move_to(ring * 1.05f, 0);  // below the lower limit: stopped
    assert(!(pad(1350 * ms).buttons & kB));
    move_to(ring * 1.2f, 0);  // and the delay starts again from the upper limit, not here
    assert(!(pad(1400 * ms).buttons & kB));
    assert(!(pad(1800 * ms).buttons & kB));  // however long it stays between the limits
    move_to(ring * 1.5f, 0);
    assert(!(pad(1900 * ms).buttons & kB));
    assert(pad(2160 * ms).buttons & kB);
    // The run setting off: no run.
    g_cfg["input.touch_run"] = "0";
    assert(!(pad(2300 * ms).buttons & kB));
    g_cfg.erase("input.touch_run");

    // The overlay: the layout's buttons plus the stick's ring and thumb while a finger is down, inside the window.
    size_t k = touch_input_overlay(shapes, 64);
    assert(k == n + 2);
    for (size_t i = 0; i < k; i++) assert(shapes[i].x >= 47 / W - 1e-4f && shapes[i].x <= (W - 47) / W + 1e-4f);
    // A shape of the first button is where the window mapping puts it.
    float ex, ey;
    win(b[0].x, b[0].y, ex, ey);
    assert(near(shapes[0].x, ex) && near(shapes[0].y, ey));
    touch_input_up(4);
    k = touch_input_overlay(shapes, 64);
    assert(k == n + 1);  // the stick's rest hint

    // A controller joining lets go of a stick finger too.
    assert(touch_input_down(5, sx, sy));
    touch_input_set_controllers(1);
    touch_input_set_controllers(0);
    assert(pad(0).lx == 0);
    touch_input_move(5, sx + 0.1f, sy);
    assert(pad(0).lx == 0);

    // A finger down again in the right half but off the buttons is not taken (the stick is the left half's).
    float rx, ry;
    win(0.5f, 0.02f, rx, ry);
    if (!touch_hit(0.5f, 0.02f, aspect)) assert(!touch_input_down(6, rx, ry));
    touch_input_clear();
    std::puts("touch_input_test: ok");
    return 0;
}
