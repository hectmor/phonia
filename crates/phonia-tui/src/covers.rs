//! Covers: fetching them from TIDAL's image CDN, decoding them, and encoding them for whatever
//! graphics protocol the terminal supports.
//!
//! Covers are a resource for drawing, like [`crate::theme::Theme`], not app state: they never go
//! through [`crate::app::State`], which stays pure and comparable (`Eq`), so a cover that is still
//! loading, or one that failed, never has to be modelled there. [`wanted`] decides which cover (if
//! any) the main panel's current header shows; actually drawing one is `view/browse.rs`'s job.

use crate::app;
use crate::browse::Header;
use futures_util::future::BoxFuture;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Resize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

/// How many encoded covers are kept ready at once: enough to go back and forth between a few
/// recently opened albums or playlists without re-fetching, not so many that a long session holds
/// onto images nobody is looking at any more.
const READY_CAPACITY: usize = 8;

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
    /// The encoded cover, and the bytes it was decoded from, so a resize can encode them again
    /// without asking the CDN twice.
    Ready {
        protocol: Protocol,
        bytes: Arc<Vec<u8>>,
    },
    Failed(String),
}

/// Fetches the image at `url` unless `bytes` already has it (a resize re-encoding what was
/// downloaded before), decodes it, and encodes it with `picker` to fit `cells`. The decode and
/// the encode are CPU-bound, so both run on a blocking thread rather than one of the async
/// runtime's own.
pub async fn fetch(
    url: String,
    picker: Picker,
    cells: Size,
    source: CoverSource,
    bytes: Option<Arc<Vec<u8>>>,
) -> Outcome {
    let bytes = match bytes {
        Some(bytes) => bytes,
        None => match source(url).await {
            Ok(bytes) => Arc::new(bytes),
            Err(reason) => return Outcome::Failed(reason),
        },
    };
    let kept = bytes.clone();
    let decoded = tokio::task::spawn_blocking(move || {
        let image = image::load_from_memory(&bytes).map_err(|error| error.to_string())?;
        picker
            .new_protocol(image, cells, Resize::default())
            .map_err(|error| error.to_string())
    })
    .await;
    match decoded {
        Ok(Ok(protocol)) => Outcome::Ready {
            protocol,
            bytes: kept,
        },
        Ok(Err(reason)) => Outcome::Failed(reason),
        Err(_) => Outcome::Failed("decoding was interrupted".to_string()),
    }
}

/// A cover ready to draw: what it was encoded for, the encoded protocol itself, and the bytes it
/// was decoded from -- kept so a resize re-encodes at the new size without asking the CDN again.
struct Ready {
    cells: Size,
    protocol: Protocol,
    bytes: Arc<Vec<u8>>,
}

/// Covers ready to draw, what is being asked for right now, and what has already failed: so the
/// same URL is never fetched twice at once, a cover that failed is not retried on every redraw,
/// and one already decoded is not fetched or decoded again.
///
/// Holds the [`Picker`] too (`None` means covers are off, or the terminal has none of the
/// protocols this needs): a resource for drawing, like [`crate::theme::Theme`], kept out of
/// [`crate::app::State`] so a cover that is loading, or a terminal that cannot show one at all,
/// never has to be modelled in state that must stay pure and comparable.
pub struct Covers {
    picker: Option<Picker>,
    /// What is being fetched now, and the size it was asked for: a resize while one runs
    /// supersedes it rather than racing it.
    in_flight: HashMap<String, Size>,
    failed: HashSet<String>,
    ready: HashMap<String, Ready>,
    /// The order covers were made ready in, oldest first, so the least recently finished one is
    /// what gets evicted once [`READY_CAPACITY`] is reached.
    ready_order: VecDeque<String>,
}

impl Covers {
    pub fn new(picker: Option<Picker>) -> Self {
        Self {
            picker,
            in_flight: HashMap::new(),
            failed: HashSet::new(),
            ready: HashMap::new(),
            ready_order: VecDeque::new(),
        }
    }

    /// No picker: covers are off, or the terminal has none of the protocols this needs.
    pub fn disabled() -> Self {
        Self::new(None)
    }

    pub fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// The cover at `url` ready to draw in `cells`: the one encoded for this size, or, while a
    /// resize's own re-encode is still running, an older one that still fits -- which draws a
    /// little small instead of leaving the reserved space blank. A protocol *bigger* than the
    /// area is no use: `Image` silently draws nothing then.
    pub fn ready(&self, url: &str, cells: Size) -> Option<&Protocol> {
        let ready = self.ready.get(url)?;
        let encoded = ready.protocol.size();
        (encoded.width <= cells.width && encoded.height <= cells.height).then_some(&ready.protocol)
    }

    /// The downloaded bytes of `url`, if still kept, to encode again at another size.
    pub fn bytes(&self, url: &str) -> Option<Arc<Vec<u8>>> {
        Some(self.ready.get(url)?.bytes.clone())
    }

    /// Whether `url` is worth asking for *at this size*: not that very fetch already in flight,
    /// not failed, and not already encoded for exactly these cells. (Not by `protocol.size()`:
    /// `Resize::Fit` never upscales, so an encode can come out smaller than it was asked for.)
    fn should_fetch(&self, url: &str, cells: Size) -> bool {
        self.in_flight.get(url) != Some(&cells)
            && !self.failed.contains(url)
            && self.ready.get(url).map(|ready| ready.cells) != Some(cells)
    }

    /// Marks `url` at `cells` as being fetched now, if it is worth fetching; `false` (and no
    /// change) otherwise, so the caller does not ask again. A fetch already running at another
    /// size is superseded.
    pub fn start(&mut self, url: &str, cells: Size) -> bool {
        if !self.should_fetch(url, cells) {
            return false;
        }
        self.in_flight.insert(url.to_string(), cells);
        true
    }

    /// The fetch for `url` at `cells` is done: no longer in flight, and, depending on how it went,
    /// remembered as ready to draw (evicting the oldest ready cover first, if that means there are
    /// now too many) or as failed.
    pub fn finish(&mut self, url: String, cells: Size, outcome: Outcome) {
        // Superseded while it ran: the screen was resized and a fetch for the size it wants now is
        // on its way. Dropping this keeps a slow old result from evicting the new one.
        if self.in_flight.get(&url) != Some(&cells) {
            return;
        }
        self.in_flight.remove(&url);
        match outcome {
            Outcome::Ready { protocol, bytes } => {
                if !self.ready.contains_key(&url) {
                    self.ready_order.push_back(url.clone());
                    if self.ready_order.len() > READY_CAPACITY
                        && let Some(oldest) = self.ready_order.pop_front()
                    {
                        self.ready.remove(&oldest);
                    }
                }
                self.ready.insert(
                    url,
                    Ready {
                        cells,
                        protocol,
                        bytes,
                    },
                );
            }
            Outcome::Failed(_) => {
                self.failed.insert(url);
            }
        }
    }
}

/// A cover the screen currently wants: the URL to ask for (or look up in [`Covers::ready`]), and
/// the size, in cells, it should be encoded at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub url: String,
    pub cells: Size,
}

/// Which cover, if any, the main panel's current header, picture or now-playing track wants,
/// given its own area. `None` when there is no picker (covers off, or none detected), nothing to
/// show a cover of, or `header_area` is too small to fit one.
pub fn wanted(state: &app::State, covers: &Covers, header_area: Rect) -> Option<Wanted> {
    let picker = covers.picker()?;
    let (kind, id) = state.open_cover()?;
    let cells = cover_size(header_area, picker.font_size())?;
    let url = url_at(kind, id, cells, picker.font_size())?;
    Some(Wanted { url, cells })
}

/// The URL for `header`'s cover, at a size that fits `cells`, if it has a cover at all.
pub fn cover_url(header: &Header, cells: Size, font_size: FontSize) -> Option<String> {
    let id = header.cover()?;
    let kind = match header {
        Header::Album(_) => phonia_ipc::image::Kind::AlbumCover,
        Header::Playlist(_) => phonia_ipc::image::Kind::PlaylistCover,
        // Never reached: a radio's own `cover()` is always `None`, already returned above.
        Header::Radio { .. } => phonia_ipc::image::Kind::AlbumCover,
    };
    url_at(kind, id, cells, font_size)
}

/// The URL for an artist's `picture`, at a size that fits `cells`, if there is one at all.
pub fn picture_url(picture: Option<&str>, cells: Size, font_size: FontSize) -> Option<String> {
    url_at(
        phonia_ipc::image::Kind::ArtistPicture,
        picture?,
        cells,
        font_size,
    )
}

/// The URL for the now-playing track's own album `cover`, at a size that fits `cells`, if there
/// is one at all.
pub fn track_cover_url(cover: Option<&str>, cells: Size, font_size: FontSize) -> Option<String> {
    url_at(
        phonia_ipc::image::Kind::AlbumCover,
        cover?,
        cells,
        font_size,
    )
}

fn url_at(
    kind: phonia_ipc::image::Kind,
    id: &str,
    cells: Size,
    font_size: FontSize,
) -> Option<String> {
    let min_px = u32::from(cells.width) * u32::from(font_size.width);
    phonia_ipc::image::url(kind, id, min_px)
}

/// Columns left between the cover and the text beside it.
pub const COVER_GAP_COLS: u16 = 2;
/// Rows left between the cover and the list beneath it.
pub const COVER_GAP_ROWS: u16 = 2;

/// The size, in cells, a cover should be encoded at to fit `area`: the largest tier that is at
/// most a third of its height, with the width computed from the terminal's own font size so the
/// image comes out square in pixels, not just in character cells (usually about twice as tall as
/// wide). `None` if `area` is too small for even the smallest tier, or if any cover would leave
/// the header's own text beside it under 20 columns.
pub fn cover_size(area: Rect, font_size: FontSize) -> Option<Size> {
    /// Below this a cover is a smudge, so none shows instead.
    const MIN_HEIGHT: u16 = 6;
    /// However narrow the cover ends up, the header's text keeps this much room beside it.
    const MIN_TEXT_WIDTH: u16 = 20;
    /// The heights a cover is ever drawn at: every row up to 12, which is where the old cap was,
    /// so no terminal that showed a cover before #175 gets a smaller one now; then steps, because
    /// above that a ladder means dragging a terminal's edge re-encodes once per step instead of
    /// once every three rows, and two rows either way are invisible on a cover that big. The top
    /// step is also the download cap: at a typical 10x20 cell it asks TIDAL for 640 px, never the
    /// 1280 px one.
    const TIERS: [u16; 11] = [6, 7, 8, 9, 10, 11, 12, 16, 20, 24, 30];

    if font_size.width == 0 || font_size.height == 0 {
        return None;
    }
    // Square in pixels, not just in cells: cells are usually about twice as tall as wide.
    let width_of = |height: u16| {
        ((u32::from(height) * u32::from(font_size.height)) / u32::from(font_size.width)).max(1)
            as u16
    };
    let fits = |height: u16| {
        height + COVER_GAP_ROWS <= area.height
            && width_of(height) + COVER_GAP_COLS + MIN_TEXT_WIDTH <= area.width
    };
    // A third of the pane, so the list under it always keeps the larger share -- except for the
    // smallest tier, which shows whenever it fits at all: phonia is designed for 80x24, where a
    // third is only five rows, and losing the cover there would be a regression (#175).
    let height = TIERS
        .iter()
        .rev()
        .copied()
        .find(|&height| height <= area.height / 3 && fits(height))
        .or_else(|| fits(MIN_HEIGHT).then_some(MIN_HEIGHT))?;
    Some(Size::new(width_of(height), height))
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
            None,
        )
        .await;
        assert!(matches!(outcome, Outcome::Ready { .. }));
    }

    #[tokio::test]
    async fn a_source_that_fails_is_a_failed_outcome_not_a_panic() {
        let outcome = fetch(
            "http://example/cover.jpg".into(),
            Picker::halfblocks(),
            Size::new(8, 4),
            fake_source(Err("connection refused".into())),
            None,
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
            None,
        )
        .await;
        assert!(matches!(outcome, Outcome::Failed(_)));
    }

    const C: Size = Size::new(8, 4);

    fn ready_outcome() -> Outcome {
        Outcome::Ready {
            protocol: a_tiny_protocol(),
            bytes: Arc::new(vec![]),
        }
    }

    fn a_tiny_protocol() -> Protocol {
        Picker::halfblocks()
            .new_protocol(
                image::DynamicImage::ImageRgb8(RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 0]))),
                Size::new(1, 1),
                Resize::default(),
            )
            .unwrap()
    }

    #[test]
    fn the_same_url_is_not_fetched_twice_at_once() {
        let mut covers = Covers::disabled();
        assert!(covers.start("a", C));
        assert!(!covers.start("a", C), "already in flight");
        assert!(covers.start("b", C), "a different url is unaffected");
    }

    #[test]
    fn a_failed_cover_is_not_retried_but_stays_fetchable_elsewhere() {
        let mut covers = Covers::disabled();
        covers.start("a", C);
        covers.finish("a".into(), C, Outcome::Failed("404".into()));
        assert!(!covers.start("a", C), "a failure is remembered");
        assert!(covers.ready("a", C).is_none());
    }

    #[test]
    fn a_ready_cover_is_not_fetched_again_but_can_be_looked_up() {
        let mut covers = Covers::disabled();
        covers.start("b", C);
        covers.finish("b".into(), C, ready_outcome());
        assert!(
            !covers.start("b", C),
            "already ready: fetching again would be wasted"
        );
        assert!(covers.ready("b", C).is_some());
    }

    #[test]
    fn only_the_most_recently_finished_covers_are_kept() {
        let mut covers = Covers::disabled();
        for n in 0..READY_CAPACITY + 1 {
            let url = n.to_string();
            covers.start(&url, C);
            covers.finish(url, C, ready_outcome());
        }
        assert!(
            covers.ready("0", C).is_none(),
            "the oldest one fell out once one too many arrived"
        );
        assert!(covers.ready(&READY_CAPACITY.to_string(), C).is_some());
    }

    #[tokio::test]
    async fn a_resize_to_a_new_cell_size_re_encodes_without_fetching_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let bytes = a_jpeg();
        let source: CoverSource = Arc::new(move |_url: String| {
            counted.fetch_add(1, Ordering::SeqCst);
            let bytes = bytes.clone();
            Box::pin(async move { Ok(bytes) })
        });
        let picker = Picker::halfblocks();
        let url = "http://example/cover.jpg";
        let (small, large) = (Size::new(8, 4), Size::new(16, 8));

        let mut covers = Covers::new(Some(picker.clone()));
        assert!(covers.start(url, small));
        let outcome = fetch(url.into(), picker.clone(), small, source.clone(), None).await;
        covers.finish(url.into(), small, outcome);

        // The same url at a new size is wanted again, but its bytes are already held.
        assert!(covers.start(url, large));
        let held = covers.bytes(url);
        assert!(held.is_some(), "the bytes were kept");
        let outcome = fetch(url.into(), picker, large, source, held).await;
        covers.finish(url.into(), large, outcome);

        assert_eq!(calls.load(Ordering::SeqCst), 1, "downloaded exactly once");
        assert!(covers.ready(url, large).is_some());
    }

    #[test]
    fn a_cover_still_encoded_for_a_smaller_size_is_drawn_rather_than_leaving_the_space_blank() {
        let mut covers = Covers::disabled();
        covers.start("a", C);
        covers.finish("a".into(), C, ready_outcome()); // a 1x1 protocol
        let bigger = Size::new(C.width + 4, C.height + 2);
        assert!(
            covers.ready("a", bigger).is_some(),
            "still fits: drawn small"
        );
        assert!(covers.start("a", bigger), "but a better fit is asked for");
        assert!(covers.ready("a", bigger).is_some(), "and kept meanwhile");
        assert!(
            covers.ready("a", Size::new(0, 0)).is_none(),
            "one bigger than its area draws nothing, so it is not offered"
        );
    }

    #[test]
    fn a_result_for_a_size_the_screen_has_already_grown_out_of_is_dropped() {
        let mut covers = Covers::disabled();
        let newer = Size::new(16, 8);
        covers.start("a", C);
        covers.start("a", newer); // superseded
        covers.finish("a".into(), C, ready_outcome());
        assert!(
            covers.ready("a", newer).is_none(),
            "the stale result is gone"
        );
        assert!(
            !covers.start("a", newer),
            "and the newer fetch is still the one in flight"
        );
        covers.finish("a".into(), newer, ready_outcome());
        assert!(!covers.start("a", newer), "now ready at exactly that size");
    }

    #[test]
    fn a_cover_already_ready_at_this_very_size_is_not_fetched_again() {
        let mut covers = Covers::disabled();
        covers.start("a", C);
        covers.finish("a".into(), C, ready_outcome());
        assert!(!covers.start("a", C));
        assert!(covers.start("a", Size::new(9, 4)), "another size is");
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

    // --- cover_size, cover_url and wanted ------------------------------------------------------

    /// `Picker::halfblocks()`'s own fixed font size (10x20), used throughout: deterministic,
    /// unlike `from_query_stdio`, which actually asks the terminal.
    fn font_size() -> FontSize {
        Picker::halfblocks().font_size()
    }

    /// The pane `cover_size` is handed for a terminal of `width` x `height`.
    fn pane(width: u16, height: u16) -> Rect {
        crate::view::content_area(Rect::new(0, 0, width, height))
    }

    #[test]
    fn the_cover_grows_with_the_pane_in_tiers_and_stays_square_in_pixels() {
        for ((w, h), (cw, ch)) in [
            ((80, 24), (12, 6)),
            ((100, 30), (14, 7)),
            ((120, 40), (22, 11)),
            ((140, 45), (24, 12)),
            ((160, 50), (24, 12)),
            ((200, 60), (32, 16)),
            ((300, 80), (48, 24)),
        ] {
            let cells = cover_size(pane(w, h), font_size()).unwrap();
            assert_eq!((cells.width, cells.height), (cw, ch), "at {w}x{h}");
            // 10x20 px cells: twice as many columns as rows is square in pixels.
            assert_eq!(cells.width, cells.height * 2, "at {w}x{h}");
        }
    }

    #[test]
    fn the_cover_is_the_same_size_as_before_at_eighty_by_twenty_four() {
        assert_eq!(
            cover_size(pane(80, 24), font_size()),
            Some(Size::new(12, 6))
        );
    }

    #[test]
    fn the_cover_only_takes_the_ladders_heights_between_the_smallest_and_the_top() {
        let heights: Vec<u16> = (18..=120)
            .map(|rows| {
                cover_size(Rect::new(0, 0, 300, rows), font_size())
                    .unwrap()
                    .height
            })
            .collect();
        assert!(
            heights.windows(2).all(|pair| pair[0] <= pair[1]),
            "{heights:?}"
        );
        let mut distinct = heights.clone();
        distinct.dedup();
        assert_eq!(distinct, [6, 7, 8, 9, 10, 11, 12, 16, 20, 24, 30]);
        // Never more than a third of the pane, except the smallest tier.
        for rows in 18..=120u16 {
            let cells = cover_size(Rect::new(0, 0, 300, rows), font_size()).unwrap();
            assert!(cells.height <= (rows / 3).max(6), "{rows} rows: {cells:?}");
        }
    }

    #[test]
    fn a_pane_too_short_for_the_smallest_tier_shows_no_cover_at_all() {
        assert_eq!(cover_size(Rect::new(0, 0, 100, 7), font_size()), None);
        assert!(cover_size(Rect::new(0, 0, 100, 8), font_size()).is_some());
    }

    #[test]
    fn a_pane_too_narrow_leaves_the_header_its_twenty_columns() {
        // The smallest tier is 12 columns wide: 12 + 2 + 20 = 34.
        assert_eq!(cover_size(Rect::new(0, 0, 33, 30), font_size()), None);
        assert_eq!(
            cover_size(Rect::new(0, 0, 34, 30), font_size()),
            Some(Size::new(12, 6))
        );
        // A narrow pane picks a smaller tier rather than none: 14 + 2 + 20 = 36 does not fit in
        // 35 columns, 12 + 2 + 20 does, so the 7-row tier falls to the 6-row one.
        assert_eq!(
            cover_size(Rect::new(0, 0, 35, 30), font_size()),
            Some(Size::new(12, 6))
        );
    }

    #[test]
    fn the_tallest_tier_never_asks_tidal_for_the_1280_pixel_cover() {
        let top = cover_size(Rect::new(0, 0, 500, 300), font_size()).unwrap();
        assert_eq!(top.height, 30);
        let url = cover_url(&album_header(Some(COVER_ID)), top, font_size()).unwrap();
        assert!(url.ends_with("640x640.jpg"), "{url}");
    }

    #[test]
    fn too_small_an_area_fits_no_cover() {
        // Not even 6 rows plus the 2 of gap.
        assert_eq!(cover_size(Rect::new(0, 0, 100, 7), font_size()), None);
        // Tall enough, but no room left for the header's text beside a cover this wide.
        assert_eq!(cover_size(Rect::new(0, 0, 15, 30), font_size()), None);
    }

    fn album_header(cover: Option<&str>) -> Header {
        Header::Album(phonia_ipc::AlbumSummary {
            id: "9".into(),
            title: "Issues".into(),
            version: None,
            artists: vec![],
            release_date: None,
            track_count: None,
            duration_ms: None,
            explicit: false,
            quality: None,
            kind: None,
            copyright: None,
            cover: cover.map(str::to_string),
        })
    }

    fn playlist_header(cover: Option<&str>) -> Header {
        Header::Playlist(phonia_ipc::PlaylistSummary {
            id: "p-1".into(),
            title: "Road trip".into(),
            creator: None,
            description: None,
            track_count: None,
            duration_ms: None,
            cover: cover.map(str::to_string),
        })
    }

    const COVER_ID: &str = "3c6247c7-d0d7-4978-91b1-0bddc13f45b5";

    #[test]
    fn an_album_and_a_playlist_ask_for_their_own_kind_of_image() {
        let cells = Size::new(20, 10);
        let album_url = cover_url(&album_header(Some(COVER_ID)), cells, font_size()).unwrap();
        assert!(album_url.contains("3c6247c7/d0d7"), "{album_url}");
        assert!(album_url.ends_with("320x320.jpg"), "{album_url}");

        // At the smallest possible request, AlbumCover has an 80px size and PlaylistCover does
        // not (its smallest is 160): the two kinds pick from genuinely different size tables.
        let tiny = Size::new(1, 1);
        let album_url = cover_url(&album_header(Some(COVER_ID)), tiny, font_size()).unwrap();
        let playlist_url = cover_url(&playlist_header(Some(COVER_ID)), tiny, font_size()).unwrap();
        assert!(album_url.ends_with("80x80.jpg"), "{album_url}");
        assert!(playlist_url.ends_with("160x160.jpg"), "{playlist_url}");

        assert!(album_header(None).cover().is_none());
        assert_eq!(cover_url(&album_header(None), cells, font_size()), None);
    }

    /// `state`, on the Search section, with an album (or, with no cover, nothing distinguishing)
    /// open on top of its stack.
    fn opened_album(cover: Option<&str>) -> app::State {
        use crate::browse::{TrackListView, View};
        let mut state = app::State::default();
        state.sidebar.select(2, app::Section::ALL.len()); // Search
        state.search_views.push(
            0,
            View::TrackList(TrackListView::new(
                phonia_ipc::CatalogRef::Album { id: "9".into() },
                album_header(cover),
            )),
        );
        state
    }

    #[test]
    fn wanted_is_none_without_a_picker_without_a_cover_id_or_with_nothing_open() {
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(
            wanted(&opened_album(Some(COVER_ID)), &Covers::disabled(), area),
            None,
            "no picker: covers off"
        );
        let covers = Covers::new(Some(Picker::halfblocks()));
        assert_eq!(
            wanted(&opened_album(None), &covers, area),
            None,
            "nothing to show a cover of"
        );
        assert_eq!(
            wanted(&app::State::default(), &covers, area),
            None,
            "nothing open at all"
        );
    }

    #[test]
    fn wanted_names_the_open_albums_cover_at_the_size_the_header_fits() {
        let covers = Covers::new(Some(Picker::halfblocks()));
        let area = Rect::new(0, 0, 100, 30);
        let found = wanted(&opened_album(Some(COVER_ID)), &covers, area).unwrap();
        assert_eq!(found.cells, cover_size(area, font_size()).unwrap());
        assert_eq!(
            found.url,
            cover_url(&album_header(Some(COVER_ID)), found.cells, font_size()).unwrap()
        );
    }

    #[test]
    fn an_artists_picture_is_its_own_kind_distinct_from_an_albums_cover() {
        let cells = Size::new(1, 1);
        let found_picture = picture_url(Some(COVER_ID), cells, font_size()).unwrap();
        let found_album = cover_url(&album_header(Some(COVER_ID)), cells, font_size()).unwrap();
        // ArtistPicture has no 80px size; AlbumCover does: the same id, the same cells, two
        // different URLs, because they come from two different kinds' size tables.
        assert!(found_picture.ends_with("160x160.jpg"), "{found_picture}");
        assert!(found_album.ends_with("80x80.jpg"), "{found_album}");
        assert_eq!(picture_url(None, cells, font_size()), None);
    }

    /// `state`, on the Search section, with an artist (with or without a picture) open on top of
    /// its stack.
    fn opened_artist(picture: Option<&str>) -> app::State {
        use crate::browse::{ArtistView, View};
        let mut state = app::State::default();
        state.sidebar.select(2, app::Section::ALL.len()); // Search
        state.search_views.push(
            0,
            View::Artist(ArtistView::new(
                "780".into(),
                "Korn".into(),
                picture.map(str::to_string),
            )),
        );
        state
    }

    #[test]
    fn wanted_also_names_an_open_artists_picture() {
        let covers = Covers::new(Some(Picker::halfblocks()));
        let area = Rect::new(0, 0, 100, 30);
        let found = wanted(&opened_artist(Some(COVER_ID)), &covers, area).unwrap();
        assert_eq!(found.cells, cover_size(area, font_size()).unwrap());
        assert_eq!(
            found.url,
            picture_url(Some(COVER_ID), found.cells, font_size()).unwrap()
        );
        assert_eq!(
            wanted(&opened_artist(None), &covers, area),
            None,
            "no picture: nothing wanted"
        );
    }
}
