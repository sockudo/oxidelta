use oxidelta::compress::decoder;
use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::secondary::SecondaryCompression;
use proptest::prelude::*;

fn encode(source: &[u8], target: &[u8], level: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encoder::encode_all(
        &mut out,
        source,
        target,
        CompressOptions {
            level,
            secondary: SecondaryCompression::None,
            ..Default::default()
        },
    )
    .unwrap();
    out
}

proptest! {
    #[test]
    fn prop_encode_decode_roundtrip(
        source in proptest::collection::vec(any::<u8>(), 0..4096),
        target in proptest::collection::vec(any::<u8>(), 0..4096),
        level in 0u32..=9u32
    ) {
        let delta = encode(&source, &target, level);
        let decoded = decoder::decode_all(&source, &delta).unwrap();
        prop_assert_eq!(decoded, target);
    }

    #[test]
    fn prop_identical_data_is_highly_compressible(
        source in proptest::collection::vec(any::<u8>(), 32..8192),
        level in 1u32..=9u32
    ) {
        let target = source.clone();
        let delta = encode(&source, &target, level);
        prop_assert!(delta.len() < target.len(), "delta={} target={}", delta.len(), target.len());
    }

    #[test]
    fn prop_small_mutation_keeps_delta_bounded(
        source in proptest::collection::vec(any::<u8>(), 256..8192),
        level in 1u32..=9u32
    ) {
        let mut target = source.clone();
        let len = target.len();
        for i in (0..len).step_by((len / 32).max(1)) {
            target[i] = target[i].wrapping_add(1);
        }
        let delta = encode(&source, &target, level);
        // Small inputs can exceed target size due VCDIFF framing overhead.
        // Keep this as a bounded-growth invariant rather than strict shrink.
        prop_assert!(
            delta.len() <= target.len() + 512,
            "delta={} target={}",
            delta.len(),
            target.len()
        );
    }
}

#[test]
#[ignore = "performance properties are workload and machine dependent"]
fn perf_property_decode_not_pathological() {
    use std::time::Instant;
    let make = |n: usize| -> Vec<u8> { (0..n).map(|i| (i % 251) as u8).collect() };
    let source = make(4 * 1024 * 1024);
    let mut target = source.clone();
    for i in (0..target.len()).step_by(4096) {
        target[i] = target[i].wrapping_add(3);
    }

    let delta = encode(&source, &target, 6);
    let t0 = Instant::now();
    let decoded = decoder::decode_all(&source, &delta).unwrap();
    let dt = t0.elapsed();
    assert_eq!(decoded, target);
    assert!(dt.as_secs_f64() < 20.0, "decode took {:?}", dt);
}
