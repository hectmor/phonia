//! Helpers shared by unit tests across the crate.

/// Sample rate of the WAVs built by [`wav`].
pub const RATE: u32 = 44_100;

/// A distinctive value per sample, so a misplaced or repeated frame shows up immediately.
pub fn sample_at(index: usize) -> i16 {
    ((index * 7) % 20_001) as i16 - 10_000
}

/// A 16-bit stereo PCM WAV with `frames` frames, where sample `i` (counting L and R) is
/// `sample_at(i)`. WAV needs no external file and is seekable in memory.
pub fn wav(frames: usize) -> Vec<u8> {
    wav_slice(0, frames)
}

/// Like [`wav`], but holding frames `from_frame..from_frame + frames` of the same signal: what a
/// stream reopened partway through a track carries.
pub fn wav_slice(from_frame: usize, frames: usize) -> Vec<u8> {
    let data_len = (frames * 4) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // channels
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 4).to_le_bytes()); // byte rate
    out.extend_from_slice(&4u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in from_frame * 2..(from_frame + frames) * 2 {
        out.extend_from_slice(&sample_at(i).to_le_bytes());
    }
    out
}

/// What the decoder must produce for `sample_at(from_sample..to_sample)`: left-justified in an
/// `i32`.
pub fn expected(from_sample: usize, to_sample: usize) -> Vec<i32> {
    (from_sample..to_sample).map(|i| i32::from(sample_at(i)) << 16).collect()
}

/// A source that can be read but never repositioned, like a network stream.
pub struct NonSeekable<R>(pub R);

impl<R: std::io::Read> std::io::Read for NonSeekable<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl<R> std::io::Seek for NonSeekable<R> {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::other("this source can't seek"))
    }
}

impl<R: std::io::Read + Send + Sync> symphonia::core::io::MediaSource for NonSeekable<R> {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// A private `dbus-daemon`, so tests can pretend to be WirePlumber or a keyring without ever
/// touching the user's bus. Needs the `dbus-daemon` binary; the tests that use it are ignored by
/// default.
pub struct Bus {
    child: std::process::Child,
    config: std::path::PathBuf,
    pub address: String,
}

impl Bus {
    pub fn start() -> Self {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        // A bus of its own, with no service activation: the session bus's config would start the
        // user's real services (a keyring, say) on demand, and tests must never reach those.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let config = std::env::temp_dir().join(format!("phonia-test-bus-{}-{n}.conf", std::process::id()));
        std::fs::write(
            &config,
            "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>\
             <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
             <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is needed for these tests");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
        Bus { child, config, address: address.trim().to_string() }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config);
    }
}
