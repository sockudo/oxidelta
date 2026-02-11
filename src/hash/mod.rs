// Hash and matching algorithms for xdelta delta compression.
//
// This module provides:
// - Rolling hash functions (small 4-byte and large Rabin-Karp)
// - Hash tables with HASH_CKOFFSET semantics
// - Block matching with forward/backward extension
// - Matcher profiles (fastest..slow)

pub mod config;
pub mod matching;
pub mod rolling;
pub mod table;
