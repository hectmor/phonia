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

use crate::dash::{self, DashSegments};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use tidlers::TidalClient;
use tidlers::client::models::playback::AudioQuality;

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

/// Maps our `AudioQuality` to the exact string TIDAL's API expects for the `audioquality` query
/// parameter.
///
/// Deliberately does **not** use `AudioQuality`'s `Display` impl: on `tidlers` 0.5.0 that prints
/// `"HI_RES"` for `AudioQuality::HiRes`, which is TIDAL's legacy MQA tier, not true HiRes FLAC.
/// The value the API actually wants for lossless HiRes streaming is `"HI_RES_LOSSLESS"`.
fn api_quality(q: &AudioQuality) -> &'static str {
    match q {
        AudioQuality::Low => "LOW",
        AudioQuality::High => "HIGH",
        AudioQuality::Lossless => "LOSSLESS",
        AudioQuality::HiRes => "HI_RES_LOSSLESS",
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

/// Fetches and decodes the `playbackinfopostpaywall` response for `track_id` at `quality`.
///
/// `http` should be a client built with [`build_http_client`] and reused across this call and
/// the subsequent segment downloads.
pub async fn fetch_playback_info(
    http: &reqwest::Client,
    client: &TidalClient,
    track_id: &str,
    quality: AudioQuality,
) -> Result<PlaybackInfo> {
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

    let url = format!(
        "{}/tracks/{}/playbackinfopostpaywall",
        tidlers::urls::API_V1_LOCATION,
        track_id
    );

    let response = http
        .get(&url)
        .bearer_auth(access_token)
        .query(&[
            ("countryCode", country_code.as_str()),
            ("audioquality", api_quality(&quality)),
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
        bail!("TIDAL responded {status} to playbackinfopostpaywall:\n{body}");
    }

    let raw: RawPlaybackInfo = serde_json::from_str(&body)
        .with_context(|| format!("parsing the playbackinfopostpaywall JSON response:\n{body}"))?;

    if matches!(quality, AudioQuality::HiRes) && raw.audio_quality != "HI_RES_LOSSLESS" {
        eprintln!(
            "Warning: TIDAL downgraded the quality to {}: the track doesn't exist in HiRes, or \
             the token doesn't have that entitlement. If you used an old login, re-run `phonia login`.",
            raw.audio_quality
        );
    }

    let manifest_bytes = BASE64
        .decode(&raw.manifest)
        .context("decoding the manifest field (base64)")?;
    let manifest_text =
        String::from_utf8(manifest_bytes).context("the decoded manifest is not valid UTF-8")?;

    let manifest = if let Ok(json_manifest) = serde_json::from_str::<RawJsonManifest>(&manifest_text) {
        let url = json_manifest
            .urls
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("the JSON manifest contains no URL"))?;
        ManifestKind::Json { url, codecs: json_manifest.codecs }
    } else {
        let dash = dash::parse_mpd(&manifest_text).context("parsing the DASH (MPD) manifest")?;
        ManifestKind::Dash(dash)
    };

    Ok(PlaybackInfo {
        track_id: raw.track_id,
        audio_mode: raw.audio_mode,
        audio_quality: raw.audio_quality,
        manifest_mime_type: raw.manifest_mime_type,
        bit_depth: raw.bit_depth,
        sample_rate: raw.sample_rate,
        manifest,
    })
}

pub fn print_playback_info(info: &PlaybackInfo) {
    println!("Track ID:        {}", info.track_id);
    println!("Audio mode:      {}", info.audio_mode);
    println!("Quality:         {}", info.audio_quality);
    println!(
        "Bit depth:       {}",
        info.bit_depth.map(|b| b.to_string()).unwrap_or_else(|| "?".to_string())
    );
    println!(
        "Sample rate:     {}",
        info.sample_rate.map(|r| format!("{r} Hz")).unwrap_or_else(|| "?".to_string())
    );
    println!("Manifest MIME:   {}", info.manifest_mime_type);
    println!("Codecs:          {}", info.codecs().unwrap_or("?"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_quality_maps_hires_to_hi_res_lossless_not_display() {
        // Regression test: `AudioQuality::HiRes`'s `Display` impl on tidlers 0.5.0 prints
        // "HI_RES" (the legacy MQA tier), which is NOT what the API wants for true HiRes FLAC.
        assert_eq!(api_quality(&AudioQuality::HiRes), "HI_RES_LOSSLESS");
        assert_ne!(api_quality(&AudioQuality::HiRes), AudioQuality::HiRes.to_string());
    }

    #[test]
    fn api_quality_maps_the_other_tiers_directly() {
        assert_eq!(api_quality(&AudioQuality::Low), "LOW");
        assert_eq!(api_quality(&AudioQuality::High), "HIGH");
        assert_eq!(api_quality(&AudioQuality::Lossless), "LOSSLESS");
    }
}
