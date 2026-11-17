//! Translate desktop gamepad events into the launcher's shared navigation actions.

use gilrs::{Axis, Button, EventType, Gilrs};

use crate::vm::Nav;

/// Poll pending gamepad events and return the latest navigation action, if any.
pub fn poll_nav(gilrs: &mut Gilrs) -> Option<Nav> {
    let mut nav = None;
    while let Some(event) = gilrs.next_event() {
        nav = match event.event {
            EventType::ButtonPressed(Button::DPadUp, _) => Some(Nav::Previous),
            EventType::ButtonPressed(Button::DPadDown, _) => Some(Nav::Next),
            EventType::ButtonPressed(Button::South, _) => Some(Nav::Activate),
            EventType::ButtonPressed(Button::East, _) => Some(Nav::Back),
            EventType::AxisChanged(Axis::LeftStickY, value, _) if value > 0.7 => {
                Some(Nav::Previous)
            }
            EventType::AxisChanged(Axis::LeftStickY, value, _) if value < -0.7 => Some(Nav::Next),
            _ => nav,
        };
    }
    nav
}
