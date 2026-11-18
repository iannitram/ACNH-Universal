//! The join between the Continue page's view model and `ui/continue.slint`: the page view as
//! the Slint struct, and a clicked button as the view model's focus target.

use crate::vm::continue_page::{CardLines, FocusTarget, PageView, Side};
use crate::{CardView, ContinueView, PadTarget};

fn card_view(lines: &CardLines) -> CardView {
    CardView {
        saved: lines.saved.as_str().into(),
        played: lines.played.as_str().into(),
        version: lines.version.as_str().into(),
        badges: lines.badges.as_str().into(),
    }
}

/// The button a click hit, as the view model names it.
pub fn from_slint(target: PadTarget) -> FocusTarget {
    match target {
        PadTarget::Play => FocusTarget::Play,
        PadTarget::KeepThis => FocusTarget::Keep(Side::This),
        PadTarget::KeepRelay => FocusTarget::Keep(Side::Relay),
        PadTarget::Update => FocusTarget::Update,
        PadTarget::Back => FocusTarget::Back,
        PadTarget::Saves => FocusTarget::Saves,
        PadTarget::Settings => FocusTarget::Settings,
        PadTarget::Fleet => FocusTarget::Fleet,
        PadTarget::About => FocusTarget::About,
    }
}

fn pad_target(focus: FocusTarget) -> PadTarget {
    match focus {
        FocusTarget::Play => PadTarget::Play,
        FocusTarget::Keep(Side::This) => PadTarget::KeepThis,
        FocusTarget::Keep(Side::Relay) => PadTarget::KeepRelay,
        FocusTarget::Update => PadTarget::Update,
        FocusTarget::Back => PadTarget::Back,
        FocusTarget::Saves => PadTarget::Saves,
        FocusTarget::Settings => PadTarget::Settings,
        FocusTarget::Fleet => PadTarget::Fleet,
        FocusTarget::About => PadTarget::About,
    }
}

/// What the page draws, as `ui/continue.slint` takes it.
pub fn to_slint(view: &PageView) -> ContinueView {
    let empty = CardLines::default();
    let (this_card, relay_card) = match &view.chooser {
        Some((this, relay)) => (card_view(this), card_view(relay)),
        None => (card_view(&empty), card_view(&empty)),
    };
    ContinueView {
        status: view.status.as_str().into(),
        has_card: view.card.is_some(),
        card: card_view(view.card.as_ref().unwrap_or(&empty)),
        message: view.message.as_str().into(),
        button: view.button.as_str().into(),
        button_enabled: view.button_enabled,
        choosing: view.chooser.is_some(),
        this_card,
        relay_card,
        keep_enabled: view.keep_enabled,
        warning: view.warning.as_str().into(),
        play_anyway: view.play_anyway,
        back: view.back,
        pages_enabled: view.pages_enabled,
        focus: pad_target(view.focus),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::continue_page::ContinuePageVm;
    use crate::vm::Nav;
    use acnh_core::savecard::SaveCard;
    use std::path::PathBuf;

    fn card(saved_unix: u64, device: &str, play_seconds: u64) -> SaveCard {
        SaveCard {
            saved_unix,
            device: device.to_string(),
            play_seconds,
            version: "3.0.2".to_string(),
            picture: PathBuf::from("assets/acnhrecomp.png"),
        }
    }

    #[test]
    fn the_island_card_carries_the_island_not_sample_text() {
        let vm = ContinuePageVm::new(Some(card(1000, "Living room PC", 7260)), None);
        let view = to_slint(&vm.view());
        assert!(view.has_card);
        assert!(view.card.saved.starts_with("Living room PC, "));
        assert_eq!(view.card.played, "2 h 01 min played");
        assert_eq!(view.card.version, "Version 3.0.2");
        assert_eq!(view.status, "Up to date");
        assert_eq!(view.button, "Play");
        assert!(view.button_enabled);
        assert!(!view.choosing);
        assert_eq!(view.focus, PadTarget::Play);

        let empty = to_slint(&ContinuePageVm::new(None, None).view());
        assert!(!empty.has_card);
        assert_eq!(empty.card.saved, "");
        assert_eq!(empty.status, "No island yet");
    }

    #[test]
    fn the_chooser_carries_both_cards_and_the_focused_keep_button() {
        let mut vm = ContinuePageVm::new(Some(card(1000, "AMD handheld", 3600)), None);
        vm.set_conflict(
            card(1000, "AMD handheld", 3600),
            card(2000, "Strix Halo tablet", 7200),
        );
        assert!(!to_slint(&vm.view()).choosing);
        vm.handle(Nav::Accept);
        let view = to_slint(&vm.view());
        assert!(view.choosing);
        assert!(view.this_card.saved.starts_with("AMD handheld, "));
        assert!(view.relay_card.saved.starts_with("Strix Halo tablet, "));
        assert_eq!(view.relay_card.badges, "Newer, Longer play");
        assert_eq!(view.focus, PadTarget::KeepRelay);
        assert_eq!(view.button, "");
        assert!(view.back);
    }

    #[test]
    fn every_button_on_the_page_maps_to_its_target_and_back() {
        for target in [
            FocusTarget::Play,
            FocusTarget::Keep(Side::This),
            FocusTarget::Keep(Side::Relay),
            FocusTarget::Update,
            FocusTarget::Back,
            FocusTarget::Saves,
            FocusTarget::Settings,
            FocusTarget::Fleet,
            FocusTarget::About,
        ] {
            assert_eq!(from_slint(pad_target(target)), target);
        }
    }
}
