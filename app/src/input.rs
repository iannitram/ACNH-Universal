//! Translate gamepad and keyboard input into the launcher's shared navigation actions
//! (docs/design/continue-screen.md, "Controller map").

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
use slint::platform::Key;

use crate::vm::{InputDevice, Nav};

/// Identify a controller without depending on a particular input backend.
/// IDs refer to USB vendor and product IDs; name disambiguates 8BitDo modes.
pub fn controller_layout(vendor: Option<u16>, product: Option<u16>, name: &str) -> InputDevice {
    match (vendor, product) {
        (Some(0x28de), Some(0x1205)) => InputDevice::SteamDeck,
        (Some(0x057e), Some(0x2006 | 0x2007 | 0x2008 | 0x2009 | 0x200e)) => InputDevice::Switch,
        (Some(0x054c), Some(0x05c4 | 0x09cc | 0x0ce6 | 0x0df2)) => InputDevice::PlayStation,
        (Some(0x045e), Some(0x028e | 0x02d1 | 0x02dd | 0x02ea | 0x0b12 | 0x0b13)) => {
            InputDevice::Xbox
        }
        (Some(0x17ef), _) if name.to_ascii_lowercase().contains("legion") => InputDevice::Xbox,
        (
            Some(0x2dc8),
            Some(0x6001 | 0x6002 | 0x6006 | 0x6007 | 0x6101 | 0x6102 | 0x9006 | 0x9012),
        ) => InputDevice::Switch,
        (Some(0x2dc8), Some(0x2000)) => InputDevice::Xbox,
        (Some(0x2dc8), _) if name.to_ascii_lowercase().contains("switch") => InputDevice::Switch,
        _ => InputDevice::Generic,
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
    /// Update the held direction and emit all presses due at `now`.
    pub fn update(&mut self, direction: Option<Nav>, now: Duration) -> Vec<Nav> {
        if self.held.map(|(nav, _)| nav) != direction {
            self.held = direction.map(|nav| (nav, now + Duration::from_millis(400)));
            return direction.into_iter().collect();
        }
        let mut result = Vec::new();
        if let Some((nav, next)) = &mut self.held {
            while now >= *next {
                result.push(*nav);
                *next += Duration::from_millis(100);
            }
        }
        result
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
        assert_eq!(
            repeat.update(Some(Nav::Right), ms(600)),
            vec![Nav::Right, Nav::Right]
        );
        assert!(repeat.update(None, ms(601)).is_empty());
        assert!(repeat.update(None, ms(1000)).is_empty());
        assert_eq!(repeat.update(Some(Nav::Left), ms(1000)), vec![Nav::Left]);
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
}
