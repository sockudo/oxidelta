use oxidelta::compress::decoder;
use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::secondary::SecondaryCompression;

#[derive(Debug)]
struct Vector {
    name: String,
    source: Vec<u8>,
    target: Vec<u8>,
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    let s = s.trim();
    if s.is_empty() {
        return Vec::new();
    }
    assert!(
        s.len().is_multiple_of(2),
        "hex string must have even length"
    );
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn load_vectors() -> Vec<Vector> {
    let manifest = include_str!("vectors/manifest.tsv");
    manifest
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let parts: Vec<_> = line.split('|').collect();
            assert_eq!(parts.len(), 4, "invalid vector row: {line}");
            Vector {
                name: parts[0].to_string(),
                source: hex_to_bytes(parts[2]),
                target: hex_to_bytes(parts[3]),
            }
        })
        .collect()
}

fn encode_rust(source: &[u8], target: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encoder::encode_all(
        &mut out,
        source,
        target,
        CompressOptions {
            level: 6,
            secondary: SecondaryCompression::None,
            ..Default::default()
        },
    )
    .unwrap();
    out
}

#[test]
fn vector_database_is_non_empty() {
    let vectors = load_vectors();
    assert!(!vectors.is_empty());
}

#[test]
fn rust_encode_xdelta_decode_all_vectors() {
    for v in load_vectors() {
        let delta = encode_rust(&v.source, &v.target);
        let decoded = xdelta3::decode(&delta, &v.source)
            .unwrap_or_else(|| panic!("xdelta decode failed for {}", v.name));
        assert_eq!(decoded, v.target, "vector {}", v.name);
    }
}

#[test]
fn xdelta_encode_rust_decode_all_vectors() {
    for v in load_vectors() {
        let delta = xdelta3::encode(&v.target, &v.source)
            .unwrap_or_else(|| panic!("xdelta encode failed for {}", v.name));
        let decoded =
            decoder::decode_all(&v.source, &delta).unwrap_or_else(|_| panic!("vector {}", v.name));
        assert_eq!(decoded, v.target, "vector {}", v.name);
    }
}

#[test]
fn rust_roundtrip_all_vectors() {
    for v in load_vectors() {
        let delta = encode_rust(&v.source, &v.target);
        let decoded = decoder::decode_all(&v.source, &delta).unwrap();
        assert_eq!(decoded, v.target, "vector {}", v.name);
    }
}
