//! A DAC's own hardware volume: an ALSA mixer control (a "Selem"), found by the card's id and
//! driven through its own dB range so the percentage phonia shows agrees with alsamixer's and the
//! desktop's.
//!
//! A hardware control belongs to the card, not to phonia (see `docs/DECISIONS.md`): nothing here
//! ever seeds, restores or writes a value the listener did not just ask for, and every operation
//! re-resolves the card fresh ([`device::resolve`]), the same by-id precedent [`crate::output::alsa`]
//! itself follows for the PCM -- a replugged card, possibly a different model with a different
//! control entirely, is found again rather than assumed unchanged. The stream itself is never
//! touched: this is a completely different mechanism from [`super::AudioSink::set_gain`], which
//! `alsa::AlsaSink` never overrides, so exclusive mode stays bit-perfect whatever this module does.

use super::device::{self, Device};
use super::{Volume, VolumeControl, VolumeHandler};
use alsa::Round;
use alsa::mixer::{MilliBel, Mixer, Selem, SelemChannelId};
use anyhow::{Result, anyhow, bail};
use std::path::Path;
use std::sync::Mutex;

/// What [`probe`] found on a card: the control it would drive, and what that control reports.
/// `db_range` is `None` for a control with no usable dB data (an error reading it, or a reported
/// range that isn't actually a range) -- [`HardwareVolume`] then falls back to the control's raw
/// linear range, which is what alsamixer itself does in that case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlInfo {
    pub name: String,
    pub db_range: Option<(i32, i32)>,
    pub has_switch: bool,
}

/// Converts a volume percentage to the dB figure alsamixer and the desktop's mixers show for it:
/// 100% is 0 dB (unity), 50% is about -18 dB -- the same cubic-in-amplitude curve
/// [`super::Volume`]'s own doc describes for shared mode's percent semantics, so a number means the
/// same loudness change whichever mode produced it. `0%` has no finite figure on this curve (it
/// asks for silence, not a very large attenuation), so callers treat it as "the control's own
/// minimum" instead of calling this.
fn percent_to_millibel(percent: u8) -> i32 {
    let db = 60.0 * f64::from(percent.clamp(1, 100)).log10() - 120.0;
    (db * 100.0).round() as i32
}

/// The inverse of [`percent_to_millibel`], for reading a raw hardware value back as a percentage.
fn millibel_to_percent(millibel: i32) -> u8 {
    let db = f64::from(millibel) / 100.0;
    let percent = 100.0 * 10f64.powf(db / 60.0);
    percent.round().clamp(0.0, 100.0) as u8
}

/// The millibel value to actually ask a control for: `percent`'s own figure on the curve above,
/// floored at the control's reported minimum and never above the lower of its maximum or unity (0
/// dB) -- phonia never selects a gain above unity even on a control that offers one. `0%` always
/// means the control's exact minimum, not a computed, possibly-unreachable figure.
fn target_millibel(percent: u8, range: (i32, i32)) -> i32 {
    let (min, max) = range;
    if percent == 0 {
        return min;
    }
    percent_to_millibel(percent).clamp(min, max.min(0))
}

/// The percentage a raw dB reading shows as, within a control's own range: `0` below the minimum
/// (including the "mute" sentinel some controls report there), `100` at or above the lower of the
/// maximum or unity.
fn millibel_to_percent_in(millibel: i32, range: (i32, i32)) -> u8 {
    let (min, max) = range;
    if millibel <= min {
        0
    } else if millibel >= max.min(0) {
        100
    } else {
        millibel_to_percent(millibel)
    }
}

/// The raw value to ask for, linearly across a control's raw range: used only when the control has
/// no usable dB data at all.
fn target_raw_linear(percent: u8, range: (i64, i64)) -> i64 {
    let (min, max) = range;
    if max <= min {
        return min;
    }
    min + ((max - min) as f64 * f64::from(percent.min(100)) / 100.0).round() as i64
}

/// The percentage a raw value shows as, linearly across a control's raw range.
fn raw_to_percent_linear(raw: i64, range: (i64, i64)) -> u8 {
    let (min, max) = range;
    if max <= min {
        return 0;
    }
    (((raw - min) as f64 * 100.0 / (max - min) as f64).round()).clamp(0.0, 100.0) as u8
}

/// Picks which control to drive, from every control a card reports (name, whether it has a
/// playback volume), in this order: `"Master"`, then `"PCM"`, then the single remaining
/// volume-capable control if there is exactly one. A card with several other, unrelated volume
/// controls (unusual) is left alone rather than guessed at.
fn choose_control<'a>(controls: &[(&'a str, bool)]) -> Option<&'a str> {
    let has_volume = |name: &str| {
        controls
            .iter()
            .any(|&(n, has_volume)| n == name && has_volume)
    };
    if has_volume("Master") {
        return Some("Master");
    }
    if has_volume("PCM") {
        return Some("PCM");
    }
    let mut volume_capable = controls.iter().filter(|&&(_, has_volume)| has_volume);
    let only = volume_capable.next()?;
    volume_capable.next().is_none().then_some(only.0)
}

/// Every control a card's mixer reports, with whether each has a playback volume.
fn list_controls(mixer: &Mixer) -> Vec<(String, bool)> {
    mixer
        .iter()
        .filter_map(Selem::new)
        .filter_map(|selem| {
            let name = selem.get_id().get_name().ok()?.to_string();
            Some((name, selem.has_playback_volume()))
        })
        .collect()
}

fn find_selem<'m>(mixer: &'m Mixer, name: &str) -> Option<Selem<'m>> {
    mixer
        .iter()
        .filter_map(Selem::new)
        .find(|selem| selem.get_id().get_name() == Ok(name))
}

/// A usable dB range, or `None` if the control didn't report one worth trusting.
fn db_range_of(selem: &Selem) -> Option<(i32, i32)> {
    let (MilliBel(min), MilliBel(max)) = selem.get_playback_db_range();
    (min < max).then_some((min as i32, max as i32))
}

/// Opens the card's mixer and picks the control [`choose_control`] would, or `None` if `device`
/// isn't a numbered hardware card, or the card has no usable playback-volume control at all.
/// Read-only, safe to call whether or not phonia is currently playing: the control device has
/// nothing to do with the PCM's own reservation.
pub fn probe(device: &str) -> Option<ControlInfo> {
    let resolved = device::resolve(device, Path::new(device::ASOUND)).ok()?;
    let Device::Hw { card, .. } = resolved else {
        return None;
    };
    let mixer = Mixer::new(&format!("hw:{card}"), false).ok()?;
    let controls = list_controls(&mixer);
    let refs: Vec<(&str, bool)> = controls.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let name = choose_control(&refs)?;
    let selem = find_selem(&mixer, name)?;
    Some(ControlInfo {
        name: name.to_string(),
        db_range: db_range_of(&selem),
        has_switch: selem.has_playback_switch(),
    })
}

/// A [`VolumeControl`] backed by a card's own hardware mixer. Every call re-resolves the device
/// and re-opens the mixer: there is nothing cached to go stale across a replug (see the module
/// doc), and the `alsa` crate's `Mixer` is not `Sync`, so nothing could be kept open across calls
/// from different threads regardless.
pub struct HardwareVolume {
    device: String,
    handler: Mutex<Option<VolumeHandler>>,
}

impl HardwareVolume {
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            handler: Mutex::new(None),
        }
    }

    /// Whether this device currently has a usable hardware volume control; re-probed fresh every
    /// time. [`super::SinkFactory::volume`] calls this to decide whether to advertise a volume at
    /// all.
    pub fn probe(&self) -> Option<ControlInfo> {
        probe(&self.device)
    }

    fn open(&self) -> Result<(Mixer, ControlInfo)> {
        let resolved = device::resolve(&self.device, Path::new(device::ASOUND))?;
        let Device::Hw { card, .. } = resolved else {
            bail!("{} is not a hardware device", self.device);
        };
        let mixer = Mixer::new(&format!("hw:{card}"), false)?;
        let controls = list_controls(&mixer);
        let refs: Vec<(&str, bool)> = controls.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        let name = choose_control(&refs)
            .ok_or_else(|| anyhow!("{} has no usable hardware volume control", self.device))?
            .to_string();
        let info = {
            let selem = find_selem(&mixer, &name)
                .ok_or_else(|| anyhow!("the {name} control disappeared"))?;
            ControlInfo {
                name,
                db_range: db_range_of(&selem),
                has_switch: selem.has_playback_switch(),
            }
        };
        Ok((mixer, info))
    }

    fn read(&self) -> Result<Volume> {
        let (mixer, info) = self.open()?;
        let selem = find_selem(&mixer, &info.name)
            .ok_or_else(|| anyhow!("the {} control disappeared", info.name))?;
        let percent = match info.db_range {
            Some(range) => {
                let MilliBel(current) = selem.get_playback_vol_db(SelemChannelId::FrontLeft)?;
                millibel_to_percent_in(current as i32, range)
            }
            None => {
                let range = selem.get_playback_volume_range();
                let raw = selem.get_playback_volume(SelemChannelId::FrontLeft)?;
                raw_to_percent_linear(raw, range)
            }
        };
        let muted = info.has_switch && selem.get_playback_switch(SelemChannelId::FrontLeft)? == 0;
        Ok(Volume { percent, muted })
    }

    fn write(&self, volume: Volume) -> Result<()> {
        let (mixer, info) = self.open()?;
        if volume.muted && !info.has_switch {
            bail!("{}'s volume has no mute switch", info.name);
        }
        let selem = find_selem(&mixer, &info.name)
            .ok_or_else(|| anyhow!("the {} control disappeared", info.name))?;
        match info.db_range {
            Some(range) => {
                let target = target_millibel(volume.percent, range);
                selem.set_playback_db_all(MilliBel(i64::from(target)), Round::Floor)?;
            }
            None => {
                let range = selem.get_playback_volume_range();
                selem.set_playback_volume_all(target_raw_linear(volume.percent, range))?;
            }
        }
        if info.has_switch {
            selem.set_playback_switch_all(i32::from(!volume.muted))?;
        }
        Ok(())
    }
}

impl VolumeControl for HardwareVolume {
    /// Never fails: a card that has gone away since the last probe reads as the same locked 100%
    /// a card with no control at all would show.
    fn get(&self) -> Volume {
        self.read().unwrap_or_default()
    }

    fn set(&self, volume: Volume) -> Result<()> {
        self.write(volume)
    }

    /// Stored, but never called in this part: nothing here watches for a change made outside
    /// phonia (`alsamixer`, or the desktop reclaiming the card) yet. That is a later part of #31.
    fn on_change(&self, handler: VolumeHandler) {
        *self.handler.lock().unwrap() = Some(handler);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_matches_its_own_doc() {
        assert_eq!(percent_to_millibel(100), 0);
        assert!(
            (percent_to_millibel(50) - (-1806)).abs() <= 1,
            "{}",
            percent_to_millibel(50)
        );
        assert_eq!(percent_to_millibel(10), -6000);
        assert_eq!(percent_to_millibel(1), -12000);
    }

    #[test]
    fn the_curve_round_trips() {
        for percent in 1..=100u8 {
            let back = millibel_to_percent(percent_to_millibel(percent));
            assert!(
                back.abs_diff(percent) <= 1,
                "percent {percent} round-tripped to {back}"
            );
        }
    }

    #[test]
    fn target_never_goes_above_unity_even_with_headroom() {
        // A control offering +6 dB of headroom: 100% still means 0 dB, not +6.
        assert_eq!(target_millibel(100, (-6000, 600)), 0);
    }

    #[test]
    fn target_floors_at_the_controls_own_minimum() {
        // A control shallower than the curve's own -60 dB at 10%: clamped, not unreachable.
        assert_eq!(target_millibel(10, (-3000, 0)), -3000);
    }

    #[test]
    fn zero_percent_is_always_the_exact_minimum_not_a_computed_figure() {
        assert_eq!(target_millibel(0, (-6300, 0)), -6300);
    }

    #[test]
    fn reading_back_a_value_below_the_minimum_is_zero_percent() {
        // The "mute" sentinel some controls report at their floor.
        assert_eq!(millibel_to_percent_in(-9_999_999, (-6300, 0)), 0);
        assert_eq!(millibel_to_percent_in(-6300, (-6300, 0)), 0);
    }

    #[test]
    fn reading_back_at_or_above_the_ceiling_is_a_hundred_percent() {
        assert_eq!(millibel_to_percent_in(0, (-6300, 0)), 100);
        assert_eq!(
            millibel_to_percent_in(600, (-6300, 600)),
            100,
            "headroom is still 100%, never more"
        );
    }

    #[test]
    fn the_linear_fallback_covers_the_whole_raw_range() {
        assert_eq!(target_raw_linear(0, (0, 63)), 0);
        assert_eq!(target_raw_linear(100, (0, 63)), 63);
        assert_eq!(raw_to_percent_linear(0, (0, 63)), 0);
        assert_eq!(raw_to_percent_linear(63, (0, 63)), 100);
    }

    #[test]
    fn master_is_preferred_over_pcm_and_anything_else() {
        assert_eq!(
            choose_control(&[("PCM", true), ("Master", true), ("Mic", true)]),
            Some("Master")
        );
    }

    #[test]
    fn pcm_is_used_when_there_is_no_master() {
        assert_eq!(
            choose_control(&[("PCM", true), ("Mic", false)]),
            Some("PCM")
        );
    }

    #[test]
    fn a_single_other_volume_control_is_used_when_nothing_preferred_exists() {
        assert_eq!(choose_control(&[("Speaker", true)]), Some("Speaker"));
    }

    #[test]
    fn several_unrelated_volume_controls_with_no_preferred_name_are_left_alone() {
        assert_eq!(
            choose_control(&[("Speaker", true), ("Headphone", true)]),
            None
        );
    }

    #[test]
    fn a_switch_only_control_never_qualifies() {
        assert_eq!(choose_control(&[("IEC958", false)]), None);
    }

    #[test]
    fn no_controls_at_all_is_none() {
        assert_eq!(choose_control(&[]), None);
    }

    #[test]
    fn a_device_that_is_not_a_numbered_hardware_card_has_no_hardware_volume() {
        assert_eq!(probe("default"), None);
        assert_eq!(probe("plughw:0,0"), None);
    }

    // ---- against real hardware -----------------------------------------------------------
    //
    // Read-only or restore-after-itself; ignored by default. Run with:
    // `PHONIA_TEST_DEVICE=hw:DS2,0 cargo test -p phonia-core mixer::tests::hardware -- --ignored --nocapture`
    // (the device may be named by card id, or be `auto`, the default)

    /// Needs a real DAC with a usable hardware volume control. Read-only. Written against the
    /// Fosi Audio DS2's "PCM" control (a UAC Feature Unit, -63..0 dB in exact 1 dB steps).
    #[test]
    #[ignore = "needs a real ALSA device with a usable mixer control"]
    fn hardware_mixer_probe_finds_the_dacs_own_control() {
        let wanted = std::env::var("PHONIA_TEST_DEVICE").unwrap_or_else(|_| "auto".into());
        let device = device::resolve(&wanted, Path::new(device::ASOUND))
            .expect("finding the device")
            .alsa_name();
        let found = probe(&device);
        println!("{device}: {found:?}");
        let info = found.expect("the DS2 has a usable PCM control");
        assert_eq!(info.name, "PCM");
        assert_eq!(info.db_range, Some((-6300, 0)));
        assert!(info.has_switch);
    }

    /// Needs a real card with no usable playback-volume control (an HDMI output is a good
    /// choice: it typically has only a switch-only `IEC958` control). Read-only.
    #[test]
    #[ignore = "needs a real ALSA device with no usable mixer control, e.g. an HDMI output"]
    fn hardware_mixer_probe_finds_nothing_on_a_switch_only_card() {
        let wanted = std::env::var("PHONIA_TEST_LOCKED_DEVICE").expect(
            "set PHONIA_TEST_LOCKED_DEVICE to a card with no playback volume control \
             (e.g. an HDMI output's card)",
        );
        let device = device::resolve(&wanted, Path::new(device::ASOUND))
            .expect("finding the device")
            .alsa_name();
        assert_eq!(
            probe(&device),
            None,
            "{device} unexpectedly has a volume control"
        );
    }

    /// Needs a real DAC with a usable hardware volume control; nothing is played. Restores the
    /// control to whatever it was before the test, and never asks for anything louder than that
    /// starting point (the DS2's own control may be in real use, e.g. by WirePlumber).
    #[test]
    #[ignore = "needs a real ALSA device with a usable mixer control"]
    fn hardware_mixer_round_trip() {
        let wanted = std::env::var("PHONIA_TEST_DEVICE").unwrap_or_else(|_| "auto".into());
        let device = device::resolve(&wanted, Path::new(device::ASOUND))
            .expect("finding the device")
            .alsa_name();
        let control = HardwareVolume::new(device.clone());
        assert!(
            control.probe().is_some(),
            "{device} has no usable mixer control; set PHONIA_TEST_DEVICE to one that does"
        );
        let original = control.get();
        println!("{device} starts at {original:?}");

        struct Restore<'a> {
            control: &'a HardwareVolume,
            original: Volume,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                let _ = self.control.set(self.original);
            }
        }
        let _restore = Restore {
            control: &control,
            original,
        };

        // Never louder than where it started.
        for percent in [original.percent.min(50), original.percent.min(20), 0] {
            control
                .set(Volume {
                    percent,
                    muted: false,
                })
                .unwrap();
            let read_back = control.get();
            println!("asked {percent}%, got {read_back:?}");
            assert!(read_back.percent.abs_diff(percent) <= 2, "{read_back:?}");
            assert!(!read_back.muted);
        }

        let quiet = original.percent.min(10);
        control
            .set(Volume {
                percent: quiet,
                muted: true,
            })
            .unwrap();
        assert!(control.get().muted, "mute did not take");
        control
            .set(Volume {
                percent: quiet,
                muted: false,
            })
            .unwrap();
        assert!(!control.get().muted, "unmute did not take");
    }
}
