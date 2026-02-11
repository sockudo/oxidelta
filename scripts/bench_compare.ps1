param(
    [string]$OutputDir = "target/bench-reports",
    [string]$BenchName = "criterion_benchmarks"
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$outFile = Join-Path $OutputDir "bench-$timestamp.txt"

Write-Host "Running criterion benchmarks..."
cargo bench --bench $BenchName -- --output-format bencher | Tee-Object -FilePath $outFile

Write-Host "Benchmark report saved to $outFile"
Write-Host "Tip: commit this file or upload as CI artifact to track regressions."
