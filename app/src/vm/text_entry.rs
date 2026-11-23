//! The on-screen keyboard (docs/design/continue-screen.md, "Text entry"): one text field edited
//! with the controller. Every key is reachable with the D-pad and Accept; Back leaves the
//! editor; a physical keyboard and touch edit the same text. In a text field Space enters a
//! space, Backspace deletes, Enter submits and Esc goes back (the launcher's key handler calls
//! `type_text`, `backspace`, `submit` and `leave`).
//!
//! The text of a secret field (the app password) is drawn as dots and is handed over once, by
//! `take`; nothing else keeps a copy.

use crate::vm::Nav;

/// Keys on one row of the grid (the last row is shorter).
pub const COLUMNS: usize = 10;

/// What the editor asks the page to do after a press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Keep editing.
    Editing,
    /// Done: the text is ready to `take`.
    Done,
    /// Back or Cancel: leave the editor and drop the text.
    Leave,
}

/// Which set of keys the grid shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Lower,
    Upper,
    Symbols,
}

/// One key of the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Shift,
    Layers,
    Space,
    Delete,
    Done,
    Cancel,
}

impl Key {
    /// What the key says on its face.
    pub fn label(self, layer: Layer) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Shift if layer == Layer::Upper => "abc".into(),
            Key::Shift => "ABC".into(),
            Key::Layers if layer == Layer::Symbols => "abc".into(),
            Key::Layers => "123".into(),
            Key::Space => "Space".into(),
            Key::Delete => "Delete".into(),
            Key::Done => "Done".into(),
            Key::Cancel => "Cancel".into(),
        }
    }
}

const LOWER: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl-", "zxcvbnm._/"];
const UPPER: [&str; 4] = ["1234567890", "QWERTYUIOP", "ASDFGHJKL-", "ZXCVBNM._/"];
const SYMBOLS: [&str; 4] = ["!@#$%^&*()", "-_=+[]{}:;", "'\".,<>/?\\|", "`~"];

/// The longest text the field takes.
const MAX_LEN: usize = 200;

/// One text field and the keyboard that edits it.
#[derive(Clone, Debug)]
pub struct TextEntry {
    label: String,
    secret: bool,
    text: String,
    layer: Layer,
    focus: usize,
}

impl TextEntry {
    /// A field named `label`, starting with `initial`. `secret` draws the text as dots.
    pub fn new(label: impl Into<String>, initial: &str, secret: bool) -> Self {
        let mut entry = Self {
            label: label.into(),
            secret,
            text: initial.chars().take(MAX_LEN).collect(),
            layer: Layer::Lower,
            focus: 0,
        };
        entry.focus = entry.keys().len() - 1 - 1; // Done
        entry
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn layer(&self) -> Layer {
        self.layer
    }

    /// The text as the page draws it, with the caret at the end.
    pub fn shown(&self) -> String {
        let text = if self.secret {
            "\u{2022}".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        };
        format!("{text}|")
    }

    /// How many characters there are (never their value).
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The keys of the grid in reading order, `COLUMNS` to a row (the command keys follow the
    /// last character, so the last row is the shortest).
    pub fn keys(&self) -> Vec<Key> {
        let rows = match self.layer {
            Layer::Lower => LOWER,
            Layer::Upper => UPPER,
            Layer::Symbols => SYMBOLS,
        };
        let mut keys: Vec<Key> = rows
            .iter()
            .flat_map(|row| row.chars())
            .map(Key::Char)
            .collect();
        keys.extend([
            Key::Shift,
            Key::Layers,
            Key::Space,
            Key::Delete,
            Key::Done,
            Key::Cancel,
        ]);
        keys
    }

    /// The labels of `keys`, for drawing.
    pub fn labels(&self) -> Vec<String> {
        self.keys()
            .into_iter()
            .map(|key| key.label(self.layer))
            .collect()
    }

    /// The key that has focus.
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// Move focus with the D-pad; Accept presses the key and Back leaves the editor.
    pub fn handle(&mut self, nav: Nav) -> Outcome {
        match nav {
            Nav::Accept => self.press(self.focus),
            Nav::Back => Outcome::Leave,
            Nav::Left => {
                self.focus = self.focus.saturating_sub(1);
                Outcome::Editing
            }
            Nav::Right => {
                self.focus = (self.focus + 1).min(self.keys().len() - 1);
                Outcome::Editing
            }
            Nav::Up => {
                self.focus = self.focus.saturating_sub(COLUMNS);
                Outcome::Editing
            }
            Nav::Down => {
                let last = self.keys().len() - 1;
                let next = self.focus + COLUMNS;
                // Down from a column the last row does not have lands on its last key.
                self.focus = if next <= last {
                    next
                } else if self.focus / COLUMNS < last / COLUMNS {
                    last
                } else {
                    self.focus
                };
                Outcome::Editing
            }
            _ => Outcome::Editing,
        }
    }

    /// Press the key at `index` (Accept on it, or a click or tap).
    pub fn press(&mut self, index: usize) -> Outcome {
        let Some(key) = self.keys().get(index).copied() else {
            return Outcome::Editing;
        };
        self.focus = index;
        match key {
            Key::Char(c) => self.type_text(c),
            Key::Space => self.type_text(' '),
            Key::Delete => self.backspace(),
            Key::Shift => {
                self.layer = if self.layer == Layer::Upper {
                    Layer::Lower
                } else {
                    Layer::Upper
                };
                self.focus_on(Key::Shift);
            }
            Key::Layers => {
                self.layer = if self.layer == Layer::Symbols {
                    Layer::Lower
                } else {
                    Layer::Symbols
                };
                self.focus_on(Key::Layers);
            }
            Key::Done => return Outcome::Done,
            Key::Cancel => return Outcome::Leave,
        }
        Outcome::Editing
    }

    /// Keep focus on `key` after the layer changed (the keys moved): a second Accept switches
    /// back instead of landing on another key.
    fn focus_on(&mut self, key: Key) {
        if let Some(at) = self.keys().iter().position(|k| *k == key) {
            self.focus = at;
        }
    }

    /// A typed character (a physical keyboard, or a key of the grid). Control characters are
    /// ignored.
    pub fn type_text(&mut self, c: char) {
        if !c.is_control() && self.text.chars().count() < MAX_LEN {
            self.text.push(c);
        }
    }

    /// Backspace, or the Delete key.
    pub fn backspace(&mut self) {
        self.text.pop();
    }

    /// Enter on a physical keyboard.
    pub fn submit(&self) -> Outcome {
        Outcome::Done
    }

    /// Esc on a physical keyboard.
    pub fn leave(&self) -> Outcome {
        Outcome::Leave
    }

    /// The text, handed over once. The editor is spent afterwards.
    pub fn take(mut self) -> String {
        std::mem::take(&mut self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press_all(entry: &mut TextEntry, text: &str) {
        for c in text.chars() {
            let index = entry
                .keys()
                .iter()
                .position(|key| *key == Key::Char(c))
                .unwrap_or_else(|| panic!("no key for {c}"));
            entry.press(index);
        }
    }

    #[test]
    fn every_key_is_reached_with_the_dpad_alone_on_every_layer() {
        for layer in [Layer::Lower, Layer::Upper, Layer::Symbols] {
            let mut entry = TextEntry::new("Server", "", false);
            entry.layer = layer;
            entry.focus = entry.focus.min(entry.keys().len() - 1);
            let count = entry.keys().len();
            let mut seen = vec![false; count];
            let mut stack = vec![entry.focus()];
            while let Some(at) = stack.pop() {
                if std::mem::replace(&mut seen[at], true) {
                    continue;
                }
                for nav in [Nav::Up, Nav::Down, Nav::Left, Nav::Right] {
                    let mut probe = entry.clone();
                    probe.focus = at;
                    probe.handle(nav);
                    stack.push(probe.focus());
                }
            }
            assert!(seen.iter().all(|s| *s), "{layer:?}: {seen:?}");
        }
    }

    #[test]
    fn accept_types_the_focused_key_and_the_layers_switch() {
        let mut entry = TextEntry::new("User", "", false);
        press_all(&mut entry, "alex");
        let shift = entry.keys().iter().position(|k| *k == Key::Shift).unwrap();
        entry.press(shift);
        assert_eq!(entry.layer(), Layer::Upper);
        press_all(&mut entry, "Q");
        entry.press(shift);
        assert_eq!(entry.layer(), Layer::Lower);
        let layers = entry.keys().iter().position(|k| *k == Key::Layers).unwrap();
        entry.press(layers);
        press_all(&mut entry, ":");
        let space = entry.keys().iter().position(|k| *k == Key::Space).unwrap();
        entry.press(space);
        let delete = entry.keys().iter().position(|k| *k == Key::Delete).unwrap();
        entry.press(delete);
        assert_eq!(entry.clone().take(), "alexQ:");
    }

    #[test]
    fn focus_stays_on_the_layout_key_so_a_second_press_switches_back() {
        let mut entry = TextEntry::new("User", "keep", false);
        let layers = entry.keys().iter().position(|k| *k == Key::Layers).unwrap();
        entry.handle(Nav::Up);
        entry.focus = layers;
        assert_eq!(entry.handle(Nav::Accept), Outcome::Editing);
        assert_eq!(entry.layer(), Layer::Symbols);
        assert_eq!(entry.keys()[entry.focus()], Key::Layers);
        assert_eq!(entry.handle(Nav::Accept), Outcome::Editing);
        assert_eq!(entry.layer(), Layer::Lower);
        assert_eq!(entry.keys()[entry.focus()], Key::Layers);
        let shift = entry.keys().iter().position(|k| *k == Key::Shift).unwrap();
        entry.focus = shift;
        entry.handle(Nav::Accept);
        assert_eq!(entry.keys()[entry.focus()], Key::Shift);
        assert_eq!(entry.handle(Nav::Accept), Outcome::Editing);
        assert_eq!(entry.clone().take(), "keep");
    }

    #[test]
    fn dpad_moves_and_accept_presses_with_nav_only() {
        let mut entry = TextEntry::new("User", "", false);
        // Start on Done; go to the top-left key ("1") and type it.
        for _ in 0..6 {
            entry.handle(Nav::Up);
        }
        for _ in 0..12 {
            entry.handle(Nav::Left);
        }
        assert_eq!(entry.focus(), 0);
        assert_eq!(entry.handle(Nav::Accept), Outcome::Editing);
        assert_eq!(entry.clone().take(), "1");
        assert_eq!(entry.handle(Nav::Back), Outcome::Leave);
    }

    #[test]
    fn done_and_cancel_keys_and_the_physical_keyboard() {
        let mut entry = TextEntry::new("Server", "https://", false);
        entry.type_text('x');
        entry.type_text(' ');
        entry.backspace();
        entry.backspace();
        assert_eq!(entry.clone().take(), "https://");
        assert_eq!(entry.submit(), Outcome::Done);
        assert_eq!(entry.leave(), Outcome::Leave);
        let done = entry.keys().iter().position(|k| *k == Key::Done).unwrap();
        assert_eq!(entry.press(done), Outcome::Done);
        let cancel = entry.keys().iter().position(|k| *k == Key::Cancel).unwrap();
        assert_eq!(entry.press(cancel), Outcome::Leave);
    }

    #[test]
    fn a_secret_is_drawn_as_dots_and_handed_over_once() {
        let mut entry = TextEntry::new("App password", "", true);
        press_all(&mut entry, "abc");
        assert_eq!(entry.shown(), "\u{2022}\u{2022}\u{2022}|");
        assert!(!entry.shown().contains('a'));
        assert_eq!(entry.take(), "abc");
    }

    #[test]
    fn control_characters_and_long_text_are_ignored() {
        let mut entry = TextEntry::new("User", "", false);
        entry.type_text('\n');
        entry.type_text('\u{7f}');
        assert!(entry.is_empty());
        for _ in 0..300 {
            entry.type_text('a');
        }
        assert_eq!(entry.len(), MAX_LEN);
    }
}
