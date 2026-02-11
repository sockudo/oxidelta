#![no_main]
use libfuzzer_sys::fuzz_target;
use oxidelta::vcdiff::{
    decoder,
    encoder::{SourceWindow, StreamEncoder, WindowEncoder},
};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }

    // Use first byte as control flags.
    let flags = data[0];
    let payload = &data[1..];
    let use_source = flags & 1 != 0;

    // Split payload into "source" and "target".
    let split = payload.len() / 2;
    let (source_data, target_data) = if use_source && split > 0 {
        (&payload[..split], &payload[split..])
    } else {
        (&[] as &[u8], payload)
    };

    if target_data.is_empty() {
        return;
    }

    let src_win = if source_data.is_empty() {
        None
    } else {
        Some(SourceWindow {
            len: source_data.len() as u64,
            offset: 0,
        })
    };

    let mut we = WindowEncoder::new(src_win, true);

    // Simple strategy: ADD the entire target.
    we.add(target_data);

    let mut delta = Vec::new();
    let mut enc = StreamEncoder::new(&mut delta, true);
    enc.write_window(we, Some(target_data)).unwrap();
    let _ = enc.finish().unwrap();

    // Decode and verify roundtrip.
    let decoded = decoder::decode_memory(&delta, source_data).unwrap();
    assert_eq!(decoded, target_data);
});
