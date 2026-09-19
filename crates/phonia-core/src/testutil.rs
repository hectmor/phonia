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
    for i in 0..frames * 2 {
        out.extend_from_slice(&sample_at(i).to_le_bytes());
    }
    out
}

/// What the decoder must produce for `sample_at(from_sample..to_sample)`: left-justified in an
/// `i32`.
pub fn expected(from_sample: usize, to_sample: usize) -> Vec<i32> {
    (from_sample..to_sample).map(|i| i32::from(sample_at(i)) << 16).collect()
}
