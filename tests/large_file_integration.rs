use oxidelta::io::{decode_file, encode_file};
use std::io::{Seek, Write};
use tempfile::NamedTempFile;

#[test]
#[ignore = "multi-GB test is opt-in due runtime and disk requirements"]
fn multi_gb_sparse_file_roundtrip() {
    let mut source = NamedTempFile::new().unwrap();
    let mut target = NamedTempFile::new().unwrap();
    let delta = NamedTempFile::new().unwrap();
    let output = NamedTempFile::new().unwrap();

    // Create sparse 2 GiB files and touch a few blocks with deterministic mutations.
    source
        .as_file_mut()
        .set_len(2 * 1024 * 1024 * 1024)
        .unwrap();
    target
        .as_file_mut()
        .set_len(2 * 1024 * 1024 * 1024)
        .unwrap();

    source
        .as_file_mut()
        .seek(std::io::SeekFrom::Start(64 * 1024))
        .unwrap();
    source.as_file_mut().write_all(b"baseline-block").unwrap();

    target
        .as_file_mut()
        .seek(std::io::SeekFrom::Start(64 * 1024))
        .unwrap();
    target.as_file_mut().write_all(b"mutated-block!").unwrap();
    target
        .as_file_mut()
        .seek(std::io::SeekFrom::Start(1024 * 1024 * 1024))
        .unwrap();
    target.as_file_mut().write_all(b"middle-chunk").unwrap();

    let enc = encode_file(
        source.path(),
        target.path(),
        delta.path(),
        oxidelta::compress::encoder::CompressOptions {
            level: 6,
            window_size: 256 * 1024,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(enc.delta_size > 0);

    let dec = decode_file(source.path(), delta.path(), output.path()).unwrap();
    assert_eq!(dec.output_size, 2 * 1024 * 1024 * 1024);

    let out_meta = std::fs::metadata(output.path()).unwrap();
    let tgt_meta = std::fs::metadata(target.path()).unwrap();
    assert_eq!(out_meta.len(), tgt_meta.len());

    let mut out_f = std::fs::File::open(output.path()).unwrap();
    let mut tgt_f = std::fs::File::open(target.path()).unwrap();
    for off in [
        0u64,
        64 * 1024,
        1024 * 1024 * 1024,
        (2 * 1024 * 1024 * 1024) - 32,
    ] {
        out_f.seek(std::io::SeekFrom::Start(off)).unwrap();
        tgt_f.seek(std::io::SeekFrom::Start(off)).unwrap();
        let mut ob = [0u8; 32];
        let mut tb = [0u8; 32];
        use std::io::Read;
        out_f.read_exact(&mut ob).unwrap();
        tgt_f.read_exact(&mut tb).unwrap();
        assert_eq!(ob, tb, "mismatch at offset {off}");
    }
}

#[test]
fn edge_case_matrix() {
    let cases: Vec<(&[u8], &[u8])> = vec![
        (b"", b""),
        (b"", b"x"),
        (b"x", b""),
        (b"\0\0\0\0\0", b"\0\0\0\0\0"),
        (b"\0\0\0\0\0", b"\0\0\0\0\x01"),
    ];

    for (source, target) in cases {
        let mut delta = Vec::new();
        oxidelta::compress::encoder::encode_all(
            &mut delta,
            source,
            target,
            oxidelta::compress::encoder::CompressOptions::default(),
        )
        .unwrap();
        let decoded = oxidelta::compress::decoder::decode_all(source, &delta).unwrap();
        assert_eq!(decoded, target);
    }
}
