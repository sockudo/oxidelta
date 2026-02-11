// Streaming delta encoder.
//
// DeltaEncoder provides a streaming API for delta compression:
//   - Source is indexed once upfront (MatchEngine reused across windows)
//   - Target data is fed in chunks via write_target()
//   - Each full window is compressed and written immediately
//   - Constant memory: only one target window buffered at a time

use std::io::Write;

use crate::hash::config::{self, MatcherConfig};
use crate::hash::matching::MatchEngine;
use crate::vcdiff::code_table::Instruction;
use crate::vcdiff::encoder::{SourceWindow, StreamEncoder, WindowEncoder};

use super::pipeline;
use super::secondary::{self, SecondaryCompression};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Configuration for the streaming delta encoder.
#[derive(Debug, Clone)]
pub struct CompressOptions {
    /// Compression level (0-9). Level 0 = store only (no matching).
    pub level: u32,
    /// Maximum target window size in bytes.
    pub window_size: usize,
    /// Emit Adler-32 checksums per window.
    pub checksum: bool,
    /// Secondary compression algorithm for VCDIFF sections.
    pub secondary: SecondaryCompression,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            level: 6,
            window_size: 1 << 23, // 8 MiB
            checksum: true,
            secondary: SecondaryCompression::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum EncodeError {
    Io(std::io::Error),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for EncodeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// DeltaEncoder
// ---------------------------------------------------------------------------

/// Streaming delta encoder.
///
/// Indexes the source once upfront, then processes target data in windows.
/// Each completed window is immediately compressed and written to the output.
///
/// # Example
/// ```no_run
/// use oxidelta::compress::encoder::{DeltaEncoder, CompressOptions};
/// let source = b"original data";
/// let target = b"modified data";
/// let mut output = Vec::new();
/// let mut enc = DeltaEncoder::new(&mut output, source, CompressOptions::default());
/// enc.write_target(target).unwrap();
/// enc.finish().unwrap();
/// ```
pub struct DeltaEncoder<'s, W: Write> {
    stream: StreamEncoder<W>,
    opts: CompressOptions,
    _config: MatcherConfig,
    source: &'s [u8],
    engine: Option<MatchEngine>,
    buffer: Vec<u8>,
    bytes_in: u64,
    windows_written: u64,
    /// Section size hints from the previous window (for capacity pre-allocation).
    last_data_size: usize,
    last_inst_size: usize,
    last_addr_size: usize,
}

impl<'s, W: Write> DeltaEncoder<'s, W> {
    /// Create a new streaming encoder.
    ///
    /// The source is indexed immediately. For level 0, no index is built.
    pub fn new(writer: W, source: &'s [u8], opts: CompressOptions) -> Self {
        let config = config::config_for_level(opts.level);

        let mut stream = StreamEncoder::new(writer, opts.checksum);
        if let Some(backend) = opts.secondary.backend() {
            stream.set_secondary_id(backend.id());
        }

        // Build the match engine and index the source (reused across windows).
        let engine = if opts.level > 0 && !source.is_empty() {
            let src: &[u8] = source;
            let mut eng = MatchEngine::new(config, src.len() as u64, opts.window_size.max(64));
            eng.index_source(&src);
            Some(eng)
        } else if opts.level > 0 {
            // No source, but still do target self-matching.
            Some(MatchEngine::new(config, 0, opts.window_size.max(64)))
        } else {
            None // Level 0: no matching at all.
        };

        Self {
            stream,
            opts,
            _config: config,
            source,
            engine,
            buffer: Vec::new(),
            bytes_in: 0,
            windows_written: 0,
            last_data_size: 0,
            last_inst_size: 0,
            last_addr_size: 0,
        }
    }

    /// Feed target data to the encoder.
    ///
    /// Data is buffered internally. Whenever the buffer reaches `window_size`,
    /// a complete window is encoded and written to the output.
    pub fn write_target(&mut self, data: &[u8]) -> Result<(), EncodeError> {
        self.bytes_in += data.len() as u64;
        let mut offset = 0usize;

        // Complete a partially buffered window first.
        if !self.buffer.is_empty() {
            let need = self.opts.window_size - self.buffer.len();
            let take = need.min(data.len());
            self.buffer.extend_from_slice(&data[..take]);
            offset += take;

            if self.buffer.len() == self.opts.window_size {
                let window = std::mem::take(&mut self.buffer);
                self.encode_window(&window)?;
                self.buffer = window;
                self.buffer.clear();
            }
        }

        // Fast path: encode full windows directly from caller-provided input.
        while offset + self.opts.window_size <= data.len() {
            let end = offset + self.opts.window_size;
            self.encode_window(&data[offset..end])?;
            offset = end;
        }

        // Buffer any trailing partial window.
        if offset < data.len() {
            self.buffer.extend_from_slice(&data[offset..]);
        }

        Ok(())
    }

    /// Flush any remaining buffered data and finalize the stream.
    ///
    /// Returns the underlying writer and the total number of windows written.
    pub fn finish(mut self) -> Result<(W, u64), EncodeError> {
        // Encode the remaining buffer as a final window.
        if !self.buffer.is_empty() {
            let remaining = std::mem::take(&mut self.buffer);
            self.encode_window(&remaining)?;
        }

        // Handle empty target (no windows written at all).
        if self.windows_written == 0 {
            let we = WindowEncoder::new(None, self.opts.checksum);
            self.stream.write_window(we, Some(b""))?;
        }

        let windows = self.windows_written;
        Ok((self.stream.finish()?, windows))
    }

    /// Number of target bytes received so far.
    pub fn bytes_in(&self) -> u64 {
        self.bytes_in
    }

    /// Number of windows written so far.
    pub fn windows_written(&self) -> u64 {
        self.windows_written
    }

    /// Encode a single target window.
    fn encode_window(&mut self, window: &[u8]) -> Result<(), EncodeError> {
        let source_win = if !self.source.is_empty() {
            Some(SourceWindow {
                len: self.source.len() as u64,
                offset: 0,
            })
        } else {
            None
        };

        // Find matches (or just ADD for level 0).
        let instructions = if self.opts.level == 0 {
            if window.is_empty() {
                Vec::new()
            } else {
                vec![Instruction::Add {
                    len: window.len() as u32,
                }]
            }
        } else {
            let raw = self.find_matches(window);
            pipeline::optimize(&raw, window)
        };

        // Build the VCDIFF window with capacity hints from previous window.
        let mut we = if self.last_data_size > 0 {
            WindowEncoder::with_capacity(
                source_win,
                self.opts.checksum,
                self.last_data_size,
                self.last_inst_size,
                self.last_addr_size,
            )
        } else {
            WindowEncoder::new(source_win, self.opts.checksum)
        };
        emit_instructions(&mut we, window, &instructions);

        // Finalize: with or without secondary compression.
        if let Some(backend) = self.opts.secondary.backend() {
            let sections = we.finish_sections(Some(window));
            // Track section sizes for next window's capacity hints.
            self.last_data_size = sections.data_section.len();
            self.last_inst_size = sections.inst_section.len();
            self.last_addr_size = sections.addr_section.len();

            let (comp_data, comp_inst, comp_addr, del_ind) = secondary::compress_sections(
                backend.as_ref(),
                &sections.data_section,
                &sections.inst_section,
                &sections.addr_section,
            )?;

            let assembled_sections = crate::vcdiff::encoder::WindowSections {
                source_window: sections.source_window,
                target_len: sections.target_len,
                checksum: sections.checksum,
                data_section: comp_data,
                inst_section: comp_inst,
                addr_section: comp_addr,
            };

            let encoded = assembled_sections.assemble(del_ind);
            self.stream.write_raw_window(&encoded)?;
        } else {
            // Track section sizes via finish_sections for capacity hints.
            let sections = we.finish_sections(Some(window));
            self.last_data_size = sections.data_section.len();
            self.last_inst_size = sections.inst_section.len();
            self.last_addr_size = sections.addr_section.len();
            let encoded = sections.assemble(0);
            self.stream.write_raw_window(&encoded)?;
        }

        self.windows_written += 1;
        Ok(())
    }

    /// Find matches using the (reused) match engine.
    fn find_matches(&mut self, target: &[u8]) -> Vec<Instruction> {
        let engine = self.engine.as_mut().expect("engine required for level > 0");

        if self.source.is_empty() {
            engine.find_matches(target, None::<&&[u8]>)
        } else {
            let src: &[u8] = self.source;
            engine.find_matches(target, Some(&src))
        }
    }
}

/// Convenience: encode an entire target at once.
pub fn encode_all<W: Write>(
    writer: W,
    source: &[u8],
    target: &[u8],
    mut opts: CompressOptions,
) -> Result<W, EncodeError> {
    // Cap window_size to actual target length to avoid over-allocating
    // hash tables for small inputs.
    if target.len() < opts.window_size {
        opts.window_size = target.len().max(64);
    }
    let mut enc = DeltaEncoder::new(writer, source, opts);
    enc.write_target(target)?;
    let (w, _) = enc.finish()?;
    Ok(w)
}

/// Convenience: encode an entire target using parallel independent windows.
///
/// This path is gated behind the `parallel` feature and is disabled by default.
/// It preserves output validity and compatibility, but because each window is
/// matched independently, instruction choices may differ from `encode_all`.
#[cfg(feature = "parallel")]
pub fn encode_all_parallel<W: Write>(
    writer: W,
    source: &[u8],
    target: &[u8],
    mut opts: CompressOptions,
) -> Result<W, EncodeError> {
    if target.len() < opts.window_size {
        opts.window_size = target.len().max(64);
    }

    // Keep behavior identical for empty targets.
    if target.is_empty() {
        return encode_all(writer, source, target, opts);
    }

    let window_size = opts.window_size.max(64);
    let config = config::config_for_level(opts.level);
    let source_win = if !source.is_empty() {
        Some(SourceWindow {
            len: source.len() as u64,
            offset: 0,
        })
    } else {
        None
    };
    let chunks: Vec<&[u8]> = target.chunks(window_size).collect();

    let windows: Result<Vec<Vec<u8>>, EncodeError> = chunks
        .par_iter()
        .map(|chunk| {
            let instructions = if opts.level == 0 {
                if chunk.is_empty() {
                    Vec::new()
                } else {
                    vec![Instruction::Add {
                        len: chunk.len() as u32,
                    }]
                }
            } else {
                let mut engine = if !source.is_empty() {
                    let src: &[u8] = source;
                    let mut eng = MatchEngine::new(config, src.len() as u64, chunk.len().max(64));
                    eng.index_source(&src);
                    eng
                } else {
                    MatchEngine::new(config, 0, chunk.len().max(64))
                };

                let raw = if source.is_empty() {
                    engine.find_matches(chunk, None::<&&[u8]>)
                } else {
                    let src: &[u8] = source;
                    engine.find_matches(chunk, Some(&src))
                };
                pipeline::optimize(&raw, chunk)
            };

            let mut we = WindowEncoder::new(source_win, opts.checksum);
            emit_instructions(&mut we, chunk, &instructions);

            if let Some(backend) = opts.secondary.backend() {
                let sections = we.finish_sections(Some(chunk));
                let (comp_data, comp_inst, comp_addr, del_ind) = secondary::compress_sections(
                    backend.as_ref(),
                    &sections.data_section,
                    &sections.inst_section,
                    &sections.addr_section,
                )?;
                let assembled_sections = crate::vcdiff::encoder::WindowSections {
                    source_window: sections.source_window,
                    target_len: sections.target_len,
                    checksum: sections.checksum,
                    data_section: comp_data,
                    inst_section: comp_inst,
                    addr_section: comp_addr,
                };
                Ok(assembled_sections.assemble(del_ind))
            } else {
                Ok(we.finish_sections(Some(chunk)).assemble(0))
            }
        })
        .collect();

    let mut stream = StreamEncoder::new(writer, opts.checksum);
    if let Some(backend) = opts.secondary.backend() {
        stream.set_secondary_id(backend.id());
    }

    for window in windows? {
        stream.write_raw_window(&window)?;
    }

    Ok(stream.finish()?)
}

// ---------------------------------------------------------------------------
// Instruction emission helper
// ---------------------------------------------------------------------------

fn emit_instructions(we: &mut WindowEncoder, target: &[u8], instructions: &[Instruction]) {
    let mut target_pos = 0usize;

    for inst in instructions {
        match *inst {
            Instruction::Add { len } => {
                let len = len as usize;
                we.add(&target[target_pos..target_pos + len]);
                target_pos += len;
            }
            Instruction::Copy { len, addr, .. } => {
                we.copy_with_auto_mode(len, addr);
                target_pos += len as usize;
            }
            Instruction::Run { len } => {
                let byte = target[target_pos];
                we.run(len, byte);
                target_pos += len as usize;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(source: &[u8], target: &[u8], opts: CompressOptions) -> Vec<u8> {
        let mut output = Vec::new();
        let enc = DeltaEncoder::new(&mut output, source, opts);
        // Use encode_all convenience instead to keep it simple.
        drop(enc);
        output.clear();

        encode_all(&mut output, source, target, CompressOptions::default()).unwrap();

        let decoded = crate::vcdiff::decoder::decode_memory(&output, source).unwrap();
        assert_eq!(decoded, target, "roundtrip mismatch");
        output
    }

    #[test]
    fn encode_all_roundtrip() {
        let source = b"The quick brown fox jumps over the lazy dog.";
        let target = b"The quick brown cat sits on the lazy mat.";
        roundtrip(source, target, CompressOptions::default());
    }

    #[test]
    fn level_0_store_only() {
        let target = b"Hello, world!";
        let mut output = Vec::new();
        encode_all(
            &mut output,
            b"",
            target,
            CompressOptions {
                level: 0,
                ..Default::default()
            },
        )
        .unwrap();

        let decoded = crate::vcdiff::decoder::decode_memory(&output, b"").unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn all_levels_roundtrip() {
        let source = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";
        let target = b"ABCDEFGHIJKLMNOP--CHANGED--UVWXYZ0123456789abcdefghijklmnopqrstuvwxyz!!!";

        for level in 0..=9 {
            let mut output = Vec::new();
            encode_all(
                &mut output,
                source,
                target,
                CompressOptions {
                    level,
                    checksum: true,
                    ..Default::default()
                },
            )
            .unwrap();

            let decoded = crate::vcdiff::decoder::decode_memory(&output, source).unwrap();
            assert_eq!(decoded, target, "level {level} roundtrip failed");
        }
    }

    #[test]
    fn streaming_matches_bulk() {
        let source = b"AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH";
        let target = b"AAAA CCCC DDDD EEEE xxxx GGGG HHHH IIII";

        // Bulk encode.
        let mut bulk_output = Vec::new();
        encode_all(
            &mut bulk_output,
            source,
            target,
            CompressOptions {
                level: 6,
                window_size: 1024, // small windows
                ..Default::default()
            },
        )
        .unwrap();

        // Streaming encode (4-byte chunks).
        let mut stream_output = Vec::new();
        let mut enc = DeltaEncoder::new(
            &mut stream_output,
            source,
            CompressOptions {
                level: 6,
                window_size: 1024,
                ..Default::default()
            },
        );
        for chunk in target.chunks(4) {
            enc.write_target(chunk).unwrap();
        }
        enc.finish().unwrap();

        // Both should decode to the same target.
        let bulk_decoded = crate::vcdiff::decoder::decode_memory(&bulk_output, source).unwrap();
        let stream_decoded = crate::vcdiff::decoder::decode_memory(&stream_output, source).unwrap();
        assert_eq!(bulk_decoded, target);
        assert_eq!(stream_decoded, target);
    }

    #[test]
    fn empty_target() {
        let mut output = Vec::new();
        encode_all(&mut output, b"source", b"", CompressOptions::default()).unwrap();
        let decoded = crate::vcdiff::decoder::decode_memory(&output, b"source").unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn no_source() {
        let target = b"standalone data without any source reference";
        let mut output = Vec::new();
        encode_all(&mut output, b"", target, CompressOptions::default()).unwrap();
        let decoded = crate::vcdiff::decoder::decode_memory(&output, b"").unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn progress_tracking() {
        let target = vec![0xAA; 1000];
        let mut output = Vec::new();
        let mut enc = DeltaEncoder::new(&mut output, b"", CompressOptions::default());
        assert_eq!(enc.bytes_in(), 0);
        assert_eq!(enc.windows_written(), 0);
        enc.write_target(&target).unwrap();
        assert_eq!(enc.bytes_in(), 1000);
        enc.finish().unwrap();
    }

    #[test]
    fn xdelta3_can_decode_our_output() {
        let source = b"The quick brown fox jumps over the lazy dog. 1234567890";
        let target = b"The quick brown cat sits on the lazy mat. 1234567890!!!";
        let mut output = Vec::new();
        encode_all(&mut output, source, target, CompressOptions::default()).unwrap();

        // Verify xdelta3 C library can decode it.
        match xdelta3::decode(&output, source) {
            Some(decoded) => assert_eq!(decoded, target),
            None => panic!("xdelta3 crate failed to decode our output"),
        }
    }

    #[cfg(feature = "parallel")]
    #[test]
    fn parallel_encode_roundtrip() {
        let source: Vec<u8> = (0..=255).cycle().take(512 * 1024).collect();
        let mut target = source.clone();
        for i in (0..target.len()).step_by(997) {
            target[i] ^= 0xA5;
        }

        let mut output = Vec::new();
        encode_all_parallel(
            &mut output,
            &source,
            &target,
            CompressOptions {
                level: 6,
                window_size: 64 * 1024,
                ..Default::default()
            },
        )
        .unwrap();

        let decoded = crate::vcdiff::decoder::decode_memory(&output, &source).unwrap();
        assert_eq!(decoded, target);
    }

    #[cfg(feature = "lzma-secondary")]
    #[test]
    fn secondary_lzma_roundtrip() {
        let source: Vec<u8> = b"ABCDEFGHIJ".iter().copied().cycle().take(4096).collect();
        let mut target = source.clone();
        target[100] = b'X';
        target[500] = b'Y';

        let mut output = Vec::new();
        encode_all(
            &mut output,
            &source,
            &target,
            CompressOptions {
                level: 6,
                secondary: SecondaryCompression::Lzma,
                ..Default::default()
            },
        )
        .unwrap();

        let decoded = crate::vcdiff::decoder::decode_memory(&output, &source).unwrap();
        assert_eq!(decoded, target);
    }

    #[cfg(feature = "zlib-secondary")]
    #[test]
    fn secondary_zlib_roundtrip() {
        let source: Vec<u8> = b"ABCDEFGHIJ".iter().copied().cycle().take(4096).collect();
        let mut target = source.clone();
        target[100] = b'X';
        target[500] = b'Y';

        let mut output = Vec::new();
        encode_all(
            &mut output,
            &source,
            &target,
            CompressOptions {
                level: 6,
                secondary: SecondaryCompression::Zlib { level: 6 },
                ..Default::default()
            },
        )
        .unwrap();

        let decoded = crate::vcdiff::decoder::decode_memory(&output, &source).unwrap();
        assert_eq!(decoded, target);
    }
}
