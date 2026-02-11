// VCDIFF format implementation (RFC 3284).
//
// This module provides encoding and decoding of the VCDIFF delta format,
// byte-for-byte compatible with xdelta3.
//
// # Modules
//
// - `varint`        — Variable-length integer encoding (base-128, big-endian)
// - `address_cache` — NEAR/SAME address cache for COPY instruction addresses
// - `code_table`    — Default RFC 3284 code table (256 entries)
// - `header`        — File header and per-window header encoding/decoding
// - `encoder`       — Instruction encoding and window emission
// - `decoder`       — Instruction decoding and window reconstruction

pub mod address_cache;
pub mod code_table;
pub mod decoder;
pub mod encoder;
pub mod header;
pub mod varint;

// Re-export key types for convenience.
pub use address_cache::AddressCache;
pub use code_table::{CodeTable, CodeTableEntry, Instruction};
pub use decoder::{DecodeError, InstructionIterator, StreamDecoder, decode_memory};
pub use encoder::{SourceWindow, StreamEncoder, WindowEncoder, WindowSections};
pub use header::{FileHeader, VCDIFF_MAGIC, WindowHeader};
