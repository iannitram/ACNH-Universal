// Thin C++ wrapper forwarding nn::hid HLE, input_script, and touch_layout
// to the Rust implementation in crates/runtime/src/input.rs.
#include "platform.h"
#include "touch_layout.h"

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <string>

#include <nx/game.h>
#include <nx/runtime.h>

#ifdef NX_RUST_RUNTIME
#include <acnh_runtime.h>
#endif

#ifndef NX_RUST_RUNTIME
extern "C" {
struct AcnhPadState {
    uint64_t buttons;
    int32_t lx;
    int32_t ly;
    int32_t rx;
    int32_t ry;
};
struct AcnhTouchButton {
    float x;
    float y;
    float r;
    uint64_t button;
};
struct AcnhOverlayShape {
    float x;
    float y;
    float r;
    float alpha;
    uint32_t label;
};
struct AcnhInputHost {
    uint64_t (*frame)();
    size_t (*data_root)(char*, size_t);
    void (*log)(const char*);
    size_t (*describe)(uint64_t, char*, size_t);
    size_t (*hle_current_name)(char*, size_t);
    bool (*live_files)();
    void (*read_pad)(AcnhPadState*);
    bool (*read_player_pad)(int32_t, AcnhPadState*);
    void (*rumble)(float, float, uint32_t);
    uint64_t (*monotonic_ns)();
    size_t (*game_id)(char*, size_t);
    uint32_t (*touch_y_bias)();
};
void acnh_input_set_host(const AcnhInputHost* host);
void acnh_input_get_npad_state(uint8_t* out, uint32_t id);
uint64_t acnh_input_get_npad_states_full_key(uint8_t* out, int32_t n, uint32_t id, uint64_t lr);
uint64_t acnh_input_get_npad_states_other(uint8_t* out, int32_t n, uint32_t id);
uint64_t acnh_input_get_mouse_states(uint8_t* out, int32_t n);
void acnh_input_get_debug_pad_state(uint8_t* out);
void acnh_input_get_keyboard_state(uint8_t* out);
void acnh_input_note_keycode();
uint64_t acnh_input_get_npad_style_set(uint32_t id);
uint64_t acnh_input_get_vibration_device_handles(uint8_t* out, int32_t count, uint32_t id);
void acnh_input_get_vibration_device_info(uint8_t* out);
void acnh_input_send_vibration_value(float amp_low, float amp_high);
void acnh_input_show_controller_support(uint8_t* out);
void acnh_input_get_touch_screen_state1(uint8_t* out, uint64_t lr);
uint64_t acnh_input_get_touch_screen_states16(uint8_t* out, int32_t count, uint64_t lr);
void acnh_input_get_npad_controller_color(uint8_t* p0, uint8_t* p1);
bool acnh_input_mouse_take_hover(uint32_t* out_x, uint32_t* out_y);
bool acnh_input_touch_take_tap(uint32_t* out_x, uint32_t* out_y, uint64_t* out_frame);
void acnh_input_hid_hold_buttons(uint64_t buttons, uint32_t frames);
void acnh_input_set_keyboard(uint64_t modifiers, const uint8_t* keys);
bool acnh_input_keyboard_typing();
void acnh_input_keyboard_text_entry(bool open);
bool acnh_input_keyboard_state_known();
void acnh_input_set_mouse_touch(bool down, float u, float v);
void acnh_input_set_picture_box(float x, float y, float w, float h);
void acnh_input_script(AcnhPadState* p);
bool acnh_input_touch_enabled();
bool acnh_input_touch_mirrored();
float acnh_input_touch_opacity_scale();
const AcnhTouchButton* acnh_input_touch_buttons(size_t* out_count, float aspect);
void acnh_input_touch_stick_rest(float aspect, bool mirrored, float* out_x, float* out_y, float* out_r);
const char* acnh_input_touch_group(size_t index);
const char* acnh_input_touch_label(size_t index);
void acnh_input_touch_layout_move(const char* group, float dx, float dy, float aspect);
uint64_t acnh_input_touch_hit(float x, float y, float aspect);
size_t acnh_input_touch_overlay(AcnhOverlayShape* out, size_t max);
}
#endif

static_assert(sizeof(nx::plat::PadState) == sizeof(AcnhPadState) && alignof(nx::plat::PadState) == alignof(AcnhPadState));
static_assert(sizeof(nx::plat::TouchButton) == sizeof(AcnhTouchButton) &&
              alignof(nx::plat::TouchButton) == alignof(AcnhTouchButton));
static_assert(sizeof(nx::plat::OverlayShape) == sizeof(AcnhOverlayShape) &&
              alignof(nx::plat::OverlayShape) == alignof(AcnhOverlayShape));

namespace nx {

namespace {

size_t copy_str(const std::string& s, char* buf, size_t cap) {
    if (buf && cap > 0) {
        size_t n = std::min(s.size(), cap - 1);
        if (n > 0) std::memcpy(buf, s.data(), n);
        buf[n] = '\0';
    }
    return s.size();
}

uint64_t host_frame() { return g_frame.load(); }
size_t host_data_root(char* out, size_t cap) { return copy_str(plat::data_root(), out, cap); }

void host_log(const char* msg) {
    if (msg) log("%s", msg);
}

size_t host_describe(uint64_t addr, char* out, size_t cap) { return copy_str(describe(addr), out, cap); }

size_t host_hle_current_name(char* out, size_t cap) {
    const char* name = hle_current_name();
    return copy_str(name ? std::string(name) : std::string(), out, cap);
}

bool host_live_files() { return plat::live_files(); }

void host_read_pad(AcnhPadState* out) {
    if (!out) return;
    plat::PadState p;
    std::memcpy(&p, out, sizeof p);
    plat::read_pad(p);
    std::memcpy(out, &p, sizeof p);
}

bool host_read_player_pad(int32_t index, AcnhPadState* out) {
    if (!out) return false;
    plat::PadState p;
    std::memcpy(&p, out, sizeof p);
    bool ok = plat::read_player_pad(index, p);
    std::memcpy(out, &p, sizeof p);
    return ok;
}

void host_rumble(float low_freq, float high_freq, uint32_t duration_ms) {
    plat::rumble(low_freq, high_freq, duration_ms);
}

uint64_t host_monotonic_ns() { return plat::monotonic_ns(); }
size_t host_game_id(char* out, size_t cap) { return copy_str(game::kInfo.id ? game::kInfo.id : "", out, cap); }
uint32_t host_touch_y_bias() { return game::kInfo.touch_y_bias; }

std::once_flag g_host_once;

void ensure_host() {
    std::call_once(g_host_once, [] {
        AcnhInputHost host{};
        host.frame = host_frame;
        host.data_root = host_data_root;
        host.log = host_log;
        host.describe = host_describe;
        host.hle_current_name = host_hle_current_name;
        host.live_files = host_live_files;
        host.read_pad = host_read_pad;
        host.read_player_pad = host_read_player_pad;
        host.rumble = host_rumble;
        host.monotonic_ns = host_monotonic_ns;
        host.game_id = host_game_id;
        host.touch_y_bias = host_touch_y_bias;
        acnh_input_set_host(&host);
    });
}

const char* const kGetKeyCodeName =
    "_ZN2nn3hid10GetKeyCodeEPtiNS_4util10BitFlagSetILi32ENS0_16KeyboardModifierEEENS2_7BitPackIjNS0_14KeyboardLayoutEEE";

void zero(CPU& c) { c.x[0] = 0; }
void one(CPU& c) { c.x[0] = 1; }
void noop(CPU& c) { c.x[0] = 0; }

void GetNpadState(CPU& c) {
    ensure_host();
    acnh_input_get_npad_state(reinterpret_cast<uint8_t*>(c.x[0]), ld<uint32_t>(c.x[1]));
}

void GetNpadStatesFullKey(CPU& c) {
    ensure_host();
    c.x[0] = acnh_input_get_npad_states_full_key(reinterpret_cast<uint8_t*>(c.x[0]), int32_t(c.x[1]),
                                                  ld<uint32_t>(c.x[2]), c.x[30]);
}

void GetNpadStatesOther(CPU& c) {
    ensure_host();
    c.x[0] = acnh_input_get_npad_states_other(reinterpret_cast<uint8_t*>(c.x[0]), int32_t(c.x[1]),
                                               ld<uint32_t>(c.x[2]));
}

void GetMouseStates(CPU& c) {
    ensure_host();
    c.x[0] = acnh_input_get_mouse_states(reinterpret_cast<uint8_t*>(c.x[0]), int32_t(c.x[1]));
}

void GetDebugPadState(CPU& c) {
    ensure_host();
    acnh_input_get_debug_pad_state(reinterpret_cast<uint8_t*>(c.x[0]));
}

void GetKeyboardState(CPU& c) {
    ensure_host();
    acnh_input_get_keyboard_state(reinterpret_cast<uint8_t*>(c.x[0]));
}

void GetKeyCode(CPU& c) {
    ensure_host();
    acnh_input_note_keycode();
    static const uint64_t real = find_symbol(kGetKeyCodeName);
    call(c, real);
}

void GetNpadStyleSet(CPU& c) {
    ensure_host();
    c.x[0] = acnh_input_get_npad_style_set(ld<uint32_t>(c.x[0]));
}

void BindNpadStyleSetUpdateEvent(CPU& c) {
    uint64_t create = find_symbol("_ZN2nn2os17CreateSystemEventEPNS0_15SystemEventTypeENS0_14EventClearModeEb");
    uint64_t ev = c.x[1], mode = c.x[2];
    uint32_t id = ld<uint32_t>(c.x[0]);
    c.x[0] = ev;
    c.x[1] = mode;
    c.x[2] = 0;
    call(c, create);
    static uint64_t signal = find_symbol("_ZN2nn2os17SignalSystemEventEPNS0_15SystemEventTypeE");
    if (signal) {
        c.x[0] = ev;
        call(c, signal);
    }
    log("hid: style set event bound for id %#x, signalled", id);
}

void GetVibrationDeviceHandles(CPU& c) {
    ensure_host();
    uint32_t id = c.x[2] ? ld<uint32_t>(c.x[2]) : 0;
    c.x[0] = acnh_input_get_vibration_device_handles(reinterpret_cast<uint8_t*>(c.x[0]), int32_t(c.x[1]), id);
}

void GetVibrationDeviceInfo(CPU& c) {
    ensure_host();
    acnh_input_get_vibration_device_info(reinterpret_cast<uint8_t*>(c.x[0]));
    c.x[0] = 0;
}

void SendVibrationValue(CPU& c) {
    ensure_host();
    if (c.x[1]) {
        acnh_input_send_vibration_value(ld<float>(c.x[1]), ld<float>(c.x[1] + 8));
    }
    c.x[0] = 0;
}

void ShowControllerSupport(CPU& c) {
    ensure_host();
    acnh_input_show_controller_support(reinterpret_cast<uint8_t*>(c.x[0]));
    c.x[0] = 0;
}

void GetTouchScreenState1(CPU& c) {
    ensure_host();
    acnh_input_get_touch_screen_state1(reinterpret_cast<uint8_t*>(c.x[0]), c.x[30]);
}

void GetTouchScreenStates16(CPU& c) {
    ensure_host();
    c.x[0] = acnh_input_get_touch_screen_states16(reinterpret_cast<uint8_t*>(c.x[0]), int32_t(c.x[1]), c.x[30]);
}

void GetSupportedNpadStyleSet(CPU& c) { c.x[0] = 0x1F; }

void GetNpadControllerColor(CPU& c) {
    ensure_host();
    acnh_input_get_npad_controller_color(reinterpret_cast<uint8_t*>(c.x[0]), reinterpret_cast<uint8_t*>(c.x[1]));
    c.x[0] = 0;
}

void GetPlayerLedPattern(CPU& c) { c.x[0] = 1; }

}  // namespace

bool mouse_take_hover(uint32_t& x, uint32_t& y) {
    ensure_host();
    return acnh_input_mouse_take_hover(&x, &y);
}

bool touch_take_tap(uint32_t& x, uint32_t& y, uint64_t& frame) {
    ensure_host();
    return acnh_input_touch_take_tap(&x, &y, &frame);
}

void hid_hold_buttons(uint64_t buttons, uint32_t frames) {
    ensure_host();
    acnh_input_hid_hold_buttons(buttons, frames);
}

NX_HLE("_ZN2nn3hid14InitializeNpadEv", noop);
NX_HLE("_ZN2nn3hid24SetSupportedNpadStyleSetENS_4util10BitFlagSetILi32ENS0_12NpadStyleTagEEE", noop);
NX_HLE("_ZN2nn3hid18SetNpadJoyHoldTypeENS0_15NpadJoyHoldTypeE", noop);
NX_HLE("_ZN2nn3hid22SetSupportedNpadIdTypeEPKjm", noop);
NX_HLE("_ZN2nn3hid27BindNpadStyleSetUpdateEventERKjPNS_2os15SystemEventTypeENS3_14EventClearModeE", BindNpadStyleSetUpdateEvent);
NX_HLE("_ZN2nn3hid15GetNpadStyleSetERKj", GetNpadStyleSet);
NX_HLE("_ZN2nn3hid12GetNpadStateEPNS0_16NpadFullKeyStateERKj", GetNpadState);
NX_HLE("_ZN2nn3hid12GetNpadStateEPNS0_16NpadJoyDualStateERKj", GetNpadState);
NX_HLE("_ZN2nn3hid12GetNpadStateEPNS0_17NpadHandheldStateERKj", GetNpadState);
NX_HLE("_ZN2nn3hid25GetVibrationDeviceHandlesEPNS0_21VibrationDeviceHandleEiRKjNS_4util10BitFlagSetILi32ENS0_12NpadStyleTagEEE", GetVibrationDeviceHandles);
NX_HLE("_ZN2nn3hid25InitializeVibrationDeviceERKNS0_21VibrationDeviceHandleE", noop);
NX_HLE("_ZN2nn3hid22GetVibrationDeviceInfoEPNS0_19VibrationDeviceInfoERKNS0_21VibrationDeviceHandleE", GetVibrationDeviceInfo);
NX_HLE("_ZN2nn3hid21ShowControllerSupportEPNS0_27ControllerSupportResultInfoERKNS0_20ControllerSupportArgE", ShowControllerSupport);
NX_HLE("_ZN2nn3hid13GetNpadStatesEPNS0_16NpadFullKeyStateEiRKj", GetNpadStatesFullKey);
NX_HLE("_ZN2nn3hid13GetNpadStatesEPNS0_16NpadJoyDualStateEiRKj", GetNpadStatesOther);
NX_HLE("_ZN2nn3hid13GetNpadStatesEPNS0_16NpadJoyLeftStateEiRKj", GetNpadStatesOther);
NX_HLE("_ZN2nn3hid13GetNpadStatesEPNS0_17NpadHandheldStateEiRKj", GetNpadStatesOther);
NX_HLE("_ZN2nn3hid13GetNpadStatesEPNS0_17NpadJoyRightStateEiRKj", GetNpadStatesOther);
NX_HLE("_ZN2nn3hid15InitializeMouseEv", noop);
NX_HLE("_ZN2nn3hid14GetMouseStatesEPNS0_10MouseStateEi", GetMouseStates);
NX_HLE("_ZN2nn3hid18InitializeKeyboardEv", noop);
NX_HLE("_ZN2nn3hid18InitializeDebugPadEv", noop);
NX_HLE("_ZN2nn3hid16GetDebugPadStateEPNS0_13DebugPadStateE", GetDebugPadState);
NX_HLE("_ZN2nn3hid16GetKeyboardStateEPNS0_13KeyboardStateE", GetKeyboardState);
NX_HLE(kGetKeyCodeName, GetKeyCode);
NX_HLE("_ZN2nn3hid14DisconnectNpadERKj", noop);
NX_HLE("_ZN2nn3hid18SwapNpadAssignmentERKjS2_", noop);
NX_HLE("_ZN2nn3hid30SetNpadJoyAssignmentModeSingleERKj", noop);
NX_HLE("_ZN2nn3hid21StartLrAssignmentModeEv", noop);
NX_HLE("_ZN2nn3hid20StopLrAssignmentModeEv", noop);
NX_HLE("_ZN2nn3hid20GetNpadJoyAssignmentERKj", zero);
NX_HLE("_ZN2nn3hid20IsVibrationPermittedEv", one);
NX_HLE("_ZN2nn3hid21InitializeTouchScreenEv", noop);
NX_HLE("_ZN2nn3hid19GetTouchScreenStateILm1EEEvPNS0_16TouchScreenStateIXT_EEE", GetTouchScreenState1);
NX_HLE("_ZN2nn3hid20GetTouchScreenStatesILm16EEEiPNS0_16TouchScreenStateIXT_EEEi", GetTouchScreenStates16);
NX_HLE("_ZN2nn3hid24GetSupportedNpadStyleSetEv", GetSupportedNpadStyleSet);
NX_HLE("_ZN2nn3hid22GetNpadControllerColorEPNS0_19NpadControllerColorES2_RKj", GetNpadControllerColor);
NX_HLE("_ZN2nn3hid19GetPlayerLedPatternERKj", GetPlayerLedPattern);
NX_HLE("_ZN2nn3hid30SetNpadJoyAssignmentModeSingleERKjNS0_17NpadJoyDeviceTypeE", noop);
NX_HLE("_ZN2nn3hid28SetNpadJoyAssignmentModeDualERKj", noop);
NX_HLE("_ZN2nn3hid23MergeSingleJoyAsDualJoyERKjS2_", noop);
NX_HLE("_ZN2nn3hid18SendVibrationValueERKNS0_21VibrationDeviceHandleERKNS0_14VibrationValueE", SendVibrationValue);
NX_HLE("_ZN2nn3hid23GetSixAxisSensorHandlesEPNS0_19SixAxisSensorHandleEiRKjNS_4util10BitFlagSetILi32ENS0_12NpadStyleTagEEE", noop);
NX_HLE("_ZN2nn3hid18StartSixAxisSensorERKNS0_19SixAxisSensorHandleE", noop);
NX_HLE("_ZN2nn3hid22GetSixAxisSensorStatesEPNS0_18SixAxisSensorStateEiRKNS0_19SixAxisSensorHandleE", zero);

}  // namespace nx

namespace nx::plat {

void set_keyboard(uint64_t modifiers, const uint8_t keys[32]) {
    ensure_host();
    acnh_input_set_keyboard(modifiers, keys);
}

bool keyboard_typing() {
    ensure_host();
    return acnh_input_keyboard_typing();
}

void keyboard_text_entry(bool open) {
    ensure_host();
    acnh_input_keyboard_text_entry(open);
}

bool keyboard_state_known() {
    ensure_host();
    return acnh_input_keyboard_state_known();
}

void set_mouse_touch(bool down, float u, float v) {
    ensure_host();
    acnh_input_set_mouse_touch(down, u, v);
}

void set_picture_box(float x, float y, float w, float h) {
    ensure_host();
    acnh_input_set_picture_box(x, y, w, h);
}

void input_script(PadState& p) {
    ensure_host();
    acnh_input_script(reinterpret_cast<AcnhPadState*>(&p));
}

bool touch_enabled() {
    ensure_host();
    return acnh_input_touch_enabled();
}

bool touch_mirrored() {
    ensure_host();
    return acnh_input_touch_mirrored();
}

float touch_opacity_scale() {
    ensure_host();
    return acnh_input_touch_opacity_scale();
}

const TouchButton* touch_buttons(size_t& count, float aspect) {
    ensure_host();
    return reinterpret_cast<const TouchButton*>(acnh_input_touch_buttons(&count, aspect));
}

void touch_stick_rest(float aspect, bool mirrored, float& x, float& y, float& r) {
    ensure_host();
    acnh_input_touch_stick_rest(aspect, mirrored, &x, &y, &r);
}

const char* touch_group(size_t index) {
    ensure_host();
    return acnh_input_touch_group(index);
}

const char* touch_label(size_t index) {
    ensure_host();
    return acnh_input_touch_label(index);
}

void touch_layout_move(const char* group, float dx, float dy, float aspect) {
    ensure_host();
    acnh_input_touch_layout_move(group, dx, dy, aspect);
}

uint64_t touch_hit(float x, float y, float aspect) {
    ensure_host();
    return acnh_input_touch_hit(x, y, aspect);
}

// Android draws them in platform_android_input_wrapper.cpp, iOS in platform_mac_wrapper.cpp (touch_input.cpp).
#if !defined(__ANDROID__) && !NX_TOUCH_IOS
size_t touch_overlay(OverlayShape* out, size_t max) {
    ensure_host();
    return acnh_input_touch_overlay(reinterpret_cast<AcnhOverlayShape*>(out), max);
}
#endif

}  // namespace nx::plat
