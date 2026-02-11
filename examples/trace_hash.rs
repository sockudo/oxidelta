use oxidelta::hash::config;
use oxidelta::hash::rolling::LargeHash;

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

    // Compute hash for source[217..226] and target[217..226]
    let src_hash = lh.checksum(&source[217..]);
    let tgt_hash = lh.checksum(&target[217..]);
    println!("source[217..226] hash: {}", src_hash);
    println!("target[217..226] hash: {}", tgt_hash);
    println!("Match: {}", src_hash == tgt_hash);

    // Compute hash for all indexed source positions, find collisions with pos 217
    let look = cfg.large_look;
    let step = cfg.large_step;
    let mut pos = source.len() - look;
    let hash_217 = lh.checksum(&source[217..]);

    // Build the hash table manually to check
    use oxidelta::hash::rolling::HashCfg;
    let large_slots = (source.len() / step).max(8);
    let hcfg = HashCfg::new(large_slots);
    let bucket_217 = hcfg.bucket(hash_217);
    println!(
        "\nBucket for source[217] hash: {} (table size {})",
        bucket_217, hcfg.size
    );

    // Find all source positions that map to the same bucket
    let mut collisions = Vec::new();
    loop {
        let h = lh.checksum(&source[pos..]);
        let b = hcfg.bucket(h);
        if b == bucket_217 {
            collisions.push(pos);
        }
        if pos < step {
            break;
        }
        pos -= step;
    }
    println!(
        "Source positions mapping to bucket {}: {:?}",
        bucket_217, collisions
    );
    println!("(indexed in reverse, last write = first in list wins)");

    // Check: since we index in reverse (largest pos first, smallest last),
    // the LAST position written to this bucket should be the smallest pos
    // (earliest position wins). If 217 is overwritten by an earlier position,
    // the lookup returns the wrong source position.
    if let Some(&winner) = collisions.last() {
        println!("Winner (last written, earliest pos): {}", winner);
        if winner != 217 {
            println!(
                "COLLISION! Position 217 was overwritten by position {}",
                winner
            );
            // Check if target[217] still matches via that other position + extension
            println!(
                "source[{}..{}] = {:?}",
                winner,
                winner + look,
                &source[winner..winner + look]
            );
            println!("target[217..226] = {:?}", &target[217..226]);
        }
    }
}
