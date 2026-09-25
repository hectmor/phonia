//! Every output phonia can play on: the sound cards it can have to itself (exclusive) and the
//! outputs of the desktop's sound server (shared), each with an id that says which is which.
//!
//! The ids are what the daemon's clients name an output by: `exclusive:hw:DS2,0` is a card,
//! `shared:default` follows the desktop's default output, `shared:<name>` is one output by name.

use super::device;
use super::shared::pulse;
use crate::config::OutputSpec;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Exclusive,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub mode: Mode,
    pub name: String,
    pub detail: Option<String>,
    pub bit_perfect: bool,
    pub lossy: bool,
    pub codec: Option<String>,
    /// The entry that follows the desktop's default output.
    pub is_default: bool,
}

/// Every output there is right now. The sound server being absent just means there are no shared
/// ones.
pub async fn list() -> Vec<Entry> {
    let mut entries = cards(Path::new(device::ASOUND));
    if let Ok(outputs) = pulse::outputs().await {
        entries.extend(shared(&outputs));
    }
    entries
}

/// The sound cards with playback, one entry per playback device.
pub fn cards(asound: &Path) -> Vec<Entry> {
    let Ok(cards) = device::cards(asound) else { return Vec::new() };
    let mut entries = Vec::new();
    for card in cards.iter().filter(|card| !card.playback.is_empty()) {
        for playback in &card.playback {
            let many = card.playback.len() > 1;
            entries.push(Entry {
                id: format!("exclusive:hw:{},{playback}", card.id),
                mode: Mode::Exclusive,
                name: if many { format!("{} (device {playback})", card.name) } else { card.name.clone() },
                detail: Some(if card.usb { format!("USB, card {}", card.index) } else { format!("card {}", card.index) }),
                bit_perfect: true,
                lossy: false,
                codec: None,
                is_default: false,
            });
        }
    }
    entries
}

/// The sound server's outputs, led by the one that follows the desktop's default.
pub fn shared(outputs: &[pulse::Output]) -> Vec<Entry> {
    let default = outputs.iter().find(|output| output.is_default);
    let mut entries = vec![Entry {
        id: "shared:default".to_string(),
        mode: Mode::Shared,
        name: match default {
            Some(default) => format!("The desktop's default output ({})", default.description),
            None => "The desktop's default output".to_string(),
        },
        detail: default.map(|default| default.kind.label().to_string()),
        bit_perfect: false,
        lossy: default.is_some_and(|default| default.lossy),
        codec: default.and_then(|default| default.codec.clone()),
        is_default: true,
    }];
    entries.extend(outputs.iter().map(|output| Entry {
        id: format!("shared:{}", output.name),
        mode: Mode::Shared,
        name: output.description.clone(),
        detail: Some(output.kind.label().to_string()),
        bit_perfect: false,
        lossy: output.lossy,
        codec: output.codec.clone(),
        is_default: false,
    }));
    entries
}

/// A name for the output `spec` names, for people: the entry's own name when it is listed.
pub fn describe(spec: &OutputSpec, entries: &[Entry]) -> String {
    let id = spec.id();
    match entries.iter().find(|entry| entry.id == id) {
        Some(entry) => entry.name.clone(),
        None => spec.describe(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::pulse::{Output, OutputKind};

    fn output(name: &str, description: &str, kind: OutputKind, codec: Option<&str>, lossy: bool, is_default: bool) -> Output {
        Output {
            name: name.into(),
            description: description.into(),
            kind,
            codec: codec.map(str::to_string),
            lossy,
            rate: 48_000,
            index: 1,
            is_default,
        }
    }

    #[test]
    fn shared_outputs_start_with_the_default_and_carry_their_kind_and_codec() {
        let entries = shared(&[
            output("alsa_output.usb-DS2", "Fosi Audio DS2", OutputKind::Usb, None, false, true),
            output("bluez_output.AA", "Soundcore Life P2", OutputKind::Bluetooth, Some("SBC"), true, false),
        ]);
        assert_eq!(
            entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
            ["shared:default", "shared:alsa_output.usb-DS2", "shared:bluez_output.AA"]
        );
        assert_eq!(entries[0].name, "The desktop's default output (Fosi Audio DS2)");
        assert!(entries[0].is_default && !entries[1].is_default);
        assert!(entries.iter().all(|entry| !entry.bit_perfect && entry.mode == Mode::Shared));
        let bluetooth = &entries[2];
        assert_eq!((bluetooth.lossy, bluetooth.codec.as_deref(), bluetooth.detail.as_deref()), (true, Some("SBC"), Some("Bluetooth")));
    }

    #[test]
    fn the_default_entry_is_there_even_when_the_server_has_no_outputs() {
        let entries = shared(&[]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "shared:default");
    }

    #[test]
    fn a_spec_is_described_by_its_entry_or_by_itself() {
        let entries = shared(&[output("bluez_output.AA", "Soundcore Life P2", OutputKind::Bluetooth, Some("SBC"), true, false)]);
        let listed = OutputSpec::Shared { sink: Some("bluez_output.AA".into()) };
        assert_eq!(describe(&listed, &entries), "Soundcore Life P2");
        let unlisted = OutputSpec::Exclusive { device: "hw:DS2,0".into() };
        assert_eq!(describe(&unlisted, &entries), "device hw:DS2,0");
    }
}
