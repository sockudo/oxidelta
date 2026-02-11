use std::path::Path;

use oxidelta::compress::encoder::CompressOptions;
use oxidelta::io::{decode_file, encode_file};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = Path::new("source.bin");
    let target = Path::new("target.bin");
    let delta = Path::new("patch.vcdiff");
    let output = Path::new("restored.bin");

    let enc = encode_file(source, target, delta, CompressOptions::default())?;
    let dec = decode_file(source, delta, output)?;

    println!(
        "encode: source={} target={} delta={} windows={}",
        enc.source_size, enc.target_size, enc.delta_size, enc.windows
    );
    println!(
        "decode: source={} delta={} output={} windows={}",
        dec.source_size, dec.delta_size, dec.output_size, dec.windows
    );

    Ok(())
}
