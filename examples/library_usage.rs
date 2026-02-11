use oxidelta::compress::decoder::DeltaDecoder;
use oxidelta::compress::encoder::{CompressOptions, DeltaEncoder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = b"ABCD-ABCD-ABCD-ABCD";
    let target = b"ABCD-XXXX-ABCD-YYYY";

    let mut delta = Vec::new();
    let mut enc = DeltaEncoder::new(
        &mut delta,
        source,
        CompressOptions {
            level: 6,
            window_size: 1024,
            ..Default::default()
        },
    );
    enc.write_target(target)?;
    enc.finish()?;

    let mut dec = DeltaDecoder::new(std::io::Cursor::new(&delta));
    let mut src: &[u8] = source;
    let mut out = Vec::new();
    dec.decode_to(&mut src, &mut out)?;

    assert_eq!(out, target);
    println!("windows decoded: {}", dec.windows_decoded());
    Ok(())
}
