//! Which picture a prompt is drawn with for the pad in hand. `ui/hints.slint` draws the same
//! choice from the same device number (`InputDevice`'s value); this is the Rust side of that
//! table, so a test can check the glyph set changes when one pad replaces another.

use crate::vm::InputDevice;

/// A prompt's action; the value is the column of the glyph table in `hints.slint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum HintAction {
    Move = 0,
    Select = 1,
    Back = 2,
    Menu = 3,
}

/// The glyph file for a prompt, or `None` where the prompt is words (a keyboard) or "Tap".
pub fn glyph(device: InputDevice, action: HintAction) -> Option<String> {
    let set = match device {
        InputDevice::Keyboard | InputDevice::Touch => return None,
        InputDevice::Generic => "generic",
        InputDevice::Xbox => "xbox",
        InputDevice::PlayStation => "playstation",
        InputDevice::Switch => "switch",
        InputDevice::SteamDeck => "deck",
    };
    let kind = match action {
        HintAction::Move => "dpad",
        HintAction::Select => "south",
        HintAction::Back => "east",
        HintAction::Menu => "menu",
    };
    Some(format!("{set}-{kind}.svg"))
}

/// The prompts of a page and what each does, for the page's current state. `editor` is the
/// on-screen keyboard.
pub fn relay_prompts(editor: bool, device: InputDevice) -> Vec<(HintAction, &'static str)> {
    if editor {
        let select = if device == InputDevice::Keyboard {
            "Done"
        } else {
            "Press key"
        };
        vec![
            (HintAction::Move, "Move"),
            (HintAction::Select, select),
            (HintAction::Back, "Leave editor"),
        ]
    } else {
        vec![
            (HintAction::Move, "Move"),
            (HintAction::Select, "Select"),
            (HintAction::Back, "Back"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{DeviceEvent, Devices};

    fn pad(id: usize, vendor: u16, product: u16, name: &str) -> DeviceEvent {
        DeviceEvent::Connected {
            id,
            vendor: Some(vendor),
            product: Some(product),
            name: name.into(),
        }
    }

    #[test]
    fn the_relay_hints_change_set_when_a_playstation_pad_replaces_an_xbox_one() {
        let mut devices = Devices::default();
        devices.apply(pad(0, 0x045e, 0x028e, "Xbox pad"));
        devices.apply(DeviceEvent::Used(0));
        assert_eq!(devices.current(), InputDevice::Xbox);
        let xbox: Vec<_> = relay_prompts(false, devices.current())
            .into_iter()
            .map(|(action, _)| glyph(devices.current(), action).unwrap())
            .collect();
        assert_eq!(xbox, ["xbox-dpad.svg", "xbox-south.svg", "xbox-east.svg"]);

        devices.apply(pad(1, 0x054c, 0x0ce6, "DualSense"));
        devices.apply(DeviceEvent::Used(1));
        assert_eq!(devices.current(), InputDevice::PlayStation);
        for editor in [false, true] {
            let play: Vec<_> = relay_prompts(editor, devices.current())
                .into_iter()
                .map(|(action, _)| glyph(devices.current(), action).unwrap())
                .collect();
            assert_eq!(
                play,
                [
                    "playstation-dpad.svg",
                    "playstation-south.svg",
                    "playstation-east.svg"
                ]
            );
        }

        // Unplugging the PlayStation pad gives the Xbox set back; a keyboard has words.
        devices.apply(DeviceEvent::Disconnected(1));
        assert_eq!(
            glyph(devices.current(), HintAction::Select).unwrap(),
            "xbox-south.svg"
        );
        devices.apply(DeviceEvent::Keyboard);
        assert_eq!(glyph(devices.current(), HintAction::Select), None);
    }

    #[test]
    fn the_keyboard_prompts_say_what_each_key_does() {
        let editor = relay_prompts(true, InputDevice::Xbox);
        assert_eq!(editor[1].1, "Press key");
        assert_eq!(editor[2].1, "Leave editor");
        assert_eq!(relay_prompts(true, InputDevice::Keyboard)[1].1, "Done");
        assert_eq!(relay_prompts(false, InputDevice::Xbox)[2].1, "Back");
    }

    /// `ui/hints.slint` draws image `(device - 2) * 4 + action`: its table must hold the file
    /// this module names, for every set and every action, and the page's words must match.
    #[test]
    fn the_slint_glyph_table_matches_this_one() {
        let slint = include_str!("../../ui/hints.slint");
        let names: Vec<&str> = slint
            .split("@image-url(\"glyphs/")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap())
            .collect();
        assert_eq!(names.len(), 20);
        for device in [
            InputDevice::Generic,
            InputDevice::Xbox,
            InputDevice::PlayStation,
            InputDevice::Switch,
            InputDevice::SteamDeck,
        ] {
            for action in [
                HintAction::Move,
                HintAction::Select,
                HintAction::Back,
                HintAction::Menu,
            ] {
                let index = (device as usize - 2) * 4 + action as usize;
                assert_eq!(names[index], glyph(device, action).unwrap());
            }
        }
        let page = include_str!("../../ui/first_run.slint");
        for words in ["Press key", "Leave editor", "Done"] {
            assert!(page.contains(words), "{words}");
        }
    }
}
