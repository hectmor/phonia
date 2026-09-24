//! Reserving a sound card over D-Bus, with the `org.freedesktop.ReserveDevice1` protocol that
//! WirePlumber and PulseAudio use to share cards.
//!
//! Whoever owns the bus name `org.freedesktop.ReserveDevice1.Audio<N>` may use card `N`. To take
//! a card that someone holds, phonia calls `RequestRelease` on the holder and, if it agrees,
//! takes the name over. While it holds the name it answers `RequestRelease` itself, through the
//! handler the engine installs. Dropping the reservation closes the connection, which frees the
//! name, and WirePlumber then takes the card back.
//!
//! Everything here is async on the runtime it is given, and [`DeviceReserver::acquire`] blocks on
//! it, which is fine on the audio thread (a plain OS thread) and inside `spawn_blocking`, and
//! panics in an async task.

use super::ReleaseRequest;
use super::reserve::{DeviceReserver, Reservation, ReserveError};
use crate::output::ReleaseHandler;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Handle;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::BusName;
use zbus::{Connection, Proxy, connection, interface};

const NAME_PREFIX: &str = "org.freedesktop.ReserveDevice1.Audio";
const PATH_PREFIX: &str = "/org/freedesktop/ReserveDevice1/Audio";
const INTERFACE: &str = "org.freedesktop.ReserveDevice1";

/// How long a holder has to answer `RequestRelease`.
const HOLDER_ANSWER_TIMEOUT: Duration = Duration::from_secs(3);
/// After the holder agrees, the name may take a moment to become free.
const TAKE_RETRIES: u32 = 10;
const TAKE_RETRY_DELAY: Duration = Duration::from_millis(100);

type SharedHandler = Arc<Mutex<Option<ReleaseHandler>>>;

pub struct DbusReserver {
    rt: Handle,
    priority: i32,
    handler: SharedHandler,
    /// A bus other than the user's session bus; only tests use it.
    bus_address: Option<String>,
}

impl DbusReserver {
    /// `rt` runs the connection; `priority` is what phonia claims when asking for a card.
    pub fn new(rt: Handle, priority: i32) -> Self {
        Self { rt, priority, handler: Arc::new(Mutex::new(None)), bus_address: None }
    }

    /// Reserve on the bus at `address` instead of the session bus.
    pub fn on_bus(mut self, address: impl Into<String>) -> Self {
        self.bus_address = Some(address.into());
        self
    }

    async fn connect(&self, card: u32, device_name: &str) -> Result<Connection, ReserveError> {
        let builder = match &self.bus_address {
            Some(address) => connection::Builder::address(address.as_str()),
            None => connection::Builder::session(),
        }
        .map_err(|error| ReserveError::NoBus(error.to_string()))?;
        let object = Reserve {
            card,
            application_name: "phonia".to_string(),
            device_name: device_name.to_string(),
            priority: self.priority,
            handler: self.handler.clone(),
        };
        // The object is exported before the name is requested, so a request that arrives right
        // after we own the name is never lost.
        builder
            .serve_at(format!("{PATH_PREFIX}{card}"), object)
            .map_err(|error| ReserveError::Failed(error.to_string()))?
            .build()
            .await
            .map_err(|error| ReserveError::NoBus(error.to_string()))
    }

    async fn acquire_async(&self, card: u32, device_name: &str) -> Result<Box<dyn Reservation>, ReserveError> {
        let connection = self.connect(card, device_name).await?;
        let name = format!("{NAME_PREFIX}{card}");

        let took_over = match request(&connection, &name, false).await? {
            Taken::Yes => false,
            Taken::No => {
                self.take_from_holder(&connection, card, &name, device_name).await?;
                true
            }
        };
        Ok(Box::new(DbusReservation { _connection: connection, took_over }))
    }

    /// Someone else has the card: ask them to let go, then take the name.
    async fn take_from_holder(
        &self,
        connection: &Connection,
        card: u32,
        name: &str,
        device_name: &str,
    ) -> Result<(), ReserveError> {
        let path = format!("{PATH_PREFIX}{card}");
        let holder = Proxy::new(connection, name.to_string(), path, INTERFACE)
            .await
            .map_err(|error| ReserveError::Failed(error.to_string()))?;
        // Best effort: only for the messages.
        let holder_name = holder
            .get_property::<String>("ApplicationName")
            .await
            .unwrap_or_else(|_| "another program".to_string());
        let holder_priority = holder.get_property::<i32>("Priority").await.ok();

        let answer = tokio::time::timeout(
            HOLDER_ANSWER_TIMEOUT,
            holder.call::<_, _, bool>("RequestRelease", &(self.priority,)),
        )
        .await;
        match answer {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                return Err(ReserveError::Refused {
                    device: device_name.to_string(),
                    holder: holder_name,
                    priority: holder_priority,
                });
            }
            Ok(Err(_)) | Err(_) => {
                return Err(ReserveError::Unresponsive { device: device_name.to_string(), holder: holder_name });
            }
        }

        for _ in 0..TAKE_RETRIES {
            if let Taken::Yes = request(connection, name, true).await? {
                return Ok(());
            }
            tokio::time::sleep(TAKE_RETRY_DELAY).await;
        }
        Err(ReserveError::Failed(format!(
            "{holder_name} agreed to release {device_name}, but the card is still reserved"
        )))
    }
}

enum Taken {
    /// We own the name now.
    Yes,
    /// Someone else does.
    No,
}

/// Asks the bus for `name`. Never queues, and never allows anyone to replace us; `replace` takes
/// it from a holder that allows that.
async fn request(connection: &Connection, name: &str, replace: bool) -> Result<Taken, ReserveError> {
    let flags = if replace {
        RequestNameFlags::DoNotQueue | RequestNameFlags::ReplaceExisting
    } else {
        RequestNameFlags::DoNotQueue.into()
    };
    match connection.request_name_with_flags(name, flags).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Ok(Taken::Yes),
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => Ok(Taken::No),
        Err(error) => Err(ReserveError::Failed(error.to_string())),
    }
}

impl DeviceReserver for DbusReserver {
    fn acquire(&self, card: u32, device_name: &str) -> Result<Box<dyn Reservation>, ReserveError> {
        self.rt.block_on(self.acquire_async(card, device_name))
    }

    fn on_release_request(&self, handler: ReleaseHandler) {
        *self.handler.lock().unwrap() = Some(handler);
    }
}

/// Holds the bus connection, and with it the name. Dropping it gives the card back.
struct DbusReservation {
    _connection: Connection,
    took_over: bool,
}

impl Reservation for DbusReservation {
    fn took_over(&self) -> bool {
        self.took_over
    }
}

/// What other programs see of phonia's reservation.
struct Reserve {
    card: u32,
    application_name: String,
    device_name: String,
    priority: i32,
    handler: SharedHandler,
}

#[interface(name = "org.freedesktop.ReserveDevice1")]
impl Reserve {
    /// Another program wants the card. Answers whether phonia has given it up, which it has done
    /// (and the name is free) by the time the answer goes out.
    async fn request_release(
        &self,
        priority: i32,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> bool {
        let handler = self.handler.lock().unwrap().clone();
        let Some(handler) = handler else { return false };

        let by = match header.sender() {
            Some(sender) => process_name(connection, BusName::from(sender.to_owned())).await,
            None => None,
        };
        // The engine answers from its audio thread, which can take a moment.
        let granted = tokio::task::spawn_blocking(move || handler(ReleaseRequest { by, priority }))
            .await
            .unwrap_or(false);
        if granted {
            // Normally the engine has dropped the reservation already; this makes sure the name
            // is free before the requester hears the answer.
            let _ = connection.release_name(format!("{NAME_PREFIX}{}", self.card)).await;
        }
        granted
    }

    #[zbus(property)]
    fn priority(&self) -> i32 {
        self.priority
    }

    #[zbus(property)]
    fn application_name(&self) -> String {
        self.application_name.clone()
    }

    #[zbus(property)]
    fn application_device_name(&self) -> String {
        self.device_name.clone()
    }
}

/// The name of the program behind a bus connection, for the messages.
pub(crate) async fn process_name(connection: &Connection, peer: BusName<'static>) -> Option<String> {
    let bus = DBusProxy::new(connection).await.ok()?;
    let pid = bus.get_connection_unix_process_id(peer).await.ok()?;
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(name.trim().to_string())
}

/// These talk to a private `dbus-daemon`, never to the user's bus, so they can pretend to be
/// WirePlumber. They are ignored by default because they need the binary:
/// `cargo test -p phonia-core dbus -- --ignored`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reserve::PRIORITY;

    use crate::testutil::Bus;

    /// What WirePlumber does: owns the card's name, allows it to be replaced, and answers
    /// `RequestRelease` by letting go (or not).
    struct Holder {
        name: String,
        agree: bool,
    }

    #[interface(name = "org.freedesktop.ReserveDevice1")]
    impl Holder {
        async fn request_release(&self, _priority: i32, #[zbus(connection)] connection: &Connection) -> bool {
            if self.agree {
                connection.release_name(self.name.clone()).await.unwrap();
            }
            self.agree
        }

        #[zbus(property)]
        fn priority(&self) -> i32 {
            -20
        }

        #[zbus(property)]
        fn application_name(&self) -> String {
            "WirePlumber".to_string()
        }

        #[zbus(property)]
        fn application_device_name(&self) -> String {
            "alsa_card.fake".to_string()
        }
    }

    async fn client(bus: &Bus) -> Connection {
        connection::Builder::address(bus.address.as_str()).unwrap().build().await.unwrap()
    }

    async fn holder(bus: &Bus, card: u32, agree: bool) -> Connection {
        let name = format!("{NAME_PREFIX}{card}");
        let connection = connection::Builder::address(bus.address.as_str())
            .unwrap()
            .serve_at(format!("{PATH_PREFIX}{card}"), Holder { name: name.clone(), agree })
            .unwrap()
            .build()
            .await
            .unwrap();
        let flags = RequestNameFlags::AllowReplacement | RequestNameFlags::DoNotQueue;
        connection.request_name_with_flags(name, flags).await.unwrap();
        connection
    }

    async fn has_owner(connection: &Connection, card: u32) -> bool {
        let name = format!("{NAME_PREFIX}{card}");
        DBusProxy::new(connection).await.unwrap().name_has_owner(name.as_str().try_into().unwrap()).await.unwrap()
    }

    /// Waits up to a second for the name to be free: the bus notices a closed connection a moment
    /// after it happens.
    async fn becomes_free(connection: &Connection, card: u32) -> bool {
        for _ in 0..20 {
            if !has_owner(connection, card).await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    fn reserver(rt: &tokio::runtime::Runtime, bus: &Bus) -> DbusReserver {
        DbusReserver::new(rt.handle().clone(), PRIORITY).on_bus(bus.address.clone())
    }

    #[test]
    #[ignore = "needs dbus-daemon"]
    fn a_free_card_is_reserved_and_freed_when_the_reservation_is_dropped() {
        let (rt, bus) = (tokio::runtime::Runtime::new().unwrap(), Bus::start());
        let observer = rt.block_on(client(&bus));

        let reservation = reserver(&rt, &bus).acquire(3, "hw:DS2,0").unwrap();
        assert!(!reservation.took_over());
        assert!(rt.block_on(has_owner(&observer, 3)));

        drop(reservation);
        assert!(rt.block_on(becomes_free(&observer, 3)), "dropping the reservation frees the name");
    }

    #[test]
    #[ignore = "needs dbus-daemon"]
    fn a_holder_that_agrees_gives_the_card_up() {
        let (rt, bus) = (tokio::runtime::Runtime::new().unwrap(), Bus::start());
        let wireplumber = rt.block_on(holder(&bus, 3, true));

        let reservation = reserver(&rt, &bus).acquire(3, "hw:DS2,0").unwrap();
        assert!(reservation.took_over());
        assert!(rt.block_on(has_owner(&wireplumber, 3)), "and phonia owns the name now");

        drop(reservation);
        assert!(rt.block_on(becomes_free(&wireplumber, 3)));
    }

    #[test]
    #[ignore = "needs dbus-daemon"]
    fn a_holder_that_refuses_is_named_in_the_error() {
        let (rt, bus) = (tokio::runtime::Runtime::new().unwrap(), Bus::start());
        let wireplumber = rt.block_on(holder(&bus, 3, false));

        let Err(error) = reserver(&rt, &bus).acquire(3, "hw:DS2,0") else { panic!("the card was not free") };
        let text = error.to_string();
        assert!(matches!(error, ReserveError::Refused { priority: Some(-20), .. }), "{text}");
        assert!(text.contains("WirePlumber") && text.contains("hw:DS2,0"), "{text}");
        assert!(rt.block_on(has_owner(&wireplumber, 3)), "the holder keeps the card");
    }

    #[test]
    #[ignore = "needs dbus-daemon"]
    fn a_program_that_matters_more_gets_the_card_before_the_answer_arrives() {
        let (rt, bus) = (tokio::runtime::Runtime::new().unwrap(), Bus::start());
        let requester = rt.block_on(client(&bus));
        let dbus = reserver(&rt, &bus);

        // The engine's side: on a request, drop the reservation (what `release` does) and say yes.
        let held: Arc<Mutex<Option<Box<dyn Reservation>>>> = Arc::new(Mutex::new(None));
        let asked: Arc<Mutex<Option<ReleaseRequest>>> = Arc::new(Mutex::new(None));
        let (engine_held, engine_asked) = (held.clone(), asked.clone());
        dbus.on_release_request(Arc::new(move |request| {
            *engine_asked.lock().unwrap() = Some(request);
            engine_held.lock().unwrap().take();
            true
        }));
        *held.lock().unwrap() = Some(dbus.acquire(3, "hw:DS2,0").unwrap());

        let granted = rt.block_on(async {
            let phonia = Proxy::new(&requester, format!("{NAME_PREFIX}3"), format!("{PATH_PREFIX}3"), INTERFACE)
                .await
                .unwrap();
            let name = phonia.get_property::<String>("ApplicationName").await.unwrap();
            assert_eq!(name, "phonia");
            let granted = phonia.call::<_, _, bool>("RequestRelease", &(100i32,)).await.unwrap();
            assert!(!has_owner(&requester, 3).await, "the name is free by the time the answer is heard");
            granted
        });
        assert!(granted);
        let request = asked.lock().unwrap().take().unwrap();
        assert_eq!(request.priority, 100);
        assert!(request.by.is_some(), "the requester is named from its process");
    }

    #[test]
    #[ignore = "needs dbus-daemon"]
    fn without_a_handler_a_request_is_refused() {
        let (rt, bus) = (tokio::runtime::Runtime::new().unwrap(), Bus::start());
        let requester = rt.block_on(client(&bus));
        let reservation = reserver(&rt, &bus).acquire(3, "hw:DS2,0").unwrap();

        let granted = rt.block_on(async {
            let phonia = Proxy::new(&requester, format!("{NAME_PREFIX}3"), format!("{PATH_PREFIX}3"), INTERFACE)
                .await
                .unwrap();
            phonia.call::<_, _, bool>("RequestRelease", &(100i32,)).await.unwrap()
        });
        assert!(!granted);
        drop(reservation);
    }

    #[test]
    #[ignore = "needs dbus-daemon"]
    fn a_blocking_task_on_the_same_runtime_can_reserve() {
        // What `phonia probe-device` does: reserve from `spawn_blocking`.
        let (rt, bus) = (tokio::runtime::Runtime::new().unwrap(), Bus::start());
        let reserver = reserver(&rt, &bus);
        let reservation = rt.block_on(async { tokio::task::spawn_blocking(move || reserver.acquire(3, "hw:DS2,0")).await.unwrap() });
        assert!(reservation.is_ok());
    }

    #[test]
    fn no_session_bus_is_reported_as_such() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reserver = DbusReserver::new(rt.handle().clone(), PRIORITY).on_bus("unix:path=/nonexistent/phonia-bus");
        assert!(matches!(reserver.acquire(3, "hw:DS2,0"), Err(ReserveError::NoBus(_))));
    }
}
