//! Translate gamepad and keyboard input into the launcher's shared navigation actions
//! (docs/design/continue-screen.md, "Controller map").

use gilrs::{Axis, Button, EventType, Gilrs};
use slint::platform::Key;

use crate::vm::Nav;

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

/// The left stick as a D-pad: one action each time it is pushed past the threshold, none while
/// it is held there.
#[derive(Debug, Default)]
pub struct Stick {
    x: Option<Nav>,
    y: Option<Nav>,
}

impl Stick {
    pub fn moved(&mut self, axis: Axis, value: f32) -> Option<Nav> {
        let (held, low, high) = match axis {
            Axis::LeftStickX => (&mut self.x, Nav::Left, Nav::Right),
            Axis::LeftStickY => (&mut self.y, Nav::Down, Nav::Up),
            _ => return None,
        };
        let now = if value > STICK_THRESHOLD {
            Some(high)
        } else if value < -STICK_THRESHOLD {
            Some(low)
        } else {
            None
        };
        let fresh = if now == *held { None } else { now };
        *held = now;
        fresh
    }
}

/// Poll pending gamepad events and return their navigation actions, in order.
pub fn poll_nav(gilrs: &mut Gilrs, stick: &mut Stick) -> Vec<Nav> {
    let mut navs = Vec::new();
    while let Some(event) = gilrs.next_event() {
        let nav = match event.event {
            EventType::ButtonPressed(button, _) => button_nav(button),
            EventType::AxisChanged(axis, value, _) => stick.moved(axis, value),
            _ => None,
        };
        navs.extend(nav);
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
    fn the_stick_gives_one_action_per_push() {
        let mut stick = Stick::default();
        assert_eq!(stick.moved(Axis::LeftStickX, 0.9), Some(Nav::Right));
        assert_eq!(stick.moved(Axis::LeftStickX, 1.0), None);
        assert_eq!(stick.moved(Axis::LeftStickX, 0.1), None);
        assert_eq!(stick.moved(Axis::LeftStickX, -0.9), Some(Nav::Left));
        assert_eq!(stick.moved(Axis::LeftStickY, 0.9), Some(Nav::Up));
        assert_eq!(stick.moved(Axis::LeftStickY, -0.9), Some(Nav::Down));
        assert_eq!(stick.moved(Axis::RightStickX, 1.0), None);
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
