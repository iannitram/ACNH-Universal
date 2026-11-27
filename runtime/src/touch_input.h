// The on-screen touch controls of the iPhone and iPad player (platform_sdl_wrapper.cpp, TARGET_OS_IPHONE): fingers in, the pad
// the game reads and the circles the present pass draws out. It is the Android host's touch path
// (platform_android_input.cpp) without Android: the same layout and hit test (Rust input module, via input_wrapper.cpp), the same
// stick on the left half that also holds the run button past its ring, the same shapes (nvn_present.cpp draws them).
// The differences are the safe area (the layout sits inside the notch's and the home indicator's insets) and that no
// controller may be connected (touch_safe.h controls_visible).
//
// Fingers arrive on the main thread; the pad is read there too (sample_input) and the shapes on the present thread,
// so the state is under a lock.
#pragma once

#include <cstddef>
#include <cstdint>

#include "platform.h"
#include "touch_safe.h"

namespace nx::plat {

// The window's size and its safe-area insets, in the same units (points). Before the first finger and on every change.
void touch_input_screen(float width, float height, touch::Insets insets);
// How many game controllers are connected (waiting or joined). Any: the controls hide, fingers down are let go.
void touch_input_set_controllers(int connected);
// The shape (width / height) of the area the controls are laid out in: the safe area, not the window. The layout editor
// (settings.cpp) uses it so what it shows and clamps is what the game draws.
float touch_input_layout_aspect();
// True while the controls are drawn and take fingers: the setting is on, no controller, and the game has a layout.
bool touch_input_active();
// One finger (an id of the touch device), at fx, fy: 0..1 across the window. Returns whether the controls took it.
bool touch_input_down(int64_t id, float fx, float fy);
void touch_input_move(int64_t id, float fx, float fy);
void touch_input_up(int64_t id);
void touch_input_clear();  // every finger up (the settings screen opened, the app left the screen)
// The buttons and stick the fingers hold, added to player 1's pad (the stick sets its axes). now_ns: monotonic_ns().
void touch_input_pad(PadState& p, uint64_t now_ns);
// The drawn controls (brighter while held), in the window's uv; 0 shapes while hidden. Present thread.
size_t touch_input_overlay(OverlayShape* out, size_t max);

}  // namespace nx::plat
