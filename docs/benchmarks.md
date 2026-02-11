# Benchmark Results and Analysis

Date: 2026-02-11  
Host: Linux x86_64 (`AMD Ryzen 9 7950X 16-Core Processor`)  
Command: `cargo bench --bench criterion_benchmarks -- --noplot --output-format bencher`

## Scope

The benchmark suite in `benches/criterion_benchmarks.rs` covers:

- Encoding speed vs file size
- Decoding speed vs delta size
- Compression ratio vs level
- Memory-pressure proxy vs file size
- Hash table performance
- Oxidelta vs `xdelta3` comparison
- Real-world workload scenarios

## Key Results

### Encoding speed (MiB/s)

| Input size | Throughput |
|---|---:|
| 64 KiB | ~779.894 MiB/s |
| 1 MiB | ~393.507 MiB/s |
| 8 MiB | ~476.015 MiB/s |

Observation: encoding target (>=100 MB/s) is met on tested workloads.

### Decoding speed (delta-throughput MiB/s)

| Workload size | Throughput (delta bytes/s) |
|---|---:|
| 64 KiB | ~116.537 MiB/s |
| 1 MiB | ~101.109 MiB/s |
| 8 MiB | ~85.070 MiB/s |

Observation: decode benchmark is intentionally normalized by *delta size* (`decoding_speed_vs_delta`), so this is stricter than target-bytes/s. On this run it still does not meet the >=200 MB/s target under this metric.

### Oxidelta vs xdelta3 encode (1 MiB workload)

| Implementation | Median time |
|---|---:|
| Oxidelta (Rust) | ~1.374587 ms |
| xdelta3 | ~1.580913 ms |

Observation: Oxidelta encode is ~13.05% faster on this workload.

### Real-world scenario throughput (MiB/s)

| Scenario | Throughput |
|---|---:|
| software_update | ~605.055 |
| document_versioning | ~575.095 |
| database_snapshot | ~547.340 |
| large_video_like | ~489.512 |
| compressed_payload | ~760.643 |

### Compression ratio vs level

Generated snapshot: `target/criterion/custom_reports/ratio_snapshot.csv`

From the latest snapshot:

- Level 0 ratio: `1.0000167` (store-only behavior)
- Levels 1..9 ratio: `0.0017219` on the benchmark’s highly-similar workload

### Delta size comparison vs xdelta3

Generated snapshot: `target/criterion/custom_reports/xdelta_compare.csv`

- Oxidelta delta size: `7193`
- xdelta3 delta size: `7189`
- Difference: `+4 bytes` (very close; compatibility target effectively met for this workload)

## Targets vs Current

| Goal | Status |
|---|---|
| Encoding >=100 MB/s | Met |
| Decoding >=200 MB/s | Not met under current delta-throughput metric |
| Memory <= xdelta | Partially covered (proxy benchmark present; no RSS profiler yet) |
| Compression ratio identical to xdelta | Near-identical on sampled workload (+4 bytes) |

## CI and Regression Tracking

- Benchmark workflow: `.github/workflows/benchmarks.yml`
- Coverage + test workflow: `.github/workflows/ci.yml`
- Local benchmark helper: `scripts/bench_compare.ps1`
- Criterion artifacts are uploaded in CI for PR review and regression analysis.
