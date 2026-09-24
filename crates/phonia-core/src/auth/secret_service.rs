//! The session in the desktop keyring, through the freedesktop Secret Service D-Bus API that
//! GNOME Keyring, KWallet and KeePassXC all serve (`org.freedesktop.secrets`).
//!
//! The API is small and phonia already has a D-Bus client for the sound card, so this speaks it
//! directly instead of pulling in a crate: open a session, find the item by its attributes,
//! unlock it if the keyring is locked (which may show a prompt), read or write its secret.
//!
//! The session is opened with the `plain` algorithm, that is, the secret crosses the bus
//! unencrypted. That costs nothing: the Secret Service does not check which program asks, so any
//! process of yours can read an unlocked keyring, and can also watch your session bus. What the
//! keyring does buy is encryption on disk (behind your login password), a session that does not
//! travel in backups or synced dotfiles, and a collection that locks when you log out.

use super::store::{SessionStore, StoreError, StoredSession};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type, Value};
use zbus::{Connection, Proxy, connection};

const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_INTERFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_INTERFACE: &str = "org.freedesktop.Secret.Item";
const PROMPT_INTERFACE: &str = "org.freedesktop.Secret.Prompt";
/// What the API answers when there is no object (no default collection, no prompt needed).
const NOTHING: &str = "/";
/// A person has to type a password, which takes as long as it takes, within reason.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
/// The first call is where D-Bus starts the keyring's daemon if it is not running yet, which is
/// quick when it works and takes minutes to fail when it doesn't.
const SERVICE_TIMEOUT: Duration = Duration::from_secs(15);

const LABEL_PROPERTY: &str = "org.freedesktop.Secret.Item.Label";
const ATTRIBUTES_PROPERTY: &str = "org.freedesktop.Secret.Item.Attributes";

/// Whether a locked keyring may ask the user to unlock it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interaction {
    /// Show the prompt and wait for it. What a command the user typed does.
    Allow,
    /// Fail with [`StoreError::Locked`] instead. What a daemon does at startup, with nobody there.
    Never,
}

/// The wire form of a secret: the session it is for, the algorithm's parameters (none for `plain`),
/// the value and its content type.
#[derive(Serialize, Deserialize, Type)]
struct Secret {
    session: OwnedObjectPath,
    parameters: Vec<u8>,
    value: Vec<u8>,
    content_type: String,
}

pub struct SecretServiceStore {
    interaction: Interaction,
    /// A bus other than the user's session bus; only tests use it.
    bus_address: Option<String>,
}

impl SecretServiceStore {
    pub fn new(interaction: Interaction) -> Self {
        Self { interaction, bus_address: None }
    }

    /// Talk to the service on the bus at `address` instead of the session bus.
    pub fn on_bus(mut self, address: impl Into<String>) -> Self {
        self.bus_address = Some(address.into());
        self
    }

    /// The program that provides the service (`gnome-keyring-d`, `ksecretd`...), if it can be found.
    pub async fn provider(&self) -> Option<String> {
        let connection = self.connect().await.ok()?;
        let bus = zbus::fdo::DBusProxy::new(&connection).await.ok()?;
        let owner = bus.get_name_owner(SERVICE.try_into().ok()?).await.ok()?;
        crate::output::dbus::process_name(&connection, zbus::names::BusName::from(owner.into_inner())).await
    }

    async fn connect(&self) -> Result<Connection, StoreError> {
        let builder = match &self.bus_address {
            Some(address) => connection::Builder::address(address.as_str()),
            None => connection::Builder::session(),
        }
        .map_err(|error| StoreError::Unavailable(format!("no D-Bus session bus: {error}")))?;
        builder.build().await.map_err(|error| StoreError::Unavailable(format!("no D-Bus session bus: {error}")))
    }

    async fn open(&self) -> Result<Open, StoreError> {
        let connection = self.connect().await?;
        let service = proxy(&connection, SERVICE_PATH, SERVICE_INTERFACE).await?;
        let (_, session): (OwnedValue, OwnedObjectPath) =
            tokio::time::timeout(SERVICE_TIMEOUT, service.call("OpenSession", &("plain", Value::from(""))))
                .await
                .map_err(|_| {
                    StoreError::Unavailable(format!(
                        "the keyring did not answer within {} s (is its daemon stuck?)",
                        SERVICE_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(reply_error)?;
        Ok(Open { connection, service, session, interaction: self.interaction })
    }
}

/// A connection with an open session. Dropping it closes both.
struct Open {
    connection: Connection,
    service: Proxy<'static>,
    session: OwnedObjectPath,
    interaction: Interaction,
}

impl Open {
    /// The collection new items go to: the user's default one.
    async fn default_collection(&self) -> Result<OwnedObjectPath, StoreError> {
        let path: OwnedObjectPath = self.service.call("ReadAlias", &("default",)).await.map_err(reply_error)?;
        if path.as_str() == NOTHING {
            return Err(StoreError::Unavailable(
                "the keyring has no default collection; create one (Seahorse, KDE Wallet Manager...)".to_string(),
            ));
        }
        Ok(path)
    }

    /// Items with phonia's attributes, unlocked. A locked keyring is unlocked first.
    async fn find(&self) -> Result<Vec<OwnedObjectPath>, StoreError> {
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
            self.service.call("SearchItems", &(attributes(),)).await.map_err(reply_error)?;
        let mut found = unlocked;
        if !locked.is_empty() {
            found.extend(self.unlock(locked).await?);
        }
        Ok(found)
    }

    /// Unlocks `objects`, asking the user if that takes a prompt and that is allowed. Returns the
    /// ones that are unlocked afterwards.
    async fn unlock(&self, objects: Vec<OwnedObjectPath>) -> Result<Vec<OwnedObjectPath>, StoreError> {
        let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
            self.service.call("Unlock", &(&objects,)).await.map_err(reply_error)?;
        if prompt.as_str() == NOTHING {
            return Ok(unlocked);
        }
        if self.interaction == Interaction::Never {
            return Err(StoreError::Locked(
                "the keyring is locked and unlocking it needs a prompt, which is not possible here".to_string(),
            ));
        }
        self.run_prompt(&prompt).await?;
        // The prompt reports what it unlocked, but asking again is simpler than decoding that.
        let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
            self.service.call("Unlock", &(&objects,)).await.map_err(reply_error)?;
        if prompt.as_str() != NOTHING || unlocked.is_empty() {
            return Err(StoreError::Locked("the keyring is still locked".to_string()));
        }
        Ok(unlocked)
    }

    /// Shows a prompt and waits for the user to finish with it.
    async fn run_prompt(&self, prompt: &OwnedObjectPath) -> Result<(), StoreError> {
        if prompt.as_str() == NOTHING {
            return Ok(());
        }
        if self.interaction == Interaction::Never {
            return Err(StoreError::Locked("the keyring needs a prompt, which is not possible here".to_string()));
        }
        let prompt = proxy(&self.connection, prompt.as_str(), PROMPT_INTERFACE).await?;
        // Listen before asking, or the answer could arrive first.
        let mut completed = prompt.receive_signal("Completed").await.map_err(reply_error)?;
        prompt.call_method("Prompt", &("",)).await.map_err(reply_error)?;
        let message = tokio::time::timeout(PROMPT_TIMEOUT, completed.next())
            .await
            .map_err(|_| StoreError::Locked("nobody answered the keyring's prompt".to_string()))?
            .ok_or_else(|| StoreError::Unavailable("the keyring went away while it was asking".to_string()))?;
        let (dismissed, _): (bool, OwnedValue) =
            message.body().deserialize().map_err(|error| StoreError::Other(error.to_string()))?;
        if dismissed { Err(StoreError::Locked("the keyring's prompt was dismissed".to_string())) } else { Ok(()) }
    }

    async fn read(&self, item: &OwnedObjectPath) -> Result<StoredSession, StoreError> {
        let item = proxy(&self.connection, item.as_str(), ITEM_INTERFACE).await?;
        let secret: Secret = item.call("GetSecret", &(&self.session,)).await.map_err(reply_error)?;
        serde_json::from_slice(&secret.value)
            .map_err(|error| StoreError::Corrupt(format!("the keyring's item is not a phonia session: {error}")))
    }
}

async fn proxy(connection: &Connection, path: &str, interface: &str) -> Result<Proxy<'static>, StoreError> {
    Proxy::new(connection, SERVICE, path.to_string(), interface.to_string()).await.map_err(reply_error)
}

/// What identifies phonia's item. The label is for the person looking at it in a keyring manager.
fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", "phonia"), ("service", "tidal")])
}

fn reply_error(error: zbus::Error) -> StoreError {
    let name = match &error {
        zbus::Error::MethodError(name, ..) => Some(name.as_str()),
        _ => None,
    };
    match name {
        Some("org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.NameHasNoOwner") => {
            StoreError::Unavailable("nothing on the session bus provides org.freedesktop.secrets".to_string())
        }
        _ => StoreError::Other(format!("the keyring answered: {error}")),
    }
}

impl SessionStore for SecretServiceStore {
    fn describe(&self) -> String {
        "the desktop keyring".to_string()
    }

    fn provider(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(SecretServiceStore::provider(self))
    }

    fn load(&self) -> BoxFuture<'_, Result<Option<StoredSession>, StoreError>> {
        Box::pin(async move {
            let open = self.open().await?;
            match open.find().await?.first() {
                Some(item) => open.read(item).await.map(Some),
                None => Ok(None),
            }
        })
    }

    fn save<'a>(&'a self, session: &'a StoredSession) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let open = self.open().await?;
            let collection = open.default_collection().await?;
            let collection = proxy(&open.connection, collection.as_str(), COLLECTION_INTERFACE).await?;
            if collection.get_property::<bool>("Locked").await.unwrap_or(false) {
                let path = OwnedObjectPath::try_from(collection.path().as_str())
                    .map_err(|error| StoreError::Other(error.to_string()))?;
                open.unlock(vec![path]).await?;
            }

            let properties: HashMap<&str, Value<'_>> = HashMap::from([
                (LABEL_PROPERTY, Value::from("phonia: TIDAL session")),
                (ATTRIBUTES_PROPERTY, Value::from(attributes())),
            ]);
            let secret = Secret {
                session: open.session.clone(),
                parameters: Vec::new(),
                value: serde_json::to_vec(session).map_err(|error| StoreError::Other(error.to_string()))?,
                content_type: "application/json".to_string(),
            };
            // `replace` makes a second login update the item instead of adding another.
            let (_item, prompt): (OwnedObjectPath, OwnedObjectPath) =
                collection.call("CreateItem", &(properties, secret, true)).await.map_err(reply_error)?;
            open.run_prompt(&prompt).await
        })
    }

    fn delete(&self) -> BoxFuture<'_, Result<bool, StoreError>> {
        Box::pin(async move {
            let open = self.open().await?;
            let items = open.find().await?;
            for item in &items {
                let item = proxy(&open.connection, item.as_str(), ITEM_INTERFACE).await?;
                let prompt: OwnedObjectPath = item.call("Delete", &()).await.map_err(reply_error)?;
                open.run_prompt(&prompt).await?;
            }
            Ok(!items.is_empty())
        })
    }
}

/// These talk to a fake Secret Service on a private `dbus-daemon`, never to the user's keyring.
/// They are ignored by default because they need the binary:
/// `cargo test -p phonia-core secret_service -- --ignored`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{MigratingStore, FileStore};
    use crate::testutil::Bus;
    use std::sync::{Arc, Mutex};
    use zbus::fdo;
    use zbus::object_server::{InterfaceRef, SignalEmitter};
    use zbus::{ObjectServer, interface};

    const COLLECTION: &str = "/org/freedesktop/secrets/collection/login";

    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    enum Unlocking {
        /// Unlocking needs no prompt.
        #[default]
        Silent,
        Prompt,
        DismissedPrompt,
    }

    #[derive(Clone)]
    struct Stored {
        id: u32,
        label: String,
        attributes: HashMap<String, String>,
        value: Vec<u8>,
        content_type: String,
    }

    #[derive(Default)]
    struct State {
        locked: bool,
        unlocking: Unlocking,
        no_default: bool,
        items: Vec<Stored>,
        next_id: u32,
    }

    type Shared = Arc<Mutex<State>>;

    fn item_path(id: u32) -> OwnedObjectPath {
        OwnedObjectPath::try_from(format!("{COLLECTION}/{id}")).unwrap()
    }

    fn nothing() -> OwnedObjectPath {
        OwnedObjectPath::try_from(NOTHING).unwrap()
    }

    struct FakeService(Shared);
    struct FakeCollection(Shared);
    struct FakeItem(Shared, u32);
    struct FakePrompt(Shared);

    /// Makes the object for an item exist, as a real service would for each item it holds.
    async fn expose(server: &ObjectServer, state: &Shared, id: u32) {
        let _ = server.at(item_path(id).as_str(), FakeItem(state.clone(), id)).await;
    }

    #[interface(name = "org.freedesktop.Secret.Service")]
    impl FakeService {
        async fn open_session(&self, algorithm: &str, _input: Value<'_>) -> fdo::Result<(OwnedValue, OwnedObjectPath)> {
            if algorithm != "plain" {
                return Err(fdo::Error::NotSupported("only plain".into()));
            }
            let output = OwnedValue::try_from(Value::from("")).unwrap();
            Ok((output, OwnedObjectPath::try_from("/org/freedesktop/secrets/session/1").unwrap()))
        }

        fn read_alias(&self, name: &str) -> OwnedObjectPath {
            if name == "default" && !self.0.lock().unwrap().no_default {
                OwnedObjectPath::try_from(COLLECTION).unwrap()
            } else {
                nothing()
            }
        }

        async fn search_items(
            &self,
            attributes: HashMap<String, String>,
            #[zbus(object_server)] server: &ObjectServer,
        ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
            let (locked, matching): (bool, Vec<u32>) = {
                let state = self.0.lock().unwrap();
                let ids = state
                    .items
                    .iter()
                    .filter(|item| attributes.iter().all(|(k, v)| item.attributes.get(k) == Some(v)))
                    .map(|item| item.id)
                    .collect();
                (state.locked, ids)
            };
            let mut paths = Vec::new();
            for id in matching {
                expose(server, &self.0, id).await;
                paths.push(item_path(id));
            }
            if locked { (Vec::new(), paths) } else { (paths, Vec::new()) }
        }

        fn unlock(&self, objects: Vec<OwnedObjectPath>) -> (Vec<OwnedObjectPath>, OwnedObjectPath) {
            let mut state = self.0.lock().unwrap();
            if !state.locked {
                return (objects, nothing());
            }
            match state.unlocking {
                Unlocking::Silent => {
                    state.locked = false;
                    (objects, nothing())
                }
                Unlocking::Prompt | Unlocking::DismissedPrompt => {
                    (Vec::new(), OwnedObjectPath::try_from("/org/freedesktop/secrets/prompt/1").unwrap())
                }
            }
        }
    }

    #[interface(name = "org.freedesktop.Secret.Collection")]
    impl FakeCollection {
        #[zbus(property)]
        fn locked(&self) -> bool {
            self.0.lock().unwrap().locked
        }

        async fn create_item(
            &self,
            properties: HashMap<String, OwnedValue>,
            secret: Secret,
            replace: bool,
            #[zbus(object_server)] server: &ObjectServer,
        ) -> (OwnedObjectPath, OwnedObjectPath) {
            let attributes: HashMap<String, String> =
                properties[ATTRIBUTES_PROPERTY].clone().try_into().expect("attributes are a{ss}");
            let label: String = properties[LABEL_PROPERTY].clone().try_into().expect("the label is a string");
            let id = {
                let mut state = self.0.lock().unwrap();
                if replace {
                    state.items.retain(|item| item.attributes != attributes);
                }
                state.next_id += 1;
                let id = state.next_id;
                state.items.push(Stored {
                    id,
                    label,
                    attributes,
                    value: secret.value,
                    content_type: secret.content_type,
                });
                id
            };
            expose(server, &self.0, id).await;
            (item_path(id), nothing())
        }
    }

    #[interface(name = "org.freedesktop.Secret.Item")]
    impl FakeItem {
        fn get_secret(&self, session: OwnedObjectPath) -> Secret {
            let state = self.0.lock().unwrap();
            let item = state.items.iter().find(|item| item.id == self.1).expect("the item exists");
            Secret {
                session,
                parameters: Vec::new(),
                value: item.value.clone(),
                content_type: item.content_type.clone(),
            }
        }

        fn delete(&self) -> OwnedObjectPath {
            self.0.lock().unwrap().items.retain(|item| item.id != self.1);
            nothing()
        }
    }

    #[interface(name = "org.freedesktop.Secret.Prompt")]
    impl FakePrompt {
        async fn prompt(&self, _window: &str, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
            let dismissed = self.0.lock().unwrap().unlocking == Unlocking::DismissedPrompt;
            if !dismissed {
                self.0.lock().unwrap().locked = false;
            }
            Self::completed(&emitter, dismissed, Value::from("")).await.unwrap();
        }

        #[zbus(signal)]
        async fn completed(emitter: &SignalEmitter<'_>, dismissed: bool, result: Value<'_>) -> zbus::Result<()>;
    }

    /// A private bus with the fake service on it. Keep it alive for the test.
    struct Fake {
        state: Shared,
        bus: Bus,
        _service: Connection,
    }

    impl Fake {
        async fn start(setup: impl FnOnce(&mut State)) -> Fake {
            let bus = Bus::start();
            let mut initial = State::default();
            setup(&mut initial);
            let state: Shared = Arc::new(Mutex::new(initial));
            let service = connection::Builder::address(bus.address.as_str())
                .unwrap()
                .serve_at(SERVICE_PATH, FakeService(state.clone()))
                .unwrap()
                .serve_at(COLLECTION, FakeCollection(state.clone()))
                .unwrap()
                .serve_at("/org/freedesktop/secrets/prompt/1", FakePrompt(state.clone()))
                .unwrap()
                .name(SERVICE)
                .unwrap()
                .build()
                .await
                .unwrap();
            Fake { state, bus, _service: service }
        }

        fn store(&self, interaction: Interaction) -> SecretServiceStore {
            SecretServiceStore::new(interaction).on_bus(self.bus.address.clone())
        }
    }

    fn session() -> StoredSession {
        StoredSession { v: 1, refresh_token: "refresh-secret".into(), client_id: "id".into(), client_secret: "s".into() }
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn a_session_round_trips_through_the_keyring_and_is_replaced_not_duplicated() {
        let fake = Fake::start(|_| {}).await;
        let store = fake.store(Interaction::Never);
        assert_eq!(store.load().await.unwrap(), None);

        store.save(&session()).await.unwrap();
        assert_eq!(store.load().await.unwrap(), Some(session()));
        let newer = StoredSession { refresh_token: "newer".into(), ..session() };
        store.save(&newer).await.unwrap();
        assert_eq!(store.load().await.unwrap(), Some(newer));

        {
            let state = fake.state.lock().unwrap();
            assert_eq!(state.items.len(), 1, "a second save replaces the item");
            let item = &state.items[0];
            assert_eq!(item.attributes, HashMap::from([("application".into(), "phonia".into()), ("service".into(), "tidal".into())]));
            assert_eq!(item.content_type, "application/json");
            assert!(item.label.contains("phonia"));
        }

        assert!(store.delete().await.unwrap());
        assert!(!store.delete().await.unwrap());
        assert_eq!(store.load().await.unwrap(), None);
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn a_locked_keyring_is_unlocked_through_its_prompt_when_that_is_allowed() {
        let fake = Fake::start(|state| {
            state.locked = true;
            state.unlocking = Unlocking::Prompt;
            state.items.push(Stored {
                id: 1,
                label: "phonia".into(),
                attributes: HashMap::from([("application".into(), "phonia".into()), ("service".into(), "tidal".into())]),
                value: serde_json::to_vec(&session()).unwrap(),
                content_type: "application/json".into(),
            });
            state.next_id = 1;
        })
        .await;

        let error = fake.store(Interaction::Never).load().await.unwrap_err();
        assert!(matches!(error, StoreError::Locked(_)), "{error}");

        assert_eq!(fake.store(Interaction::Allow).load().await.unwrap(), Some(session()));
        assert!(!fake.state.lock().unwrap().locked, "the prompt unlocked it");
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn a_dismissed_prompt_is_a_locked_keyring_and_writing_to_one_asks_too() {
        let fake = Fake::start(|state| {
            state.locked = true;
            state.unlocking = Unlocking::DismissedPrompt;
        })
        .await;
        let error = fake.store(Interaction::Allow).save(&session()).await.unwrap_err();
        assert!(matches!(error, StoreError::Locked(_)), "{error}");
        assert!(fake.state.lock().unwrap().items.is_empty(), "nothing was written");
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn saving_into_a_locked_keyring_unlocks_it_first() {
        let fake = Fake::start(|state| {
            state.locked = true;
            state.unlocking = Unlocking::Prompt;
        })
        .await;
        fake.store(Interaction::Allow).save(&session()).await.unwrap();
        assert_eq!(fake.state.lock().unwrap().items.len(), 1);
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn a_keyring_without_a_default_collection_says_so() {
        let fake = Fake::start(|state| state.no_default = true).await;
        let error = fake.store(Interaction::Never).save(&session()).await.unwrap_err();
        assert!(matches!(&error, StoreError::Unavailable(why) if why.contains("default collection")), "{error}");
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn a_bus_with_no_secret_service_is_unavailable() {
        let bus = Bus::start();
        let store = SecretServiceStore::new(Interaction::Never).on_bus(bus.address.clone());
        let error = store.load().await.unwrap_err();
        assert!(matches!(error, StoreError::Unavailable(_)), "{error}");
    }

    #[tokio::test]
    async fn no_bus_at_all_is_unavailable() {
        let store = SecretServiceStore::new(Interaction::Never).on_bus("unix:path=/nonexistent/phonia-bus");
        assert!(matches!(store.load().await, Err(StoreError::Unavailable(_))));
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn something_that_is_not_a_session_in_the_item_is_corrupt() {
        let fake = Fake::start(|state| {
            state.items.push(Stored {
                id: 1,
                label: "x".into(),
                attributes: HashMap::from([("application".into(), "phonia".into()), ("service".into(), "tidal".into())]),
                value: b"not json".to_vec(),
                content_type: "text/plain".into(),
            });
            state.next_id = 1;
        })
        .await;
        let error = fake.store(Interaction::Never).load().await.unwrap_err();
        assert!(matches!(error, StoreError::Corrupt(_)), "{error}");
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn an_old_session_file_moves_into_the_keyring_end_to_end() {
        let fake = Fake::start(|_| {}).await;
        let dir = std::env::temp_dir().join(format!("phonia-ss-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        std::fs::write(&path, serde_json::to_string(&session()).unwrap()).unwrap();

        let store = MigratingStore::new(Arc::new(fake.store(Interaction::Never)), FileStore::new(&path));
        assert_eq!(store.load().await.unwrap(), Some(session()));
        assert!(!path.exists(), "the file is gone");
        assert_eq!(fake.state.lock().unwrap().items.len(), 1, "and the keyring has the session");
    }

    #[tokio::test]
    #[ignore = "needs dbus-daemon"]
    async fn the_provider_of_the_service_is_named() {
        let fake = Fake::start(|_| {}).await;
        let provider = fake.store(Interaction::Never).provider().await;
        assert!(provider.is_some(), "the process behind the bus name is found");
    }

    // Keeps the compiler from flagging the helper used only by some builds.
    #[allow(dead_code)]
    fn _unused(_: InterfaceRef<FakeItem>) {}
}
