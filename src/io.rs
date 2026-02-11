// File-level I/O helpers for delta encoding/decoding.
//
// Provides `encode_file()` and `decode_file()` convenience functions that
// wrap the streaming pipeline with proper buffered I/O. Optionally computes
// streaming SHA-256 checksums (feature-gated behind `file-io`).

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

#[cfg(feature = "file-io")]
use sha2::Digest;

use crate::compress::decoder::DeltaDecoder;
use crate::compress::encoder::{CompressOptions, DeltaEncoder, EncodeError};
use crate::vcdiff::decoder::DecodeError;

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Statistics returned by `encode_file()`.
#[derive(Debug, Clone)]
pub struct EncodeStats {
    /// Source file size in bytes.
    pub source_size: u64,
    /// Target file size in bytes.
    pub target_size: u64,
    /// Delta output size in bytes.
    pub delta_size: u64,
    /// Number of VCDIFF windows written.
    pub windows: u64,
    /// SHA-256 of the source file (if `file-io` feature is enabled).
    pub source_sha256: Option<[u8; 32]>,
    /// SHA-256 of the target file (if `file-io` feature is enabled).
    pub target_sha256: Option<[u8; 32]>,
}

/// Statistics returned by `decode_file()`.
#[derive(Debug, Clone)]
pub struct DecodeStats {
    /// Source file size in bytes.
    pub source_size: u64,
    /// Delta file size in bytes.
    pub delta_size: u64,
    /// Reconstructed output size in bytes.
    pub output_size: u64,
    /// Number of VCDIFF windows decoded.
    pub windows: u64,
    /// SHA-256 of the reconstructed output (if `file-io` feature is enabled).
    pub output_sha256: Option<[u8; 32]>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error type for file I/O operations.
#[derive(Debug)]
pub enum IoError {
    /// I/O error (file open, read, write).
    Io(io::Error),
    /// Delta encoding error.
    Encode(EncodeError),
    /// Delta decoding error.
    Decode(DecodeError),
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Encode(e) => write!(f, "encode error: {e}"),
            Self::Decode(e) => write!(f, "decode error: {e}"),
        }
    }
}

impl std::error::Error for IoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Encode(e) => Some(e),
            Self::Decode(e) => Some(e),
        }
    }
}

impl From<io::Error> for IoError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<EncodeError> for IoError {
    fn from(e: EncodeError) -> Self {
        Self::Encode(e)
    }
}

impl From<DecodeError> for IoError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

// ---------------------------------------------------------------------------
// Default buffer size
// ---------------------------------------------------------------------------

const BUF_SIZE: usize = 64 * 1024; // 64 KiB

// ---------------------------------------------------------------------------
// encode_file
// ---------------------------------------------------------------------------

/// Encode a delta between a source file and target file, writing to `delta_path`.
///
/// The source is read fully into memory (matching xdelta3's behavior).
/// The target is streamed through a `BufReader`. The delta output uses `BufWriter`.
///
/// When the `file-io` feature is enabled, SHA-256 checksums are computed
/// incrementally as data flows through the pipeline.
pub fn encode_file(
    source_path: &Path,
    target_path: &Path,
    delta_path: &Path,
    opts: CompressOptions,
) -> Result<EncodeStats, IoError> {
    // Read source fully into memory.
    let source = std::fs::read(source_path)?;
    let source_size = source.len() as u64;

    // Open target for streaming read.
    let target_file = File::open(target_path)?;
    let target_size = target_file.metadata()?.len();
    let mut target_reader = BufReader::with_capacity(BUF_SIZE, target_file);

    // Open delta output.
    let delta_file = File::create(delta_path)?;
    let delta_writer = BufWriter::with_capacity(BUF_SIZE, delta_file);

    // Create encoder.
    let mut encoder = DeltaEncoder::new(delta_writer, &source, opts);

    // Stream target through the encoder, optionally hashing.
    #[cfg(feature = "file-io")]
    let mut target_hasher = sha2::Sha256::new();
    #[cfg(feature = "file-io")]
    let source_sha256 = {
        let mut h = sha2::Sha256::new();
        h.update(&source);
        Some(h.finalize().into())
    };
    #[cfg(not(feature = "file-io"))]
    let source_sha256: Option<[u8; 32]> = None;

    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = target_reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        #[cfg(feature = "file-io")]
        {
            target_hasher.update(&buf[..n]);
        }
        encoder.write_target(&buf[..n])?;
    }

    let (writer, windows) = encoder.finish()?;
    let delta_size = writer
        .into_inner()
        .map_err(|e| e.into_error())?
        .metadata()?
        .len();

    #[cfg(feature = "file-io")]
    let target_sha256 = Some(target_hasher.finalize().into());
    #[cfg(not(feature = "file-io"))]
    let target_sha256: Option<[u8; 32]> = None;

    Ok(EncodeStats {
        source_size,
        target_size,
        delta_size,
        windows,
        source_sha256,
        target_sha256,
    })
}

// ---------------------------------------------------------------------------
// decode_file
// ---------------------------------------------------------------------------

/// Decode a VCDIFF delta file using a source file, writing to `output_path`.
///
/// The source is read fully into memory. The delta is streamed via `BufReader`.
/// The output uses `BufWriter`.
///
/// When the `file-io` feature is enabled, a SHA-256 checksum of the output
/// is computed incrementally.
pub fn decode_file(
    source_path: &Path,
    delta_path: &Path,
    output_path: &Path,
) -> Result<DecodeStats, IoError> {
    // Read source fully into memory.
    let source = std::fs::read(source_path)?;
    let source_size = source.len() as u64;

    // Open delta for streaming read.
    let delta_file = File::open(delta_path)?;
    let delta_size = delta_file.metadata()?.len();
    let delta_reader = BufReader::with_capacity(BUF_SIZE, delta_file);

    // Open output.
    let output_file = File::create(output_path)?;

    // Wrap the output writer to optionally hash as we write.
    #[cfg(feature = "file-io")]
    let mut output_hasher = sha2::Sha256::new();

    let mut output_writer = BufWriter::with_capacity(BUF_SIZE, output_file);

    // Decode.
    let mut decoder = DeltaDecoder::new(delta_reader);
    let mut src: &[u8] = &source;

    #[cfg(feature = "file-io")]
    let output_size = {
        let mut hashing_writer = HashingWriter {
            inner: &mut output_writer,
            hasher: &mut output_hasher,
        };
        decoder.decode_to(&mut src, &mut hashing_writer)?
    };

    #[cfg(not(feature = "file-io"))]
    let output_size = decoder.decode_to(&mut src, &mut output_writer)?;

    let windows = decoder.windows_decoded();

    output_writer.flush()?;

    #[cfg(feature = "file-io")]
    let output_sha256 = Some(output_hasher.finalize().into());
    #[cfg(not(feature = "file-io"))]
    let output_sha256: Option<[u8; 32]> = None;

    Ok(DecodeStats {
        source_size,
        delta_size,
        output_size,
        windows,
        output_sha256,
    })
}

// ---------------------------------------------------------------------------
// Hashing writer (used with file-io feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "file-io")]
struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: &'a mut sha2::Sha256,
}

#[cfg(feature = "file-io")]
impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        use sha2::Digest;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_file(name: &str, data: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("xdelta_io_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(data).unwrap();
        path
    }

    fn cleanup_temp_files(paths: &[&Path]) {
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn encode_decode_file_roundtrip() {
        let source_data = b"The quick brown fox jumps over the lazy dog. 1234567890";
        let target_data = b"The quick brown cat sits on the lazy mat. 1234567890!!!";

        let source_path = write_temp_file("source.bin", source_data);
        let target_path = write_temp_file("target.bin", target_data);
        let delta_path = write_temp_file("delta.vcdiff", b"");
        let output_path = write_temp_file("output.bin", b"");

        let enc_stats = encode_file(
            &source_path,
            &target_path,
            &delta_path,
            CompressOptions::default(),
        )
        .unwrap();

        assert_eq!(enc_stats.source_size, source_data.len() as u64);
        assert_eq!(enc_stats.target_size, target_data.len() as u64);
        assert!(enc_stats.delta_size > 0);
        assert!(enc_stats.windows >= 1);

        let dec_stats = decode_file(&source_path, &delta_path, &output_path).unwrap();

        assert_eq!(dec_stats.output_size, target_data.len() as u64);
        assert!(dec_stats.windows >= 1);

        let output_data = std::fs::read(&output_path).unwrap();
        assert_eq!(output_data, target_data);

        cleanup_temp_files(&[&source_path, &target_path, &delta_path, &output_path]);
    }

    #[test]
    fn encode_decode_no_source() {
        let target_data = b"standalone data without any source";

        let source_path = write_temp_file("nosrc_source.bin", b"");
        let target_path = write_temp_file("nosrc_target.bin", target_data);
        let delta_path = write_temp_file("nosrc_delta.vcdiff", b"");
        let output_path = write_temp_file("nosrc_output.bin", b"");

        encode_file(
            &source_path,
            &target_path,
            &delta_path,
            CompressOptions::default(),
        )
        .unwrap();

        decode_file(&source_path, &delta_path, &output_path).unwrap();

        let output_data = std::fs::read(&output_path).unwrap();
        assert_eq!(output_data, target_data);

        cleanup_temp_files(&[&source_path, &target_path, &delta_path, &output_path]);
    }

    #[cfg(feature = "file-io")]
    #[test]
    fn sha256_checksums_computed() {
        let source_data = b"source for checksum test";
        let target_data = b"target for checksum test";

        let source_path = write_temp_file("sha_source.bin", source_data);
        let target_path = write_temp_file("sha_target.bin", target_data);
        let delta_path = write_temp_file("sha_delta.vcdiff", b"");
        let output_path = write_temp_file("sha_output.bin", b"");

        let enc_stats = encode_file(
            &source_path,
            &target_path,
            &delta_path,
            CompressOptions::default(),
        )
        .unwrap();

        assert!(enc_stats.source_sha256.is_some());
        assert!(enc_stats.target_sha256.is_some());

        let dec_stats = decode_file(&source_path, &delta_path, &output_path).unwrap();

        assert!(dec_stats.output_sha256.is_some());
        // The output SHA-256 should match the target SHA-256 from encoding.
        assert_eq!(dec_stats.output_sha256, enc_stats.target_sha256);

        cleanup_temp_files(&[&source_path, &target_path, &delta_path, &output_path]);
    }

    #[test]
    fn large_file_multi_window() {
        // 1 MiB of data with small windows to force multiple windows.
        let source_data: Vec<u8> = (0..=255u8).cycle().take(1 << 20).collect();
        let mut target_data = source_data.clone();
        // Modify scattered bytes.
        for i in (0..target_data.len()).step_by(4096) {
            target_data[i] = target_data[i].wrapping_add(1);
        }

        let source_path = write_temp_file("large_source.bin", &source_data);
        let target_path = write_temp_file("large_target.bin", &target_data);
        let delta_path = write_temp_file("large_delta.vcdiff", b"");
        let output_path = write_temp_file("large_output.bin", b"");

        let enc_stats = encode_file(
            &source_path,
            &target_path,
            &delta_path,
            CompressOptions {
                window_size: 64 * 1024, // 64 KiB windows
                ..Default::default()
            },
        )
        .unwrap();

        assert!(enc_stats.windows > 1, "expected multiple windows");
        assert!(
            enc_stats.delta_size < enc_stats.target_size,
            "delta should be smaller than target"
        );

        let dec_stats = decode_file(&source_path, &delta_path, &output_path).unwrap();
        assert_eq!(dec_stats.output_size, target_data.len() as u64);

        let output_data = std::fs::read(&output_path).unwrap();
        assert_eq!(output_data, target_data);

        cleanup_temp_files(&[&source_path, &target_path, &delta_path, &output_path]);
    }
}
