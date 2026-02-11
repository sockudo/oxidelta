use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use oxidelta::compress::decoder;
use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::secondary::SecondaryCompression;
use oxidelta::hash::table::SmallTable;
use std::fs;
use std::path::Path;

fn gen_data(size: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    let mut out = Vec::with_capacity(size);
    for _ in 0..size {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
        out.push((s >> 33) as u8);
    }
    out
}

fn mutate(base: &[u8], stride: usize) -> Vec<u8> {
    let mut out = base.to_vec();
    for i in (0..out.len()).step_by(stride.max(1)) {
        out[i] = out[i].wrapping_add(1);
    }
    out
}

fn encode_delta(source: &[u8], target: &[u8], level: u32) -> Vec<u8> {
    let mut delta = Vec::new();
    encoder::encode_all(
        &mut delta,
        source,
        target,
        CompressOptions {
            level,
            secondary: SecondaryCompression::None,
            ..Default::default()
        },
    )
    .unwrap();
    delta
}

fn write_ratio_snapshot() {
    let source = gen_data(2 * 1024 * 1024, 123);
    let target = mutate(&source, 4096);
    let mut csv = String::from("level,delta_bytes,target_bytes,ratio\n");
    for level in 0u32..=9 {
        let delta = encode_delta(&source, &target, level);
        let ratio = delta.len() as f64 / target.len() as f64;
        csv.push_str(&format!(
            "{level},{},{},{}\n",
            delta.len(),
            target.len(),
            ratio
        ));
    }
    let out_dir = Path::new("target/criterion/custom_reports");
    let _ = fs::create_dir_all(out_dir);
    let _ = fs::write(out_dir.join("ratio_snapshot.csv"), csv);
}

fn write_compare_snapshot() {
    let source = gen_data(1024 * 1024, 8);
    let target = mutate(&source, 1024);
    let rust = encode_delta(&source, &target, 6);
    let c = xdelta3::encode(&target, &source).unwrap();
    let report = format!(
        "workload,source_bytes,target_bytes,rust_delta_bytes,xdelta3_delta_bytes\nencode_compare,{},{},{},{}\n",
        source.len(),
        target.len(),
        rust.len(),
        c.len()
    );
    let out_dir = Path::new("target/criterion/custom_reports");
    let _ = fs::create_dir_all(out_dir);
    let _ = fs::write(out_dir.join("xdelta_compare.csv"), report);
}

fn bench_encoding_speed(c: &mut Criterion) {
    let mut g = c.benchmark_group("encoding_speed_mb_s");
    for size in [64 * 1024usize, 1024 * 1024, 8 * 1024 * 1024] {
        let source = gen_data(size, 1);
        let target = mutate(&source, 1024);
        g.throughput(Throughput::Bytes(size as u64));
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let delta = encode_delta(black_box(&source), black_box(&target), 6);
                black_box(delta);
            });
        });
    }
    g.finish();
}

fn bench_decoding_speed(c: &mut Criterion) {
    let mut g = c.benchmark_group("decoding_speed_vs_delta");
    for size in [64 * 1024usize, 1024 * 1024, 8 * 1024 * 1024] {
        let source = gen_data(size, 2);
        let target = mutate(&source, 2048);
        let delta = encode_delta(&source, &target, 6);
        g.throughput(Throughput::Bytes(delta.len() as u64));
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let out = decoder::decode_all(black_box(&source), black_box(&delta)).unwrap();
                black_box(out);
            });
        });
    }
    g.finish();
}

fn bench_ratio_vs_level(c: &mut Criterion) {
    write_ratio_snapshot();
    let mut g = c.benchmark_group("compression_ratio_vs_level");
    let source = gen_data(2 * 1024 * 1024, 3);
    let target = mutate(&source, 4096);
    for level in 0u32..=9u32 {
        g.bench_with_input(BenchmarkId::from_parameter(level), &level, |b, level| {
            b.iter(|| {
                let delta = encode_delta(&source, &target, *level);
                let ratio = delta.len() as f64 / target.len() as f64;
                black_box(ratio);
            });
        });
    }
    g.finish();
}

fn bench_hash_table(c: &mut Criterion) {
    let mut g = c.benchmark_group("hash_table_performance");
    for slots in [1usize << 14, 1 << 16, 1 << 18] {
        g.bench_with_input(BenchmarkId::from_parameter(slots), &slots, |b, slots| {
            b.iter(|| {
                let mut table = SmallTable::new(*slots, *slots / 2);
                for i in 0..(*slots / 2) {
                    table.insert((i * 2654435761) as u64, i as u64);
                }
                black_box(table.lookup(0x1234_5678));
            });
        });
    }
    g.finish();
}

fn bench_xdelta_compare(c: &mut Criterion) {
    write_compare_snapshot();
    let mut g = c.benchmark_group("rust_vs_xdelta_encode");
    let source = gen_data(1024 * 1024, 8);
    let target = mutate(&source, 1024);

    g.bench_function("rust_encode", |b| {
        b.iter(|| {
            let d = encode_delta(black_box(&source), black_box(&target), 6);
            black_box(d);
        });
    });

    g.bench_function("xdelta3_encode", |b| {
        b.iter(|| {
            let d = xdelta3::encode(black_box(&target), black_box(&source)).unwrap();
            black_box(d);
        });
    });
    g.finish();
}

fn bench_real_world_scenarios(c: &mut Criterion) {
    let mut g = c.benchmark_group("real_world_scenarios");
    let scenarios = [
        ("software_update", 4 * 1024 * 1024usize, 1024usize),
        ("document_versioning", 512 * 1024usize, 256usize),
        ("database_snapshot", 8 * 1024 * 1024usize, 4096usize),
        ("large_video_like", 16 * 1024 * 1024usize, 8192usize),
        ("compressed_payload", 2 * 1024 * 1024usize, 16384usize),
    ];

    for (name, size, stride) in scenarios {
        let source = gen_data(size, size as u64);
        let target = mutate(&source, stride);
        g.throughput(Throughput::Bytes(size as u64));
        g.bench_function(name, |b| {
            b.iter(|| {
                let delta = encode_delta(&source, &target, 6);
                let out = decoder::decode_all(&source, &delta).unwrap();
                black_box(out);
            });
        });
    }
    g.finish();
}

fn bench_memory_proxy(c: &mut Criterion) {
    let mut g = c.benchmark_group("memory_proxy_vs_size");
    for size in [256 * 1024usize, 1024 * 1024, 4 * 1024 * 1024] {
        let source = gen_data(size, 10);
        let target = mutate(&source, 2048);
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let delta = encode_delta(&source, &target, 6);
                // Proxy for memory pressure: live bytes touched in workload.
                let working_set = source.len() + target.len() + delta.len();
                black_box(working_set);
            });
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    bench_encoding_speed,
    bench_decoding_speed,
    bench_ratio_vs_level,
    bench_memory_proxy,
    bench_hash_table,
    bench_xdelta_compare,
    bench_real_world_scenarios
);
criterion_main!(benches);
