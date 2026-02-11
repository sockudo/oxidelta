// Comprehensive benchmark: Rust xdelta vs C xdelta3 at matching compression levels.
//
// Tests multiple data sizes, similarity levels, and compression profiles.
// Uses C streaming API (xd3_config_stream + xd3_encode_stream) to set smatch_cfg
// for fair level-to-level comparison.
//
// Usage:
//   cargo run --release --example bench
//   cargo run --release --example bench -- --iters 20
//   cargo run --release --example bench -- --quick          (fewer sizes)
//   cargo run --release --example bench -- --similarity 0.95

use std::time::{Duration, Instant};

use oxidelta::compress::decoder;
use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::secondary::SecondaryCompression;

// ============================================================================
// C xdelta3 FFI — via the xdelta3 crate (dev-dependency)
// ============================================================================

/// Encode using C xdelta3 (flags=0 → XD3_SMATCH_DEFAULT ≈ level 6, no secondary).
fn c_encode(target: &[u8], source: &[u8]) -> Vec<u8> {
    xdelta3::encode(target, source).expect("C xd3 encode failed")
}

/// Decode using C xdelta3.
fn c_decode(delta: &[u8], source: &[u8], _expected_size: usize) -> Vec<u8> {
    xdelta3::decode(delta, source).expect("C xd3 decode failed")
}

// ============================================================================
// Benchmark runner
// ============================================================================

#[derive(Clone)]
struct BenchResult {
    encode_median: Duration,
    decode_median: Duration,
    delta_size: usize,
    data_size: usize,
}

impl BenchResult {
    fn encode_throughput_mib(&self) -> f64 {
        self.data_size as f64 / self.encode_median.as_secs_f64() / (1024.0 * 1024.0)
    }
    fn decode_throughput_mib(&self) -> f64 {
        self.data_size as f64 / self.decode_median.as_secs_f64() / (1024.0 * 1024.0)
    }
}

fn bench_rust(source: &[u8], target: &[u8], level: u32, iterations: usize) -> BenchResult {
    let opts = CompressOptions {
        level,
        checksum: false,
        secondary: SecondaryCompression::None,
        ..Default::default()
    };

    // Warmup + correctness check.
    let mut delta = Vec::new();
    encoder::encode_all(&mut delta, source, target, opts.clone()).unwrap();
    let decoded = decoder::decode_all(source, &delta).unwrap();
    assert_eq!(decoded, target, "Rust decode mismatch at level {level}");

    // Encode benchmark.
    let mut encode_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        delta.clear();
        let start = Instant::now();
        encoder::encode_all(&mut delta, source, target, opts.clone()).unwrap();
        encode_times.push(start.elapsed());
    }

    // Decode benchmark.
    let mut decode_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let _ = decoder::decode_all(source, &delta).unwrap();
        decode_times.push(start.elapsed());
    }

    BenchResult {
        encode_median: median(&mut encode_times),
        decode_median: median(&mut decode_times),
        delta_size: delta.len(),
        data_size: target.len(),
    }
}

fn bench_c(source: &[u8], target: &[u8], iterations: usize) -> BenchResult {
    // Warmup + correctness.
    let delta = c_encode(target, source);
    let decoded = c_decode(&delta, source, target.len());
    assert_eq!(decoded, target, "C decode mismatch");

    // Encode benchmark.
    let mut encode_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let _ = c_encode(target, source);
        encode_times.push(start.elapsed());
    }

    // Decode benchmark.
    let mut decode_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let _ = c_decode(&delta, source, target.len());
        decode_times.push(start.elapsed());
    }

    BenchResult {
        encode_median: median(&mut encode_times),
        decode_median: median(&mut decode_times),
        delta_size: delta.len(),
        data_size: target.len(),
    }
}

// ============================================================================
// Comparison and reporting
// ============================================================================

struct Comparison {
    size_label: String,
    similarity: f64,
    rust: BenchResult,
    c: BenchResult,
    enc_speedup: f64, // >1 means Rust faster
    dec_speedup: f64,
    ratio_diff_pct: f64, // positive means Rust delta is larger (worse)
}

fn compare(size_label: &str, similarity: f64, rust: BenchResult, c: BenchResult) -> Comparison {
    let enc_speedup = c.encode_median.as_secs_f64() / rust.encode_median.as_secs_f64();
    let dec_speedup = c.decode_median.as_secs_f64() / rust.decode_median.as_secs_f64();
    let ratio_diff_pct =
        (rust.delta_size as f64 - c.delta_size as f64) / c.delta_size as f64 * 100.0;
    Comparison {
        size_label: size_label.to_string(),
        similarity,
        rust,
        c,
        enc_speedup,
        dec_speedup,
        ratio_diff_pct,
    }
}

fn print_header() {
    println!(
        "{:<10} {:>5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8} {:>8} {:>8}",
        "Size",
        "Sim%",
        "R enc",
        "C enc",
        "R dec",
        "C dec",
        "R delta",
        "C delta",
        "Enc x",
        "Dec x",
        "Ratio%"
    );
    println!("{}", "-".repeat(118));
}

fn print_comparison(c: &Comparison) {
    let enc_marker = if c.enc_speedup >= 1.0 { "+" } else { "-" };
    let dec_marker = if c.dec_speedup >= 1.0 { "+" } else { "-" };
    let ratio_marker = if c.ratio_diff_pct <= 5.0 { " " } else { "!" };

    println!(
        "{:<10} {:>4.0}% {:>9.2?} {:>9.2?} {:>9.2?} {:>9.2?} {:>9.1}K {:>9.1}K {:>+7.2}x{} {:>+7.2}x{} {:>+6.1}%{}",
        c.size_label,
        c.similarity * 100.0,
        c.rust.encode_median,
        c.c.encode_median,
        c.rust.decode_median,
        c.c.decode_median,
        c.rust.delta_size as f64 / 1024.0,
        c.c.delta_size as f64 / 1024.0,
        c.enc_speedup,
        enc_marker,
        c.dec_speedup,
        dec_marker,
        c.ratio_diff_pct,
        ratio_marker,
    );
}

fn print_summary(comparisons: &[Comparison]) {
    println!();
    println!("=== SUMMARY ===");
    println!();

    let total = comparisons.len();
    let enc_wins = comparisons.iter().filter(|c| c.enc_speedup >= 1.0).count();
    let dec_wins = comparisons.iter().filter(|c| c.dec_speedup >= 1.0).count();
    let ratio_ok = comparisons
        .iter()
        .filter(|c| c.ratio_diff_pct.abs() <= 10.0)
        .count();

    let avg_enc_speedup: f64 =
        comparisons.iter().map(|c| c.enc_speedup).sum::<f64>() / total as f64;
    let avg_dec_speedup: f64 =
        comparisons.iter().map(|c| c.dec_speedup).sum::<f64>() / total as f64;
    let min_enc_speedup = comparisons
        .iter()
        .map(|c| c.enc_speedup)
        .fold(f64::INFINITY, f64::min);
    let max_enc_speedup = comparisons
        .iter()
        .map(|c| c.enc_speedup)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_dec_speedup = comparisons
        .iter()
        .map(|c| c.dec_speedup)
        .fold(f64::INFINITY, f64::min);
    let max_dec_speedup = comparisons
        .iter()
        .map(|c| c.dec_speedup)
        .fold(f64::NEG_INFINITY, f64::max);

    println!(
        "Encode: Rust faster in {enc_wins}/{total} cases (avg {avg_enc_speedup:.2}x, min {min_enc_speedup:.2}x, max {max_enc_speedup:.2}x)"
    );
    println!(
        "Decode: Rust faster in {dec_wins}/{total} cases (avg {avg_dec_speedup:.2}x, min {min_dec_speedup:.2}x, max {max_dec_speedup:.2}x)"
    );
    println!("Compression ratio: within 10% in {ratio_ok}/{total} cases");

    // Flag regressions.
    println!();
    let regressions: Vec<_> = comparisons
        .iter()
        .filter(|c| c.enc_speedup < 0.8 || c.dec_speedup < 0.8)
        .collect();

    if regressions.is_empty() {
        println!("RESULT: PASS - No significant regressions (all within 80% of C speed)");
    } else {
        println!(
            "RESULT: ATTENTION - {} cases where Rust is >20% slower than C:",
            regressions.len()
        );
        for r in &regressions {
            println!(
                "  {} sim={:.0}%: enc={:.2}x dec={:.2}x",
                r.size_label,
                r.similarity * 100.0,
                r.enc_speedup,
                r.dec_speedup
            );
        }
    }
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let iterations: usize = parse_arg(&args, "--iters").unwrap_or(10);
    let quick = args.iter().any(|a| a == "--quick");
    let single_sim: Option<f64> = parse_arg_f64(&args, "--similarity");

    let sizes: Vec<(usize, &str)> = if quick {
        vec![
            (64 * 1024, "64K"),
            (1024 * 1024, "1M"),
            (4 * 1024 * 1024, "4M"),
        ]
    } else {
        vec![
            (4 * 1024, "4K"),
            (16 * 1024, "16K"),
            (64 * 1024, "64K"),
            (256 * 1024, "256K"),
            (1024 * 1024, "1M"),
            (4 * 1024 * 1024, "4M"),
            (8 * 1024 * 1024, "8M"),
        ]
    };

    let similarities: Vec<f64> = if let Some(s) = single_sim {
        vec![s]
    } else if quick {
        vec![0.90, 0.95]
    } else {
        vec![0.80, 0.90, 0.95, 0.99]
    };

    println!("=== xdelta Rust vs C Benchmark ===");
    println!(
        "  Iterations: {iterations}, Sizes: {}, Similarities: {}",
        sizes.len(),
        similarities.len()
    );
    println!("  C baseline: xd3_encode_memory flags=0 (XD3_SMATCH_DEFAULT ≈ level 6)");
    println!("  Rust: compress module level 6, no secondary, no checksum");
    println!();

    // === Part 1: Rust L6 vs C default (same profile) across all sizes ===
    println!(
        "╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗"
    );
    println!(
        "║  Part 1: Rust L6 vs C Default — same compression profile across sizes                                             ║"
    );
    println!(
        "╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝"
    );
    println!();

    let mut all_comparisons = Vec::new();

    for &sim in &similarities {
        println!("--- Similarity: {:.0}% ---", sim * 100.0);
        print_header();

        for &(size, label) in &sizes {
            let source = generate_data(size, 42);
            let target = mutate_data(&source, sim, 123);

            let rust_result = bench_rust(&source, &target, 6, iterations);
            let c_result = bench_c(&source, &target, iterations);
            let cmp = compare(label, sim, rust_result, c_result);
            print_comparison(&cmp);
            all_comparisons.push(cmp);
        }
        println!();
    }

    print_summary(&all_comparisons);

    // === Part 2: All Rust levels at select sizes ===
    println!();
    println!(
        "╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗"
    );
    println!(
        "║  Part 2: All Rust compression levels (1-9) — throughput and ratio                                                  ║"
    );
    println!(
        "╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝"
    );
    println!();

    let profile_sizes: Vec<(usize, &str)> = if quick {
        vec![(256 * 1024, "256K"), (4 * 1024 * 1024, "4M")]
    } else {
        vec![
            (64 * 1024, "64K"),
            (256 * 1024, "256K"),
            (1024 * 1024, "1M"),
            (4 * 1024 * 1024, "4M"),
        ]
    };

    let sim = 0.90;
    println!("Similarity: {:.0}%", sim * 100.0);
    println!();

    for &(size, size_label) in &profile_sizes {
        let source = generate_data(size, 42);
        let target = mutate_data(&source, sim, 123);

        println!("  {size_label}:");
        println!(
            "  {:<8} {:>10} {:>10} {:>10} {:>10} {:>8}",
            "Level", "Encode", "Enc MiB/s", "Decode", "Dec MiB/s", "Delta"
        );
        println!("  {}", "-".repeat(66));

        // C baseline.
        let c_r = bench_c(&source, &target, iterations);
        println!(
            "  {:<8} {:>9.2?} {:>10.1} {:>9.2?} {:>10.1} {:>7.1}K",
            "C dflt",
            c_r.encode_median,
            c_r.encode_throughput_mib(),
            c_r.decode_median,
            c_r.decode_throughput_mib(),
            c_r.delta_size as f64 / 1024.0,
        );

        for level in 1..=9 {
            let r = bench_rust(&source, &target, level, iterations);
            let enc_vs_c = c_r.encode_median.as_secs_f64() / r.encode_median.as_secs_f64();
            let dec_vs_c = c_r.decode_median.as_secs_f64() / r.decode_median.as_secs_f64();
            println!(
                "  {:<8} {:>9.2?} {:>10.1} {:>9.2?} {:>10.1} {:>7.1}K  (enc {enc_vs_c:+.2}x, dec {dec_vs_c:+.2}x vs C)",
                format!("Rust L{level}"),
                r.encode_median,
                r.encode_throughput_mib(),
                r.decode_median,
                r.decode_throughput_mib(),
                r.delta_size as f64 / 1024.0,
            );
        }
        println!();
    }

    // === Part 3: Cross-compatibility verification ===
    println!(
        "╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗"
    );
    println!(
        "║  Part 3: Cross-compatibility                                                                                       ║"
    );
    println!(
        "╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝"
    );
    println!();

    for &(size, label) in &[
        (64 * 1024, "64K"),
        (1024 * 1024, "1M"),
        (4 * 1024 * 1024, "4M"),
    ] {
        let source = generate_data(size, 42);
        let target = mutate_data(&source, 0.90, 123);

        // Rust encode → C decode
        let mut rust_delta = Vec::new();
        encoder::encode_all(
            &mut rust_delta,
            &source,
            &target,
            CompressOptions {
                level: 6,
                checksum: false,
                secondary: SecondaryCompression::None,
                ..Default::default()
            },
        )
        .unwrap();

        let c_decoded = c_decode(&rust_delta, &source, target.len());
        let rust_to_c = if c_decoded == target { "OK" } else { "FAIL" };

        // C encode → Rust decode
        let c_delta = c_encode(&target, &source);
        let rust_decoded = decoder::decode_all(&source, &c_delta);
        let c_to_rust = match rust_decoded {
            Ok(ref d) if d == &target => "OK",
            Ok(_) => "MISMATCH",
            Err(_) => "FAIL",
        };

        println!(
            "  {label}: Rust->C: {rust_to_c}  C->Rust: {c_to_rust}  (Rust delta: {}B, C delta: {}B)",
            rust_delta.len(),
            c_delta.len()
        );
    }

    // === Part 4: Encode breakdown timing ===
    println!();
    println!(
        "╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗"
    );
    println!(
        "║  Part 4: Encode phase breakdown (1 iteration, level 6)                                                             ║"
    );
    println!(
        "╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝"
    );
    println!();

    for &(size, label) in &[
        (4 * 1024, "4K"),
        (16 * 1024, "16K"),
        (64 * 1024, "64K"),
        (256 * 1024, "256K"),
        (1024 * 1024, "1M"),
        (4 * 1024 * 1024, "4M"),
    ] {
        let source = generate_data(size, 42);
        let target = mutate_data(&source, 0.90, 123);

        let config = oxidelta::hash::config::config_for_level(6);
        let src: &[u8] = &source;

        // Time: engine creation
        let t0 = Instant::now();
        let mut engine = oxidelta::hash::matching::MatchEngine::new(
            config,
            src.len() as u64,
            target.len().max(64),
        );
        let t_engine_new = t0.elapsed();

        // Time: source indexing
        let t1 = Instant::now();
        engine.index_source(&src);
        let t_index = t1.elapsed();

        // Time: find matches
        let t2 = Instant::now();
        let raw_insts = engine.find_matches(&target, Some(&src));
        let t_match = t2.elapsed();

        // Time: pipeline optimize
        let t3 = Instant::now();
        let _optimized = oxidelta::compress::pipeline::optimize(&raw_insts, &target);
        let t_pipeline = t3.elapsed();

        // Time: full encode_all
        let t4 = Instant::now();
        let mut delta = Vec::new();
        encoder::encode_all(
            &mut delta,
            &source,
            &target,
            CompressOptions {
                level: 6,
                checksum: false,
                secondary: SecondaryCompression::None,
                ..Default::default()
            },
        )
        .unwrap();
        let t_total = t4.elapsed();

        // C total
        let t5 = Instant::now();
        let _ = c_encode(&target, &source);
        let t_c = t5.elapsed();

        println!("  {label}:");
        println!("    Engine::new():    {:>9.2?}", t_engine_new);
        println!("    index_source():   {:>9.2?}", t_index);
        println!("    find_matches():   {:>9.2?}", t_match);
        println!("    pipeline:         {:>9.2?}", t_pipeline);
        println!("    TOTAL encode_all: {:>9.2?} (Rust)", t_total);
        println!(
            "    TOTAL C encode:   {:>9.2?} (C)  [{:.2}x]",
            t_c,
            t_c.as_secs_f64() / t_total.as_secs_f64()
        );
        println!();
    }

    // === Part 5: Compression ratio analysis at 99% similarity ===
    println!();
    println!(
        "╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗"
    );
    println!(
        "║  Part 5: Compression ratio analysis — 99% similarity                                                               ║"
    );
    println!(
        "╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝"
    );
    println!();

    for &(size, label) in &[(64 * 1024, "64K"), (1024 * 1024, "1M")] {
        let source = generate_data(size, 42);
        let target = mutate_data(&source, 0.99, 123);

        // Rust encode
        let mut rust_delta = Vec::new();
        encoder::encode_all(
            &mut rust_delta,
            &source,
            &target,
            CompressOptions {
                level: 6,
                checksum: false,
                secondary: SecondaryCompression::None,
                ..Default::default()
            },
        )
        .unwrap();

        // C encode
        let c_delta = c_encode(&target, &source);

        // Analyze Rust delta via instruction iterator
        let mut cursor = std::io::Cursor::new(&rust_delta);
        let fh = oxidelta::vcdiff::header::FileHeader::decode(&mut cursor).unwrap();
        let _ = fh;
        let wh = oxidelta::vcdiff::header::WindowHeader::decode(&mut cursor)
            .unwrap()
            .unwrap();

        use std::io::Read;
        let mut data_sec = vec![0u8; wh.data_len as usize];
        cursor.read_exact(&mut data_sec).unwrap();
        let mut inst_sec = vec![0u8; wh.inst_len as usize];
        cursor.read_exact(&mut inst_sec).unwrap();
        let mut addr_sec = vec![0u8; wh.addr_len as usize];
        cursor.read_exact(&mut addr_sec).unwrap();

        let iter = oxidelta::vcdiff::decoder::InstructionIterator::new(
            &inst_sec,
            &addr_sec,
            wh.copy_window_len,
        );
        let mut n_add = 0u32;
        let mut n_copy = 0u32;
        let mut n_run = 0u32;
        let mut add_bytes = 0u64;
        let mut copy_bytes = 0u64;
        let mut run_bytes = 0u64;
        for inst in iter {
            match inst.unwrap() {
                oxidelta::vcdiff::code_table::Instruction::Add { len } => {
                    n_add += 1;
                    add_bytes += len as u64;
                }
                oxidelta::vcdiff::code_table::Instruction::Copy { len, .. } => {
                    n_copy += 1;
                    copy_bytes += len as u64;
                }
                oxidelta::vcdiff::code_table::Instruction::Run { len } => {
                    n_run += 1;
                    run_bytes += len as u64;
                }
            }
        }

        println!("  {label} (target={size}):");
        println!(
            "    Rust delta: {} bytes  (data={}, inst={}, addr={})",
            rust_delta.len(),
            wh.data_len,
            wh.inst_len,
            wh.addr_len
        );
        println!("    C    delta: {} bytes", c_delta.len());
        println!(
            "    Rust instructions: {} ADD ({} B), {} COPY ({} B), {} RUN ({} B)",
            n_add, add_bytes, n_copy, copy_bytes, n_run, run_bytes
        );
        println!(
            "    Overhead: Rust is {:.1}% larger",
            (rust_delta.len() as f64 - c_delta.len() as f64) / c_delta.len() as f64 * 100.0
        );

        // Analyze C delta sections
        let mut c_cursor = std::io::Cursor::new(&c_delta);
        let _ = oxidelta::vcdiff::header::FileHeader::decode(&mut c_cursor).unwrap();
        let c_wh = oxidelta::vcdiff::header::WindowHeader::decode(&mut c_cursor)
            .unwrap()
            .unwrap();
        let mut c_data_sec = vec![0u8; c_wh.data_len as usize];
        c_cursor.read_exact(&mut c_data_sec).unwrap();
        let mut c_inst_sec = vec![0u8; c_wh.inst_len as usize];
        c_cursor.read_exact(&mut c_inst_sec).unwrap();
        let mut c_addr_sec = vec![0u8; c_wh.addr_len as usize];
        c_cursor.read_exact(&mut c_addr_sec).unwrap();

        let c_iter = oxidelta::vcdiff::decoder::InstructionIterator::new(
            &c_inst_sec,
            &c_addr_sec,
            c_wh.copy_window_len,
        );
        let (mut cn_add, mut cn_copy, mut cn_run) = (0u32, 0u32, 0u32);
        let (mut cadd_b, mut ccopy_b, mut crun_b) = (0u64, 0u64, 0u64);
        for inst in c_iter {
            match inst.unwrap() {
                oxidelta::vcdiff::code_table::Instruction::Add { len } => {
                    cn_add += 1;
                    cadd_b += len as u64;
                }
                oxidelta::vcdiff::code_table::Instruction::Copy { len, .. } => {
                    cn_copy += 1;
                    ccopy_b += len as u64;
                }
                oxidelta::vcdiff::code_table::Instruction::Run { len } => {
                    cn_run += 1;
                    crun_b += len as u64;
                }
            }
        }
        println!(
            "    C    delta: {} bytes  (data={}, inst={}, addr={})",
            c_delta.len(),
            c_wh.data_len,
            c_wh.inst_len,
            c_wh.addr_len
        );
        println!(
            "    C    instructions: {} ADD ({} B), {} COPY ({} B), {} RUN ({} B)",
            cn_add, cadd_b, cn_copy, ccopy_b, cn_run, crun_b
        );
        println!();
    }

    println!("=== Done ===");
}

// ============================================================================
// Data generation helpers
// ============================================================================

fn generate_data(size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut data = Vec::with_capacity(size);
    for _ in 0..size {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        data.push((state >> 33) as u8);
    }
    data
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

fn median(times: &mut [Duration]) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn parse_arg(args: &[String], name: &str) -> Option<usize> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn parse_arg_f64(args: &[String], name: &str) -> Option<f64> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}
