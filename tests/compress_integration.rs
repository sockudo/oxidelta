// Integration tests for the compress module.
//
// Tests the full pipeline: DeltaEncoder -> VCDIFF stream -> DeltaDecoder,
// including streaming, all compression levels, secondary compression
// (LZMA + Zlib), cross-compatibility with xdelta3, and large data.

use oxidelta::compress::decoder::{self, DeltaDecoder};
use oxidelta::compress::encoder::{self, CompressOptions, DeltaEncoder};
use oxidelta::compress::secondary::SecondaryCompression;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn roundtrip(source: &[u8], target: &[u8], opts: CompressOptions) {
    let mut delta = Vec::new();
    encoder::encode_all(&mut delta, source, target, opts).unwrap();
    let decoded = decoder::decode_all(source, &delta).unwrap();
    assert_eq!(
        decoded,
        target,
        "roundtrip mismatch (source={}, target={}, delta={})",
        source.len(),
        target.len(),
        delta.len()
    );
}

fn generate_data(size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut data = Vec::with_capacity(size);
    for _ in 0..size {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        data.push((state >> 33) as u8);
    }
    data
}

fn mutate_data(source: &[u8], change_pct: f64, seed: u64) -> Vec<u8> {
    let mut target = source.to_vec();
    let mut state = seed;
    let changes = ((change_pct / 100.0) * source.len() as f64) as usize;
    for _ in 0..changes {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let pos = (state >> 33) as usize % target.len();
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        target[pos] = (state >> 33) as u8;
    }
    target
}

/// Helper to build repetitive data large enough for secondary compression.
fn repetitive_data(pattern: &[u8], total: usize) -> Vec<u8> {
    pattern.iter().copied().cycle().take(total).collect()
}

// ---------------------------------------------------------------------------
// All levels roundtrip
// ---------------------------------------------------------------------------

#[test]
fn all_levels_1kb() {
    let source = generate_data(1024, 1);
    let target = mutate_data(&source, 5.0, 2);
    for level in 0..=9 {
        roundtrip(
            &source,
            &target,
            CompressOptions {
                level,
                ..Default::default()
            },
        );
    }
}

#[test]
fn all_levels_64kb() {
    let source = generate_data(64 * 1024, 10);
    let target = mutate_data(&source, 5.0, 20);
    for level in 0..=9 {
        roundtrip(
            &source,
            &target,
            CompressOptions {
                level,
                ..Default::default()
            },
        );
    }
}

#[test]
fn all_levels_1mb() {
    let source = generate_data(1024 * 1024, 100);
    let target = mutate_data(&source, 2.0, 200);
    for level in [0, 1, 6, 9] {
        roundtrip(
            &source,
            &target,
            CompressOptions {
                level,
                ..Default::default()
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Level 0 (store only)
// ---------------------------------------------------------------------------

#[test]
fn level_0_is_store_only() {
    let target = b"Hello, world! This is stored without compression.";
    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        b"",
        target,
        CompressOptions {
            level: 0,
            checksum: true,
            ..Default::default()
        },
    )
    .unwrap();

    // Delta should contain the full target data (plus VCDIFF overhead).
    assert!(delta.len() >= target.len());

    let decoded = decoder::decode_all(b"", &delta).unwrap();
    assert_eq!(decoded, target);
}

// ---------------------------------------------------------------------------
// Streaming encode
// ---------------------------------------------------------------------------

#[test]
fn streaming_encode_small_chunks() {
    let source = generate_data(4096, 42);
    let target = mutate_data(&source, 10.0, 99);

    // Encode in tiny 37-byte chunks (prime number to stress boundary handling).
    let mut delta = Vec::new();
    let mut enc = DeltaEncoder::new(
        &mut delta,
        &source,
        CompressOptions {
            level: 6,
            window_size: 1024,
            ..Default::default()
        },
    );
    for chunk in target.chunks(37) {
        enc.write_target(chunk).unwrap();
    }
    enc.finish().unwrap();

    let decoded = decoder::decode_all(&source, &delta).unwrap();
    assert_eq!(decoded, target);
}

#[test]
fn streaming_encode_single_byte() {
    let target = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut delta = Vec::new();
    let mut enc = DeltaEncoder::new(
        &mut delta,
        b"",
        CompressOptions {
            level: 1,
            window_size: 16,
            ..Default::default()
        },
    );
    for &byte in target.iter() {
        enc.write_target(&[byte]).unwrap();
    }
    enc.finish().unwrap();

    let decoded = decoder::decode_all(b"", &delta).unwrap();
    assert_eq!(decoded, target);
}

// ---------------------------------------------------------------------------
// Streaming decode
// ---------------------------------------------------------------------------

#[test]
fn streaming_decode_window_by_window() {
    let source = generate_data(2048, 11);
    let target = mutate_data(&source, 5.0, 22);

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            window_size: 512,
            ..Default::default()
        },
    )
    .unwrap();

    let mut decoder = DeltaDecoder::new(std::io::Cursor::new(&delta));
    let mut src: &[u8] = &source;
    let mut output = Vec::new();

    while let Some(win_size) = decoder.decode_window_to(&mut src, &mut output).unwrap() {
        assert!(win_size > 0);
    }

    assert_eq!(output, target);
    assert!(decoder.windows_decoded() >= 1);
    assert_eq!(decoder.bytes_decoded(), target.len() as u64);
}

// ---------------------------------------------------------------------------
// Secondary compression — LZMA
// ---------------------------------------------------------------------------

#[cfg(feature = "lzma-secondary")]
#[test]
fn secondary_lzma_roundtrip() {
    let source = repetitive_data(b"ABCDEFGHIJ", 4096);
    let mut target = source.clone();
    target[100] = b'X';
    target[500] = b'Y';
    target[2000] = b'Z';

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            checksum: true,
            secondary: SecondaryCompression::Lzma,
            ..Default::default()
        },
    )
    .unwrap();

    let decoded = decoder::decode_all(&source, &delta).unwrap();
    assert_eq!(decoded, target);
}

#[cfg(feature = "lzma-secondary")]
#[test]
fn secondary_lzma_vs_no_secondary() {
    let source = repetitive_data(b"The quick brown fox jumps over the lazy dog. ", 8192);
    let target = mutate_data(&source, 5.0, 77);

    let mut delta_plain = Vec::new();
    encoder::encode_all(
        &mut delta_plain,
        &source,
        &target,
        CompressOptions {
            level: 6,
            secondary: SecondaryCompression::None,
            ..Default::default()
        },
    )
    .unwrap();

    let mut delta_lzma = Vec::new();
    encoder::encode_all(
        &mut delta_lzma,
        &source,
        &target,
        CompressOptions {
            level: 6,
            secondary: SecondaryCompression::Lzma,
            ..Default::default()
        },
    )
    .unwrap();

    // Both must produce correct output.
    assert_eq!(decoder::decode_all(&source, &delta_plain).unwrap(), target);
    assert_eq!(decoder::decode_all(&source, &delta_lzma).unwrap(), target);
}

// ---------------------------------------------------------------------------
// Secondary compression — Zlib
// ---------------------------------------------------------------------------

#[cfg(feature = "zlib-secondary")]
#[test]
fn secondary_zlib_roundtrip() {
    let source = repetitive_data(b"ABCDEFGHIJ", 4096);
    let mut target = source.clone();
    target[100] = b'X';
    target[500] = b'Y';
    target[2000] = b'Z';

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            checksum: true,
            secondary: SecondaryCompression::Zlib { level: 6 },
            ..Default::default()
        },
    )
    .unwrap();

    let decoded = decoder::decode_all(&source, &delta).unwrap();
    assert_eq!(decoded, target);
}

#[cfg(feature = "zlib-secondary")]
#[test]
fn secondary_zlib_all_levels() {
    let source = repetitive_data(b"ABCDEFGHIJ", 4096);
    let mut target = source.clone();
    target[100] = b'X';

    for zlib_level in [0, 1, 6, 9] {
        let mut delta = Vec::new();
        encoder::encode_all(
            &mut delta,
            &source,
            &target,
            CompressOptions {
                level: 6,
                secondary: SecondaryCompression::Zlib { level: zlib_level },
                ..Default::default()
            },
        )
        .unwrap();

        let decoded = decoder::decode_all(&source, &delta).unwrap();
        assert_eq!(decoded, target, "zlib level {zlib_level} roundtrip failed");
    }
}

#[cfg(all(feature = "lzma-secondary", feature = "zlib-secondary"))]
#[test]
fn secondary_lzma_vs_zlib_comparison() {
    let source = repetitive_data(b"The quick brown fox jumps over the lazy dog. ", 8192);
    let target = mutate_data(&source, 5.0, 77);

    let mut delta_lzma = Vec::new();
    encoder::encode_all(
        &mut delta_lzma,
        &source,
        &target,
        CompressOptions {
            level: 6,
            secondary: SecondaryCompression::Lzma,
            ..Default::default()
        },
    )
    .unwrap();

    let mut delta_zlib = Vec::new();
    encoder::encode_all(
        &mut delta_zlib,
        &source,
        &target,
        CompressOptions {
            level: 6,
            secondary: SecondaryCompression::Zlib { level: 6 },
            ..Default::default()
        },
    )
    .unwrap();

    // Both must produce correct output.
    let decoded_lzma = decoder::decode_all(&source, &delta_lzma).unwrap();
    let decoded_zlib = decoder::decode_all(&source, &delta_zlib).unwrap();
    assert_eq!(decoded_lzma, target);
    assert_eq!(decoded_zlib, target);

    // Both should be smaller than the no-compression delta.
    let mut delta_plain = Vec::new();
    encoder::encode_all(
        &mut delta_plain,
        &source,
        &target,
        CompressOptions::default(),
    )
    .unwrap();

    // At least one secondary method should compress better.
    let smallest_secondary = delta_lzma.len().min(delta_zlib.len());
    assert!(
        smallest_secondary <= delta_plain.len(),
        "secondary ({smallest_secondary}) should not be larger than plain ({})",
        delta_plain.len()
    );
}

// ---------------------------------------------------------------------------
// Custom backend
// ---------------------------------------------------------------------------

#[test]
fn custom_backend_roundtrip() {
    use oxidelta::compress::secondary::CompressBackend;
    use oxidelta::vcdiff::decoder::DecodeError;
    use std::sync::Arc;

    /// Simple XOR-based "compressor" for testing the plugin architecture.
    /// Prefixes compressed data with 1-byte XOR key so it's self-describing.
    struct XorBackend {
        key: u8,
    }

    impl CompressBackend for XorBackend {
        fn id(&self) -> u8 {
            200 // Custom ID
        }
        fn compress(&self, data: &[u8]) -> std::io::Result<Vec<u8>> {
            let mut out = Vec::with_capacity(data.len() + 1);
            out.push(self.key);
            for &b in data {
                out.push(b ^ self.key);
            }
            Ok(out)
        }
        fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
            if data.is_empty() {
                return Ok(Vec::new());
            }
            let key = data[0];
            Ok(data[1..].iter().map(|&b| b ^ key).collect())
        }
    }

    // Note: custom backends can't be decoded without the backend being registered
    // in backend_for_id(). This test verifies the encode path works.
    // For a full roundtrip, we'd need to register the backend for decoding.
    // Here we test that encoding completes without error.
    let source = repetitive_data(b"ABCDEFGHIJ", 4096);
    let mut target = source.clone();
    target[100] = b'X';

    let backend = Arc::new(XorBackend { key: 0x42 });
    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            secondary: SecondaryCompression::Custom(backend),
            ..Default::default()
        },
    )
    .unwrap();

    // Delta should be non-empty (encoding succeeded).
    assert!(!delta.is_empty());
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn empty_source_empty_target() {
    roundtrip(b"", b"", CompressOptions::default());
}

#[test]
fn empty_source() {
    let target = b"standalone data";
    roundtrip(b"", target, CompressOptions::default());
}

#[test]
fn empty_target() {
    roundtrip(b"some source data", b"", CompressOptions::default());
}

#[test]
fn identical_source_and_target() {
    let data = generate_data(4096, 55);
    roundtrip(&data, &data, CompressOptions::default());
}

#[test]
fn single_byte_target() {
    roundtrip(b"", b"X", CompressOptions::default());
}

#[test]
fn all_zeros() {
    let target = vec![0u8; 4096];
    roundtrip(b"", &target, CompressOptions::default());
}

#[test]
fn all_ones() {
    let target = vec![0xFF; 4096];
    roundtrip(b"", &target, CompressOptions::default());
}

// ---------------------------------------------------------------------------
// Cross-compatibility with xdelta3 crate (C FFI)
// ---------------------------------------------------------------------------

#[test]
fn xdelta3_can_decode_compress_output() {
    let source = b"The quick brown fox jumps over the lazy dog. 1234567890";
    let target = b"The quick brown cat sits on the lazy mat. 1234567890!!!";

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        source,
        target,
        CompressOptions {
            level: 6,
            secondary: SecondaryCompression::None,
            ..Default::default()
        },
    )
    .unwrap();

    let decoded = xdelta3::decode(&delta, source).expect("xdelta3 C failed to decode");
    assert_eq!(decoded, target);
}

#[test]
fn compress_can_decode_xdelta3_output() {
    let source = b"The quick brown fox jumps over the lazy dog. 1234567890";
    let target = b"The quick brown cat sits on the lazy mat. 1234567890!!!";

    let delta = xdelta3::encode(target, source).expect("xdelta3 C encode failed");
    let decoded = decoder::decode_all(source, &delta).unwrap();
    assert_eq!(decoded, target);
}

#[test]
fn xdelta3_interop_all_levels() {
    let source = generate_data(4096, 33);
    let target = mutate_data(&source, 5.0, 44);

    for level in [1, 6, 9] {
        let mut delta = Vec::new();
        encoder::encode_all(
            &mut delta,
            &source,
            &target,
            CompressOptions {
                level,
                secondary: SecondaryCompression::None,
                ..Default::default()
            },
        )
        .unwrap();

        let decoded = xdelta3::decode(&delta, &source)
            .unwrap_or_else(|| panic!("xdelta3 failed to decode level {level}"));
        assert_eq!(decoded, target, "xdelta3 interop failed at level {level}");
    }
}

// ---------------------------------------------------------------------------
// Instruction optimization effectiveness
// ---------------------------------------------------------------------------

#[test]
fn optimization_does_not_break_correctness() {
    let mut target = Vec::new();
    target.extend_from_slice(b"HEADER");
    target.extend(std::iter::repeat_n(0xAA, 100));
    target.extend_from_slice(b"MIDDLE");
    target.extend(std::iter::repeat_n(0xBB, 200));
    target.extend_from_slice(b"FOOTER");

    roundtrip(b"", &target, CompressOptions::default());
}

// ---------------------------------------------------------------------------
// Multi-window processing
// ---------------------------------------------------------------------------

#[test]
fn multi_window_large_data() {
    let source = generate_data(32 * 1024, 1);
    let target = mutate_data(&source, 3.0, 2);

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            window_size: 8 * 1024,
            ..Default::default()
        },
    )
    .unwrap();

    let mut dec = DeltaDecoder::new(std::io::Cursor::new(&delta));
    let mut src: &[u8] = &source;
    let mut output = Vec::new();
    dec.decode_to(&mut src, &mut output).unwrap();

    assert_eq!(output, target);
    assert!(
        dec.windows_decoded() >= 4,
        "expected at least 4 windows, got {}",
        dec.windows_decoded()
    );
}

// ---------------------------------------------------------------------------
// Delta is actually smaller than target for similar data
// ---------------------------------------------------------------------------

#[test]
fn delta_compression_effective() {
    let source = generate_data(16 * 1024, 7);
    let target = mutate_data(&source, 1.0, 8);

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(
        delta.len() < target.len() / 2,
        "delta ({}) should be much smaller than target ({}) for 99% similar data",
        delta.len(),
        target.len()
    );
}

// ---------------------------------------------------------------------------
// Multi-window with secondary compression
// ---------------------------------------------------------------------------

#[cfg(feature = "lzma-secondary")]
#[test]
fn multi_window_lzma_secondary() {
    let source = repetitive_data(b"ABCDEFGHIJKLMNOP", 32 * 1024);
    let target = mutate_data(&source, 2.0, 55);

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            window_size: 8 * 1024,
            secondary: SecondaryCompression::Lzma,
            ..Default::default()
        },
    )
    .unwrap();

    let decoded = decoder::decode_all(&source, &delta).unwrap();
    assert_eq!(decoded, target);
}

#[cfg(feature = "zlib-secondary")]
#[test]
fn multi_window_zlib_secondary() {
    let source = repetitive_data(b"ABCDEFGHIJKLMNOP", 32 * 1024);
    let target = mutate_data(&source, 2.0, 55);

    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            window_size: 8 * 1024,
            secondary: SecondaryCompression::Zlib { level: 6 },
            ..Default::default()
        },
    )
    .unwrap();

    let decoded = decoder::decode_all(&source, &delta).unwrap();
    assert_eq!(decoded, target);
}

// ---------------------------------------------------------------------------
// Streaming encode + secondary compression
// ---------------------------------------------------------------------------

#[cfg(feature = "zlib-secondary")]
#[test]
fn streaming_encode_with_zlib() {
    let source = repetitive_data(b"ABCDEFGHIJ", 4096);
    let mut target = source.clone();
    target[100] = b'X';
    target[500] = b'Y';

    let mut delta = Vec::new();
    let mut enc = DeltaEncoder::new(
        &mut delta,
        &source,
        CompressOptions {
            level: 6,
            window_size: 1024,
            secondary: SecondaryCompression::Zlib { level: 6 },
            ..Default::default()
        },
    );
    for chunk in target.chunks(37) {
        enc.write_target(chunk).unwrap();
    }
    enc.finish().unwrap();

    let decoded = decoder::decode_all(&source, &delta).unwrap();
    assert_eq!(decoded, target);
}
