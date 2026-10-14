//! Reports a finished play to TIDAL as its own `playback_session` event, so it shows up in
//! Recently Played on the account.
//!
//! There is no simpler, documented "mark this as played" endpoint: TIDAL's official apps send a
//! generic analytics event (the "TIDAL Event Platform") to an ingest endpoint behind AWS SQS.
//! The event's name, shape and transport come from TIDAL's own open-source SDKs
//! (`tidal-sdk-web`'s `event-producer`/`player` packages); the specific body shape used here (the
//! "mobile" shape, with top-level `user`/`client` objects, rather than the plain web shape) was
//! cross-checked against another open-source Linux TIDAL client that reports plays the same way
//! and verified it live against a real account — no code from it is used here, only the protocol
//! facts, the same category of information as the SDK source itself. See `docs/DECISIONS.md` for
//! the sources and the full reasoning.
//!
//! Everything marked `PROVISIONAL (#120)` is a best-effort guess, not a documented contract:
//! TIDAL could change this without notice. A failure here is best-effort and silent — it never
//! affects playback.
//!
//! Only `phoniad` sends these, through the one TIDAL session it owns (`openers::TidalOpener::
//! play_log`), the same as every other call to TIDAL.

use crate::config::Quality;
use crate::engine::EndReason;
use crate::session::TidalSession;
use crate::tidal;
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// TIDAL's own ingest endpoint for client analytics events: an AWS SQS `SendMessageBatch` behind
/// an API Gateway path. Not documented publicly.
pub const EVENT_BATCH_URL: &str = "https://ec.tidal.com/api/event-batch";

/// The most sessions that may ride in one batch (SQS's own `SendMessageBatch` limit, which
/// TIDAL's SDKs also use as their own batch size).
const MAX_BATCH: usize = 10;

/// A play counts once this much has actually been heard (wall-clock time spent in `Playing`,
/// excluding any paused span), whatever `EndReason` it ends with and however long the track is.
/// TIDAL's own rule, not a heuristic of phonia's.
pub const MIN_HEARD: Duration = Duration::from_secs(30);

/// PROVISIONAL (#120): the client identity phonia's events claim to be. Events ride on the
/// access token's own client id — the PKCE client `tidlers` authenticates with is a native
/// (Android-type), not a browser, client — so the event must describe that kind of client, not
/// phonia itself: nothing here may name phonia or carry its version. Bump the app version
/// occasionally; it drifts as TIDAL ships new releases.
const APP_VERSION: &str = "2.145.0";
const OS_NAME: &str = "Android";
const OS_VERSION: &str = "14";
const DEVICE_MODEL: &str = "Pixel 8";
const DEVICE_VENDOR: &str = "Google";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- JWT claims -------------------------------------------------------------------------------

/// The claims needed to attribute an event to the account: decoded from the access token itself,
/// not asked of TIDAL. No signature check — the token already proved itself by being accepted
/// when it was used to authenticate the request that is about to carry it.
#[derive(Debug, Clone, Default)]
struct Claims {
    uid: Option<u64>,
    cid: Option<u64>,
    sid: Option<String>,
}

fn claims(access_token: &str) -> Claims {
    let Some(segment) = access_token.split('.').nth(1) else {
        return Claims::default();
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(segment)
        .or_else(|_| URL_SAFE.decode(segment));
    let Ok(bytes) = decoded else {
        return Claims::default();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Claims::default();
    };
    let as_u64 = |key: &str| match value.get(key) {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    };
    Claims {
        uid: as_u64("uid"),
        cid: as_u64("cid"),
        sid: value.get("sid").and_then(|v| v.as_str()).map(String::from),
    }
}

// ---- The event itself ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionType {
    PlaybackStart,
    PlaybackStop,
}

impl ActionType {
    fn as_api(self) -> &'static str {
        match self {
            ActionType::PlaybackStart => "PLAYBACK_START",
            ActionType::PlaybackStop => "PLAYBACK_STOP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Action {
    kind: ActionType,
    position: Duration,
    at_ms: i64,
}

/// The facts needed to report one finished play. Built by [`SessionTracker`]; sent by
/// [`PlayLog::send`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackSession {
    id: String,
    product_id: String,
    quality: Option<Quality>,
    audio_mode: Option<String>,
    start_position: Duration,
    start_ms: i64,
    end_position: Duration,
    end_ms: i64,
    actions: Vec<Action>,
}

/// The `payload` object: what TIDAL's own `playback_session` event (`group: "play_log"`) carries.
///
/// `sourceType`/`sourceId` are always `ITEM` + the track's own id: phonia has no plumbing today
/// to tell a play came from an album, a playlist or a mix, and a play reported with no source at
/// all is accepted but never produces a Recently Played row — `ITEM` + the track id is the
/// smallest shape that does. Richer attribution is a seam for a future issue.
fn payload(session: &PlaybackSession) -> Value {
    let mut value = json!({
        "actions": session.actions.iter().map(|action| json!({
            "actionType": action.kind.as_api(),
            "assetPosition": action.position.as_secs_f64(),
            "timestamp": action.at_ms,
        })).collect::<Vec<_>>(),
        "actualAssetPresentation": "FULL",
        "actualProductId": session.product_id,
        "endAssetPosition": session.end_position.as_secs_f64(),
        "endTimestamp": session.end_ms,
        "isPostPaywall": true,
        "playbackSessionId": session.id,
        "productType": "TRACK",
        "requestedProductId": session.product_id,
        "sourceId": session.product_id,
        "sourceType": "ITEM",
        "startAssetPosition": session.start_position.as_secs_f64(),
        "startTimestamp": session.start_ms,
    });
    if let Some(quality) = session.quality {
        value["actualQuality"] = json!(tidal::api_quality(quality));
    }
    if let Some(mode) = &session.audio_mode {
        value["actualAudioMode"] = json!(mode);
    }
    value
}

/// The MessageBody JSON for one event: the mobile/Android shape (`user`+`client` objects), which
/// is what actually surfaces a row in Recently Played — not the plainer shape TIDAL's web SDK
/// sends, which describes a different kind of client. `uuid` becomes both the SQS entry's `Id`
/// and this body's own `uuid` field.
fn message_body(session: &PlaybackSession, claims: &Claims, uuid: &str) -> String {
    let mut body = json!({
        "group": "play_log",
        "name": "playback_session",
        "version": 2,
        "ts": session.end_ms,
        "uuid": uuid,
        "client": {
            "deviceType": "mobile",
            "platform": "android",
            "version": APP_VERSION,
        },
        "payload": payload(session),
    });
    if let Some(cid) = claims.cid {
        body["client"]["token"] = json!(cid.to_string());
    }
    let mut user = serde_json::Map::new();
    if let Some(uid) = claims.uid {
        user.insert("id".to_string(), json!(uid));
    }
    if let Some(cid) = claims.cid {
        user.insert("clientId".to_string(), json!(cid.to_string()));
    }
    if let Some(sid) = &claims.sid {
        user.insert("sessionId".to_string(), json!(sid));
    }
    if !user.is_empty() {
        body["user"] = Value::Object(user);
    }
    body.to_string()
}

/// The per-event `Headers` MessageAttribute (a JSON string): the key set TIDAL's SDKs send for a
/// native (non-browser) client — no `app-name`, `browser-name` or `browser-version`, which are
/// web-only and are omitted entirely rather than sent empty, same as every other default header.
fn event_headers(oauth_client_id: &str, access_token: &str, now_ms: i64) -> Value {
    json!({
        "client-id": oauth_client_id,
        "app-version": APP_VERSION,
        "os-name": OS_NAME,
        "os-version": OS_VERSION,
        "device-model": DEVICE_MODEL,
        "device-vendor": DEVICE_VENDOR,
        "consent-category": "NECESSARY",
        "requested-sent-timestamp": now_ms.to_string(),
        // Bare token: the "Bearer " prefix belongs to the real HTTP Authorization header, not
        // this one.
        "authorization": access_token,
    })
}

/// Encodes events as an SQS `SendMessageBatch` form body. `entries` is (body, headers) pairs, at
/// most [`MAX_BATCH`] of them.
fn batch_form(entries: &[(String, String)]) -> Vec<(String, String)> {
    let mut form = Vec::with_capacity(entries.len() * 8);
    for (i, (body, headers)) in entries.iter().enumerate() {
        let n = i + 1;
        let key = |suffix: &str| format!("SendMessageBatchRequestEntry.{n}.{suffix}");
        form.push((key("Id"), uuid::Uuid::new_v4().to_string()));
        form.push((key("MessageBody"), body.clone()));
        form.push((key("MessageAttribute.1.Name"), "Name".to_string()));
        form.push((
            key("MessageAttribute.1.Value.StringValue"),
            "playback_session".to_string(),
        ));
        form.push((
            key("MessageAttribute.1.Value.DataType"),
            "String".to_string(),
        ));
        form.push((key("MessageAttribute.2.Name"), "Headers".to_string()));
        form.push((key("MessageAttribute.2.Value.StringValue"), headers.clone()));
        form.push((
            key("MessageAttribute.2.Value.DataType"),
            "String".to_string(),
        ));
    }
    form
}

// ---- Sending --------------------------------------------------------------------------------

/// Sends finished plays to TIDAL, through the process's one TIDAL session. Cheap to clone: it is
/// only a client and a shared handle to that session.
#[derive(Clone)]
pub struct PlayLog {
    http: reqwest::Client,
    session: Arc<TidalSession>,
}

impl PlayLog {
    pub(crate) fn new(http: reqwest::Client, session: Arc<TidalSession>) -> Self {
        Self { http, session }
    }

    /// Sends up to [`MAX_BATCH`] finished sessions in one request. Best-effort: a failure here
    /// never interrupts or retries playback, only tells the caller it happened, for logging.
    pub async fn send(&self, sessions: &[PlaybackSession]) -> Result<()> {
        if sessions.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            sessions.len() <= MAX_BATCH,
            "{} sessions is more than TIDAL's own batch limit of {MAX_BATCH}",
            sessions.len()
        );

        let (access_token, oauth_client_id) = {
            let client = self.session.fresh().await?;
            let (access_token, _country_code) = tidal::credentials(&client)?;
            (access_token, client.session.auth.client_id.clone())
        };
        let claims = claims(&access_token);

        let entries: Vec<(String, String)> = sessions
            .iter()
            .map(|session| {
                let uuid = uuid::Uuid::new_v4().to_string();
                let body = message_body(session, &claims, &uuid);
                let headers = event_headers(&oauth_client_id, &access_token, now_ms()).to_string();
                (body, headers)
            })
            .collect();

        let response = self
            .http
            .post(EVENT_BATCH_URL)
            .bearer_auth(&access_token)
            .form(&batch_form(&entries))
            .send()
            .await
            .context("sending a playback_session event to TIDAL")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("reading TIDAL's event-batch response")?;
        if !status.is_success() {
            return Err(anyhow!("TIDAL's event endpoint answered {status}: {body}"));
        }
        if body.contains("BatchResultErrorEntry") {
            return Err(anyhow!("TIDAL rejected the playback_session event: {body}"));
        }
        Ok(())
    }
}

// ---- Turning playback events into sessions ---------------------------------------------------

/// A session still being tracked, not yet finished.
#[derive(Debug, Clone)]
struct Open {
    id: String,
    product_id: String,
    quality: Option<Quality>,
    audio_mode: Option<String>,
    start_position: Duration,
    start_ms: i64,
    last_position: Duration,
    actions: Vec<Action>,
    /// Wall-clock time heard so far, not counting the live span below.
    heard: Duration,
    /// When the live (currently playing) span started, if there is one right now.
    playing_since_ms: Option<i64>,
}

impl Open {
    fn fold_playing_time(&mut self, now_ms: i64) {
        if let Some(since) = self.playing_since_ms.take() {
            self.heard += Duration::from_millis((now_ms - since).max(0) as u64);
        }
    }

    fn heard_through(&self, now_ms: i64) -> Duration {
        let live = self
            .playing_since_ms
            .map(|since| Duration::from_millis((now_ms - since).max(0) as u64))
            .unwrap_or_default();
        self.heard + live
    }
}

/// A pure state machine turning the engine's playback events into finished [`PlaybackSession`]s,
/// ready for [`PlayLog::send`]. Every method takes `now_ms` rather than reading the clock itself,
/// so it is driven by tests without waiting on real time; `phoniad` passes the real one.
#[derive(Debug, Default)]
pub struct SessionTracker {
    open: Option<Open>,
}

impl SessionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// A track started playing. `product_id` is the TIDAL track id, or `None` for a local file
    /// (nothing is tracked for it: there is nothing to report to TIDAL). If a previous session
    /// was still open, it is closed first (its last known position, not a full duration — this
    /// should not normally happen, since `TrackEnded` is expected to have closed it already, but
    /// is handled defensively rather than silently losing a play).
    pub fn started(
        &mut self,
        product_id: Option<String>,
        quality: Option<Quality>,
        audio_mode: Option<String>,
        now_ms: i64,
    ) -> Option<PlaybackSession> {
        let displaced = self.close_open(now_ms);
        self.open = product_id.map(|product_id| Open {
            id: uuid::Uuid::new_v4().to_string(),
            product_id,
            quality,
            audio_mode,
            start_position: Duration::ZERO,
            start_ms: now_ms,
            last_position: Duration::ZERO,
            actions: vec![Action {
                kind: ActionType::PlaybackStart,
                position: Duration::ZERO,
                at_ms: now_ms,
            }],
            heard: Duration::ZERO,
            playing_since_ms: Some(now_ms),
        });
        displaced
    }

    /// The last known position within the current track, from `Event::Position`.
    pub fn position(&mut self, position: Duration) {
        if let Some(open) = &mut self.open {
            open.last_position = position;
        }
    }

    pub fn paused(&mut self, now_ms: i64) {
        if let Some(open) = &mut self.open
            && open.playing_since_ms.is_some()
        {
            open.fold_playing_time(now_ms);
            open.actions.push(Action {
                kind: ActionType::PlaybackStop,
                position: open.last_position,
                at_ms: now_ms,
            });
        }
    }

    pub fn resumed(&mut self, now_ms: i64) {
        if let Some(open) = &mut self.open
            && open.playing_since_ms.is_none()
        {
            open.playing_since_ms = Some(now_ms);
            open.actions.push(Action {
                kind: ActionType::PlaybackStart,
                position: open.last_position,
                at_ms: now_ms,
            });
        }
    }

    /// A seek landed on `to`. Folds the live span so far (if any) and resumes counting from the
    /// new position, without resetting or closing anything.
    pub fn seeked(&mut self, to: Duration, now_ms: i64) {
        if let Some(open) = &mut self.open {
            let was_playing = open.playing_since_ms.is_some();
            open.fold_playing_time(now_ms);
            open.actions.push(Action {
                kind: ActionType::PlaybackStop,
                position: open.last_position,
                at_ms: now_ms,
            });
            open.last_position = to;
            open.actions.push(Action {
                kind: ActionType::PlaybackStart,
                position: to,
                at_ms: now_ms,
            });
            if was_playing {
                open.playing_since_ms = Some(now_ms);
            }
        }
    }

    /// The current track ended. `Completed` reports the full `duration`; `Interrupted` and
    /// `Failed` report the last known position instead, since the track never reached its end.
    pub fn ended(
        &mut self,
        reason: EndReason,
        duration: Option<Duration>,
        now_ms: i64,
    ) -> Option<PlaybackSession> {
        let mut open = self.open.take()?;
        open.fold_playing_time(now_ms);
        let end_position = match reason {
            EndReason::Completed => duration.unwrap_or(open.last_position),
            EndReason::Interrupted | EndReason::Failed => open.last_position,
        };
        Self::finish(open, end_position, now_ms)
    }

    fn close_open(&mut self, now_ms: i64) -> Option<PlaybackSession> {
        let mut open = self.open.take()?;
        open.fold_playing_time(now_ms);
        let end_position = open.last_position;
        Self::finish(open, end_position, now_ms)
    }

    /// Below [`MIN_HEARD`], TIDAL's own rule says this was never a play at all: nothing is
    /// reported, whatever the reason it ended for.
    fn finish(mut open: Open, end_position: Duration, now_ms: i64) -> Option<PlaybackSession> {
        if open.heard_through(now_ms) < MIN_HEARD {
            return None;
        }
        open.actions.push(Action {
            kind: ActionType::PlaybackStop,
            position: end_position,
            at_ms: now_ms,
        });
        Some(PlaybackSession {
            id: open.id,
            product_id: open.product_id,
            quality: open.quality,
            audio_mode: open.audio_mode,
            start_position: open.start_position,
            start_ms: open.start_ms,
            end_position,
            end_ms: now_ms,
            actions: open.actions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- claims ----

    #[test]
    fn claims_are_read_from_the_jwt_without_asking_tidal() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"uid":173234555,"cid":8017,"sid":"abc"}"#);
        let token = format!("header.{payload}.sig");
        let c = claims(&token);
        assert_eq!(c.uid, Some(173234555));
        assert_eq!(c.cid, Some(8017));
        assert_eq!(c.sid.as_deref(), Some("abc"));
    }

    #[test]
    fn a_token_with_no_usable_claims_section_is_not_an_error() {
        assert_eq!(claims("not-a-jwt"), Claims::default());
        assert_eq!(claims(""), Claims::default());
    }

    impl PartialEq for Claims {
        fn eq(&self, other: &Self) -> bool {
            self.uid == other.uid && self.cid == other.cid && self.sid == other.sid
        }
    }

    // ---- body shape ----

    fn sample_session() -> PlaybackSession {
        PlaybackSession {
            id: "sess-123".into(),
            product_id: "42".into(),
            quality: Some(Quality::Lossless),
            audio_mode: Some("STEREO".into()),
            start_position: Duration::ZERO,
            start_ms: 1_000,
            end_position: Duration::from_secs(200),
            end_ms: 201_000,
            actions: vec![
                Action {
                    kind: ActionType::PlaybackStart,
                    position: Duration::ZERO,
                    at_ms: 1_000,
                },
                Action {
                    kind: ActionType::PlaybackStop,
                    position: Duration::from_secs(200),
                    at_ms: 201_000,
                },
            ],
        }
    }

    fn sample_claims() -> Claims {
        Claims {
            uid: Some(1),
            cid: Some(8017),
            sid: Some("sid-x".into()),
        }
    }

    #[test]
    fn the_body_is_the_mobile_shape_not_the_web_shape() {
        let text = message_body(&sample_session(), &sample_claims(), "uuid-1");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["group"], "play_log");
        assert_eq!(v["name"], "playback_session");
        assert_eq!(v["version"], 2);
        assert_eq!(v["uuid"], "uuid-1");
        assert!(
            v.get("user").is_some(),
            "the mobile shape needs a user object"
        );
        assert!(
            v.get("client").is_some(),
            "the mobile shape needs a client object"
        );
        assert_eq!(v["client"]["platform"], "android");
        assert_eq!(v["client"]["deviceType"], "mobile");
        assert_eq!(v["client"]["token"], "8017", "the cid claim, as a string");
        assert_eq!(v["user"]["id"], 1);
        assert_eq!(v["user"]["clientId"], "8017");
        assert_eq!(v["user"]["sessionId"], "sid-x");
        assert_eq!(v["payload"]["playbackSessionId"], "sess-123");
        assert_eq!(v["payload"]["sourceType"], "ITEM");
        assert_eq!(v["payload"]["sourceId"], "42");
        assert_eq!(v["payload"]["productType"], "TRACK");
        assert_eq!(v["payload"]["requestedProductId"], "42");
        assert_eq!(v["payload"]["actualProductId"], "42");
        assert_eq!(v["payload"]["actualQuality"], "LOSSLESS");
        assert_eq!(v["payload"]["actualAudioMode"], "STEREO");
        assert_eq!(v["payload"]["startAssetPosition"], 0.0);
        assert_eq!(v["payload"]["endAssetPosition"], 200.0);
        assert_eq!(v["payload"]["actions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn missing_quality_and_audio_mode_are_omitted_not_sent_as_null() {
        let mut session = sample_session();
        session.quality = None;
        session.audio_mode = None;
        let text = message_body(&session, &sample_claims(), "uuid-1");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["payload"].get("actualQuality").is_none());
        assert!(v["payload"].get("actualAudioMode").is_none());
    }

    #[test]
    fn missing_claims_are_omitted_from_user_and_client() {
        let text = message_body(&sample_session(), &Claims::default(), "uuid-1");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["client"].get("token").is_none());
        assert!(v.get("user").is_none(), "no claims decoded at all");
    }

    #[test]
    fn the_event_never_names_phonia() {
        let text = message_body(&sample_session(), &sample_claims(), "uuid-1");
        let headers = event_headers("cid-x", "tok-y", 123).to_string();
        for text in [&text, &headers] {
            assert!(!text.contains("phonia"), "leaked phonia's own name: {text}");
            assert!(
                !text.contains(env!("CARGO_PKG_VERSION")),
                "leaked phonia's own version: {text}"
            );
        }
    }

    #[test]
    fn headers_are_the_native_client_set_with_no_bearer_prefix() {
        let h = event_headers("cid-x", "tok-y", 123_456);
        assert_eq!(h.as_object().unwrap().len(), 9, "exactly nine header keys");
        assert!(h.get("app-name").is_none(), "no app-name: that is web-only");
        assert!(h.get("browser-name").is_none());
        assert_eq!(h["client-id"], "cid-x");
        assert_eq!(h["os-name"], "Android");
        assert_eq!(h["consent-category"], "NECESSARY");
        assert_eq!(h["requested-sent-timestamp"], "123456");
        assert_eq!(
            h["authorization"], "tok-y",
            "bare token, no \"Bearer \" prefix"
        );
    }

    #[test]
    fn batch_form_encodes_an_sqs_send_message_batch() {
        let entries = vec![("body-1".to_string(), "headers-1".to_string())];
        let form = batch_form(&entries);
        let get = |key: &str| form.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        assert_eq!(
            get("SendMessageBatchRequestEntry.1.MessageBody"),
            Some("body-1")
        );
        assert_eq!(
            get("SendMessageBatchRequestEntry.1.MessageAttribute.1.Value.StringValue"),
            Some("playback_session")
        );
        assert_eq!(
            get("SendMessageBatchRequestEntry.1.MessageAttribute.2.Value.StringValue"),
            Some("headers-1")
        );
        assert!(get("SendMessageBatchRequestEntry.1.Id").is_some());
    }

    // ---- SessionTracker ----

    const MS: i64 = 1_000;

    #[test]
    fn thirty_seconds_of_a_long_track_counts_but_twenty_nine_does_not() {
        let mut t = SessionTracker::new();
        t.started(Some("1".into()), None, None, 0);
        let session = t.ended(
            EndReason::Completed,
            Some(Duration::from_secs(1000)),
            30 * MS,
        );
        assert!(session.is_some(), "30s heard must count");

        let mut t = SessionTracker::new();
        t.started(Some("1".into()), None, None, 0);
        let session = t.ended(EndReason::Interrupted, None, 29 * MS);
        assert!(session.is_none(), "29s heard must not count");
    }

    #[test]
    fn a_short_track_played_fully_can_never_reach_the_threshold() {
        let mut t = SessionTracker::new();
        t.started(Some("1".into()), None, None, 0);
        let session = t.ended(EndReason::Completed, Some(Duration::from_secs(25)), 25 * MS);
        assert!(
            session.is_none(),
            "25s of a 25s track is still under 30s heard"
        );
    }

    #[test]
    fn a_completed_track_reports_the_full_duration_not_the_last_position() {
        let mut t = SessionTracker::new();
        t.started(Some("1".into()), None, None, 0);
        t.position(Duration::from_secs(90));
        let session = t
            .ended(
                EndReason::Completed,
                Some(Duration::from_secs(200)),
                90 * MS,
            )
            .unwrap();
        assert_eq!(session.end_position, Duration::from_secs(200));
    }

    #[test]
    fn a_skip_halfway_reports_the_last_known_position() {
        let mut t = SessionTracker::new();
        t.started(Some("1".into()), None, None, 0);
        t.position(Duration::from_secs(45));
        let session = t
            .ended(
                EndReason::Interrupted,
                Some(Duration::from_secs(200)),
                45 * MS,
            )
            .unwrap();
        assert_eq!(session.end_position, Duration::from_secs(45));
    }

    #[test]
    fn a_long_pause_is_not_counted_as_heard_time() {
        let mut t = SessionTracker::new();
        t.started(Some("1".into()), None, None, 0);
        t.paused(10 * MS); // 10s heard so far
        // A long real-world pause: no `position`/`heard` ticking happens while paused.
        t.resumed(10_000 * MS);
        t.position(Duration::from_secs(15));
        // Another 10s actually heard after resuming: 20s total, still under the 30s threshold.
        let session = t.ended(EndReason::Interrupted, None, 10_010 * MS);
        assert!(
            session.is_none(),
            "paused time must not count toward the threshold"
        );
    }

    #[test]
    fn a_seek_keeps_accumulating_without_resetting_or_closing() {
        let mut t = SessionTracker::new();
        t.started(Some("1".into()), None, None, 0);
        t.position(Duration::from_secs(10));
        t.seeked(Duration::from_secs(150), 15 * MS); // 15s heard so far
        let session = t
            .ended(
                EndReason::Completed,
                Some(Duration::from_secs(200)),
                45 * MS,
            ) // +30s heard
            .unwrap();
        assert_eq!(session.end_position, Duration::from_secs(200));
        // start -> seek(STOP@10, START@150) -> end(STOP@200): four actions in all.
        assert_eq!(session.actions.len(), 4);
    }

    #[test]
    fn a_local_file_is_never_tracked() {
        let mut t = SessionTracker::new();
        t.started(None, None, None, 0);
        t.position(Duration::from_secs(60));
        let session = t.ended(
            EndReason::Completed,
            Some(Duration::from_secs(200)),
            60 * MS,
        );
        assert!(
            session.is_none(),
            "there is no TIDAL id to report a file with"
        );
    }

    #[test]
    fn a_gapless_join_reports_the_outgoing_track_once_with_no_double_count() {
        let mut t = SessionTracker::new();
        t.started(Some("a".into()), None, None, 0);
        t.position(Duration::from_secs(180));
        // `TrackEnded{Completed}` for A, immediately followed by `TrackStarted` for B, with no
        // `StateChanged` in between (playback never actually stopped).
        let a = t
            .ended(
                EndReason::Completed,
                Some(Duration::from_secs(180)),
                40 * MS,
            )
            .unwrap();
        assert_eq!(a.product_id, "a");
        let displaced = t.started(Some("b".into()), None, None, 40 * MS);
        assert!(
            displaced.is_none(),
            "A was already closed by `ended`; `started` must not report it again"
        );
    }

    #[test]
    fn a_manual_skip_with_no_explicit_track_ended_is_still_closed_and_reported() {
        // Defensive path: if `TrackEnded` is ever missed, `started` for the next track must not
        // silently drop the outgoing one.
        let mut t = SessionTracker::new();
        t.started(Some("a".into()), None, None, 0);
        t.position(Duration::from_secs(50));
        let displaced = t.started(Some("b".into()), None, None, 50 * MS);
        let a = displaced.unwrap();
        assert_eq!(a.product_id, "a");
        assert_eq!(a.end_position, Duration::from_secs(50));
    }
}
