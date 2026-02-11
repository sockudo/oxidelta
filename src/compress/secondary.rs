// Secondary compression for VCDIFF DATA/INST/ADDR sections.
//
// Provides a pluggable `CompressBackend` trait with built-in implementations:
//   - LZMA (via lzma-rs, feature-gated `lzma-secondary`)
//   - Zlib/Deflate (via flate2, feature-gated `zlib-secondary`)
//   - NoCompression (passthrough)
//   - External/custom compressors via the trait
//
// The VCDIFF file header stores a secondary compressor ID; xdelta3 defines:
//   ID 1 = DJW (xdelta3-specific Huffman, not implemented here)
//   ID 2 = LZMA
//   ID 16 = FGK (xdelta3-specific, not implemented here)
//
// We additionally define:
//   ID 3 = Zlib/Deflate (Rust-only extension; not decodable by xdelta3 C)

use std::io;

use crate::vcdiff::decoder::DecodeError;
use crate::vcdiff::header::{VCD_ADDRCOMP, VCD_DATACOMP, VCD_INSTCOMP, VCD_LZMA_ID};

/// Secondary compressor ID for Zlib/Deflate (Rust extension, not in xdelta3 C).
pub const VCD_ZLIB_ID: u8 = 3;

/// Minimum section size worth compressing.
const MIN_COMPRESS_SIZE: usize = 32;

/// Encoded DATA/INST/ADDR sections plus delta indicator flags.
pub type CompressedSections = (Vec<u8>, Vec<u8>, Vec<u8>, u8);
/// Decoded DATA/INST/ADDR sections.
pub type DecompressedSections = (Vec<u8>, Vec<u8>, Vec<u8>);

// ---------------------------------------------------------------------------
// CompressBackend trait
// ---------------------------------------------------------------------------

/// A pluggable secondary compressor for VCDIFF sections.
///
/// Implementations compress/decompress individual DATA, INST, and ADDR sections
/// after VCDIFF encoding (or before VCDIFF decoding).
///
/// # Implementing a custom backend
///
/// ```no_run
/// use oxidelta::compress::secondary::CompressBackend;
/// use oxidelta::vcdiff::decoder::DecodeError;
///
/// struct MyCompressor;
///
/// impl CompressBackend for MyCompressor {
///     fn id(&self) -> u8 { 42 }
///     fn compress(&self, data: &[u8]) -> std::io::Result<Vec<u8>> {
///         Ok(data.to_vec()) // placeholder
///     }
///     fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
///         Ok(data.to_vec()) // placeholder
///     }
/// }
/// ```
pub trait CompressBackend: Send + Sync {
    /// The secondary compressor ID stored in the VCDIFF file header.
    ///
    /// Standard IDs: 1 (DJW), 2 (LZMA), 16 (FGK).
    /// Rust extension: 3 (Zlib).
    /// Custom implementations should use IDs that don't collide with these.
    fn id(&self) -> u8;

    /// Compress a section. Returns compressed bytes.
    ///
    /// If compression would not reduce size, implementations should return
    /// the original data unchanged (the caller checks this).
    fn compress(&self, data: &[u8]) -> io::Result<Vec<u8>>;

    /// Decompress a section previously compressed by `compress()`.
    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError>;

    /// Whether this section is worth compressing. Default: skip if < 32 bytes.
    fn should_compress(&self, data: &[u8]) -> bool {
        data.len() >= MIN_COMPRESS_SIZE
    }
}

// ---------------------------------------------------------------------------
// LZMA backend
// ---------------------------------------------------------------------------

/// LZMA secondary compressor (ID 2). Cross-compatible with xdelta3 C.
#[cfg(feature = "lzma-secondary")]
#[derive(Debug, Clone, Copy, Default)]
pub struct LzmaBackend;

#[cfg(feature = "lzma-secondary")]
impl CompressBackend for LzmaBackend {
    fn id(&self) -> u8 {
        VCD_LZMA_ID
    }

    fn compress(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        let mut input = io::Cursor::new(data);
        let mut output = Vec::new();
        lzma_rs::lzma_compress(&mut input, &mut output)?;
        Ok(output)
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut input = io::BufReader::new(io::Cursor::new(data));
        let mut output = Vec::new();
        lzma_rs::lzma_decompress(&mut input, &mut output)
            .map_err(|e| DecodeError::InvalidInput(format!("LZMA decompression failed: {e}")))?;
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Zlib backend
// ---------------------------------------------------------------------------

/// Zlib/Deflate secondary compressor (ID 3). Rust-only extension.
///
/// Uses zlib format (deflate + zlib header), not raw deflate,
/// so the stream is self-describing and includes a checksum.
#[cfg(feature = "zlib-secondary")]
#[derive(Debug, Clone, Copy)]
pub struct ZlibBackend {
    level: flate2::Compression,
}

#[cfg(feature = "zlib-secondary")]
impl ZlibBackend {
    /// Create a Zlib backend with the given compression level (0-9).
    pub fn new(level: u32) -> Self {
        Self {
            level: flate2::Compression::new(level),
        }
    }
}

#[cfg(feature = "zlib-secondary")]
impl Default for ZlibBackend {
    fn default() -> Self {
        Self::new(6)
    }
}

#[cfg(feature = "zlib-secondary")]
impl CompressBackend for ZlibBackend {
    fn id(&self) -> u8 {
        VCD_ZLIB_ID
    }

    fn compress(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        use flate2::write::ZlibEncoder;
        use io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), self.level);
        encoder.write_all(data)?;
        encoder.finish()
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        use flate2::read::ZlibDecoder;
        use io::Read;

        let mut decoder = ZlibDecoder::new(data);
        let mut output = Vec::new();
        decoder
            .read_to_end(&mut output)
            .map_err(|e| DecodeError::InvalidInput(format!("Zlib decompression failed: {e}")))?;
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// No-compression backend
// ---------------------------------------------------------------------------

/// Passthrough "compressor" that performs no compression.
///
/// Useful as a default or for testing. The `compress` method returns data
/// unchanged, so `compress_section` will never set compression flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCompression;

impl CompressBackend for NoCompression {
    fn id(&self) -> u8 {
        0 // Never written to the file header since sections aren't compressed
    }

    fn compress(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        Ok(data.to_vec())
    }

    fn should_compress(&self, _data: &[u8]) -> bool {
        false // Never compress
    }
}

// ---------------------------------------------------------------------------
// Section-level compression/decompression helpers
// ---------------------------------------------------------------------------

/// Compress a single section using the given backend.
///
/// Returns compressed data only if it's actually smaller; otherwise returns
/// the original data unchanged.
pub fn compress_section(backend: &dyn CompressBackend, data: &[u8]) -> io::Result<Vec<u8>> {
    if !backend.should_compress(data) {
        return Ok(data.to_vec());
    }

    let compressed = backend.compress(data)?;

    if compressed.len() < data.len() {
        Ok(compressed)
    } else {
        Ok(data.to_vec())
    }
}

/// Decompress a single section using the given backend.
pub fn decompress_section(
    backend: &dyn CompressBackend,
    data: &[u8],
) -> Result<Vec<u8>, DecodeError> {
    backend.decompress(data)
}

/// Compress all three VCDIFF sections independently.
///
/// Returns (data, inst, addr, del_ind) where `del_ind` has
/// VCD_DATACOMP/VCD_INSTCOMP/VCD_ADDRCOMP bits set for sections
/// that were actually compressed (i.e., compression reduced size).
pub fn compress_sections(
    backend: &dyn CompressBackend,
    data: &[u8],
    inst: &[u8],
    addr: &[u8],
) -> io::Result<CompressedSections> {
    let mut del_ind: u8 = 0;

    let comp_data = compress_section(backend, data)?;
    if comp_data.len() < data.len() {
        del_ind |= VCD_DATACOMP;
    }

    let comp_inst = compress_section(backend, inst)?;
    if comp_inst.len() < inst.len() {
        del_ind |= VCD_INSTCOMP;
    }

    let comp_addr = compress_section(backend, addr)?;
    if comp_addr.len() < addr.len() {
        del_ind |= VCD_ADDRCOMP;
    }

    let final_data = if del_ind & VCD_DATACOMP != 0 {
        comp_data
    } else {
        data.to_vec()
    };
    let final_inst = if del_ind & VCD_INSTCOMP != 0 {
        comp_inst
    } else {
        inst.to_vec()
    };
    let final_addr = if del_ind & VCD_ADDRCOMP != 0 {
        comp_addr
    } else {
        addr.to_vec()
    };

    Ok((final_data, final_inst, final_addr, del_ind))
}

/// Decompress sections according to del_ind flags.
///
/// `secondary_id` is validated against the backend's ID.
pub fn decompress_sections(
    data: &[u8],
    inst: &[u8],
    addr: &[u8],
    del_ind: u8,
    secondary_id: Option<u8>,
) -> Result<DecompressedSections, DecodeError> {
    if del_ind == 0 {
        return Ok((data.to_vec(), inst.to_vec(), addr.to_vec()));
    }

    let backend = backend_for_id(secondary_id)?;

    let dec_data = if del_ind & VCD_DATACOMP != 0 {
        decompress_section(backend.as_ref(), data)?
    } else {
        data.to_vec()
    };

    let dec_inst = if del_ind & VCD_INSTCOMP != 0 {
        decompress_section(backend.as_ref(), inst)?
    } else {
        inst.to_vec()
    };

    let dec_addr = if del_ind & VCD_ADDRCOMP != 0 {
        decompress_section(backend.as_ref(), addr)?
    } else {
        addr.to_vec()
    };

    Ok((dec_data, dec_inst, dec_addr))
}

/// Look up a decompression backend by secondary compressor ID.
///
/// This is the decode-side dispatch: given the ID from the file header,
/// return the appropriate backend to decompress sections.
pub fn backend_for_id(secondary_id: Option<u8>) -> Result<Box<dyn CompressBackend>, DecodeError> {
    match secondary_id {
        #[cfg(feature = "lzma-secondary")]
        Some(VCD_LZMA_ID) => Ok(Box::new(LzmaBackend)),

        #[cfg(not(feature = "lzma-secondary"))]
        Some(VCD_LZMA_ID) => Err(DecodeError::Unsupported(
            "LZMA secondary compression requires the 'lzma-secondary' feature".into(),
        )),

        #[cfg(feature = "zlib-secondary")]
        Some(VCD_ZLIB_ID) => Ok(Box::new(ZlibBackend::default())),

        #[cfg(not(feature = "zlib-secondary"))]
        Some(VCD_ZLIB_ID) => Err(DecodeError::Unsupported(
            "Zlib secondary compression requires the 'zlib-secondary' feature".into(),
        )),

        Some(id) => Err(DecodeError::Unsupported(format!(
            "unsupported secondary compressor ID: {id}"
        ))),
        None => Err(DecodeError::InvalidInput(
            "del_ind indicates secondary compression but no compressor ID in file header".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Default backend selection (for encoder convenience)
// ---------------------------------------------------------------------------

/// The secondary compression algorithm to use.
#[derive(Clone, Default)]
pub enum SecondaryCompression {
    /// No secondary compression.
    #[default]
    None,
    /// LZMA (ID 2). Cross-compatible with xdelta3 C.
    #[cfg(feature = "lzma-secondary")]
    Lzma,
    /// Zlib/Deflate (ID 3). Rust-only extension.
    #[cfg(feature = "zlib-secondary")]
    Zlib {
        /// Zlib compression level (0-9). Default: 6.
        level: u32,
    },
    /// A custom backend provided by the caller.
    Custom(std::sync::Arc<dyn CompressBackend>),
}

impl std::fmt::Debug for SecondaryCompression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            #[cfg(feature = "lzma-secondary")]
            Self::Lzma => write!(f, "Lzma"),
            #[cfg(feature = "zlib-secondary")]
            Self::Zlib { level } => write!(f, "Zlib {{ level: {level} }}"),
            Self::Custom(b) => write!(f, "Custom(id={})", b.id()),
        }
    }
}

impl SecondaryCompression {
    /// Return the backend implementation, or `None` for no compression.
    pub fn backend(&self) -> Option<Box<dyn CompressBackend>> {
        match self {
            Self::None => None,
            #[cfg(feature = "lzma-secondary")]
            Self::Lzma => Some(Box::new(LzmaBackend)),
            #[cfg(feature = "zlib-secondary")]
            Self::Zlib { level } => Some(Box::new(ZlibBackend::new(*level))),
            Self::Custom(b) => Some(Box::new(ArcBackend(b.clone()))),
        }
    }

    /// Whether secondary compression is enabled.
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Wrapper to make `Arc<dyn CompressBackend>` implement `CompressBackend`.
struct ArcBackend(std::sync::Arc<dyn CompressBackend>);

impl CompressBackend for ArcBackend {
    fn id(&self) -> u8 {
        self.0.id()
    }
    fn compress(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        self.0.compress(data)
    }
    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        self.0.decompress(data)
    }
    fn should_compress(&self, data: &[u8]) -> bool {
        self.0.should_compress(data)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "lzma-secondary")]
    #[test]
    fn lzma_compress_decompress_roundtrip() {
        let backend = LzmaBackend;
        let data: Vec<u8> = b"Hello, world! This is test data. "
            .iter()
            .copied()
            .cycle()
            .take(1024)
            .collect();
        let compressed = backend.compress(&data).unwrap();
        assert!(compressed.len() < data.len());
        let decompressed = backend.decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[cfg(feature = "zlib-secondary")]
    #[test]
    fn zlib_compress_decompress_roundtrip() {
        let backend = ZlibBackend::default();
        let data: Vec<u8> = b"Hello, world! This is test data. "
            .iter()
            .copied()
            .cycle()
            .take(1024)
            .collect();
        let compressed = backend.compress(&data).unwrap();
        assert!(compressed.len() < data.len());
        let decompressed = backend.decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn no_compression_passthrough() {
        let backend = NoCompression;
        let data = b"test data";
        assert!(!backend.should_compress(data));
        let compressed = backend.compress(data).unwrap();
        assert_eq!(compressed, data);
        let decompressed = backend.decompress(data).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn small_data_not_compressed() {
        #[cfg(feature = "lzma-secondary")]
        {
            let backend = LzmaBackend;
            assert!(!backend.should_compress(b"tiny"));
            assert!(!backend.should_compress(&[]));
        }
    }

    #[cfg(feature = "lzma-secondary")]
    #[test]
    fn compress_sections_roundtrip_lzma() {
        let backend = LzmaBackend;
        let data = vec![0xAAu8; 200];
        let inst = vec![0x42u8; 100];
        let addr = vec![0x00u8; 80];

        let (c_data, c_inst, c_addr, del_ind) =
            compress_sections(&backend, &data, &inst, &addr).unwrap();

        let (d_data, d_inst, d_addr) =
            decompress_sections(&c_data, &c_inst, &c_addr, del_ind, Some(VCD_LZMA_ID)).unwrap();

        assert_eq!(d_data, data);
        assert_eq!(d_inst, inst);
        assert_eq!(d_addr, addr);
    }

    #[cfg(feature = "zlib-secondary")]
    #[test]
    fn compress_sections_roundtrip_zlib() {
        let backend = ZlibBackend::default();
        let data = vec![0xAAu8; 200];
        let inst = vec![0x42u8; 100];
        let addr = vec![0x00u8; 80];

        let (c_data, c_inst, c_addr, del_ind) =
            compress_sections(&backend, &data, &inst, &addr).unwrap();

        let (d_data, d_inst, d_addr) =
            decompress_sections(&c_data, &c_inst, &c_addr, del_ind, Some(VCD_ZLIB_ID)).unwrap();

        assert_eq!(d_data, data);
        assert_eq!(d_inst, inst);
        assert_eq!(d_addr, addr);
    }

    #[cfg(feature = "lzma-secondary")]
    #[test]
    fn incompressible_data_preserved() {
        let backend = LzmaBackend;
        let data: Vec<u8> = (0..=255).cycle().take(256).collect();
        let compressed = compress_section(&backend, &data).unwrap();
        if compressed.len() < data.len() {
            let decompressed = backend.decompress(&compressed).unwrap();
            assert_eq!(decompressed, data);
        } else {
            assert_eq!(compressed, data);
        }
    }

    #[test]
    fn wrong_compressor_id_rejected() {
        let result = decompress_sections(b"data", b"inst", b"addr", VCD_DATACOMP, Some(99));
        assert!(result.is_err());
    }

    #[test]
    fn missing_compressor_id_rejected() {
        let result = decompress_sections(b"data", b"inst", b"addr", VCD_DATACOMP, None);
        assert!(result.is_err());
    }

    #[test]
    fn backend_for_id_dispatch() {
        #[cfg(feature = "lzma-secondary")]
        {
            let b = backend_for_id(Some(VCD_LZMA_ID)).unwrap();
            assert_eq!(b.id(), VCD_LZMA_ID);
        }
        #[cfg(feature = "zlib-secondary")]
        {
            let b = backend_for_id(Some(VCD_ZLIB_ID)).unwrap();
            assert_eq!(b.id(), VCD_ZLIB_ID);
        }
        assert!(backend_for_id(Some(99)).is_err());
        assert!(backend_for_id(None).is_err());
    }

    #[test]
    fn secondary_compression_enum() {
        assert!(!SecondaryCompression::None.is_enabled());
        assert!(SecondaryCompression::None.backend().is_none());

        #[cfg(feature = "lzma-secondary")]
        {
            assert!(SecondaryCompression::Lzma.is_enabled());
            let b = SecondaryCompression::Lzma.backend().unwrap();
            assert_eq!(b.id(), VCD_LZMA_ID);
        }

        #[cfg(feature = "zlib-secondary")]
        {
            let zlib = SecondaryCompression::Zlib { level: 6 };
            assert!(zlib.is_enabled());
            let b = zlib.backend().unwrap();
            assert_eq!(b.id(), VCD_ZLIB_ID);
        }
    }

    #[test]
    fn custom_backend() {
        struct TestBackend;
        impl CompressBackend for TestBackend {
            fn id(&self) -> u8 {
                42
            }
            fn compress(&self, data: &[u8]) -> io::Result<Vec<u8>> {
                // Trivial: reverse the bytes
                Ok(data.iter().rev().copied().collect())
            }
            fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
                Ok(data.iter().rev().copied().collect())
            }
        }

        let backend = TestBackend;
        let data = b"hello world";
        let compressed = backend.compress(data).unwrap();
        let decompressed = backend.decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
        assert_eq!(backend.id(), 42);
    }

    #[cfg(all(feature = "lzma-secondary", feature = "zlib-secondary"))]
    #[test]
    fn zlib_vs_lzma_comparison() {
        let data: Vec<u8> = b"ABCDEFGHIJKLMNOP"
            .iter()
            .copied()
            .cycle()
            .take(4096)
            .collect();

        let lzma = LzmaBackend;
        let zlib = ZlibBackend::default();

        let lzma_compressed = lzma.compress(&data).unwrap();
        let zlib_compressed = zlib.compress(&data).unwrap();

        // Both should compress well.
        assert!(lzma_compressed.len() < data.len());
        assert!(zlib_compressed.len() < data.len());

        // Both should roundtrip correctly.
        assert_eq!(lzma.decompress(&lzma_compressed).unwrap(), data);
        assert_eq!(zlib.decompress(&zlib_compressed).unwrap(), data);
    }
}
