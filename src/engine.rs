// Delta engine: ties hash/matching to VCDIFF encoding/decoding.
//
// Provides high-level encode/decode APIs that orchestrate:
//   - Block matching (hash module) to find COPY/RUN/ADD instructions
//   - VCDIFF encoding (vcdiff module) to produce the delta stream
//   - VCDIFF decoding to reconstruct target from source + delta

use crate::hash::config::{self, MatcherConfig};
use crate::hash::matching::{MatchEngine, SourceData};
use crate::vcdiff::code_table::Instruction;
use crate::vcdiff::decoder::{self, DecodeError};
use crate::vcdiff::encoder::{SourceWindow, StreamEncoder, WindowEncoder};

// ---------------------------------------------------------------------------
// Encode options
// ---------------------------------------------------------------------------

/// Configuration for delta encoding.
#[derive(Debug, Clone)]
pub struct EncodeOptions {
    /// Compression level (0-9). Maps to matcher profiles.
    pub level: u32,
    /// Maximum target window size for chunking large inputs.
    pub window_size: usize,
    /// Whether to emit Adler-32 checksums per window.
    pub checksum: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            level: 6,
            window_size: 1 << 23, // 8 MiB
            checksum: true,
        }
    }
}

// ---------------------------------------------------------------------------
// High-level encode
// ---------------------------------------------------------------------------

/// Encode a delta between `source` and `target`, writing VCDIFF to `output`.
///
/// If `source` is empty, produces a delta with only ADD/RUN instructions.
pub fn encode(source: &[u8], target: &[u8], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    encode_with_options(source, target, output, &EncodeOptions::default())
}

/// Encode with custom options.
pub fn encode_with_options(
    source: &[u8],
    target: &[u8],
    output: &mut Vec<u8>,
    opts: &EncodeOptions,
) -> Result<(), EncodeError> {
    let config = config::config_for_level(opts.level);
    let src: &[u8] = source;

    let mut stream = StreamEncoder::new(output, opts.checksum);

    // Process target in windows.
    let mut target_offset = 0usize;

    while target_offset < target.len() {
        let win_end = (target_offset + opts.window_size).min(target.len());
        let win_target = &target[target_offset..win_end];

        let instructions = if source.is_empty() {
            find_matches_no_source(config, win_target)
        } else {
            find_matches_with_source(config, &src, win_target)
        };

        // Build VCDIFF window.
        let source_win = if !source.is_empty() {
            Some(SourceWindow {
                len: source.len() as u64,
                offset: 0,
            })
        } else {
            None
        };

        let mut we = WindowEncoder::new(source_win, opts.checksum);
        emit_instructions(&mut we, win_target, source.len() as u64, &instructions);

        stream
            .write_window(we, Some(win_target))
            .map_err(EncodeError::Io)?;

        target_offset = win_end;
    }

    // Handle empty target.
    if target.is_empty() {
        let we = WindowEncoder::new(None, opts.checksum);
        stream
            .write_window(we, Some(b""))
            .map_err(EncodeError::Io)?;
    }

    let _ = stream.finish().map_err(EncodeError::Io)?;
    Ok(())
}

fn find_matches_no_source(config: MatcherConfig, target: &[u8]) -> Vec<Instruction> {
    let mut engine = MatchEngine::new(config, 0, target.len().max(64));
    engine.find_matches(target, None::<&&[u8]>)
}

fn find_matches_with_source(
    config: MatcherConfig,
    source: &&[u8],
    target: &[u8],
) -> Vec<Instruction> {
    let src_len = source.len();
    let mut engine = MatchEngine::new(config, src_len, target.len().max(64));
    engine.index_source(source);
    engine.find_matches(target, Some(source))
}

/// Emit instructions into the window encoder, providing literal data for ADD
/// and the run byte for RUN.
fn emit_instructions(
    we: &mut WindowEncoder,
    target: &[u8],
    _source_len: u64,
    instructions: &[Instruction],
) {
    let mut target_pos = 0usize;

    for inst in instructions {
        match *inst {
            Instruction::Add { len } => {
                let len = len as usize;
                we.add(&target[target_pos..target_pos + len]);
                target_pos += len;
            }
            Instruction::Copy { len, addr, .. } => {
                let len32 = len;
                we.copy_with_auto_mode(len32, addr);
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
// High-level decode
// ---------------------------------------------------------------------------

/// Decode a VCDIFF delta, reconstructing the target.
///
/// `source` is the original file (can be empty if delta has no source copies).
/// `delta` is the VCDIFF byte stream.
pub fn decode(source: &[u8], delta: &[u8]) -> Result<Vec<u8>, DecodeError> {
    decoder::decode_memory(delta, source)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(source: &[u8], target: &[u8]) {
        let mut delta = Vec::new();
        encode(source, target, &mut delta).expect("encode failed");
        let reconstructed = decode(source, &delta).expect("decode failed");
        assert_eq!(
            reconstructed,
            target,
            "roundtrip mismatch (source={}, target={}, delta={})",
            source.len(),
            target.len(),
            delta.len()
        );
    }

    #[test]
    fn roundtrip_identical() {
        let data = b"The quick brown fox jumps over the lazy dog.";
        roundtrip(data, data);
    }

    #[test]
    fn roundtrip_small_edit() {
        let source = b"Hello, world! This is a test of the delta engine.";
        let target = b"Hello, earth! This is a test of the delta engine.";
        roundtrip(source, target);
    }

    #[test]
    fn roundtrip_no_source() {
        let target = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        roundtrip(b"", target);
    }

    #[test]
    fn roundtrip_empty_target() {
        roundtrip(b"some source", b"");
    }

    #[test]
    fn roundtrip_repeating_data() {
        let source = b"AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH";
        let target = b"AAAA CCCC DDDD EEEE xxxx GGGG HHHH IIII";
        roundtrip(source, target);
    }

    #[test]
    fn roundtrip_binary_data() {
        let source: Vec<u8> = (0..=255).cycle().take(4096).collect();
        let mut target = source.clone();
        // Modify a few bytes.
        target[100] = 0xFF;
        target[200] = 0x00;
        target[1000] = 0x42;
        roundtrip(&source, &target);
    }

    #[test]
    fn roundtrip_large_insert() {
        let source = b"Start.";
        let target = b"Start. And now a much longer piece of text that was inserted.";
        roundtrip(source, target);
    }

    #[test]
    fn roundtrip_all_levels() {
        let source = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";
        let target = b"ABCDEFGHIJKLMNOP--CHANGED--UVWXYZ0123456789abcdefghijklmnopqrstuvwxyz!!!";

        for level in [1, 3, 6, 9] {
            let opts = EncodeOptions {
                level,
                checksum: true,
                ..Default::default()
            };
            let mut delta = Vec::new();
            encode_with_options(source, target, &mut delta, &opts).expect("encode failed");
            let reconstructed = decode(source, &delta).expect("decode failed");
            assert_eq!(reconstructed, target, "level {level} roundtrip failed");
        }
    }

    #[test]
    fn roundtrip_run_data() {
        let source = b"";
        let target = vec![0xAA; 200];
        roundtrip(source, &target);
    }

    #[test]
    fn delta_is_smaller_for_similar_data() {
        let source: Vec<u8> = (0..=255).cycle().take(8192).collect();
        let mut target = source.clone();
        target[4096] ^= 0xFF; // flip one byte
        let mut delta = Vec::new();
        encode(&source, &target, &mut delta).expect("encode failed");
        // Delta should be much smaller than the target.
        assert!(
            delta.len() < target.len() / 2,
            "delta ({}) should be much smaller than target ({})",
            delta.len(),
            target.len()
        );
    }

    #[test]
    fn xdelta3_can_decode_engine_output() {
        // Verify xdelta3 binary can decode our engine output.
        use std::process::Command;

        // Check if xdelta3 is available.
        let status = Command::new("xdelta3").arg("-V").output();
        if status.is_err() {
            eprintln!("xdelta3 not found, skipping interop test");
            return;
        }

        let source = b"The quick brown fox jumps over the lazy dog. 1234567890";
        let target = b"The quick brown cat sits on the lazy mat. 1234567890!!!";

        let mut delta = Vec::new();
        encode(source, target, &mut delta).expect("encode failed");

        // Write files.
        let dir = std::env::temp_dir().join("xdelta_engine_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src_path = dir.join("source.bin");
        let delta_path = dir.join("delta.vcdiff");
        let out_path = dir.join("output.bin");

        std::fs::write(&src_path, source).unwrap();
        std::fs::write(&delta_path, &delta).unwrap();

        let result = Command::new("xdelta3")
            .args(["-d", "-s"])
            .arg(&src_path)
            .arg(&delta_path)
            .arg(&out_path)
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    let decoded = std::fs::read(&out_path).unwrap();
                    assert_eq!(decoded, target, "xdelta3 decoded different output");
                } else {
                    panic!(
                        "xdelta3 decode failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            Err(e) => eprintln!("skipping xdelta3 interop: {e}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
