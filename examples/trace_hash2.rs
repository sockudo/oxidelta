use oxidelta::hash::config;
use oxidelta::hash::rolling::{HashCfg, LargeHash};

fn generate_data(size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..size)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}
fn mutate_data(source: &[u8], similarity: f64, seed: u64) -> Vec<u8> {
    let mut target = source.to_vec();
    let mut state = seed;
    let change_count = ((1.0 - similarity) * source.len() as f64) as usize;
    for _ in 0..change_count {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let pos = (state >> 33) as usize % target.len();
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        target[pos] = (state >> 33) as u8;
    }
    target
}
fn main() {
    let source = generate_data(4096, 42);
    let target = mutate_data(&source, 0.95, 123);
    let cfg = config::config_for_level(6);
    let lh = LargeHash::new(cfg.large_look);
    let step = cfg.large_step;
    let look = cfg.large_look;
    let large_slots = (source.len() / step).max(8);
    let hcfg = HashCfg::new(large_slots);

    println!("Table size: {}", hcfg.size);

    // For each target position 215-225, check what source position the large table returns
    // by simulating the insert-then-lookup
    let mut table = vec![0u64; hcfg.size]; // 0 = empty

    // Index source in reverse (same as our code)
    let mut pos = source.len() - look;
    loop {
        let hash = lh.checksum(&source[pos..]);
        let bucket = hcfg.bucket(hash);
        table[bucket] = pos as u64 + 1; // +1 for HASH_CKOFFSET
        if pos < step {
            break;
        }
        pos -= step;
    }

    // Now for target positions 215-225, look up what source pos the table gives
    for tp in 215..226 {
        if tp + look > target.len() {
            break;
        }
        let hash = lh.checksum(&target[tp..]);
        let bucket = hcfg.bucket(hash);
        let stored = table[bucket];
        let src_pos = if stored > 0 { Some(stored - 1) } else { None };

        // Check if bytes actually match
        let actual_match = if let Some(sp) = src_pos {
            let sp = sp as usize;
            source[sp..sp + look] == target[tp..tp + look]
        } else {
            false
        };

        println!(
            "target[{}..{}]: bucket={}, stored_src_pos={:?}, actual_match={}",
            tp,
            tp + look,
            bucket,
            src_pos,
            actual_match
        );
    }
}
