// The safe-area mapping and the hide rule of the iPhone and iPad touch controls (runtime/src/touch_safe.h).
#include <cassert>
#include <cmath>

#include "touch_safe.h"

using namespace nx::plat::touch;

static bool near(float a, float b) { return std::fabs(a - b) < 1e-4f; }

int main() {
    // A 844 x 390 point phone in landscape: the notch on the left (47), the home indicator (21) at the bottom.
    const float W = 844, H = 390;
    Region r = safe_region(W, H, {47, 0, 47, 21});
    assert(near(r.x, 47) && near(r.y, 0) && near(r.w, 750) && near(r.h, 369));
    assert(near(r.aspect(), 750.0f / 369.0f));

    // A finger at the window's corners lands outside 0..1 of the layout; at the safe area's corners it is on 0 and 1.
    float lx, ly;
    window_to_layout(r, W, H, 0.0f, 0.0f, lx, ly);
    assert(lx < 0 && near(ly, 0));
    window_to_layout(r, W, H, 47.0f / W, 369.0f / H, lx, ly);
    assert(near(lx, 0) && near(ly, 1));
    window_to_layout(r, W, H, (47.0f + 750.0f) / W, 0.0f, lx, ly);
    assert(near(lx, 1));

    // A circle of the layout maps back into the window; the radius stays a fraction of the window's height.
    float wx, wy, wr;
    layout_to_window(r, W, H, 0.5f, 0.5f, 0.1f, wx, wy, wr);
    assert(near(wx, (47 + 375) / W) && near(wy, 184.5f / H) && near(wr, 36.9f / H));
    // ... and the finger-to-layout mapping inverts it.
    window_to_layout(r, W, H, wx, wy, lx, ly);
    assert(near(lx, 0.5f) && near(ly, 0.5f));

    // No insets (the Mac, an iPad without a notch): the layout is the whole window.
    r = safe_region(1000, 500, {});
    assert(near(r.x, 0) && near(r.w, 1000) && near(r.aspect(), 2.0f));
    layout_to_window(r, 1000, 500, 0.25f, 0.75f, 0.1f, wx, wy, wr);
    assert(near(wx, 0.25f) && near(wy, 0.75f) && near(wr, 0.1f));

    // Absurd insets (more than the window) and negative ones fall back to the whole window.
    r = safe_region(800, 400, {500, 0, 500, 0});
    assert(near(r.x, 0) && near(r.w, 800) && near(r.h, 400));
    r = safe_region(800, 400, {-30, -5, -1, -9});
    assert(near(r.x, 0) && near(r.y, 0) && near(r.w, 800) && near(r.h, 400));
    r = safe_region(0, 0, {});
    assert(r.w > 0 && r.h > 0);

    // The hide rule (the behaviour around it, hot plug included, is tests/touch_input_test.cpp): shown with the setting
    // on and no controller; hidden while any controller is connected; never shown with the setting off.
    assert(controls_visible(true, 0));
    assert(!controls_visible(true, 1));
    assert(!controls_visible(true, 3));
    assert(!controls_visible(false, 0));
    assert(!controls_visible(false, 2));
    return 0;
}
