// Full compression/delta encoding pipeline.
//
// This module provides the production-quality streaming API for delta
// compression, building on the core VCDIFF and hash modules:
//
// - `encoder`   — DeltaEncoder: streaming encode with source window reuse
// - `decoder`   — DeltaDecoder: streaming decode with progress tracking
// - `pipeline`  — Instruction optimization (coalescing, run detection)
// - `secondary` — Pluggable secondary compression (LZMA, Zlib, custom)

pub mod decoder;
pub mod encoder;
pub mod pipeline;
pub mod secondary;

pub use decoder::DeltaDecoder;
pub use encoder::{CompressOptions, DeltaEncoder, EncodeError};
pub use secondary::{CompressBackend, SecondaryCompression};
