//! TIDAL playback info + DASH/JSON manifest download.
//!
//! `tidlers`'s own `TidalClient::get_track_postpaywall_playback_info` is convenient, but its
//! `TrackPlaybackInfoResponse` doesn't deserialize `bitDepth`/`sampleRate` (TIDAL's real response
//! includes them, `tidlers` just doesn't have fields for them) and doesn't expose the decoded
//! raw manifest XML/JSON text (only a partially-parsed `DashManifest` that has no segment count
//! and doesn't resolve `<BaseURL>`; see `dash.rs`'s module doc). All of the request-building
//! blocks we'd need to reuse (`TidalClient::request`, `ApiRequestBuilder`, `TidalRequest`) are
//! `pub(crate)` inside `tidlers`, so rather than fork its internals we make the same HTTP call
//! ourselves here, using only the public parts of an authenticated `TidalClient` (its access
//! token, country code, audio quality and playback mode).

use crate::config::Quality;
use crate::dash::{self, DashSegments};
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use tidlers::TidalClient;

/// The exact User-Agent `tidlers` itself sends (an Android WebView UA string TIDAL's backend
/// expects); see `tidlers-0.5.0/src/requests.rs:118` (`RequestClient::new`). We make our own raw
/// HTTP calls here (see the module doc for why), so we have to set this ourselves too.
const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 12; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/91.0.4472.114 Safari/537.36";

/// Builds the single `reqwest::Client` that should be reused for the playbackinfo request and
/// all segment downloads (connection pooling, and a consistent User-Agent).
pub fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("building the HTTP client")
}

/// Maps a tier to the exact string TIDAL's API expects for the `audioquality` query parameter.
///
/// The value for lossless HiRes streaming is `"HI_RES_LOSSLESS"`; TIDAL's plain `"HI_RES"` is its
/// legacy MQA tier, which is not what we want.
fn api_quality(quality: Quality) -> &'static str {
    match quality {
        Quality::Low => "LOW",
        Quality::High => "HIGH",
        Quality::Lossless => "LOSSLESS",
        Quality::Hires => "HI_RES_LOSSLESS",
    }
}

/// The tier TIDAL says it delivered, from its `audioQuality` field.
///
/// The legacy `"HI_RES"` is MQA folded into a FLAC container: lossless, but not the full-rate
/// HiRes, so it counts as `Lossless`.
pub fn quality_from_api(text: &str) -> Option<Quality> {
    match text {
        "HI_RES_LOSSLESS" => Some(Quality::Hires),
        "LOSSLESS" | "HI_RES" => Some(Quality::Lossless),
        "HIGH" => Some(Quality::High),
        "LOW" => Some(Quality::Low),
        _ => None,
    }
}

/// Where the encoded audio actually lives, after decoding TIDAL's `manifest` field.
#[derive(Debug, Clone)]
pub enum ManifestKind {
    /// A single, directly downloadable URL (used for LOW/HIGH/LOSSLESS).
    Json { url: String, codecs: String },
    /// A DASH (fragmented MP4) manifest (used for HiRes).
    Dash(DashSegments),
}

/// Everything phase 0 needs to decode and play one track.
#[derive(Debug, Clone)]
pub struct PlaybackInfo {
    pub track_id: u64,
    pub audio_mode: String,
    pub audio_quality: String,
    pub manifest_mime_type: String,
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    /// TIDAL's own loudness measurement for the track, in dB (the gain to apply to reach its
    /// reference level); `None` on older responses or a track TIDAL hasn't measured.
    pub track_replay_gain: Option<f64>,
    /// The track's true peak sample amplitude (linear, typically close to but not over 1.0).
    pub track_peak_amplitude: Option<f64>,
    /// Loudness measurement for the album the track is on, in dB; `None` when TIDAL has no album
    /// measurement (a single, or an album it hasn't measured).
    pub album_replay_gain: Option<f64>,
    /// The album's true peak sample amplitude (linear).
    pub album_peak_amplitude: Option<f64>,
    pub manifest: ManifestKind,
}

impl PlaybackInfo {
    pub fn codecs(&self) -> Option<&str> {
        match &self.manifest {
            ManifestKind::Json { codecs, .. } => Some(codecs.as_str()),
            ManifestKind::Dash(dash) => dash.codecs.as_deref(),
        }
    }
}

#[derive(Deserialize)]
struct RawPlaybackInfo {
    #[serde(rename = "trackId")]
    track_id: u64,
    #[serde(rename = "audioMode")]
    audio_mode: String,
    #[serde(rename = "audioQuality")]
    audio_quality: String,
    #[serde(rename = "manifestMimeType")]
    manifest_mime_type: String,
    #[serde(rename = "bitDepth", default)]
    bit_depth: Option<u32>,
    #[serde(rename = "sampleRate", default)]
    sample_rate: Option<u32>,
    #[serde(rename = "trackReplayGain", default)]
    track_replay_gain: Option<f64>,
    #[serde(rename = "trackPeakAmplitude", default)]
    track_peak_amplitude: Option<f64>,
    #[serde(rename = "albumReplayGain", default)]
    album_replay_gain: Option<f64>,
    #[serde(rename = "albumPeakAmplitude", default)]
    album_peak_amplitude: Option<f64>,
    manifest: String,
}

#[derive(Deserialize)]
struct RawJsonManifest {
    #[serde(rename = "mimeType")]
    #[allow(dead_code)]
    mime_type: String,
    codecs: String,
    urls: Vec<String>,
}

/// TIDAL answered the playbackinfo request with an HTTP error.
#[derive(Debug)]
struct StatusError {
    status: reqwest::StatusCode,
    body: String,
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TIDAL responded {} to playbackinfopostpaywall:\n{}",
            self.status, self.body
        )
    }
}

impl std::error::Error for StatusError {}

/// The manifest TIDAL sent could not be read.
#[derive(Debug)]
struct BadManifest(String);

impl std::fmt::Display for BadManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the manifest could not be read: {}", self.0)
    }
}

impl std::error::Error for BadManifest {}

/// Whether asking again for a lower tier might work.
///
/// Yes for an HTTP 4xx (the track isn't there at that tier, or isn't ready) and for a manifest
/// that can't be read. No for 401, 408 and 429 (the token, a timeout, a rate limit: a lower tier
/// would meet the same), for 5xx and for a network failure: those aren't about the tier, and
/// retrying would just spend requests, or hide an outage behind a lower quality.
fn worth_a_lower_tier(error: &anyhow::Error) -> bool {
    use reqwest::StatusCode;
    if error.downcast_ref::<BadManifest>().is_some() {
        return true;
    }
    match error.downcast_ref::<StatusError>() {
        Some(StatusError { status, .. }) => {
            status.is_client_error()
                && !matches!(
                    *status,
                    StatusCode::UNAUTHORIZED
                        | StatusCode::REQUEST_TIMEOUT
                        | StatusCode::TOO_MANY_REQUESTS
                )
        }
        None => false,
    }
}

/// The access token and the country code of a logged-in client: what any request to TIDAL's API
/// needs to say who is asking and from where.
pub(crate) fn credentials(client: &TidalClient) -> Result<(String, String)> {
    let access_token = client
        .session
        .auth
        .access_token
        .clone()
        .ok_or_else(|| anyhow!("no access token; run `phonia login` first"))?;
    let country_code = client
        .user_info
        .as_ref()
        .map(|u| u.country_code.clone())
        .ok_or_else(|| anyhow!("no user info loaded; run `phonia login` first"))?;
    Ok((access_token, country_code))
}

/// The id of the logged-in user, for the endpoints that are theirs specifically (favorites, their
/// own playlists).
pub(crate) fn user_id(client: &TidalClient) -> Result<String> {
    client
        .user_info
        .as_ref()
        .map(|u| u.user_id.to_string())
        .ok_or_else(|| anyhow!("no user info loaded; run `phonia login` first"))
}

/// Fetches and decodes the `playbackinfopostpaywall` response for `track_id`, asking for `best`
/// and, if TIDAL refuses that tier, for each lower one down to `min`.
///
/// TIDAL often answers a tier it doesn't have with a lower one by itself (which the caller
/// judges by [`PlaybackInfo::audio_quality`]); this retries for when it answers with an error
/// instead. `http` should be a client built with [`build_http_client`] and reused across this
/// call and the subsequent segment downloads.
pub async fn fetch_playback_info(
    http: &reqwest::Client,
    client: &TidalClient,
    track_id: &str,
    best: Quality,
    min: Quality,
) -> Result<PlaybackInfo> {
    let (access_token, country_code) = credentials(client)?;
    let account = Account {
        base: tidlers::urls::API_V1_LOCATION,
        access_token: &access_token,
        country_code: &country_code,
    };
    fetch_with_fallback(http, &account, track_id, best, min).await
}

/// What a playbackinfo request needs to know about who is asking and where.
struct Account<'a> {
    base: &'a str,
    access_token: &'a str,
    country_code: &'a str,
}

async fn fetch_with_fallback(
    http: &reqwest::Client,
    account: &Account<'_>,
    track_id: &str,
    best: Quality,
    min: Quality,
) -> Result<PlaybackInfo> {
    let mut tier = best;
    loop {
        let error = match fetch_at(http, account, track_id, tier).await {
            Ok(info) => return Ok(info),
            Err(error) => error,
        };
        let lower = tier.lower().filter(|lower| *lower >= min);
        match lower {
            Some(lower) if worth_a_lower_tier(&error) => {
                crate::warn!(
                    "Warning: TIDAL would not give track {track_id} as {tier} ({error:#}); \
                     trying {lower}."
                );
                tier = lower;
            }
            _ => return Err(error),
        }
    }
}

/// One playbackinfo request, for one tier.
async fn fetch_at(
    http: &reqwest::Client,
    account: &Account<'_>,
    track_id: &str,
    quality: Quality,
) -> Result<PlaybackInfo> {
    let url = format!(
        "{}/tracks/{}/playbackinfopostpaywall",
        account.base, track_id
    );

    let response = http
        .get(&url)
        .bearer_auth(account.access_token)
        .query(&[
            ("countryCode", account.country_code),
            ("audioquality", api_quality(quality)),
            ("playbackmode", "STREAM"),
            ("assetpresentation", "FULL"),
        ])
        .send()
        .await
        .context("requesting playbackinfopostpaywall from TIDAL")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("reading the playbackinfopostpaywall response")?;
    if !status.is_success() {
        return Err(StatusError { status, body }.into());
    }

    let raw: RawPlaybackInfo = serde_json::from_str(&body)
        .with_context(|| format!("parsing the playbackinfopostpaywall JSON response:\n{body}"))?;

    if quality_from_api(&raw.audio_quality).is_some_and(|delivered| delivered < quality) {
        crate::warn!(
            "Warning: asked TIDAL for {quality}, got {}: the track doesn't exist at that tier, or \
             the token doesn't have that entitlement. If you used an old login, re-run `phonia login`.",
            raw.audio_quality
        );
    }

    let manifest =
        decode_manifest(&raw.manifest).map_err(|error| BadManifest(format!("{error:#}")))?;

    Ok(PlaybackInfo {
        track_id: raw.track_id,
        audio_mode: raw.audio_mode,
        audio_quality: raw.audio_quality,
        manifest_mime_type: raw.manifest_mime_type,
        bit_depth: raw.bit_depth,
        sample_rate: raw.sample_rate,
        track_replay_gain: raw.track_replay_gain,
        track_peak_amplitude: raw.track_peak_amplitude,
        album_replay_gain: raw.album_replay_gain,
        album_peak_amplitude: raw.album_peak_amplitude,
        manifest,
    })
}

/// Reads the base64 `manifest` field: a JSON with one URL, or a DASH (MPD) document.
fn decode_manifest(encoded: &str) -> Result<ManifestKind> {
    let manifest_bytes = BASE64
        .decode(encoded)
        .context("decoding the manifest field (base64)")?;
    let manifest_text =
        String::from_utf8(manifest_bytes).context("the decoded manifest is not valid UTF-8")?;

    if let Ok(json_manifest) = serde_json::from_str::<RawJsonManifest>(&manifest_text) {
        let url = json_manifest
            .urls
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("the JSON manifest contains no URL"))?;
        Ok(ManifestKind::Json {
            url,
            codecs: json_manifest.codecs,
        })
    } else {
        let dash = dash::parse_mpd(&manifest_text).context("parsing the DASH (MPD) manifest")?;
        Ok(ManifestKind::Dash(dash))
    }
}

pub fn print_playback_info(info: &PlaybackInfo) {
    println!("Track ID:        {}", info.track_id);
    println!("Audio mode:      {}", info.audio_mode);
    println!("Quality:         {}", info.audio_quality);
    println!(
        "Bit depth:       {}",
        info.bit_depth
            .map(|b| b.to_string())
            .unwrap_or_else(|| "?".to_string())
    );
    println!(
        "Sample rate:     {}",
        info.sample_rate
            .map(|r| format!("{r} Hz"))
            .unwrap_or_else(|| "?".to_string())
    );
    println!("Manifest MIME:   {}", info.manifest_mime_type);
    println!("Codecs:          {}", info.codecs().unwrap_or("?"));
    println!(
        "Track gain:      {}",
        info.track_replay_gain
            .map(|db| format!("{db:.2} dB"))
            .unwrap_or_else(|| "?".to_string())
    );
    println!(
        "Track peak:      {}",
        info.track_peak_amplitude
            .map(|peak| format!("{peak:.6}"))
            .unwrap_or_else(|| "?".to_string())
    );
    println!(
        "Album gain:      {}",
        info.album_replay_gain
            .map(|db| format!("{db:.2} dB"))
            .unwrap_or_else(|| "?".to_string())
    );
    println!(
        "Album peak:      {}",
        info.album_peak_amplitude
            .map(|peak| format!("{peak:.6}"))
            .unwrap_or_else(|| "?".to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hires_is_asked_for_as_hi_res_lossless_not_as_mqa() {
        assert_eq!(api_quality(Quality::Hires), "HI_RES_LOSSLESS");
        assert_ne!(api_quality(Quality::Hires), "HI_RES");
    }

    #[test]
    fn the_other_tiers_are_asked_for_by_name() {
        assert_eq!(api_quality(Quality::Low), "LOW");
        assert_eq!(api_quality(Quality::High), "HIGH");
        assert_eq!(api_quality(Quality::Lossless), "LOSSLESS");
    }

    #[test]
    fn what_tidal_says_it_delivered_maps_back_to_a_tier() {
        for quality in [
            Quality::Hires,
            Quality::Lossless,
            Quality::High,
            Quality::Low,
        ] {
            assert_eq!(quality_from_api(api_quality(quality)), Some(quality));
        }
        assert_eq!(quality_from_api("HI_RES"), Some(Quality::Lossless));
        assert_eq!(quality_from_api("SURROUND"), None);
    }

    /// A playbackinfo server on a local port: `answer` gets the `audioquality` asked for and
    /// returns the HTTP status and body; the tiers asked for come back in order.
    async fn serve_playbackinfo(
        answer: impl Fn(&str) -> (u16, String) + Send + Sync + 'static,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        let answer = std::sync::Arc::new(answer);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (log, answer) = (log.clone(), answer.clone());
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let n = socket.read(&mut request).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&request[..n]);
                    let line = request.lines().next().unwrap_or("");
                    let tier = line
                        .split("audioquality=")
                        .nth(1)
                        .and_then(|rest| rest.split(['&', ' ']).next())
                        .unwrap_or("")
                        .to_string();
                    log.lock().unwrap().push(tier.clone());
                    let (status, body) = answer(&tier);
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (base, asked)
    }

    /// A playbackinfo answer that says `audio_quality` and carries a one-URL JSON manifest.
    fn playbackinfo(audio_quality: &str) -> (u16, String) {
        playbackinfo_with_manifest(
            audio_quality,
            r#"{"mimeType":"audio/flac","codecs":"flac","urls":["http://cdn/a.flac"]}"#,
        )
    }

    fn playbackinfo_with_manifest(audio_quality: &str, manifest: &str) -> (u16, String) {
        let body = format!(
            r#"{{"trackId":1,"audioMode":"STEREO","audioQuality":"{audio_quality}","manifestMimeType":"application/vnd.tidal.bts","manifest":"{}"}}"#,
            BASE64.encode(manifest)
        );
        (200, body)
    }

    /// Like [`playbackinfo`], but with TIDAL's four loudness fields also present, as a real
    /// response includes them.
    fn playbackinfo_with_gain(audio_quality: &str) -> (u16, String) {
        let manifest = r#"{"mimeType":"audio/flac","codecs":"flac","urls":["http://cdn/a.flac"]}"#;
        let body = format!(
            r#"{{"trackId":1,"audioMode":"STEREO","audioQuality":"{audio_quality}","manifestMimeType":"application/vnd.tidal.bts","trackReplayGain":-6.5,"trackPeakAmplitude":0.98,"albumReplayGain":-7.2,"albumPeakAmplitude":0.99,"manifest":"{}"}}"#,
            BASE64.encode(manifest)
        );
        (200, body)
    }

    async fn fetch(base: &str, best: Quality, min: Quality) -> Result<PlaybackInfo> {
        let http = reqwest::Client::new();
        let account = Account {
            base,
            access_token: "token",
            country_code: "US",
        };
        fetch_with_fallback(&http, &account, "1", best, min).await
    }

    #[tokio::test]
    async fn a_track_tidal_has_at_the_tier_asked_for_takes_one_request() {
        let (base, asked) = serve_playbackinfo(|_| playbackinfo("HI_RES_LOSSLESS")).await;
        let info = fetch(&base, Quality::Hires, Quality::Lossless)
            .await
            .unwrap();
        assert_eq!(info.audio_quality, "HI_RES_LOSSLESS");
        assert_eq!(*asked.lock().unwrap(), ["HI_RES_LOSSLESS"]);
    }

    #[tokio::test]
    async fn a_lower_tier_that_tidal_answers_with_by_itself_is_taken_as_it_comes() {
        let (base, asked) = serve_playbackinfo(|_| playbackinfo("LOSSLESS")).await;
        let info = fetch(&base, Quality::Hires, Quality::Lossless)
            .await
            .unwrap();
        assert_eq!(info.audio_quality, "LOSSLESS");
        assert_eq!(
            asked.lock().unwrap().len(),
            1,
            "no retry: it is the caller's call"
        );
    }

    #[tokio::test]
    async fn an_http_4xx_at_a_tier_asks_for_the_next_one_down() {
        let (base, asked) = serve_playbackinfo(|tier| match tier {
            "HI_RES_LOSSLESS" => (404, "not there".to_string()),
            other => playbackinfo(other),
        })
        .await;
        let info = fetch(&base, Quality::Hires, Quality::Lossless)
            .await
            .unwrap();
        assert_eq!(info.audio_quality, "LOSSLESS");
        assert_eq!(*asked.lock().unwrap(), ["HI_RES_LOSSLESS", "LOSSLESS"]);
    }

    #[tokio::test]
    async fn the_retries_stop_at_the_floor_and_the_error_is_the_last_one() {
        let (base, asked) = serve_playbackinfo(|_| (403, "no entitlement".to_string())).await;
        let error = fetch(&base, Quality::Hires, Quality::Lossless)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("403"), "{error:#}");
        // Never HIGH: it is below the floor.
        assert_eq!(*asked.lock().unwrap(), ["HI_RES_LOSSLESS", "LOSSLESS"]);
    }

    #[tokio::test]
    async fn a_lower_floor_lets_the_retries_go_further_down() {
        let (base, asked) = serve_playbackinfo(|tier| match tier {
            "HIGH" => playbackinfo("HIGH"),
            _ => (404, String::new()),
        })
        .await;
        let info = fetch(&base, Quality::Hires, Quality::High).await.unwrap();
        assert_eq!(info.audio_quality, "HIGH");
        assert_eq!(
            *asked.lock().unwrap(),
            ["HI_RES_LOSSLESS", "LOSSLESS", "HIGH"]
        );
    }

    #[tokio::test]
    async fn errors_that_are_not_about_the_tier_are_not_retried() {
        for status in [401, 429, 500, 503] {
            let (base, asked) = serve_playbackinfo(move |_| (status, String::new())).await;
            let error = fetch(&base, Quality::Hires, Quality::Low)
                .await
                .unwrap_err();
            assert!(
                format!("{error:#}").contains(&status.to_string()),
                "{error:#}"
            );
            assert_eq!(asked.lock().unwrap().len(), 1, "status {status}");
        }
    }

    #[tokio::test]
    async fn a_manifest_that_cannot_be_read_asks_for_the_next_tier_down() {
        let (base, asked) = serve_playbackinfo(|tier| match tier {
            "HI_RES_LOSSLESS" => playbackinfo_with_manifest(tier, "not a manifest"),
            other => playbackinfo(other),
        })
        .await;
        let info = fetch(&base, Quality::Hires, Quality::Lossless)
            .await
            .unwrap();
        assert_eq!(info.audio_quality, "LOSSLESS");
        assert_eq!(asked.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_older_or_unmeasured_track_has_no_loudness_fields() {
        let (base, _) = serve_playbackinfo(|_| playbackinfo("LOSSLESS")).await;
        let info = fetch(&base, Quality::Lossless, Quality::Low).await.unwrap();
        assert_eq!(info.track_replay_gain, None);
        assert_eq!(info.track_peak_amplitude, None);
        assert_eq!(info.album_replay_gain, None);
        assert_eq!(info.album_peak_amplitude, None);
    }

    #[tokio::test]
    async fn loudness_fields_are_parsed_when_tidal_sends_them() {
        let (base, _) = serve_playbackinfo(|_| playbackinfo_with_gain("LOSSLESS")).await;
        let info = fetch(&base, Quality::Lossless, Quality::Low).await.unwrap();
        assert_eq!(info.track_replay_gain, Some(-6.5));
        assert_eq!(info.track_peak_amplitude, Some(0.98));
        assert_eq!(info.album_replay_gain, Some(-7.2));
        assert_eq!(info.album_peak_amplitude, Some(0.99));
    }

    #[tokio::test]
    async fn a_network_failure_is_not_retried() {
        // Nothing listens here.
        let error = fetch("http://127.0.0.1:1", Quality::Hires, Quality::Low)
            .await
            .unwrap_err();
        assert!(!worth_a_lower_tier(&error), "{error:#}");
    }
}
