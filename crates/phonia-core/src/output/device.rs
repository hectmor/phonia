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

use super::mixer;
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
    list_with(asound, mixer::probe_card)
}

/// [`list`], probing for a hardware volume control with `probe` instead of the real one: real
/// mixer access has no fake-filesystem equivalent (unlike `stream0`, a plain text file under
/// `asound`), so a test that is not exercising this specifically passes a closure that always
/// says there is none, to stay deterministic and free of any real hardware dependency.
fn list_with(asound: &Path, probe: impl Fn(u32) -> Option<mixer::ControlInfo>) -> Result<String> {
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
        // Only a USB device publishes this; HDA and HDMI cards have no stream0 at all. Purely
        // informational -- read without opening the device, so it costs PipeWire nothing -- never
        // what a real open negotiates: see `output::alsa::probe` for that.
        if card.usb
            && let Ok(stream0) =
                std::fs::read_to_string(asound.join(format!("card{}/stream0", card.index)))
        {
            for line in advertised_lines(&stream0) {
                lines.push(format!("      advertises {line}"));
            }
        }
        // Whether the card has a hardware mixer control phonia can drive in exclusive mode
        // (#31): also passive, the mixer control is a separate device from the PCM and costs
        // nothing to read whether or not anything is playing.
        if let Some(info) = probe(card.index) {
            lines.push(format!("      hardware volume: {}", info.describe()));
        }
    }
    lines.push("Or use `auto` for the first USB sound card.".to_string());
    Ok(lines.join("\n"))
}

/// One format/channel-count combination a USB device's `stream0` says an altset offers, with the
/// rates it claims for that altset, exactly as the kernel printed them (so a continuous range such
/// as `8000 - 192000 (continuous)` passes through unchanged, not just a comma list).
struct Advertised {
    format: String,
    channels: String,
    rates: String,
}

/// Parses the `Playback:` section of a USB device's `/proc/asound/cardN/stream0` (stopping at a
/// `Capture:` section, if there is one): one `Format`/`Channels`/`Rates` triple per altset, in the
/// order they appear. A `SPECIAL` (DSD) altset is skipped -- it is not a PCM format phonia (or
/// TIDAL) ever asks for. Never used to decide anything: it describes what the device *claims*
/// before any kernel quirk list or actual negotiation, which `output::alsa::probe` alone decides.
fn parse_advertised(stream0: &str) -> Vec<Advertised> {
    let mut found = Vec::new();
    let (mut format, mut channels) = (None, None);
    for line in stream0.lines() {
        let line = line.trim();
        if line == "Capture:" {
            break;
        }
        if let Some(value) = line.strip_prefix("Format:") {
            format = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Channels:") {
            channels = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Rates:")
            && let (Some(format), Some(channels)) = (format.take(), channels.take())
            && format != "SPECIAL"
        {
            found.push(Advertised {
                format,
                channels,
                rates: value.trim().to_string(),
            });
        }
    }
    found
}

/// [`parse_advertised`]'s altsets, grouped into one line per distinct (channels, rates) pair --
/// which, on a real DAC, is normally all of them, since one altset per format at the same rates
/// and channel count is the common USB Audio Class shape.
fn advertised_lines(stream0: &str) -> Vec<String> {
    let mut groups: Vec<(String, String, Vec<String>)> = Vec::new();
    for Advertised {
        format,
        channels,
        rates,
    } in parse_advertised(stream0)
    {
        match groups
            .iter_mut()
            .find(|(c, r, _)| *c == channels && *r == rates)
        {
            Some((_, _, formats)) => formats.push(format),
            None => groups.push((channels, rates, vec![format])),
        }
    }
    groups
        .into_iter()
        .map(|(channels, rates, formats)| {
            format!("{} at {rates} ({channels}ch)", formats.join(", "))
        })
        .collect()
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
            list_with(&root, |_| None).unwrap(),
            "Sound cards, exclusive and bit-perfect (put the device in ~/.config/phonia/config.toml under [output]):\n\
             \x20 hw:NVidia,3 hw:NVidia,7 HDA NVidia  (card 0)\n\
             \x20 hw:DS2,0         Fosi Audio DS2  (card 1, USB)\n\
             \x20 hw:sofhdadsp,0 hw:sofhdadsp,31 sof-hda-dsp  (card 2)\n\
             Or use `auto` for the first USB sound card."
        );
    }

    #[test]
    fn a_cards_hardware_volume_control_is_shown_when_it_has_one() {
        let root = asound("list");
        let text = list_with(&root, |index| {
            (index == 1).then(|| mixer::ControlInfo {
                name: "PCM".into(),
                db_range: Some((-6300, 0)),
                has_switch: true,
            })
        })
        .unwrap();
        assert!(
            text.contains("      hardware volume: PCM (-63.0..0.0 dB)"),
            "{text}"
        );
        assert_eq!(
            text.matches("hardware volume").count(),
            1,
            "only the DS2 (card 1) was given one: {text}"
        );
    }

    #[test]
    fn a_machine_without_cards_says_so() {
        let root =
            std::env::temp_dir().join(format!("phonia-device-test-{}-empty", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            list_with(&root, |_| None).unwrap(),
            "No sound cards with playback were found."
        );
    }

    // ---- stream0 -------------------------------------------------------------------------------

    /// Captured verbatim from `/proc/asound/card0/stream0` on the Fosi Audio DS2 used to verify
    /// #25 against real hardware: three PCM altsets (S16_LE, S24_3LE, S32_LE), all at the same
    /// eight rates, plus a fourth, `SPECIAL` (DSD) one that must be skipped.
    const DS2_STREAM0: &str = "\
Speed Dragon Fosi Audio DS2 at usb-0000:00:14.0-1, high speed : USB Audio

Playback:
  Status: Stop
  Interface 2
    Altset 1
    Format: S16_LE
    Channels: 2
    Endpoint: 0x03 (3 OUT) (ASYNC)
    Rates: 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000
    Data packet interval: 125 us
    Bits: 16
    Channel map: FL FR
    Sync Endpoint: 0x84 (4 IN)
    Sync EP Interface: 2
    Sync EP Altset: 1
    Implicit Feedback Mode: No
  Interface 2
    Altset 2
    Format: S24_3LE
    Channels: 2
    Endpoint: 0x03 (3 OUT) (ASYNC)
    Rates: 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000
    Data packet interval: 125 us
    Bits: 24
    Channel map: FL FR
    Sync Endpoint: 0x84 (4 IN)
    Sync EP Interface: 2
    Sync EP Altset: 2
    Implicit Feedback Mode: No
  Interface 2
    Altset 3
    Format: S32_LE
    Channels: 2
    Endpoint: 0x03 (3 OUT) (ASYNC)
    Rates: 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000
    Data packet interval: 125 us
    Bits: 32
    Channel map: FL FR
    Sync Endpoint: 0x84 (4 IN)
    Sync EP Interface: 2
    Sync EP Altset: 3
    Implicit Feedback Mode: No
  Interface 2
    Altset 4
    Format: SPECIAL
    Channels: 2
    Endpoint: 0x03 (3 OUT) (ASYNC)
    Rates: 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000
    Data packet interval: 125 us
    Bits: 32
    DSD raw: DOP=0, bitrev=0
    Channel map: FL FR
    Sync Endpoint: 0x84 (4 IN)
    Sync EP Interface: 2
    Sync EP Altset: 4
    Implicit Feedback Mode: No";

    #[test]
    fn the_real_ds2_stream0_parses_to_its_three_pcm_altsets_not_the_dsd_one() {
        let advertised = parse_advertised(DS2_STREAM0);
        assert_eq!(advertised.len(), 3, "the SPECIAL (DSD) altset is skipped");
        assert_eq!(
            advertised
                .iter()
                .map(|a| a.format.as_str())
                .collect::<Vec<_>>(),
            ["S16_LE", "S24_3LE", "S32_LE"]
        );
        for format in &advertised {
            assert_eq!(format.channels, "2");
            assert_eq!(
                format.rates,
                "44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000"
            );
        }
    }

    #[test]
    fn identical_rates_and_channels_are_grouped_into_one_line() {
        assert_eq!(
            advertised_lines(DS2_STREAM0),
            [
                "S16_LE, S24_3LE, S32_LE at 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000 (2ch)"
            ]
        );
    }

    #[test]
    fn a_continuous_rate_range_passes_through_exactly_as_the_kernel_printed_it() {
        let stream0 = "\
Some Other DAC : USB Audio

Playback:
  Status: Stop
  Interface 1
    Altset 1
    Format: S32_LE
    Channels: 2
    Rates: 8000 - 192000 (continuous)
    Bits: 32";
        assert_eq!(
            advertised_lines(stream0),
            ["S32_LE at 8000 - 192000 (continuous) (2ch)"]
        );
    }

    #[test]
    fn parsing_stops_at_a_capture_section() {
        let stream0 = "\
Some Webcam Mic : USB Audio

Playback:
  Interface 1
    Altset 1
    Format: S16_LE
    Channels: 2
    Rates: 48000

Capture:
  Interface 2
    Altset 1
    Format: S16_LE
    Channels: 1
    Rates: 16000";
        assert_eq!(advertised_lines(stream0), ["S16_LE at 48000 (2ch)"]);
    }

    #[test]
    fn an_empty_or_unparseable_stream0_advertises_nothing() {
        assert!(parse_advertised("").is_empty());
        assert!(parse_advertised("garbage, not a real stream0 at all").is_empty());
    }

    #[test]
    fn phonia_devices_shows_what_a_usb_card_advertises_and_leaves_hda_cards_alone() {
        let root = asound("stream0");
        std::fs::write(root.join("card1").join("stream0"), DS2_STREAM0).unwrap();
        let text = list_with(&root, |_| None).unwrap();
        assert!(
            text.contains(
                "      advertises S16_LE, S24_3LE, S32_LE at 44100, 48000, 88200, 96000, \
                 176400, 192000, 352800, 384000 (2ch)"
            ),
            "{text}"
        );
        // card0 (NVidia) and card2 (sofhdadsp) are not USB and have no stream0: no such line for
        // either of them.
        assert_eq!(text.matches("advertises").count(), 1);
    }
}
