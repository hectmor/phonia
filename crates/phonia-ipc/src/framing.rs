//! Newline-delimited frames: one message per line.
//!
//! JSON produced by `serde_json::to_string` never contains a raw newline (they are escaped), so
//! a line is exactly one message. Empty lines are skipped, so a person typing into `socat` can
//! press Enter freely. A line longer than [`MAX_FRAME_BYTES`] is refused *before* it is buffered,
//! so a broken or hostile peer can't make the other side allocate without limit.

use std::fmt;
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// The longest message accepted, in bytes (a large queue is a few hundred kilobytes).
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum FrameError {
    /// A line exceeded [`MAX_FRAME_BYTES`].
    TooLarge,
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooLarge => write!(f, "a message is larger than {MAX_FRAME_BYTES} bytes"),
            FrameError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        FrameError::Io(error)
    }
}

/// Reads the next non-empty line into `buf` and returns it without the newline; `None` at the end
/// of the stream. A last line with no trailing newline still counts.
pub async fn read_frame<'a, R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &'a mut Vec<u8>,
) -> Result<Option<&'a [u8]>, FrameError> {
    loop {
        buf.clear();
        let mut ended_with_newline = false;
        loop {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                break; // end of the stream
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(newline) => {
                    if buf.len() + newline > MAX_FRAME_BYTES {
                        return Err(FrameError::TooLarge);
                    }
                    buf.extend_from_slice(&available[..newline]);
                    reader.consume(newline + 1);
                    ended_with_newline = true;
                    break;
                }
                None => {
                    let length = available.len();
                    if buf.len() + length > MAX_FRAME_BYTES {
                        return Err(FrameError::TooLarge);
                    }
                    buf.extend_from_slice(available);
                    reader.consume(length);
                }
            }
        }
        if buf.last() == Some(&b'\r') {
            buf.pop(); // a client on a line-ending-happy terminal
        }
        if !buf.is_empty() {
            return Ok(Some(&buf[..]));
        }
        if !ended_with_newline {
            return Ok(None); // nothing left
        }
        // An empty line: keep reading.
    }
}

/// Writes one frame: the bytes, a newline, and a flush.
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> io::Result<()> {
    let mut line = Vec::with_capacity(bytes.len() + 1);
    line.extend_from_slice(bytes);
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader, duplex};

    async fn frames_of(input: &[u8]) -> Vec<Vec<u8>> {
        let mut reader = BufReader::new(input);
        let (mut buf, mut frames) = (Vec::new(), Vec::new());
        while let Some(frame) = read_frame(&mut reader, &mut buf).await.unwrap() {
            frames.push(frame.to_vec());
        }
        frames
    }

    #[tokio::test]
    async fn reads_one_frame_per_line() {
        assert_eq!(frames_of(b"{\"a\":1}\n{\"b\":2}\n").await, [b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]);
    }

    #[tokio::test]
    async fn a_last_line_without_a_newline_still_counts() {
        assert_eq!(frames_of(b"one\ntwo").await, [b"one".to_vec(), b"two".to_vec()]);
    }

    #[tokio::test]
    async fn empty_lines_and_carriage_returns_are_ignored() {
        assert_eq!(frames_of(b"\n\none\r\n\n\ntwo\n\n").await, [b"one".to_vec(), b"two".to_vec()]);
        assert!(frames_of(b"").await.is_empty());
        assert!(frames_of(b"\n\n\n").await.is_empty());
    }

    #[tokio::test]
    async fn a_frame_split_across_many_reads_is_reassembled() {
        let (mut writer, reader) = duplex(4); // a tiny pipe forces partial reads
        let sender = tokio::spawn(async move {
            for piece in [&b"{\"type\":"[..], b"\"status\"", b"}\n", b"second\n"] {
                writer.write_all(piece).await.unwrap();
            }
        });
        let mut reader = BufReader::with_capacity(3, reader);
        let mut buf = Vec::new();
        assert_eq!(read_frame(&mut reader, &mut buf).await.unwrap().unwrap(), b"{\"type\":\"status\"}");
        assert_eq!(read_frame(&mut reader, &mut buf).await.unwrap().unwrap(), b"second");
        sender.await.unwrap();
        assert!(read_frame(&mut reader, &mut buf).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_oversized_line_is_refused_before_it_is_buffered() {
        let (mut writer, reader) = duplex(64 * 1024);
        let sender = tokio::spawn(async move {
            let chunk = vec![b'x'; 1024 * 1024];
            // Far more than the limit, and never a newline.
            for _ in 0..12 {
                if writer.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        let error = read_frame(&mut reader, &mut buf).await.unwrap_err();
        assert!(matches!(error, FrameError::TooLarge), "{error}");
        assert!(buf.len() <= MAX_FRAME_BYTES, "held {} bytes", buf.len());
        drop(reader);
        let _ = sender.await;
    }

    #[tokio::test]
    async fn a_frame_exactly_at_the_limit_is_accepted() {
        let mut input = vec![b'y'; MAX_FRAME_BYTES];
        input.push(b'\n');
        let mut reader = BufReader::new(&input[..]);
        let mut buf = Vec::new();
        assert_eq!(read_frame(&mut reader, &mut buf).await.unwrap().unwrap().len(), MAX_FRAME_BYTES);
    }

    #[tokio::test]
    async fn write_frame_adds_the_newline() {
        let mut out = Vec::new();
        write_frame(&mut out, b"{\"a\":1}").await.unwrap();
        assert_eq!(out, b"{\"a\":1}\n");
    }
}
