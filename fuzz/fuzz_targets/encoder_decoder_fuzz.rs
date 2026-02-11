#![no_main]
use libfuzzer_sys::fuzz_target;
use oxidelta::compress::decoder;
use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::secondary::SecondaryCompression;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }

    let level = (data[0] % 10) as u32;
    let split = 2 + (data[1] as usize % (data.len() - 2));
    let source = &data[2..split];
    let target = &data[split..];

    let mut delta = Vec::new();
    let _ = encoder::encode_all(
        &mut delta,
        source,
        target,
        CompressOptions {
            level,
            secondary: SecondaryCompression::None,
            ..Default::default()
        },
    );

    let _ = decoder::decode_all(source, &delta);
});
