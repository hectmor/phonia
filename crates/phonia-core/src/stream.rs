//! Streaming `symphonia::core::io::MediaSource` backed by a background download task, so
//! playback can start after the first few segments instead of after the whole track has been
//! downloaded into memory.
//!
//! ## Producer/consumer split
//!
//! A tokio task (the "producer") downloads segments **in order** and sends each one over a
//! bounded `tokio::sync::mpsc::channel`. The channel's capacity ([`PREFETCH_DEPTH`]) *is* the
//! read-ahead buffer and the backpressure mechanism: when the decoder is ahead of the network,
//! the channel is empty and [`SegmentStream::read`] blocks in `Receiver::blocking_recv`; when the
//! decoder is behind, the channel fills up and the producer's `send().await` parks until the
//! decoder catches up. No manual buffering (`Mutex`/`Condvar`) is needed -- the channel itself
//! provides both the buffer and the backpressure.
//!
//! ## Cancellation
//!
//! `SegmentStream` owns the `Receiver` half. Dropping it (e.g. because the caller gave up on
//! playback) makes the producer's next `send().await` fail immediately, and the producer returns
//! at that point -- no leaked task, no panic.
//!
//! ## Seeking
//!
//! Symphonia's isomp4 demuxer only calls `Seek::seek` on a source when `is_seekable()` returns
//! true (see `symphonia-format-isomp4-0.6.1/src/demuxer.rs:151-274`); for a non-seekable source
//! it stays within its internal ring buffer via `seek_buffered`, which never reaches this type at
//! all. So in practice `Seek::seek` is not called during ordinary sequential fragmented-MP4
//! playback. It is still implemented here, on a best-effort basis, for forward positioning: a
//! forward seek is served by discarding bytes as they arrive off the channel. A backward seek
//! would require re-downloading or buffering already-consumed bytes, which is issue #10's job,
//! not this one's -- so it returns a clear error instead of silently returning wrong data.

use crate::dash::DashSegments;
use anyhow::{Result, anyhow};
use bytes::Bytes;
use futures_util::StreamExt;
use std::io::{self, Read, Seek, SeekFrom, Write};
use symphonia::core::io::MediaSource;
use tokio::sync::mpsc;

/// How many segments (or response chunks, for the JSON-manifest path) may be in flight ahead of
/// the decoder at once. This is the bounded channel's capacity: it is both the read-ahead buffer
/// and the backpressure limit (see the module doc).
const PREFETCH_DEPTH: usize = 3;

/// How many times a single segment/chunk request is retried on a transient failure before giving
/// up. Moved here (unchanged) from the old `tidal::download_dash`.
const MAX_SEGMENT_RETRIES: u32 = 3;

/// One item sent from the producer task to [`SegmentStream`]: either a chunk of bytes, in
/// download order, or the error that ended the download early.
type ChunkResult = Result<Bytes>;

/// A `Read + Seek + Send + Sync` source fed by a background download task, suitable for handing
/// to `symphonia::core::io::MediaSourceStream`/`Decoder::open`.
///
/// See the module doc for the producer/consumer design, cancellation and seeking behaviour.
pub struct SegmentStream {
    receiver: mpsc::Receiver<ChunkResult>,
    /// Bytes already received but not yet handed to the reader.
    current: Bytes,
    /// Total bytes served to the reader so far (i.e. the current logical read position).
    position: u64,
    /// `true` once the channel has yielded its last item (`None`, or an `Err` already surfaced).
    /// Once set, further reads return `Ok(0)` (true EOF) without touching the channel again.
    finished: bool,
    /// Optional tee sink (e.g. for `--save-mp4`): every byte that passes through this stream,
    /// whether returned to the reader or discarded by a forward seek, is written here too.
    tee: Option<Box<dyn Write + Send + Sync>>,
}

impl SegmentStream {
    fn new(receiver: mpsc::Receiver<ChunkResult>, tee: Option<Box<dyn Write + Send + Sync>>) -> Self {
        SegmentStream { receiver, current: Bytes::new(), position: 0, finished: false, tee }
    }

    /// Blocks (via `Receiver::blocking_recv`) until either more bytes are available, the stream
    /// ends, or the producer reports an error. Returns `true` if `self.current` now has bytes to
    /// serve, `false` at true end of stream. A producer error is surfaced as an `io::Error`.
    fn refill(&mut self) -> io::Result<bool> {
        if self.finished {
            return Ok(false);
        }
        match self.receiver.blocking_recv() {
            Some(Ok(chunk)) => {
                self.current = chunk;
                Ok(true)
            }
            Some(Err(e)) => {
                self.finished = true;
                Err(io::Error::other(e))
            }
            None => {
                self.finished = true;
                Ok(false)
            }
        }
    }

    /// Discards (but still tees) up to `n` bytes from the stream, advancing `position`. Used to
    /// implement forward seeking. Stops early (without error) at true end of stream, same as
    /// `read` returning `Ok(0)`.
    fn discard(&mut self, mut n: u64) -> io::Result<u64> {
        while n > 0 {
            if self.current.is_empty() && !self.refill()? {
                break;
            }
            let take = (self.current.len() as u64).min(n) as usize;
            let chunk = self.current.split_to(take);
            if let Some(tee) = self.tee.as_mut() {
                tee.write_all(&chunk)?;
            }
            self.position += take as u64;
            n -= take as u64;
        }
        Ok(self.position)
    }
}

impl Read for SegmentStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.current.is_empty() && !self.refill()? {
            return Ok(0);
        }
        let n = buf.len().min(self.current.len());
        let chunk = self.current.split_to(n);
        buf[..n].copy_from_slice(&chunk);
        if let Some(tee) = self.tee.as_mut() {
            tee.write_all(&chunk)?;
        }
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for SegmentStream {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match pos {
            SeekFrom::Current(0) => Ok(self.position),
            SeekFrom::Current(n) if n > 0 => self.discard(n as u64),
            SeekFrom::Start(target) if target >= self.position => {
                self.discard(target - self.position)
            }
            _ => Err(io::Error::other(
                "SegmentStream does not support seeking backwards yet (see issue #10); only \
                 forward positioning over the live download is supported",
            )),
        }
    }
}

impl MediaSource for SegmentStream {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Downloads a single segment/chunk with retries, treating HTTP 404/403 as "does not exist"
/// (`Ok(None)`) rather than a retryable failure -- the caller decides whether that means "no more
/// segments" or a real error. Moved here (unchanged in behaviour) from the old
/// `tidal::download_segment`.
async fn download_segment(http: &reqwest::Client, url: &str) -> Result<Option<Bytes>> {
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 1..=MAX_SEGMENT_RETRIES {
        match http.get(url).send().await {
            Ok(response) => {
                let status = response.status();
                if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::FORBIDDEN {
                    // Treated by the caller as "no more segments", not a retryable failure.
                    return Ok(None);
                }
                if !status.is_success() {
                    last_err = Some(anyhow!("HTTP {status} downloading segment"));
                    continue;
                }
                match response.bytes().await {
                    Ok(bytes) => return Ok(Some(bytes)),
                    Err(e) => last_err = Some(anyhow::Error::new(e).context("reading segment bytes")),
                }
            }
            Err(e) => last_err = Some(anyhow::Error::new(e).context(format!("attempt {attempt}/{MAX_SEGMENT_RETRIES}"))),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("unknown failure downloading segment")))
}

/// Downloads the init segment followed by the media segments of `dash` from `first_segment` on,
/// sending each one over `tx` in order. The init segment always comes first, since a decoder can
/// make nothing of a media segment without it. Honours `segment_count` when known; otherwise
/// stops at the first 404/403 (requiring at least one successful media segment first), same
/// rules the old `tidal::download_dash` used.
async fn run_dash_download(
    http: reqwest::Client,
    dash: DashSegments,
    first_segment: u32,
    tx: mpsc::Sender<ChunkResult>,
) {
    // `println!` locks stdout for the whole line, so complete lines from different threads can't
    // interleave -- only unterminated `\r` lines can. So this producer prints exactly one
    // complete line up front and never touches stdout again; only the ALSA sink owns a `\r`
    // progress line for the rest of playback.
    match dash.segment_count {
        Some(total) => crate::note!("Buffering {PREFETCH_DEPTH} segments ahead (track has {total} segments)..."),
        None => crate::note!("Buffering {PREFETCH_DEPTH} segments ahead..."),
    }

    let init = match download_segment(&http, &dash.init_url).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            let _ = tx
                .send(Err(anyhow!(
                    "the initialization segment does not exist (HTTP 404/403): {}",
                    dash.init_url
                )))
                .await;
            return;
        }
        Err(e) => {
            let _ = tx.send(Err(e)).await;
            return;
        }
    };
    if tx.send(Ok(init)).await.is_err() {
        return; // The decoder side was dropped (cancellation); stop cleanly, no panic.
    }

    let known_range = dash.segment_range_from(first_segment);
    let mut segment_number = first_segment;
    let mut downloaded = 0u32;

    loop {
        if let Some(range) = &known_range
            && segment_number > *range.end()
        {
            break;
        }

        let url = dash.segment_url(segment_number);
        match download_segment(&http, &url).await {
            Ok(Some(bytes)) => {
                downloaded += 1;
                if tx.send(Ok(bytes)).await.is_err() {
                    return; // Decoder side gone; stop cleanly.
                }
            }
            Ok(None) => {
                if let Some(count) = dash.segment_count {
                    let _ = tx
                        .send(Err(anyhow!(
                            "segment {segment_number} does not exist (HTTP 404/403) but the \
                             manifest expected {count} segments"
                        )))
                        .await;
                } else if downloaded == 0 {
                    let _ = tx
                        .send(Err(anyhow!(
                            "could not download any media segment (the first one already \
                             returned 404/403): {url}"
                        )))
                        .await;
                }
                // Otherwise (segment count unknown, at least one segment ok): a 404/403 just
                // means we've reached the end of the track -- not an error.
                break;
            }
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                break;
            }
        }

        segment_number += 1;
    }
}

/// Opens a streaming source over a DASH (fragmented MP4) representation: sends the init segment
/// first, then media segments in order, honouring `segment_count` when known and otherwise
/// stopping at the first 404/403. Only spawns the download task; fetches nothing itself, so this
/// function returns immediately.
pub fn open_dash(http: &reqwest::Client, dash: &DashSegments, tee: Option<std::fs::File>) -> SegmentStream {
    open_dash_from(http, dash, dash.start_number, tee)
}

/// Like [`open_dash`], starting at media segment `first_segment` (a `$Number$`, e.g. one from
/// [`DashSegments::segment_for_time`]). This is how a seek is done on a network stream, which
/// can't be rewound: open a new stream at the right segment and drop the old one. The init
/// segment is still sent first.
pub fn open_dash_from(
    http: &reqwest::Client,
    dash: &DashSegments,
    first_segment: u32,
    tee: Option<std::fs::File>,
) -> SegmentStream {
    let (tx, rx) = mpsc::channel(PREFETCH_DEPTH);
    let http = http.clone();
    let dash = dash.clone();
    tokio::spawn(run_dash_download(http, dash, first_segment, tx));
    SegmentStream::new(rx, tee.map(|f| Box::new(f) as Box<dyn Write + Send + Sync>))
}

/// Opens a streaming source over a single URL (the JSON-manifest / LOSSLESS path), streaming the
/// HTTP response body instead of buffering it whole. Only spawns the download task; fetches
/// nothing itself, so this function returns immediately.
pub fn open_url(http: &reqwest::Client, url: &str, tee: Option<std::fs::File>) -> SegmentStream {
    let (tx, rx) = mpsc::channel(PREFETCH_DEPTH);
    let http = http.clone();
    let url = url.to_string();
    tokio::spawn(async move {
        // See the comment in `run_dash_download`: one complete line up front, nothing else.
        crate::note!("Buffering...");

        let response = match http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                let _ = tx
                    .send(Err(anyhow::Error::new(e).context("requesting the audio (JSON manifest)")))
                    .await;
                return;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let _ = tx.send(Err(anyhow!("HTTP {status} downloading {url}"))).await;
            return;
        }

        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            match chunk {
                Ok(bytes) => {
                    if tx.send(Ok(bytes)).await.is_err() {
                        return; // Decoder side gone; stop cleanly.
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(anyhow::Error::new(e).context("reading the audio stream"))).await;
                    return;
                }
            }
        }
    });
    SegmentStream::new(rx, tee.map(|f| Box::new(f) as Box<dyn Write + Send + Sync>))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A `Write` sink that just appends to a shared `Vec<u8>`, so tests can assert on exactly
    /// what was teed without touching the filesystem.
    #[derive(Clone, Default)]
    struct RecordingSink(Arc<Mutex<Vec<u8>>>);

    impl Write for RecordingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Builds a `SegmentStream` fed by a channel this test controls directly (no network), along
    /// with the sender half and the recording tee's shared buffer.
    fn test_stream() -> (SegmentStream, mpsc::Sender<ChunkResult>, Arc<Mutex<Vec<u8>>>) {
        let (tx, rx) = mpsc::channel(PREFETCH_DEPTH);
        let sink = RecordingSink::default();
        let recorded = sink.0.clone();
        let stream = SegmentStream::new(rx, Some(Box::new(sink)));
        (stream, tx, recorded)
    }

    fn test_stream_no_tee() -> (SegmentStream, mpsc::Sender<ChunkResult>) {
        let (tx, rx) = mpsc::channel(PREFETCH_DEPTH);
        (SegmentStream::new(rx, None), tx)
    }

    #[test]
    fn sequential_reads_cross_chunk_boundaries() {
        let (mut stream, tx, _recorded) = test_stream();
        tx.try_send(Ok(Bytes::from_static(b"abc"))).unwrap();
        tx.try_send(Ok(Bytes::from_static(b"defgh"))).unwrap();
        drop(tx);

        let mut buf = [0u8; 4];
        assert_eq!(stream.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"abc");

        // The next read spans into the second chunk transparently.
        let mut buf = [0u8; 4];
        assert_eq!(stream.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf, b"defg");

        let mut buf = [0u8; 4];
        assert_eq!(stream.read(&mut buf).unwrap(), 1);
        assert_eq!(&buf[..1], b"h");

        // Clean EOF once the channel is closed and drained.
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn partial_reads_smaller_than_a_chunk() {
        let (mut stream, tx, _recorded) = test_stream();
        tx.try_send(Ok(Bytes::from_static(b"0123456789"))).unwrap();
        drop(tx);

        let mut buf = [0u8; 3];
        assert_eq!(stream.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"012");
        assert_eq!(stream.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"345");
        assert_eq!(stream.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"678");
        assert_eq!(stream.read(&mut buf).unwrap(), 1);
        assert_eq!(&buf[..1], b"9");
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn reads_larger_than_a_chunk() {
        let (mut stream, tx, _recorded) = test_stream();
        tx.try_send(Ok(Bytes::from_static(b"ab"))).unwrap();
        tx.try_send(Ok(Bytes::from_static(b"cd"))).unwrap();
        drop(tx);

        // A single big read only gets what's in the *current* chunk; the caller (symphonia) is
        // expected to loop, same as any other `Read` impl.
        let mut buf = [0u8; 100];
        assert_eq!(stream.read(&mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"ab");
        assert_eq!(stream.read(&mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"cd");
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn producer_error_surfaces_as_io_error_with_context() {
        let (mut stream, tx, _recorded) = test_stream();
        tx.try_send(Ok(Bytes::from_static(b"ok"))).unwrap();
        tx.try_send(Err(anyhow!("segment 3 does not exist (HTTP 404)"))).unwrap();
        drop(tx);

        let mut buf = [0u8; 2];
        assert_eq!(stream.read(&mut buf).unwrap(), 2);

        let err = stream.read(&mut buf).unwrap_err();
        assert!(err.to_string().contains("segment 3 does not exist (HTTP 404)"));

        // Once surfaced, the stream is done: further reads are a clean EOF, not a repeated error
        // or a panic.
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn stream_ends_cleanly_when_the_sender_is_dropped_with_nothing_sent() {
        let (mut stream, tx) = test_stream_no_tee();
        drop(tx);
        let mut buf = [0u8; 8];
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn forward_seek_by_discard_skips_bytes_and_tees_them() {
        let (mut stream, tx, recorded) = test_stream();
        tx.try_send(Ok(Bytes::from_static(b"0123456789"))).unwrap();
        drop(tx);

        // SeekFrom::Current(0) reports the position without consuming anything.
        assert_eq!(stream.stream_position().unwrap(), 0);

        // Skip the first 4 bytes.
        assert_eq!(stream.seek(SeekFrom::Current(4)).unwrap(), 4);

        let mut buf = [0u8; 3];
        assert_eq!(stream.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"456");

        // SeekFrom::Start also works, as long as it doesn't go backwards.
        assert_eq!(stream.seek(SeekFrom::Start(9)).unwrap(), 9);
        let mut buf = [0u8; 4];
        assert_eq!(stream.read(&mut buf).unwrap(), 1);
        assert_eq!(&buf[..1], b"9");

        // The tee sees every byte that passed through, including the discarded ones.
        assert_eq!(&*recorded.lock().unwrap(), b"0123456789");
    }

    #[test]
    fn backward_seek_is_rejected_not_silently_wrong() {
        let (mut stream, tx, _recorded) = test_stream();
        tx.try_send(Ok(Bytes::from_static(b"0123456789"))).unwrap();
        drop(tx);

        let mut buf = [0u8; 5];
        assert_eq!(stream.read(&mut buf).unwrap(), 5);
        assert_eq!(stream.position, 5);

        let err = stream.seek(SeekFrom::Start(1)).unwrap_err();
        assert!(err.to_string().contains("does not support seeking backwards"));

        let err = stream.seek(SeekFrom::Current(-1)).unwrap_err();
        assert!(err.to_string().contains("does not support seeking backwards"));

        let err = stream.seek(SeekFrom::End(0)).unwrap_err();
        assert!(err.to_string().contains("does not support seeking backwards"));

        // Position is unchanged by the rejected seeks.
        assert_eq!(stream.position, 5);
    }

    #[test]
    fn tee_writes_exactly_the_bytes_that_were_read() {
        let (mut stream, tx, recorded) = test_stream();
        tx.try_send(Ok(Bytes::from_static(b"hello "))).unwrap();
        tx.try_send(Ok(Bytes::from_static(b"world"))).unwrap();
        drop(tx);

        let mut out = Vec::new();
        let mut buf = [0u8; 4];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }

        assert_eq!(out, b"hello world");
        assert_eq!(&*recorded.lock().unwrap(), b"hello world");
    }

    /// Cancellation: dropping the `SegmentStream` drops its `Receiver`, which must make a
    /// producer's `send().await` fail promptly (so a real download task, awaiting `send`, would
    /// return and end instead of leaking).
    #[tokio::test]
    async fn producer_send_fails_once_the_stream_is_dropped() {
        let (tx, rx) = mpsc::channel::<ChunkResult>(PREFETCH_DEPTH);
        let stream = SegmentStream::new(rx, None);
        drop(stream);

        let result = tx.send(Ok(Bytes::from_static(b"too late"))).await;
        assert!(result.is_err(), "send should fail once the receiver has been dropped");
    }

    #[test]
    fn is_not_seekable_and_has_no_known_length() {
        let (stream, _tx) = test_stream_no_tee();
        assert!(!stream.is_seekable());
        assert_eq!(stream.byte_len(), None);
    }

    /// A minimal local HTTP server: `/init` answers `INIT`, `/seg/N` answers `SEG<N>` for N up to
    /// `last_segment` and 404 beyond it. Returns its base URL.
    async fn serve_segments(last_segment: u32) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut request = [0u8; 2048];
                    let n = socket.read(&mut request).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&request[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("");
                    let (status, body) = match path.strip_prefix("/seg/").and_then(|n| n.parse::<u32>().ok()) {
                        _ if path == "/init" => ("200 OK", "INIT".to_string()),
                        Some(n) if (1..=last_segment).contains(&n) => ("200 OK", format!("SEG{n}")),
                        _ => ("404 Not Found", String::new()),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        base
    }

    fn dash_at(base: &str, segments: u32) -> DashSegments {
        DashSegments {
            init_url: format!("{base}/init"),
            media_url_template: format!("{base}/seg/$Number$"),
            start_number: 1,
            segment_count: Some(segments),
            codecs: None,
            timescale: 1,
            timing: crate::dash::SegmentTiming::Unknown,
            presentation_duration: None,
        }
    }

    /// Reads a stream to its end off the async runtime, as the audio thread does.
    async fn read_all(mut stream: SegmentStream) -> Vec<u8> {
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            io::Read::read_to_end(&mut stream, &mut out).unwrap();
            out
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_dash_streams_the_init_segment_then_every_segment() {
        let base = serve_segments(5).await;
        let stream = open_dash(&reqwest::Client::new(), &dash_at(&base, 5), None);
        assert_eq!(read_all(stream).await, b"INITSEG1SEG2SEG3SEG4SEG5");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_dash_from_sends_the_init_segment_first_and_then_starts_at_the_given_segment() {
        let base = serve_segments(5).await;
        let stream = open_dash_from(&reqwest::Client::new(), &dash_at(&base, 5), 3, None);
        assert_eq!(read_all(stream).await, b"INITSEG3SEG4SEG5");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_dash_from_the_last_segment_and_past_the_end() {
        let base = serve_segments(5).await;
        let http = reqwest::Client::new();
        assert_eq!(read_all(open_dash_from(&http, &dash_at(&base, 5), 5, None)).await, b"INITSEG5");
        assert_eq!(read_all(open_dash_from(&http, &dash_at(&base, 5), 6, None)).await, b"INIT");
    }
}

