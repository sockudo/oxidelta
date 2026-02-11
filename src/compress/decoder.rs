// Streaming delta decoder.
//
// DeltaDecoder wraps StreamDecoder with:
//   - Streaming output via Write trait (doesn't accumulate full target)
//   - Progress tracking (bytes decoded, windows decoded)
//   - Window-by-window decoding for constant memory usage

use std::io::{Read, Write};

use crate::vcdiff::decoder::{DecodeError, SourceProvider, StreamDecoder};

// ---------------------------------------------------------------------------
// DeltaDecoder
// ---------------------------------------------------------------------------

/// Streaming delta decoder with progress tracking.
///
/// Decodes VCDIFF delta streams one window at a time, writing output
/// to any `impl Write` destination. Only one decoded window is in memory
/// at a time.
pub struct DeltaDecoder<R: Read> {
    inner: StreamDecoder<R>,
    bytes_decoded: u64,
    windows_decoded: u64,
    /// Reusable buffer for decoded window data (cleared between windows).
    window_buf: Vec<u8>,
}

impl<R: Read> DeltaDecoder<R> {
    /// Create a new streaming decoder.
    pub fn new(reader: R) -> Self {
        Self {
            inner: StreamDecoder::new(reader, true),
            bytes_decoded: 0,
            windows_decoded: 0,
            window_buf: Vec::new(),
        }
    }

    /// Create a decoder that optionally skips checksum verification.
    pub fn with_checksum(reader: R, verify: bool) -> Self {
        Self {
            inner: StreamDecoder::new(reader, verify),
            bytes_decoded: 0,
            windows_decoded: 0,
            window_buf: Vec::new(),
        }
    }

    /// Decode all windows, writing output to `writer`.
    ///
    /// Source must implement `SourceProvider` (e.g., `&[u8]`).
    /// Returns the total number of bytes decoded.
    pub fn decode_to<S: SourceProvider, W: Write>(
        &mut self,
        source: &mut S,
        writer: &mut W,
    ) -> Result<u64, DecodeError> {
        while self.decode_window_to(source, writer)?.is_some() {}
        Ok(self.bytes_decoded)
    }

    /// Decode the next window, writing its output to `writer`.
    ///
    /// Returns `Some(window_size)` if a window was decoded, or `None`
    /// if there are no more windows.
    pub fn decode_window_to<S: SourceProvider, W: Write>(
        &mut self,
        source: &mut S,
        writer: &mut W,
    ) -> Result<Option<u64>, DecodeError> {
        self.window_buf.clear();
        let has_more = self.inner.decode_window(source, &mut self.window_buf)?;

        if !has_more {
            return Ok(None);
        }

        let window_size = self.window_buf.len() as u64;
        writer
            .write_all(&self.window_buf)
            .map_err(DecodeError::Io)?;

        self.bytes_decoded += window_size;
        self.windows_decoded += 1;

        Ok(Some(window_size))
    }

    /// Total bytes decoded so far.
    pub fn bytes_decoded(&self) -> u64 {
        self.bytes_decoded
    }

    /// Number of windows decoded so far.
    pub fn windows_decoded(&self) -> u64 {
        self.windows_decoded
    }
}

// ---------------------------------------------------------------------------
// Convenience function
// ---------------------------------------------------------------------------

/// Decode a VCDIFF delta from memory.
///
/// This is a convenience wrapper around DeltaDecoder for in-memory use.
pub fn decode_all(source: &[u8], delta: &[u8]) -> Result<Vec<u8>, DecodeError> {
    // Fast path for in-memory callers: avoid the extra window staging copy
    // performed by DeltaDecoder::decode_to.
    crate::vcdiff::decoder::decode_memory(delta, source)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::encoder::{self, CompressOptions};

    fn encode_test_data(source: &[u8], target: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        encoder::encode_all(&mut output, source, target, CompressOptions::default()).unwrap();
        output
    }

    #[test]
    fn decode_all_roundtrip() {
        let source = b"Hello, world!";
        let target = b"Hello, earth!";
        let delta = encode_test_data(source, target);
        let decoded = decode_all(source, &delta).unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn streaming_decode_to_writer() {
        let source = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let target = b"ABCDEFGHIJKLMNOP--CHANGED--0123456789!!!";
        let delta = encode_test_data(source, target);

        let mut decoder = DeltaDecoder::new(std::io::Cursor::new(&delta));
        let mut src: &[u8] = source;
        let mut output = Vec::new();
        let total = decoder.decode_to(&mut src, &mut output).unwrap();

        assert_eq!(output, target);
        assert_eq!(total, target.len() as u64);
        assert_eq!(decoder.bytes_decoded(), target.len() as u64);
        assert!(decoder.windows_decoded() >= 1);
    }

    #[test]
    fn window_by_window_decode() {
        let source = b"source data for windowed decoding test";
        let target = b"source data for windowed decoding test -- with changes!";
        let delta = encode_test_data(source, target);

        let mut decoder = DeltaDecoder::new(std::io::Cursor::new(&delta));
        let mut src: &[u8] = source;
        let mut output = Vec::new();

        let mut window_count = 0u64;
        while let Some(size) = decoder.decode_window_to(&mut src, &mut output).unwrap() {
            assert!(size > 0);
            window_count += 1;
        }
        assert_eq!(window_count, decoder.windows_decoded());
        assert_eq!(output, target);
    }

    #[test]
    fn empty_target() {
        let delta = encode_test_data(b"", b"");
        let decoded = decode_all(b"", &delta).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn no_checksum_verification() {
        let target = b"test data";
        let delta = encode_test_data(b"", target);

        let mut decoder = DeltaDecoder::with_checksum(std::io::Cursor::new(&delta), false);
        let mut src: &[u8] = b"";
        let mut output = Vec::new();
        decoder.decode_to(&mut src, &mut output).unwrap();
        assert_eq!(output, target);
    }
}
