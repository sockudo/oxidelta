use std::sync::Arc;

use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::secondary::{CompressBackend, SecondaryCompression};
use oxidelta::vcdiff::decoder::DecodeError;

struct XorBackend {
    key: u8,
}

impl CompressBackend for XorBackend {
    fn id(&self) -> u8 {
        200
    }

    fn compress(&self, data: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(data.iter().map(|b| b ^ self.key).collect())
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        Ok(data.iter().map(|b| b ^ self.key).collect())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = b"same source";
    let target = b"same target with edits";

    let backend = Arc::new(XorBackend { key: 0x5A });
    let mut delta = Vec::new();

    encoder::encode_all(
        &mut delta,
        source,
        target,
        CompressOptions {
            secondary: SecondaryCompression::Custom(backend),
            ..Default::default()
        },
    )?;

    println!("custom secondary produced delta size: {}", delta.len());
    println!("note: decoding custom backend requires matching backend registration");
    Ok(())
}
