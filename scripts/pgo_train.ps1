param(
    [string]$ProfileDir = 'target/pgo-data',
    [int]$QuickIters = 4,
    [int]$FullIters = 2
)

$ErrorActionPreference = 'Stop'

$repo = (Get-Location).Path
$profileAbs = Join-Path $repo $ProfileDir

if (Test-Path $profileAbs) {
    Remove-Item -Recurse -Force $profileAbs
}
New-Item -ItemType Directory -Path $profileAbs | Out-Null

$env:RUSTFLAGS = "-Cprofile-generate=$profileAbs"

Write-Host "[PGO] Building instrumented benchmark binary..."
cargo build --profile pgo-generate --example bench

$bench = Join-Path $repo 'target/pgo-generate/examples/bench.exe'

Write-Host "[PGO] Training on quick workload..."
& $bench --quick --iters $QuickIters

Write-Host "[PGO] Training on full workload..."
& $bench --iters $FullIters

$llvmProfdata = Join-Path $env:USERPROFILE '.rustup/toolchains/stable-x86_64-pc-windows-msvc/lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-profdata.exe'
if (!(Test-Path $llvmProfdata)) {
    throw "llvm-profdata not found at $llvmProfdata (install with: rustup component add llvm-tools)"
}

$profraw = Get-ChildItem $profileAbs -Filter '*.profraw'
if ($profraw.Count -eq 0) {
    throw "No .profraw files found in $profileAbs"
}

$merged = Join-Path $profileAbs 'merged.profdata'
Write-Host "[PGO] Merging profiles to $merged"
& $llvmProfdata merge -o $merged "$profileAbs/*.profraw"

Write-Host "[PGO] Done. Merged profile: $merged"
