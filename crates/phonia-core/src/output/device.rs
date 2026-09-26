//! Naming a sound card.
//!
//! ALSA numbers cards in the order the kernel found them, so a USB DAC that was card 2 yesterday is
//! card 1 today. Its *id* (`DS2`) does not change, so a device can be written `hw:DS2,0` and is
//! turned into the current `hw:N,D` here, each time a device is opened. Everything downstream
//! (the ALSA sink, the bit-perfect check that reads `/proc/asound/card<N>`) keeps working with
//! numbers.
//!
//! The sound cards are read from a directory laid out like `/proc/asound`, passed in, so all of
//! this can be tested against a made-up tree.

use anyhow::{Result, anyhow, bail};
use std::path::Path;

/// Where the kernel describes the sound cards.
pub const ASOUND: &str = "/proc/asound";

/// A device as configured, made concrete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Device {
    /// A raw hardware device, which is what bit-perfect output needs.
    Hw {
        card: u32,
        device: u32,
        /// The card's id, when known (for messages).
        card_id: Option<String>,
    },
    /// Anything else (`default`, `plughw:...`, `null`): passed to ALSA as written. Not checked for
    /// bit-perfectness, since something may be mixing or resampling in between.
    Other(String),
}

impl Device {
    /// The name to give ALSA.
    pub fn alsa_name(&self) -> String {
        match self {
            Device::Hw { card, device, .. } => format!("hw:{card},{device}"),
            Device::Other(name) => name.clone(),
        }
    }

    /// The device for people: `hw:1,0 (DS2)`.
    pub fn describe(&self) -> String {
        match self {
            Device::Hw {
                card_id: Some(id), ..
            } => format!("{} ({id})", self.alsa_name()),
            other => other.alsa_name(),
        }
    }
}

/// A sound card, as `phonia devices` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardInfo {
    pub index: u32,
    pub id: String,
    /// The card's full name, e.g. `Fosi Audio DS2`.
    pub name: String,
    pub usb: bool,
    /// The playback devices the card has (`pcm<D>p`).
    pub playback: Vec<u32>,
}

/// Every sound card, in order.
pub fn cards(asound: &Path) -> Result<Vec<CardInfo>> {
    let names = std::fs::read_to_string(asound.join("cards")).unwrap_or_default();
    let mut found = Vec::new();
    let entries = std::fs::read_dir(asound)
        .map_err(|error| anyhow!("reading {}: {error}", asound.display()))?;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(index) = file_name
            .to_str()
            .and_then(|name| name.strip_prefix("card"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let card = entry.path();
        let Ok(id) = std::fs::read_to_string(card.join("id")) else {
            continue;
        };
        let id = id.trim().to_string();

        let mut playback: Vec<u32> = std::fs::read_dir(&card)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|pcm| {
                let name = pcm.file_name();
                name.to_str()?
                    .strip_prefix("pcm")?
                    .strip_suffix('p')?
                    .parse()
                    .ok()
            })
            .collect();
        playback.sort_unstable();

        let name = card_name(&names, index).unwrap_or_else(|| id.clone());
        found.push(CardInfo {
            index,
            id,
            name,
            usb: card.join("usbid").exists(),
            playback,
        });
    }
    found.sort_by_key(|card| card.index);
    Ok(found)
}

/// The full name of card `index` from the text of `/proc/asound/cards`, whose lines look like
/// ` 1 [DS2            ]: USB-Audio - Fosi Audio DS2`.
fn card_name(cards_file: &str, index: u32) -> Option<String> {
    cards_file.lines().find_map(|line| {
        let (number, rest) = line.trim_start().split_once(' ')?;
        (number.parse::<u32>().ok()? == index).then(|| {
            rest.split_once(" - ")
                .map(|(_, name)| name.trim().to_string())
        })?
    })
}

/// Turns what was configured into a concrete device. Accepts `hw:N,D`, `hw:ID,D`,
/// `hw:CARD=ID,DEV=D`, `hw:N` and `hw:ID` (device 0), and `auto` (the first USB card with a
/// playback device); anything else is passed on as it is.
pub fn resolve(spec: &str, asound: &Path) -> Result<Device> {
    if spec == "auto" {
        return first_usb_card(asound);
    }
    let Some(rest) = spec.strip_prefix("hw:") else {
        return Ok(Device::Other(spec.to_string()));
    };

    let (card, device) = split_hw(rest)
        .ok_or_else(|| anyhow!("{spec:?} is not a device name: expected hw:<card>,<device>"))?;
    match card.parse::<u32>() {
        Ok(card) => Ok(Device::Hw {
            card,
            device,
            card_id: card_id(asound, card),
        }),
        Err(_) => by_id(spec, card, device, asound),
    }
}

/// `1,0`, `DS2,0`, `DS2`, `CARD=DS2,DEV=0` and `CARD=DS2` as (card, device).
fn split_hw(rest: &str) -> Option<(&str, u32)> {
    let mut card = None;
    let mut device = None;
    let mut positional = 0;
    for part in rest.split(',') {
        match part.split_once('=') {
            Some(("CARD", value)) => card = Some(value),
            Some(("DEV", value)) => device = Some(value.parse().ok()?),
            Some(_) => return None,
            None => {
                match positional {
                    0 => card = Some(part),
                    1 => device = Some(part.parse().ok()?),
                    _ => return None,
                }
                positional += 1;
            }
        }
    }
    card.filter(|card| !card.is_empty())
        .map(|card| (card, device.unwrap_or(0)))
}

fn card_id(asound: &Path, card: u32) -> Option<String> {
    std::fs::read_to_string(asound.join(format!("card{card}")).join("id"))
        .ok()
        .map(|id| id.trim().to_string())
}

fn by_id(spec: &str, id: &str, device: u32, asound: &Path) -> Result<Device> {
    let cards = cards(asound)?;
    match cards.iter().find(|card| card.id == id) {
        Some(card) => Ok(Device::Hw {
            card: card.index,
            device,
            card_id: Some(card.id.clone()),
        }),
        None => {
            let known: Vec<&str> = cards.iter().map(|card| card.id.as_str()).collect();
            bail!(
                "no sound card with id {id:?} (from {spec:?}); cards now: {}. Is the DAC plugged in? Run `phonia devices`",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )
        }
    }
}

fn first_usb_card(asound: &Path) -> Result<Device> {
    let cards = cards(asound)?;
    match cards
        .iter()
        .find(|card| card.usb && !card.playback.is_empty())
    {
        Some(card) => Ok(Device::Hw {
            card: card.index,
            device: card.playback[0],
            card_id: Some(card.id.clone()),
        }),
        None => bail!(
            "device = \"auto\" found no USB sound card with playback. Is the DAC plugged in? Run `phonia devices`"
        ),
    }
}

/// The text of `phonia devices`: the cards that can play, with the string to put in the config.
pub fn list(asound: &Path) -> Result<String> {
    let cards = cards(asound)?;
    let playing: Vec<&CardInfo> = cards
        .iter()
        .filter(|card| !card.playback.is_empty())
        .collect();
    if playing.is_empty() {
        return Ok("No sound cards with playback were found.".to_string());
    }
    let mut lines = vec!["Sound cards, exclusive and bit-perfect (put the device in ~/.config/phonia/config.toml under [output]):".to_string()];
    for card in playing {
        let devices: Vec<String> = card
            .playback
            .iter()
            .map(|device| format!("hw:{},{device}", card.id))
            .collect();
        lines.push(format!(
            "  {:<16} {}  (card {}{})",
            devices.join(" "),
            card.name,
            card.index,
            if card.usb { ", USB" } else { "" }
        ));
    }
    lines.push("Or use `auto` for the first USB sound card.".to_string());
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A made-up `/proc/asound` with an NVidia HDMI card (0), a USB DAC (1) and the laptop's own
    /// sound (2, with two playback devices).
    fn asound(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("phonia-device-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let card = |index: u32, id: &str, usb: bool, pcms: &[u32]| {
            let dir = root.join(format!("card{index}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("id"), format!("{id}\n")).unwrap();
            if usb {
                std::fs::write(dir.join("usbid"), "262a:0001\n").unwrap();
            }
            for pcm in pcms {
                std::fs::create_dir_all(dir.join(format!("pcm{pcm}p"))).unwrap();
            }
            std::fs::create_dir_all(dir.join("pcm0c")).unwrap(); // a capture device: not playback
        };
        card(0, "NVidia", false, &[3, 7]);
        card(1, "DS2", true, &[0]);
        card(2, "sofhdadsp", false, &[0, 31]);
        std::fs::write(
            root.join("cards"),
            " 0 [NVidia         ]: HDA-Intel - HDA NVidia\n                      HDA NVidia at 0x88080000 irq 17\n 1 [DS2            ]: USB-Audio - Fosi Audio DS2\n                      Speed Dragon Fosi Audio DS2 at usb-0000:00:14.0-1, high speed\n 2 [sofhdadsp      ]: sof-hda-dsp - sof-hda-dsp\n",
        )
        .unwrap();
        root
    }

    fn hw(card: u32, device: u32, id: Option<&str>) -> Device {
        Device::Hw {
            card,
            device,
            card_id: id.map(str::to_string),
        }
    }

    #[test]
    fn a_numbered_device_stays_as_it_is() {
        let root = asound("numbered");
        assert_eq!(resolve("hw:1,0", &root).unwrap(), hw(1, 0, Some("DS2")));
        assert_eq!(
            resolve("hw:2", &root).unwrap(),
            hw(2, 0, Some("sofhdadsp")),
            "a device number is optional"
        );
        assert_eq!(
            resolve("hw:9,1", &root).unwrap(),
            hw(9, 1, None),
            "ALSA, not this, says a card is missing"
        );
    }

    #[test]
    fn a_card_id_becomes_the_cards_current_number() {
        let root = asound("by-id");
        assert_eq!(resolve("hw:DS2,0", &root).unwrap(), hw(1, 0, Some("DS2")));
        assert_eq!(resolve("hw:DS2", &root).unwrap(), hw(1, 0, Some("DS2")));
        assert_eq!(
            resolve("hw:NVidia,7", &root).unwrap(),
            hw(0, 7, Some("NVidia"))
        );
        assert_eq!(
            resolve("hw:CARD=DS2,DEV=0", &root).unwrap(),
            hw(1, 0, Some("DS2"))
        );
        assert_eq!(
            resolve("hw:CARD=sofhdadsp", &root).unwrap(),
            hw(2, 0, Some("sofhdadsp"))
        );
        assert_eq!(
            resolve("hw:DEV=1,CARD=NVidia", &root).unwrap(),
            hw(0, 1, Some("NVidia")),
            "the order of the parts does not matter"
        );
    }

    #[test]
    fn the_same_name_follows_the_card_when_its_number_changes() {
        let root = asound("moves");
        assert_eq!(resolve("hw:DS2,0", &root).unwrap().alsa_name(), "hw:1,0");

        // The DAC is unplugged and plugged back in: the kernel gives it another number.
        std::fs::rename(root.join("card1"), root.join("card5")).unwrap();
        assert_eq!(resolve("hw:DS2,0", &root).unwrap().alsa_name(), "hw:5,0");
    }

    #[test]
    fn a_card_that_is_not_there_says_which_ones_are() {
        let root = asound("missing");
        let error = resolve("hw:Nope,0", &root).unwrap_err().to_string();
        assert!(error.contains("no sound card with id \"Nope\""), "{error}");
        assert!(
            error.contains("DS2") && error.contains("NVidia") && error.contains("phonia devices"),
            "{error}"
        );
    }

    #[test]
    fn auto_takes_the_first_usb_card_that_can_play() {
        let root = asound("auto");
        assert_eq!(resolve("auto", &root).unwrap(), hw(1, 0, Some("DS2")));

        std::fs::remove_file(root.join("card1").join("usbid")).unwrap();
        let error = resolve("auto", &root).unwrap_err().to_string();
        assert!(error.contains("no USB sound card"), "{error}");
    }

    #[test]
    fn devices_that_are_not_raw_hardware_pass_through() {
        let root = asound("other");
        for name in ["default", "plughw:1,0", "null", "pipewire", "dmix:CARD=DS2"] {
            assert_eq!(
                resolve(name, &root).unwrap(),
                Device::Other(name.to_string())
            );
        }
        assert_eq!(Device::Other("default".into()).alsa_name(), "default");
    }

    #[test]
    fn a_malformed_hardware_name_is_refused() {
        let root = asound("malformed");
        for bad in [
            "hw:",
            "hw:,0",
            "hw:1,x",
            "hw:1,0,2",
            "hw:FOO=1",
            "hw:CARD=,DEV=0",
        ] {
            assert!(resolve(bad, &root).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn devices_read_for_people() {
        assert_eq!(hw(1, 0, Some("DS2")).describe(), "hw:1,0 (DS2)");
        assert_eq!(hw(1, 0, None).describe(), "hw:1,0");
        assert_eq!(Device::Other("null".into()).describe(), "null");
    }

    #[test]
    fn the_cards_are_listed_in_order_with_their_details() {
        let root = asound("cards");
        let cards = cards(&root).unwrap();
        assert_eq!(cards.iter().map(|c| c.index).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(
            cards[1],
            CardInfo {
                index: 1,
                id: "DS2".into(),
                name: "Fosi Audio DS2".into(),
                usb: true,
                playback: vec![0]
            }
        );
        assert_eq!(
            cards[0].playback,
            [3, 7],
            "capture devices are not playback"
        );
        assert_eq!(cards[2].playback, [0, 31]);
        assert!(!cards[0].usb);
    }

    #[test]
    fn devices_output_is_pinned() {
        let root = asound("list");
        assert_eq!(
            list(&root).unwrap(),
            "Sound cards, exclusive and bit-perfect (put the device in ~/.config/phonia/config.toml under [output]):\n\
             \x20 hw:NVidia,3 hw:NVidia,7 HDA NVidia  (card 0)\n\
             \x20 hw:DS2,0         Fosi Audio DS2  (card 1, USB)\n\
             \x20 hw:sofhdadsp,0 hw:sofhdadsp,31 sof-hda-dsp  (card 2)\n\
             Or use `auto` for the first USB sound card."
        );
    }

    #[test]
    fn a_machine_without_cards_says_so() {
        let root =
            std::env::temp_dir().join(format!("phonia-device-test-{}-empty", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            list(&root).unwrap(),
            "No sound cards with playback were found."
        );
    }
}
