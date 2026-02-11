# COMPATIBILITY

## Scope

Oxidelta targets compatibility at the **VCDIFF stream level** with xdelta3-style workflows.

Compatibility in this document means:

- A delta produced by one implementation can be decoded by the other (for supported feature combinations).
- Reconstructed output bytes are identical to the target bytes.

## Version Matrix

| Component | Status | Notes |
|---|---|---|
| xdelta 3.x decode of Oxidelta output | Supported (tested) | Covered by Rust tests using `xdelta3` crate and xdelta3 binary interop tests. |
| Oxidelta decode of xdelta 3.x output | Supported (tested) | Covered by integration/regression tests. |
| xdelta 1.x interoperability | Not guaranteed | Different historical format/behavior assumptions; no CI coverage currently. |

## Feature Compatibility Matrix

| Feature | xdelta3 | Oxidelta | Interop status |
|---|---|---|---|
| Core VCDIFF ADD/COPY/RUN | Yes | Yes | Compatible |
| Adler32 window checksum | Yes | Yes | Compatible |
| LZMA secondary compression | Yes (build dependent) | Yes (`lzma-secondary`) | Compatible when enabled on both sides |
| Zlib secondary compression ID=3 | No (non-standard in xdelta3 C) | Yes (`zlib-secondary`) | Oxidelta-only extension |
| Custom secondary compressors | Limited/internal | Yes (trait-based extension) | Not cross-compatible unless both sides implement same ID/codec |
| Legacy xdelta CLI syntax parity | Yes (native) | No (intentional) | Use migration guide/scripts |

## Known Differences

1. CLI behavior is intentionally Rust-idiomatic in Oxidelta and not argument-compatible with legacy `xdelta` flags by default.
2. Oxidelta supports a non-standard zlib secondary compressor ID (`3`) for Rust-native use; xdelta3 C does not decode it.
3. Bit-identical deltas are not guaranteed; semantic decode compatibility is the target.

## Verification Sources in Repository

- `tests/compress_integration.rs`
- `tests/regression_vectors.rs`
- `tests/vcdiff_integration.rs`
- `docs/benchmarks.md` (delta-size comparison snapshot)

## Practical Guidance

- For strict interop with xdelta3, use:
  - `--secondary lzma` or `--secondary none`
  - standard checksum behavior
- Avoid Oxidelta-specific secondary codecs when exchanging deltas with xdelta3.
