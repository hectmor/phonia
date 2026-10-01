//! Covers: fetching them from TIDAL's image CDN, decoding them, and encoding them for whatever
//! graphics protocol the terminal supports.
//!
//! Covers are a resource for drawing, like [`crate::theme::Theme`], not app state: they never go
//! through [`crate::app::State`], which stays pure and comparable (`Eq`), so a cover that is still
//! loading, or one that failed, never has to be modelled there. Deciding *which* cover the screen
//! wants, and actually drawing one, is a later part of issue #24; this is the fetch pipeline and
//! the bookkeeping (what is already being asked for, what has already failed) those parts share.

use futures_util::future::BoxFuture;
use ratatui::layout::Size;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Fetches the bytes of the image at `url`. How to actually fetch them is a parameter, the same
/// way [`crate::conn::Connector`] is, so tests use a fake instead of the real network.
pub type CoverSource =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<Vec<u8>, String>> + Send + Sync>;

/// How long a fetch may take before it counts as failed.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The real source: a plain GET with a timeout. TIDAL's image CDN needs no TIDAL session (see
/// `docs/DECISIONS.md`, 2026-10-01): this is the only network access `phonia-tui` has of its own,
/// everything else goes through the daemon.
pub fn http_source() -> CoverSource {
    Arc::new(move |url: String| {
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .map_err(|error| error.to_string())?;
            let response = client
                .get(&url)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("{url} answered {status}"));
            }
            response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|error| error.to_string())
        })
    })
}

/// A cover fetched, decoded and encoded for the terminal, or why it could not be.
pub enum Outcome {
    Ready(Protocol),
    Failed(String),
}

/// Fetches the image at `url`, decodes it, and encodes it with `picker` to fit `cells`. The
/// decode and the encode are CPU-bound, so both run on a blocking thread rather than one of the
/// async runtime's own.
pub async fn fetch(url: String, picker: Picker, cells: Size, source: CoverSource) -> Outcome {
    let bytes = match source(url).await {
        Ok(bytes) => bytes,
        Err(reason) => return Outcome::Failed(reason),
    };
    let decoded = tokio::task::spawn_blocking(move || {
        let image = image::load_from_memory(&bytes).map_err(|error| error.to_string())?;
        picker
            .new_protocol(image, cells, Resize::default())
            .map_err(|error| error.to_string())
    })
    .await;
    match decoded {
        Ok(Ok(protocol)) => Outcome::Ready(protocol),
        Ok(Err(reason)) => Outcome::Failed(reason),
        Err(_) => Outcome::Failed("decoding was interrupted".to_string()),
    }
}

/// Which covers are being asked for right now, and which ones have already failed: so the same
/// URL is never fetched twice at once, and a cover that failed is not retried on every redraw.
#[derive(Default)]
pub struct Covers {
    in_flight: HashSet<String>,
    failed: HashSet<String>,
}

impl Covers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `url` is worth asking for: not already in flight, and not already failed.
    fn should_fetch(&self, url: &str) -> bool {
        !self.in_flight.contains(url) && !self.failed.contains(url)
    }

    /// Marks `url` as being fetched now, if it is worth fetching; `false` (and no change) if it
    /// was already in flight or had already failed, so the caller does not ask again.
    pub fn start(&mut self, url: &str) -> bool {
        if !self.should_fetch(url) {
            return false;
        }
        self.in_flight.insert(url.to_string());
        true
    }

    /// The fetch for `url` is done: no longer in flight, and, if it failed, remembered as such.
    pub fn finish(&mut self, url: &str, outcome: &Outcome) {
        self.in_flight.remove(url);
        if matches!(outcome, Outcome::Failed(_)) {
            self.failed.insert(url.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};
    use std::io::Cursor;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A tiny, real JPEG, encoded on the spot: no binary fixture file needed.
    fn a_jpeg() -> Vec<u8> {
        let image = RgbImage::from_pixel(4, 4, image::Rgb([200, 40, 40]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        bytes
    }

    fn fake_source(result: Result<Vec<u8>, String>) -> CoverSource {
        Arc::new(move |_url: String| {
            let result = result.clone();
            Box::pin(async move { result })
        })
    }

    #[tokio::test]
    async fn a_real_image_is_decoded_and_encoded_for_the_terminal() {
        let picker = Picker::halfblocks();
        let outcome = fetch(
            "http://example/cover.jpg".into(),
            picker,
            Size::new(8, 4),
            fake_source(Ok(a_jpeg())),
        )
        .await;
        assert!(matches!(outcome, Outcome::Ready(_)));
    }

    #[tokio::test]
    async fn a_source_that_fails_is_a_failed_outcome_not_a_panic() {
        let outcome = fetch(
            "http://example/cover.jpg".into(),
            Picker::halfblocks(),
            Size::new(8, 4),
            fake_source(Err("connection refused".into())),
        )
        .await;
        assert!(matches!(outcome, Outcome::Failed(reason) if reason.contains("refused")));
    }

    #[tokio::test]
    async fn bytes_that_are_not_an_image_fail_to_decode_not_to_panic() {
        let outcome = fetch(
            "http://example/cover.jpg".into(),
            Picker::halfblocks(),
            Size::new(8, 4),
            fake_source(Ok(b"not a jpeg".to_vec())),
        )
        .await;
        assert!(matches!(outcome, Outcome::Failed(_)));
    }

    #[test]
    fn the_same_url_is_not_fetched_twice_at_once() {
        let mut covers = Covers::new();
        assert!(covers.start("a"));
        assert!(!covers.start("a"), "already in flight");
        assert!(covers.start("b"), "a different url is unaffected");
    }

    #[test]
    fn a_failed_cover_is_not_retried_but_a_successful_one_can_be_asked_for_again() {
        let mut covers = Covers::new();
        covers.start("a");
        covers.finish("a", &Outcome::Failed("404".into()));
        assert!(!covers.start("a"), "a failure is remembered");

        covers.start("b");
        covers.finish(
            "b",
            &Outcome::Ready(
                Picker::halfblocks()
                    .new_protocol(
                        image::DynamicImage::ImageRgb8(RgbImage::from_pixel(
                            1,
                            1,
                            image::Rgb([0, 0, 0]),
                        )),
                        Size::new(1, 1),
                        Resize::default(),
                    )
                    .unwrap(),
            ),
        );
        assert!(
            covers.start("b"),
            "a successful fetch does not block asking again (e.g. after a resize)"
        );
    }

    /// A one-request server on a local port: answers with `status` and `body`. Mirrors
    /// `phonia-core`'s own `catalog::remote::tests::serve_once`.
    async fn serve_once(status: u16, body: &[u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_vec();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let _ = socket.read(&mut request).await.unwrap_or(0);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        });
        base
    }

    #[tokio::test]
    async fn the_real_source_fetches_bytes_on_success_and_fails_on_a_bad_status() {
        let base = serve_once(200, &a_jpeg()).await;
        let bytes = http_source()(format!("{base}/cover.jpg")).await.unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());

        let base = serve_once(404, b"").await;
        assert!(http_source()(format!("{base}/cover.jpg")).await.is_err());
    }
}
