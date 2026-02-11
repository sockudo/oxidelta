# ARCHITECTURE

## Overview

Oxidelta is organized in layered modules:

1. `vcdiff`: RFC 3284 wire-format implementation
2. `hash`: match-finding primitives and hash tables
3. `compress`: high-level encode/decode pipeline and secondary compression
4. `io`: file-oriented streaming helpers
5. `cli`: optional command-line interface (feature-gated)

Top-level:

- `src/lib.rs`: library entrypoint and public module surface
- `src/main.rs`: CLI binary entrypoint (enabled with `cli` feature)

## Module Map

### `src/vcdiff/*`

- `encoder.rs`: stream/window encoder
- `decoder.rs`: stream/window decoder
- `header.rs`: file/window header parsing and encoding
- `code_table.rs`: RFC code table and instruction packing
- `address_cache.rs`: NEAR/SAME cache logic for COPY addresses
- `varint.rs`: base-128 varint encode/decode

This layer guarantees VCDIFF correctness and interoperability constraints.

### `src/hash/*`

- `config.rs`: compression level to matcher config mapping
- `rolling.rs`: rolling hash and run/match helpers
- `table.rs`: hash table implementations used during matching
- `matching.rs`: source/target match discovery and instruction candidates

This layer is the core compression-efficiency/performance engine.

### `src/compress/*`

- `encoder.rs`: high-level streaming encoder (`DeltaEncoder`) and `encode_all`
- `decoder.rs`: high-level streaming decoder (`DeltaDecoder`) and `decode_all`
- `pipeline.rs`: instruction stream optimization passes
- `secondary.rs`: secondary compression backends (LZMA, Zlib, custom trait)

This layer composes hash + VCDIFF for end-user encode/decode APIs.

### `src/io.rs`

- `encode_file`: source/target/delta file pipeline
- `decode_file`: source/delta/output file pipeline
- optional SHA-256 checksums when `file-io` feature is enabled

### `src/cli.rs`

Idiomatic clap-based CLI with subcommands:

- `encode`, `decode`, `config`
- `header`, `headers`, `delta`
- `recode`, `merge`

## Data Flow

### Encode

1. Build source match index from source bytes.
2. Stream target in windows.
3. Find COPY/RUN/ADD candidates.
4. Optimize instruction sequence.
5. Emit VCDIFF sections (DATA/INST/ADDR).
6. Optionally apply secondary compression.
7. Write RFC-compliant stream.

### Decode

1. Parse VCDIFF headers and window descriptors.
2. Optionally decompress compressed sections.
3. Execute instruction stream (ADD/COPY/RUN) against source/output history.
4. Validate optional checksum.
5. Stream reconstructed bytes to destination.

## Key Design Decisions

- Streaming-first APIs for bounded memory on large files.
- Explicit separation between wire-format logic (`vcdiff`) and compression policy (`compress`).
- Feature-gated optional components (`cli`, `lzma-secondary`, `zlib-secondary`, `file-io`, `parallel`).
- Cross-interop tests with xdelta3 for format-level compatibility validation.

## Non-Goals (Current)

- Full command-line argument compatibility with legacy xdelta CLI syntax.
- Legacy xdelta 1.x bitstream compatibility guarantees.
- Guaranteeing bit-identical deltas relative to xdelta3 for every workload.

## Extensibility Points

- Custom secondary compressors via `CompressBackend` trait.
- Additional CLI adapters/wrappers for legacy command migration.
- Optional parallelism in encode paths under `parallel` feature.
