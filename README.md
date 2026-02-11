# Oxidelta

High-performance VCDIFF (RFC 3284) delta encoding/decoding in Rust.

`oxidelta` targets interoperability with xdelta3 at the file format level, while providing a Rust-native library API and an idiomatic CLI.

## Quick Start

### Install

```bash
cargo install --locked oxidelta
```

Alternative (prebuilt binary, faster install):

```bash
cargo binstall oxidelta
```

Manual binary install:

1. Download your platform archive from GitHub Releases.
2. Extract it and place `oxidelta` on your `PATH`.

Package manager note:

- Homebrew/Scoop/apt/rpm distribution is supported by the release pipeline artifacts.
- If you maintain internal package repos, consume release tarballs and checksums from releases.

### Encode

```bash
oxidelta encode --source old.bin new.bin patch.vcdiff
```

### Decode

```bash
oxidelta decode --source old.bin patch.vcdiff restored.bin
```

### Inspect a patch

```bash
oxidelta header patch.vcdiff
oxidelta headers patch.vcdiff
oxidelta delta patch.vcdiff
```

## CLI Highlights

- Subcommand-first CLI: `encode`, `decode`, `config`, `header`, `headers`, `delta`, `recode`, `merge`
- Tunables:
  - `--level 0..9`
  - `--window-size`
  - `--source-window-size`
  - `--duplicate-window-size`
  - `--instruction-buffer-size`
  - `--secondary {none,lzma,zlib,djw,fgk}`
- Output controls:
  - `--stdout`
  - `--check-only`
  - `--json`
  - global `--force`, `--quiet`, `--verbose`

## Library Usage

```rust
use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::decoder;

fn main() {
    let source = b"hello old world";
    let target = b"hello new world";

    let mut delta = Vec::new();
    encoder::encode_all(&mut delta, source, target, CompressOptions::default()).unwrap();

    let decoded = decoder::decode_all(source, &delta).unwrap();
    assert_eq!(decoded, target);
}
```

More examples:
- `examples/basic_encode_decode.rs`
- `examples/library_usage.rs`
- `examples/custom_backend.rs`
- `examples/integration_pipeline.rs`

## Documentation

- Architecture: `ARCHITECTURE.md`
- Performance and benchmark methodology: `PERFORMANCE.md`
- Compatibility matrix and differences: `COMPATIBILITY.md`
- Migration guide from xdelta CLI workflows: `MIGRATION.md`

API docs:

```bash
cargo doc --all-features --no-deps --open
```

Hosted docs are intended at: <https://docs.rs/oxidelta>

## Release and Binaries

Automated release pipelines build and publish binaries for:

- Linux: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
- macOS: `x86_64-apple-darwin`, `aarch64-apple-darwin`
- Windows: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`

See `.github/workflows/release.yml` for details.

For crates.io publishing, configure repository secret `CARGO_REGISTRY_TOKEN` (an API token with publish permissions).

## Status

Production hardening is ongoing. The project is already heavily tested (unit, integration, property tests, cross-interop tests against xdelta3), and release/benchmark workflows are in place.
