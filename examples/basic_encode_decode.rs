use oxidelta::compress::decoder;
use oxidelta::compress::encoder::{self, CompressOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = b"Hello from source";
    let target = b"Hello from updated target";

    let mut delta = Vec::new();
    encoder::encode_all(&mut delta, source, target, CompressOptions::default())?;

    let restored = decoder::decode_all(source, &delta)?;
    assert_eq!(restored, target);

    println!(
        "encoded {} bytes -> delta {} bytes -> restored {} bytes",
        target.len(),
        delta.len(),
        restored.len()
    );

    Ok(())
}
