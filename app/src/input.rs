//! Translate gamepad and keyboard input into the launcher's shared navigation actions
//! (docs/design/continue-screen.md, "Controller map").

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
use slint::platform::Key;

use crate::vm::{InputDevice, Nav};

#[cfg(target_os = "android")]
pub mod android;

/// Identify a controller without depending on a particular input backend.
/// IDs refer to USB vendor and product IDs; name disambiguates 8BitDo modes.
///
/// The same rule as the game's (`crates/runtime/src/platform.rs` and `runtime/src/controller_kind.h`),
/// so one pad gets one set of prompts in both; all three are held to `tests/data/pad_kinds.tsv`.
/// Only the Steam Deck is told apart here (the game shows it with the generic, Xbox icons).
pub fn controller_layout(vendor: Option<u16>, product: Option<u16>, name: &str) -> InputDevice {
    let n = name.to_ascii_lowercase();
    match (vendor, product) {
        (Some(0x28de), Some(0x1205)) => return InputDevice::SteamDeck,
        (Some(0x057e), _) => return InputDevice::Switch,
        (Some(0x045e), _) => return InputDevice::Xbox,
        (Some(0x054c), _) => return InputDevice::PlayStation,
        (Some(0x17ef), _) if n.contains("legion") => return InputDevice::Xbox,
        (
            Some(0x2dc8),
            Some(0x6001 | 0x6002 | 0x6006 | 0x6007 | 0x6101 | 0x6102 | 0x9006 | 0x9012),
        ) => return InputDevice::Switch,
        (Some(0x2dc8), Some(0x2000)) => return InputDevice::Xbox,
        (Some(0x2dc8), _) if n.contains("switch") => return InputDevice::Switch,
        _ => {}
    }
    if n.contains("nintendo") || n.contains("pro controller") || n.contains("joy-con") {
        InputDevice::Switch
    } else if n.contains("xbox") {
        InputDevice::Xbox
    } else if n.contains("dualsense") || n.contains("dualshock") || n.contains("playstation") {
        InputDevice::PlayStation
    } else {
        InputDevice::Generic
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    Connected {
        id: usize,
        vendor: Option<u16>,
        product: Option<u16>,
        name: String,
    },
    Used(usize),
    Disconnected(usize),
    Keyboard,
    Touch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Keyboard,
    Touch,
    Pad(usize),
}

/// App-wide device history. Removing a disconnected pad reveals the previous source.
#[derive(Debug)]
pub struct Devices {
    pads: HashMap<usize, InputDevice>,
    history: Vec<Source>,
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            pads: HashMap::new(),
            history: vec![Source::Keyboard],
        }
    }
}

impl Devices {
    pub fn apply(&mut self, event: DeviceEvent) {
        let source = match event {
            DeviceEvent::Connected {
                id,
                vendor,
                product,
                name,
            } => {
                self.pads
                    .insert(id, controller_layout(vendor, product, &name));
                None
            }
            DeviceEvent::Used(id) if self.pads.contains_key(&id) => Some(Source::Pad(id)),
            DeviceEvent::Used(_) => None,
            DeviceEvent::Disconnected(id) => {
                self.pads.remove(&id);
                self.history.retain(|s| *s != Source::Pad(id));
                None
            }
            DeviceEvent::Keyboard => Some(Source::Keyboard),
            DeviceEvent::Touch => Some(Source::Touch),
        };
        if let Some(source) = source {
            self.history.retain(|s| *s != source);
            self.history.push(source);
        }
    }

    pub fn current(&self) -> InputDevice {
        match self.history.last().copied().unwrap_or(Source::Keyboard) {
            Source::Keyboard => InputDevice::Keyboard,
            Source::Touch => InputDevice::Touch,
            Source::Pad(id) => self.pads.get(&id).copied().unwrap_or(InputDevice::Generic),
        }
    }
}

/// How far the left stick must be pushed to count as a direction.
const STICK_THRESHOLD: f32 = 0.7;

/// The action for a gamepad button, by position (south is Accept on every layout).
pub fn button_nav(button: Button) -> Option<Nav> {
    match button {
        Button::DPadUp => Some(Nav::Up),
        Button::DPadDown => Some(Nav::Down),
        Button::DPadLeft => Some(Nav::Left),
        Button::DPadRight => Some(Nav::Right),
        Button::South => Some(Nav::Accept),
        Button::East => Some(Nav::Back),
        Button::Start => Some(Nav::Menu),
        Button::LeftTrigger => Some(Nav::Prev),
        Button::RightTrigger => Some(Nav::Next),
        _ => None,
    }
}

/// A held direction's schedule, independent of the input backend and wall clock.
#[derive(Debug, Default)]
pub struct Repeat {
    held: Option<(Nav, Duration)>,
}

impl Repeat {
    /// Update the held direction and emit the press due at `now`. Missed frames do not queue presses.
    pub fn update(&mut self, direction: Option<Nav>, now: Duration) -> Vec<Nav> {
        if self.held.map(|(nav, _)| nav) != direction {
            self.held = direction.map(|nav| (nav, now + Duration::from_millis(400)));
            return direction.into_iter().collect();
        }
        if let Some((nav, next)) = &mut self.held {
            if now >= *next {
                const INTERVAL: Duration = Duration::from_millis(100);
                let late = (now - *next).as_nanos() % INTERVAL.as_nanos();
                *next = now + (INTERVAL - Duration::from_nanos(late as u64));
                return vec![*nav];
            }
        }
        Vec::new()
    }
}

#[derive(Debug, Default)]
struct PadDirections {
    x: Repeat,
    y: Repeat,
    up: Repeat,
    down: Repeat,
    left: Repeat,
    right: Repeat,
}

impl PadDirections {
    fn ticks(&mut self, now: Duration) -> Vec<Nav> {
        let mut navs = Vec::new();
        for repeat in [
            &mut self.x,
            &mut self.y,
            &mut self.up,
            &mut self.down,
            &mut self.left,
            &mut self.right,
        ] {
            let direction = repeat.held.map(|(nav, _)| nav);
            navs.extend(repeat.update(direction, now));
        }
        navs
    }
}

/// Per-pad state for stick and D-pad repeat.
#[derive(Debug)]
pub struct Stick {
    start: Instant,
    pads: HashMap<GamepadId, PadDirections>,
}

impl Default for Stick {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            pads: HashMap::new(),
        }
    }
}

/// Poll pending gamepad events and return their navigation actions, in order.
pub fn poll_nav(gilrs: &mut Gilrs, stick: &mut Stick, devices: &mut Devices) -> Vec<Nav> {
    let mut navs = Vec::new();
    let now = stick.start.elapsed();
    while let Some(event) = gilrs.next_event() {
        let id = usize::from(event.id);
        if matches!(event.event, EventType::Disconnected) {
            stick.pads.remove(&event.id);
            devices.apply(DeviceEvent::Disconnected(id));
            continue;
        }
        if matches!(event.event, EventType::Connected) || !devices.pads.contains_key(&id) {
            let gamepad = gilrs.gamepad(event.id);
            devices.apply(DeviceEvent::Connected {
                id,
                vendor: gamepad.vendor_id(),
                product: gamepad.product_id(),
                name: gamepad.name().to_owned(),
            });
        }
        if matches!(event.event, EventType::ButtonPressed(..))
            || matches!(event.event, EventType::AxisChanged(Axis::LeftStickX | Axis::LeftStickY, value, _) if value.abs() > STICK_THRESHOLD)
        {
            devices.apply(DeviceEvent::Used(id));
        }
        let pad = stick.pads.entry(event.id).or_default();
        match event.event {
            EventType::ButtonPressed(button, _) | EventType::ButtonReleased(button, _) => {
                let pressed = matches!(event.event, EventType::ButtonPressed(..));
                let repeat = match button {
                    Button::DPadUp => Some((&mut pad.up, Nav::Up)),
                    Button::DPadDown => Some((&mut pad.down, Nav::Down)),
                    Button::DPadLeft => Some((&mut pad.left, Nav::Left)),
                    Button::DPadRight => Some((&mut pad.right, Nav::Right)),
                    _ => None,
                };
                if let Some((repeat, nav)) = repeat {
                    navs.extend(repeat.update(pressed.then_some(nav), now));
                } else if pressed {
                    navs.extend(button_nav(button));
                }
            }
            EventType::AxisChanged(axis, value, _) => {
                let (repeat, low, high) = match axis {
                    Axis::LeftStickX => (&mut pad.x, Nav::Left, Nav::Right),
                    Axis::LeftStickY => (&mut pad.y, Nav::Down, Nav::Up),
                    _ => continue,
                };
                let direction = if value > STICK_THRESHOLD {
                    Some(high)
                } else if value < -STICK_THRESHOLD {
                    Some(low)
                } else {
                    None
                };
                navs.extend(repeat.update(direction, now));
            }
            _ => {}
        }
    }
    for (id, pad) in &mut stick.pads {
        let repeated = pad.ticks(now);
        if !repeated.is_empty() {
            devices.apply(DeviceEvent::Used(usize::from(*id)));
            navs.extend(repeated);
        }
    }
    navs
}

/// The action for a key press, from the text of Slint's key event: arrows, Enter or Space
/// (Accept), Esc or Backspace (Back), F1 (Menu), Page Up and Page Down (Prev, Next).
pub fn key_nav(text: &str) -> Option<Nav> {
    let mut chars = text.chars();
    let (Some(key), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let keys = [
        (Key::UpArrow, Nav::Up),
        (Key::DownArrow, Nav::Down),
        (Key::LeftArrow, Nav::Left),
        (Key::RightArrow, Nav::Right),
        (Key::Return, Nav::Accept),
        (Key::Space, Nav::Accept),
        (Key::Escape, Nav::Back),
        (Key::Backspace, Nav::Back),
        (Key::F1, Nav::Menu),
        (Key::PageUp, Nav::Prev),
        (Key::PageDown, Nav::Next),
    ];
    keys.into_iter()
        .find(|(name, _)| char::from(*name) == key)
        .map(|(_, nav)| nav)
}

/// Android `InputDevice.SOURCE_GAMEPAD` (0x401) and `SOURCE_JOYSTICK` (0x1000010).
pub const ANDROID_SOURCE_GAMEPAD: i32 = 0x401;
pub const ANDROID_SOURCE_JOYSTICK: i32 = 0x1000010;

/// Android `KeyEvent.KEYCODE_*` values used by controller navigation.
pub const AKEYCODE_BACK: i32 = 4;
pub const AKEYCODE_DPAD_UP: i32 = 19;
pub const AKEYCODE_DPAD_DOWN: i32 = 20;
pub const AKEYCODE_DPAD_LEFT: i32 = 21;
pub const AKEYCODE_DPAD_RIGHT: i32 = 22;
pub const AKEYCODE_BUTTON_A: i32 = 96;
pub const AKEYCODE_BUTTON_B: i32 = 97;
pub const AKEYCODE_BUTTON_L1: i32 = 102;
pub const AKEYCODE_BUTTON_R1: i32 = 103;
pub const AKEYCODE_BUTTON_START: i32 = 108;

/// Description of an Android `InputDevice`, independent of JNI so tests run on every host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidDevice {
    pub id: i32,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    pub name: String,
    pub sources: i32,
    pub is_virtual: bool,
}

impl AndroidDevice {
    /// True when the device is a physical gamepad or joystick (not virtual, not keyboard-only).
    pub fn is_physical_pad(&self) -> bool {
        !self.is_virtual
            && ((self.sources & ANDROID_SOURCE_GAMEPAD) == ANDROID_SOURCE_GAMEPAD
                || (self.sources & ANDROID_SOURCE_JOYSTICK) == ANDROID_SOURCE_JOYSTICK)
    }

    pub fn device_id(&self) -> usize {
        self.id as u32 as usize
    }

    /// Classify this device using 2.04b's pad table, or `None` if it is virtual / not a pad.
    pub fn layout(&self) -> Option<InputDevice> {
        self.is_physical_pad()
            .then(|| controller_layout(self.vendor, self.product, &self.name))
    }
}

/// Tracks connected Android pads across 2-second `InputDevice.getDeviceIds()` snapshots
/// and feeds connect/disconnect/use transitions into `Devices`.
#[derive(Debug, Default)]
pub struct AndroidHotplug {
    known: HashSet<i32>,
    last_poll: Option<Instant>,
}

impl AndroidHotplug {
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    /// Reconcile `devices` with a fresh snapshot of Android `InputDevice`s.
    pub fn apply_snapshot(&mut self, snapshot: &[AndroidDevice], devices: &mut Devices) {
        let physical: Vec<&AndroidDevice> =
            snapshot.iter().filter(|d| d.is_physical_pad()).collect();
        let active_ids: HashSet<i32> = physical.iter().map(|d| d.id).collect();

        let removed: Vec<i32> = self
            .known
            .iter()
            .copied()
            .filter(|id| !active_ids.contains(id))
            .collect();
        for id in removed {
            self.known.remove(&id);
            devices.apply(DeviceEvent::Disconnected(id as u32 as usize));
        }

        for dev in physical {
            if self.known.insert(dev.id) {
                devices.apply(DeviceEvent::Connected {
                    id: dev.device_id(),
                    vendor: dev.vendor,
                    product: dev.product,
                    name: dev.name.clone(),
                });
            }
        }
    }

    /// Mark an Android pad as having produced an input event.
    pub fn record_used(&self, device_id: i32, devices: &mut Devices) {
        if self.known.contains(&device_id) {
            devices.apply(DeviceEvent::Used(device_id as u32 as usize));
        }
    }

    /// Poll `fetch` at most once every 2 seconds and apply any hot-plug changes.
    pub fn poll_if_due(
        &mut self,
        now: Instant,
        devices: &mut Devices,
        fetch: impl FnOnce() -> Vec<AndroidDevice>,
    ) -> bool {
        let due = self
            .last_poll
            .is_none_or(|prev| now.duration_since(prev) >= Self::POLL_INTERVAL);
        if !due {
            return false;
        }
        self.last_poll = Some(now);
        let snapshot = fetch();
        self.apply_snapshot(&snapshot, devices);
        true
    }
}

/// Map an Android gamepad/D-pad keycode to `Nav` by physical button position
/// (`KEYCODE_BUTTON_A` is always the bottom face button in AOSP `.kl` files).
pub fn android_keycode_nav(keycode: i32) -> Option<Nav> {
    match keycode {
        AKEYCODE_DPAD_UP => Some(Nav::Up),
        AKEYCODE_DPAD_DOWN => Some(Nav::Down),
        AKEYCODE_DPAD_LEFT => Some(Nav::Left),
        AKEYCODE_DPAD_RIGHT => Some(Nav::Right),
        AKEYCODE_BUTTON_A => Some(Nav::Accept),
        AKEYCODE_BUTTON_B | AKEYCODE_BACK => Some(Nav::Back),
        AKEYCODE_BUTTON_START => Some(Nav::Menu),
        AKEYCODE_BUTTON_L1 => Some(Nav::Prev),
        AKEYCODE_BUTTON_R1 => Some(Nav::Next),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_history_tracks_use_and_disconnect() {
        let mut devices = Devices::default();
        devices.apply(DeviceEvent::Connected {
            id: 1,
            vendor: Some(0x057e),
            product: Some(0x2009),
            name: "Pro Controller".into(),
        });
        devices.apply(DeviceEvent::Used(1));
        assert_eq!(devices.current(), InputDevice::Switch);
        assert_eq!(button_nav(Button::South), Some(Nav::Accept));
        devices.apply(DeviceEvent::Keyboard);
        assert_eq!(devices.current(), InputDevice::Keyboard);
        devices.apply(DeviceEvent::Touch);
        assert_eq!(devices.current(), InputDevice::Touch);
        devices.apply(DeviceEvent::Connected {
            id: 2,
            vendor: None,
            product: None,
            name: "Unknown".into(),
        });
        devices.apply(DeviceEvent::Used(2));
        assert_eq!(devices.current(), InputDevice::Generic);
        devices.apply(DeviceEvent::Disconnected(2));
        assert_eq!(devices.current(), InputDevice::Touch);
        devices.apply(DeviceEvent::Used(1));
        devices.apply(DeviceEvent::Disconnected(1));
        assert_eq!(devices.current(), InputDevice::Touch);
    }

    #[test]
    fn bundled_sdl_mappings_expose_south_and_east() {
        // gilrs 0.11.2, SDL_GameControllerDB/gamecontrollerdb.txt, Linux entries.
        // The database describes the raw button associated with SDL a (south) and b (east).
        // Legion Go and paired Joy-Con have no desktop entry there; their actual mapping
        // still needs verification on hardware. A mapping cannot prove raw physical position.
        for (mapping, bottom, right) in [
            (
                "03000000de2800000512000010010000,Valve Steam Deck,a:b3,b:b4",
                "b3",
                "b4",
            ),
            (
                "030000005e040000ea02000008040000,Microsoft Xbox One,a:b0,b:b1",
                "b0",
                "b1",
            ),
            (
                "030000004c050000e60c000011010000,PS5 Controller,a:b1,b:b2",
                "b1",
                "b2",
            ),
            (
                "030000004c050000cc09000000010000,PS4 Controller,a:b1,b:b2",
                "b1",
                "b2",
            ),
            (
                "030000007e0500000920000000026803,Nintendo Switch Pro Controller,a:b1,b:b0",
                "b1",
                "b0",
            ),
            (
                "03000000c82d00000660000011010000,8BitDo Pro 2,a:b1,b:b0",
                "b1",
                "b0",
            ),
        ] {
            let mapped = |raw| {
                mapping.split(',').find_map(|field| {
                    let (name, code) = field.split_once(':')?;
                    if code != raw {
                        return None;
                    }
                    match name {
                        "a" => Some(Button::South),
                        "b" => Some(Button::East),
                        _ => None,
                    }
                })
            };
            assert_eq!(mapped(bottom).and_then(button_nav), Some(Nav::Accept));
            assert_eq!(mapped(right).and_then(button_nav), Some(Nav::Back));
        }
    }

    /// The shared list the game's two classifiers are held to as well.
    #[test]
    fn controller_layout_matches_the_shared_table() {
        let table = include_str!("../../tests/data/pad_kinds.tsv");
        let mut rows = 0;
        for line in table
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 4, "{line}");
            let hex = |s: &str| u16::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
            let (vendor, product) = (hex(f[0]), hex(f[1]));
            let want = match f[3] {
                "switch" => InputDevice::Switch,
                "xbox" => InputDevice::Xbox,
                "playstation" => InputDevice::PlayStation,
                "deck" => InputDevice::SteamDeck,
                "generic" => InputDevice::Generic,
                other => panic!("{other}"),
            };
            assert_eq!(
                controller_layout(Some(vendor), Some(product), f[2]),
                want,
                "{line}"
            );
            rows += 1;
        }
        assert!(rows > 20);
    }

    #[test]
    fn known_pad_layouts() {
        for (vendor, product, name, expected) in [
            (0x28de, 0x1205, "Valve Steam Deck", InputDevice::SteamDeck),
            (0x17ef, 0x6182, "Legion Go Controller", InputDevice::Xbox),
            (0x045e, 0x02ea, "Xbox", InputDevice::Xbox),
            (0x054c, 0x0ce6, "DualSense", InputDevice::PlayStation),
            (0x054c, 0x09cc, "DualShock", InputDevice::PlayStation),
            (0x057e, 0x2009, "Switch Pro", InputDevice::Switch),
            (0x057e, 0x200e, "Joy-Con pair", InputDevice::Switch),
            (0x2dc8, 0x6006, "8BitDo Pro 2", InputDevice::Switch),
        ] {
            assert_eq!(
                controller_layout(Some(vendor), Some(product), name),
                expected
            );
            // SDL's a/b entries represent physical south/east. See gilrs 0.11.2's
            // SDL_GameControllerDB/gamecontrollerdb.txt for each platform's raw codes.
            assert_eq!(button_nav(Button::South), Some(Nav::Accept));
            assert_eq!(button_nav(Button::East), Some(Nav::Back));
        }
    }

    #[test]
    fn buttons_follow_the_controller_map() {
        assert_eq!(button_nav(Button::South), Some(Nav::Accept));
        assert_eq!(button_nav(Button::East), Some(Nav::Back));
        assert_eq!(button_nav(Button::Start), Some(Nav::Menu));
        assert_eq!(button_nav(Button::DPadLeft), Some(Nav::Left));
        assert_eq!(button_nav(Button::DPadRight), Some(Nav::Right));
        assert_eq!(button_nav(Button::DPadUp), Some(Nav::Up));
        assert_eq!(button_nav(Button::DPadDown), Some(Nav::Down));
        assert_eq!(button_nav(Button::LeftTrigger), Some(Nav::Prev));
        assert_eq!(button_nav(Button::RightTrigger), Some(Nav::Next));
        assert_eq!(button_nav(Button::North), None);
    }

    #[test]
    fn held_direction_repeats_and_stops_on_release() {
        let mut repeat = Repeat::default();
        let ms = Duration::from_millis;
        assert_eq!(repeat.update(Some(Nav::Right), ms(0)), vec![Nav::Right]);
        assert!(repeat.update(Some(Nav::Right), ms(399)).is_empty());
        assert_eq!(repeat.update(Some(Nav::Right), ms(400)), vec![Nav::Right]);
        assert!(repeat.update(Some(Nav::Right), ms(499)).is_empty());
        assert_eq!(repeat.update(Some(Nav::Right), ms(500)), vec![Nav::Right]);
        assert_eq!(repeat.update(Some(Nav::Right), ms(600)), vec![Nav::Right]);
        assert_eq!(repeat.update(Some(Nav::Right), ms(950)), vec![Nav::Right]);
        assert!(repeat.update(Some(Nav::Right), ms(999)).is_empty());
        assert_eq!(repeat.update(Some(Nav::Right), ms(1000)), vec![Nav::Right]);
        assert!(repeat.update(None, ms(1001)).is_empty());
        assert!(repeat.update(None, ms(1500)).is_empty());
        assert_eq!(repeat.update(Some(Nav::Left), ms(1500)), vec![Nav::Left]);
    }

    #[test]
    fn keys_follow_the_controller_map() {
        let text = |key: Key| char::from(key).to_string();
        assert_eq!(key_nav(&text(Key::Return)), Some(Nav::Accept));
        assert_eq!(key_nav(" "), Some(Nav::Accept));
        assert_eq!(key_nav(&text(Key::Escape)), Some(Nav::Back));
        assert_eq!(key_nav(&text(Key::Backspace)), Some(Nav::Back));
        assert_eq!(key_nav(&text(Key::LeftArrow)), Some(Nav::Left));
        assert_eq!(key_nav(&text(Key::RightArrow)), Some(Nav::Right));
        assert_eq!(key_nav(&text(Key::UpArrow)), Some(Nav::Up));
        assert_eq!(key_nav(&text(Key::DownArrow)), Some(Nav::Down));
        assert_eq!(key_nav(&text(Key::F1)), Some(Nav::Menu));
        assert_eq!(key_nav(&text(Key::PageUp)), Some(Nav::Prev));
        assert_eq!(key_nav(&text(Key::PageDown)), Some(Nav::Next));
        assert_eq!(key_nav("a"), None);
        assert_eq!(key_nav(""), None);
    }

    fn android_pad(
        id: i32,
        vendor: Option<u16>,
        product: Option<u16>,
        name: &str,
    ) -> AndroidDevice {
        AndroidDevice {
            id,
            vendor,
            product,
            name: name.into(),
            sources: ANDROID_SOURCE_GAMEPAD | ANDROID_SOURCE_JOYSTICK,
            is_virtual: false,
        }
    }

    #[test]
    fn android_device_classification_and_filtering() {
        assert_eq!(
            android_pad(1, Some(0x045e), Some(0x02ea), "Xbox Wireless Controller").layout(),
            Some(InputDevice::Xbox)
        );
        assert_eq!(
            android_pad(
                2,
                Some(0x054c),
                Some(0x0ce6),
                "DualSense Wireless Controller"
            )
            .layout(),
            Some(InputDevice::PlayStation)
        );
        assert_eq!(
            android_pad(3, Some(0x057e), Some(0x2009), "Pro Controller").layout(),
            Some(InputDevice::Switch)
        );
        assert_eq!(
            android_pad(4, Some(0x28de), Some(0x1205), "Steam Deck").layout(),
            Some(InputDevice::SteamDeck)
        );
        assert_eq!(
            android_pad(5, Some(0x1234), Some(0x5678), "Generic Gamepad").layout(),
            Some(InputDevice::Generic)
        );
        // Joystick-only source (no GAMEPAD bit) is still a physical pad.
        let mut joystick = android_pad(6, Some(0x045e), Some(0x02ea), "Xbox");
        joystick.sources = ANDROID_SOURCE_JOYSTICK;
        assert_eq!(joystick.layout(), Some(InputDevice::Xbox));

        // Virtual devices and keyboard-only devices are ignored.
        let mut virt = android_pad(7, Some(0x045e), Some(0x02ea), "Virtual");
        virt.is_virtual = true;
        assert_eq!(virt.layout(), None);

        let mut kb = android_pad(8, Some(0x045e), Some(0x02ea), "Keyboard");
        kb.sources = 0x101; // SOURCE_KEYBOARD
        assert_eq!(kb.layout(), None);
    }

    #[test]
    fn android_hotplug_transitions_and_poll_interval() {
        let mut devices = Devices::default();
        let mut hotplug = AndroidHotplug::default();
        let pad_a = android_pad(10, Some(0x045e), Some(0x02ea), "Xbox");
        let pad_b = android_pad(20, Some(0x054c), Some(0x0ce6), "DualSense");

        let t0 = Instant::now();
        assert!(hotplug.poll_if_due(t0, &mut devices, || vec![pad_a.clone()]));
        // Within 2 seconds, poll_if_due is a no-op.
        assert!(
            !hotplug.poll_if_due(t0 + Duration::from_secs(1), &mut devices, || {
                panic!("should not poll before 2s interval")
            })
        );

        hotplug.record_used(10, &mut devices);
        assert_eq!(devices.current(), InputDevice::Xbox);

        // Pad B connects and is used -> switches to B's set.
        assert!(
            hotplug.poll_if_due(t0 + Duration::from_secs(2), &mut devices, || {
                vec![pad_a.clone(), pad_b.clone()]
            })
        );
        hotplug.record_used(20, &mut devices);
        assert_eq!(devices.current(), InputDevice::PlayStation);

        // Pad B disconnects -> falls back to A's set.
        assert!(
            hotplug.poll_if_due(t0 + Duration::from_secs(4), &mut devices, || {
                vec![pad_a.clone()]
            })
        );
        assert_eq!(devices.current(), InputDevice::Xbox);

        // Tap sets Touch.
        devices.apply(DeviceEvent::Touch);
        assert_eq!(devices.current(), InputDevice::Touch);
    }

    #[test]
    fn android_aosp_key_layouts_map_bottom_to_accept_and_right_to_back() {
        // AOSP frameworks/base/data/keyboards/:
        // - Vendor_057e_Product_2009.kl (Switch Pro): swaps Linux 0x130 (B) -> BUTTON_A,
        //   0x131 (A) -> BUTTON_B so bottom face button is BUTTON_A and right is BUTTON_B.
        // - Vendor_054c_Product_05c4.kl, Vendor_054c_Product_09cc.kl, Vendor_054c_Product_0ce6.kl
        //   (DualShock 4 / DualSense): Cross (bottom) -> BUTTON_A, Circle (right) -> BUTTON_B.
        // - Vendor_045e_Product_02ea.kl (Xbox) & Generic.kl: bottom -> BUTTON_A, right -> BUTTON_B.
        for (vendor, product, name, bottom_keycode, right_keycode) in [
            (
                0x28de,
                0x1205,
                "Valve Steam Deck",
                AKEYCODE_BUTTON_A,
                AKEYCODE_BUTTON_B,
            ),
            (
                0x17ef,
                0x6182,
                "Legion Go Controller",
                AKEYCODE_BUTTON_A,
                AKEYCODE_BUTTON_B,
            ),
            (0x045e, 0x02ea, "Xbox", AKEYCODE_BUTTON_A, AKEYCODE_BUTTON_B),
            (
                0x054c,
                0x0ce6,
                "DualSense",
                AKEYCODE_BUTTON_A,
                AKEYCODE_BUTTON_B,
            ),
            (
                0x054c,
                0x09cc,
                "DualShock",
                AKEYCODE_BUTTON_A,
                AKEYCODE_BUTTON_B,
            ),
            (
                0x057e,
                0x2009,
                "Switch Pro",
                AKEYCODE_BUTTON_A,
                AKEYCODE_BUTTON_B,
            ),
            (
                0x057e,
                0x200e,
                "Joy-Con pair",
                AKEYCODE_BUTTON_A,
                AKEYCODE_BUTTON_B,
            ),
            (
                0x2dc8,
                0x6006,
                "8BitDo Pro 2",
                AKEYCODE_BUTTON_A,
                AKEYCODE_BUTTON_B,
            ),
        ] {
            let dev = android_pad(1, Some(vendor), Some(product), name);
            assert!(dev.layout().is_some());
            assert_eq!(android_keycode_nav(bottom_keycode), Some(Nav::Accept));
            assert_eq!(android_keycode_nav(right_keycode), Some(Nav::Back));
        }
    }
}
