//! Reserving the DAC before opening it, and giving it back afterwards.
//!
//! Desktop audio servers (WirePlumber, PulseAudio) hold sound cards through the
//! `org.freedesktop.ReserveDevice1` protocol: whoever owns the bus name `Audio<N>` may use card
//! `N`, and a program that needs the card asks the owner to let go. This module holds the
//! protocol-independent half: what a reservation is, when it is taken and dropped, and how the
//! sink opens under it. The D-Bus implementation of [`DeviceReserver`] lives elsewhere, so all of
//! this is testable without a bus.

use super::ReleaseHandler;
use anyhow::Result;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How much phonia insists when it asks for a card. Higher than WirePlumber (-20) and PulseAudio
/// (0), so they give way, and it is also the bar another program's request has to clear before
/// phonia gives the card up.
pub const PRIORITY: i32 = 10;

/// Retries of a device open that fails with EBUSY right after the card was taken from another
/// holder: that holder tears its side down asynchronously, so the PCM stays busy for a moment.
const BUSY_RETRIES: u32 = 20;
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(50);

/// A held reservation. Dropping it gives the card back.
pub trait Reservation: Send {
    /// Whether the card was taken from another holder (as opposed to being free).
    fn took_over(&self) -> bool;
}

/// Reserves sound cards. `card` is the ALSA card number; `device_name` is how the user knows it
/// (for example `hw:DS2,0`), used only for messages.
pub trait DeviceReserver: Send + Sync {
    fn acquire(
        &self,
        card: u32,
        device_name: &str,
    ) -> std::result::Result<Box<dyn Reservation>, ReserveError>;

    /// Installs the function to call when another program asks for a card this reserver holds.
    fn on_release_request(&self, _handler: ReleaseHandler) {}
}

#[derive(Debug)]
pub enum ReserveError {
    /// There is no bus to reserve on (no session, or nothing listening). Playback can still be
    /// attempted without a reservation.
    NoBus(String),
    /// The holder answered that it will not let go.
    Refused {
        device: String,
        holder: String,
        priority: Option<i32>,
    },
    /// The holder did not answer in time.
    Unresponsive {
        device: String,
        holder: String,
    },
    Failed(String),
}

impl fmt::Display for ReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReserveError::NoBus(why) => write!(f, "no D-Bus session to reserve the DAC on: {why}"),
            ReserveError::Refused {
                device,
                holder,
                priority,
            } => {
                write!(f, "the DAC {device} is held by {holder}")?;
                if let Some(priority) = priority {
                    write!(f, " (priority {priority})")?;
                }
                write!(
                    f,
                    ", which refused to release it to phonia (priority {PRIORITY}). Stop that application or its \
                     playback on the DAC and try again, or set `reserve = false` under [output] to skip the request."
                )
            }
            ReserveError::Unresponsive { device, holder } => {
                write!(
                    f,
                    "{holder} did not answer the request to release {device} in time"
                )
            }
            ReserveError::Failed(why) => write!(f, "could not reserve the DAC: {why}"),
        }
    }
}

impl std::error::Error for ReserveError {}

/// The device is open elsewhere. Typed so that a caller that just took the card from another
/// holder can tell it apart from other failures and try again.
#[derive(Debug)]
pub struct DeviceBusy {
    pub device: String,
    pub detail: String,
}

impl fmt::Display for DeviceBusy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "device '{}' is busy (EBUSY): some other program has it open. If it is PipeWire, leave \
             `reserve = true` under [output] so phonia asks it to let go; otherwise stop playback to that DAC \
             (`fuser -v /dev/snd/*`), or as a last resort switch the card's profile off \
             (`pactl set-card-profile <card> off`).\nOriginal ALSA error: {}",
            self.device, self.detail
        )
    }
}

impl std::error::Error for DeviceBusy {}

/// What [`ReservationSlot::ensure`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Ensured {
    /// Already held for this card: nothing to do.
    AlreadyHeld,
    Acquired {
        took_over: bool,
    },
}

/// The reservation of the card the engine is currently using, kept between sinks so that a change
/// of format from one track to the next never gives the card back to the desktop.
pub struct ReservationSlot {
    reserver: Arc<dyn DeviceReserver>,
    held: Mutex<Option<(u32, Box<dyn Reservation>)>>,
}

impl ReservationSlot {
    pub fn new(reserver: Arc<dyn DeviceReserver>) -> Self {
        Self {
            reserver,
            held: Mutex::new(None),
        }
    }

    /// Makes sure card `card` is reserved. A reservation for a different card number (the DAC
    /// was plugged back in under another one) is dropped first.
    pub fn ensure(
        &self,
        card: u32,
        device_name: &str,
    ) -> std::result::Result<Ensured, ReserveError> {
        let mut held = self.held.lock().unwrap();
        if held
            .as_ref()
            .is_some_and(|(held_card, _)| *held_card == card)
        {
            return Ok(Ensured::AlreadyHeld);
        }
        *held = None;
        let reservation = self.reserver.acquire(card, device_name)?;
        let took_over = reservation.took_over();
        *held = Some((card, reservation));
        Ok(Ensured::Acquired { took_over })
    }

    pub fn on_release_request(&self, handler: ReleaseHandler) {
        self.reserver.on_release_request(handler);
    }

    /// Gives the card back. Does nothing if none is held.
    pub fn release(&self) {
        self.held.lock().unwrap().take();
    }

    pub fn held_card(&self) -> Option<u32> {
        self.held.lock().unwrap().as_ref().map(|(card, _)| *card)
    }
}

/// Opens a device under the reservation: reserve first, then `open`, retrying while the device is
/// still busy if the card was just taken from another holder. Without a card to reserve (`None`,
/// for `default`, `plughw:` and the like) it is just `open`.
///
/// A missing bus is not an error: it is reported on stderr and the open goes ahead, which fails
/// with the usual message if something else holds the card.
pub fn open_reserved<T>(
    slot: Option<&ReservationSlot>,
    card: Option<(u32, &str)>,
    mut open: impl FnMut() -> Result<T>,
) -> Result<T> {
    open_reserved_with(slot, card, BUSY_RETRIES, BUSY_RETRY_DELAY, &mut open)
}

fn open_reserved_with<T>(
    slot: Option<&ReservationSlot>,
    card: Option<(u32, &str)>,
    retries: u32,
    delay: Duration,
    open: &mut dyn FnMut() -> Result<T>,
) -> Result<T> {
    let mut took_over = false;
    let mut no_bus = false;
    if let (Some(slot), Some((card, name))) = (slot, card) {
        match slot.ensure(card, name) {
            Ok(Ensured::Acquired { took_over: t }) => took_over = t,
            Ok(Ensured::AlreadyHeld) => {}
            Err(ReserveError::NoBus(why)) => {
                eprintln!("phonia: not asking the desktop to release the DAC: {why}");
                no_bus = true;
            }
            Err(error) => return Err(error.into()),
        }
    }

    let mut attempts_left = if took_over { retries } else { 0 };
    loop {
        match open() {
            Err(error) if error.downcast_ref::<DeviceBusy>().is_some() => {
                if attempts_left > 0 {
                    attempts_left -= 1;
                    std::thread::sleep(delay);
                    continue;
                }
                let note = if took_over {
                    "phonia reserved the card through D-Bus, but it is still busy: a program that does not use \
                     device reservation has it open (see `fuser -v /dev/snd/*`)"
                } else if no_bus {
                    "device reservation was not possible: there is no D-Bus session"
                } else {
                    return Err(error);
                };
                return Err(error.context(note));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::fake::FakeReserver;
    use std::cell::Cell;

    fn busy() -> anyhow::Error {
        DeviceBusy {
            device: "hw:2,0".into(),
            detail: "EBUSY".into(),
        }
        .into()
    }

    #[test]
    fn the_same_card_is_reserved_only_once() {
        let reserver = FakeReserver::new();
        let slot = ReservationSlot::new(reserver.clone());
        assert_eq!(
            slot.ensure(2, "hw:DS2,0").unwrap(),
            Ensured::Acquired { took_over: false }
        );
        assert_eq!(slot.ensure(2, "hw:DS2,0").unwrap(), Ensured::AlreadyHeld);
        assert_eq!(reserver.log(), ["acquire 2"]);
    }

    #[test]
    fn another_card_number_replaces_the_reservation() {
        let reserver = FakeReserver::new();
        let slot = ReservationSlot::new(reserver.clone());
        slot.ensure(2, "hw:DS2,0").unwrap();
        slot.ensure(3, "hw:DS2,0").unwrap();
        assert_eq!(reserver.log(), ["acquire 2", "release 2", "acquire 3"]);
        assert_eq!(slot.held_card(), Some(3));
    }

    #[test]
    fn release_is_idempotent() {
        let reserver = FakeReserver::new();
        let slot = ReservationSlot::new(reserver.clone());
        slot.ensure(2, "hw:DS2,0").unwrap();
        slot.release();
        slot.release();
        assert_eq!(reserver.log(), ["acquire 2", "release 2"]);
        assert_eq!(slot.held_card(), None);
    }

    #[test]
    fn a_refusal_keeps_nothing_held_and_can_be_retried() {
        let reserver = FakeReserver::new();
        reserver.script(Err(ReserveError::Refused {
            device: "hw:DS2,0".into(),
            holder: "jackd".into(),
            priority: Some(99),
        }));
        let slot = ReservationSlot::new(reserver.clone());
        let error = slot.ensure(2, "hw:DS2,0").unwrap_err().to_string();
        assert!(
            error.contains("jackd") && error.contains("reserve = false"),
            "{error}"
        );
        assert_eq!(slot.held_card(), None);
        assert!(
            slot.ensure(2, "hw:DS2,0").is_ok(),
            "the next attempt is granted"
        );
    }

    #[test]
    fn the_card_is_reserved_before_it_is_opened() {
        let reserver = FakeReserver::new();
        let slot = ReservationSlot::new(reserver.clone());
        let log = reserver.clone();
        let opened = open_reserved(Some(&slot), Some((2, "hw:DS2,0")), || {
            assert_eq!(log.log(), ["acquire 2"], "reserved before the open");
            Ok(7)
        })
        .unwrap();
        assert_eq!(opened, 7);
    }

    #[test]
    fn devices_without_a_card_are_never_reserved() {
        let reserver = FakeReserver::new();
        let slot = ReservationSlot::new(reserver.clone());
        open_reserved(Some(&slot), None, || Ok(())).unwrap();
        assert!(reserver.log().is_empty());
    }

    #[test]
    fn a_refusal_stops_the_open() {
        let reserver = FakeReserver::new();
        reserver.script(Err(ReserveError::Unresponsive {
            device: "hw:2,0".into(),
            holder: "jackd".into(),
        }));
        let slot = ReservationSlot::new(reserver);
        let opened = Cell::new(false);
        let result = open_reserved(Some(&slot), Some((2, "hw:2,0")), || {
            opened.set(true);
            Ok(())
        });
        assert!(result.is_err());
        assert!(
            !opened.get(),
            "the device must not be opened after a failed reservation"
        );
    }

    #[test]
    fn without_a_bus_the_open_goes_ahead() {
        let reserver = FakeReserver::new();
        reserver.script(Err(ReserveError::NoBus("no session".into())));
        let slot = ReservationSlot::new(reserver);
        assert_eq!(
            open_reserved(Some(&slot), Some((2, "hw:2,0")), || Ok(1)).unwrap(),
            1
        );
    }

    #[test]
    fn without_a_bus_a_busy_device_explains_why() {
        let reserver = FakeReserver::new();
        reserver.script(Err(ReserveError::NoBus("no session".into())));
        let slot = ReservationSlot::new(reserver);
        let error = open_reserved(Some(&slot), Some((2, "hw:2,0")), || -> Result<()> {
            Err(busy())
        })
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("no D-Bus session"),
            "{error:#}"
        );
    }

    #[test]
    fn a_busy_device_is_retried_only_after_taking_the_card_from_someone() {
        let attempts = Cell::new(0);
        let flaky = |attempts: &Cell<u32>| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(busy())
            } else {
                Ok(())
            }
        };

        let reserver = FakeReserver::new();
        reserver.script(Ok(true));
        let slot = ReservationSlot::new(reserver);
        open_reserved_with(
            Some(&slot),
            Some((2, "hw:2,0")),
            5,
            Duration::ZERO,
            &mut || flaky(&attempts),
        )
        .unwrap();
        assert_eq!(
            attempts.get(),
            3,
            "took over: retried until the holder let go"
        );

        attempts.set(0);
        let reserver = FakeReserver::new();
        let slot = ReservationSlot::new(reserver);
        let error = open_reserved_with(
            Some(&slot),
            Some((2, "hw:2,0")),
            5,
            Duration::ZERO,
            &mut || flaky(&attempts),
        )
        .unwrap_err();
        assert_eq!(
            attempts.get(),
            1,
            "card was free: a busy device is somebody else's, no retry"
        );
        assert!(error.downcast_ref::<DeviceBusy>().is_some());
    }

    #[test]
    fn a_device_that_stays_busy_after_a_takeover_says_so() {
        let reserver = FakeReserver::new();
        reserver.script(Ok(true));
        let slot = ReservationSlot::new(reserver);
        let error = open_reserved_with(
            Some(&slot),
            Some((2, "hw:2,0")),
            2,
            Duration::ZERO,
            &mut || -> Result<()> { Err(busy()) },
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("does not use device reservation"),
            "{error:#}"
        );
    }
}
