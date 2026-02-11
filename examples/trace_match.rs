use oxidelta::hash::config;
use oxidelta::hash::matching::MatchEngine;

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
    let src: &[u8] = &source;

    // Check what source positions are indexed
    let cfg = config::config_for_level(6);
    println!(
        "large_look={}, large_step={}",
        cfg.large_look, cfg.large_step
    );

    // Check which positions near 216-218 are indexed
    let step = cfg.large_step; // 3
    let look = cfg.large_look; // 9
    let start = 4096 - look; // 4087
    let mut indexed = Vec::new();
    let mut pos = start;
    loop {
        indexed.push(pos);
        if pos < step {
            break;
        }
        pos -= step;
    }
    // Check near 216
    for p in &indexed {
        if *p >= 210 && *p <= 225 {
            println!(
                "Indexed source pos {}: source[{}..{}] = {:?}",
                p,
                p,
                p + look,
                &source[*p..*p + look]
            );
        }
    }
    // Check target near 216
    for tp in 215..220 {
        println!(
            "Target[{}..{}] = {:?}",
            tp,
            tp + look,
            &target[tp..tp + look]
        );
        // Check if target bytes match source at same position
        let matching = (0..look)
            .filter(|&i| target[tp + i] == source[tp + i])
            .count();
        println!("  {}/{} bytes match source at same pos", matching, look);
    }

    // Run the match engine and find matches
    let mut engine = MatchEngine::new(cfg, src.len() as u64, target.len().max(64));
    engine.index_source(&src);
    let instructions = engine.find_matches(&target, Some(&src));

    // Print instructions near target pos 215
    let mut pos = 0usize;
    for inst in &instructions {
        let len = match inst {
            oxidelta::vcdiff::code_table::Instruction::Add { len } => *len as usize,
            oxidelta::vcdiff::code_table::Instruction::Copy { len, .. } => *len as usize,
            oxidelta::vcdiff::code_table::Instruction::Run { len } => *len as usize,
        };
        if pos + len > 200 && pos < 280 {
            println!("  pos={}: {:?}", pos, inst);
        }
        pos += len;
    }
}
