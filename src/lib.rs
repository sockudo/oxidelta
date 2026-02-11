//! Oxidelta: VCDIFF (RFC 3284) delta encoding/decoding in Rust.
//!
//! The crate provides:
//! - A pure-Rust VCDIFF engine (`vcdiff`)
//! - High-level compression APIs (`compress`)
//! - File-oriented helpers (`io`)
//! - An optional CLI (`cli` feature)
//!
//! # Quick Start
//!
//! ```no_run
//! use oxidelta::compress::encoder::{self, CompressOptions};
//! use oxidelta::compress::decoder;
//!
//! let source = b"hello old world";
//! let target = b"hello new world";
//!
//! let mut delta = Vec::new();
//! encoder::encode_all(&mut delta, source, target, CompressOptions::default()).unwrap();
//! let decoded = decoder::decode_all(source, &delta).unwrap();
//! assert_eq!(decoded, target);
//! ```

pub mod compress;
pub mod engine;
pub mod hash;
pub mod io;
pub mod vcdiff;

#[cfg(feature = "cli")]
pub mod cli;
