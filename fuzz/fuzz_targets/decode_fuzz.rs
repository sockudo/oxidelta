#![no_main]
use libfuzzer_sys::fuzz_target;
use oxidelta::vcdiff::decoder;

fuzz_target!(|data: &[u8]| {
    // Fuzz the decoder with arbitrary bytes.
    // The decoder must never panic — only return errors.
    let _ = decoder::decode_memory(data, &[]);

    // Also fuzz with a non-empty source.
    if data.len() >= 2 {
        let split = data.len() / 2;
        let (source, delta) = data.split_at(split);
        let _ = decoder::decode_memory(delta, source);
    }
});
