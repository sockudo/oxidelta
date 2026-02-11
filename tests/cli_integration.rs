use std::process::Command;
use tempfile::tempdir;

fn bin() -> String {
    env!("CARGO_BIN_EXE_oxidelta").to_string()
}

#[test]
fn cli_encode_decode_roundtrip() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source.bin");
    let target = dir.path().join("target.bin");
    let delta = dir.path().join("delta.vcdiff");
    let output = dir.path().join("output.bin");

    std::fs::write(&source, b"abcde12345abcde12345").unwrap();
    std::fs::write(&target, b"abcdeXXXXXabcde12345!").unwrap();

    let st = Command::new(bin())
        .arg("--force")
        .args(["encode", "--source"])
        .arg(&source)
        .arg(&target)
        .arg(&delta)
        .status()
        .unwrap();
    assert!(st.success());

    let st = Command::new(bin())
        .arg("--force")
        .args(["decode", "--source"])
        .arg(&source)
        .arg(&delta)
        .arg(&output)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(
        std::fs::read(&output).unwrap(),
        std::fs::read(&target).unwrap()
    );
}

#[test]
fn cli_no_output_flag() {
    let dir = tempdir().unwrap();
    let input = dir.path().join("in.bin");
    std::fs::write(&input, b"payload").unwrap();

    let st = Command::new(bin())
        .args(["encode", "--check-only"])
        .arg(&input)
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn cli_config_works() {
    let out = Command::new(bin()).arg("config").output().unwrap();
    assert!(out.status.success());
}
