# PERFORMANCE

## Benchmark Summary

Latest local benchmark run:

- Timestamp: `2026-02-11 15:13:19 UTC`
- Host: `Linux x86_64` (`AMD Ryzen 9 7950X 16-Core Processor`)
- Command: `cargo bench --bench criterion_benchmarks -- --noplot --output-format bencher`

Values below are derived from Criterion `new/estimates.json` medians and `new/benchmark.json` throughput bytes.

### Encode throughput

| Input size | Throughput |
|---|---:|
| 64 KiB | ~779.894 MiB/s |
| 1 MiB | ~393.507 MiB/s |
| 8 MiB | ~476.015 MiB/s |

### Decode throughput (delta-byte normalized metric)

| Workload size | Throughput |
|---|---:|
| 64 KiB | ~116.537 MiB/s |
| 1 MiB | ~101.109 MiB/s |
| 8 MiB | ~85.070 MiB/s |

### Rust vs xdelta3 encode (sample workload)

| Implementation | Median time |
|---|---:|
| Oxidelta (Rust) | ~1.374587 ms |
| xdelta3 | ~1.580913 ms |

Observation: Oxidelta is ~13.05% faster on this benchmark workload.

Delta size comparison for the sample workload:

- Oxidelta: `7193` bytes
- xdelta3: `7189` bytes
- Difference: `+4 bytes`

### Real-world scenario throughput

| Scenario | Throughput |
|---|---:|
| software_update | ~605.055 MiB/s |
| document_versioning | ~575.095 MiB/s |
| database_snapshot | ~547.340 MiB/s |
| large_video_like | ~489.512 MiB/s |
| compressed_payload | ~760.643 MiB/s |

## Reproducing Benchmarks

```bash
cargo bench --bench criterion_benchmarks -- --noplot
```

Generated artifacts:

- `target/criterion/*`
- `target/criterion/custom_reports/ratio_snapshot.csv`
- `target/criterion/custom_reports/xdelta_compare.csv`

CI benchmark workflow:

- `.github/workflows/benchmarks.yml`

## Performance Comparison Guide

Use this process to compare Oxidelta with xdelta3 fairly:

1. Use identical source/target input sets.
2. Pin CPU frequency scaling / power plan where possible.
3. Run at least 10 samples per scenario.
4. Compare both time and resulting delta size.
5. Report median and p95 instead of only mean.

Recommended command pattern:

```bash
cargo bench --bench criterion_benchmarks -- --noplot
```

For xdelta3-side comparisons, use the existing benchmark integration in `examples/bench.rs`.

## Interpreting Results

- Encode throughput is currently strong on sampled workloads.
- Decode metric here is intentionally strict (delta-byte normalized), so it may appear lower than target-byte-based decode numbers.
- Compression ratio is close to xdelta3 on tested data; exact bit-identical output is not a stated goal.

## Roadmap for Additional Gains

- Broader cross-platform benchmarking (Linux/macOS/Windows, ARM64 and x86_64).
- RSS-based memory profiling in CI (not only proxy metrics).
- Dataset-specific profile tuning by workload class.
