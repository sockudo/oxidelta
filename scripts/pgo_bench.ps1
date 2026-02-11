param(
    [string]$Profdata = 'target/pgo-data/merged.profdata',
    [switch]$Quick,
    [int]$Iters = 8
)

$ErrorActionPreference = 'Stop'

$repo = (Get-Location).Path
$profAbs = Join-Path $repo $Profdata
if (!(Test-Path $profAbs)) {
    throw "Profile data not found: $profAbs"
}

$env:RUSTFLAGS = "-Cprofile-use=$profAbs -Cllvm-args=-pgo-warn-missing-function"

Write-Host "[PGO] Building with profile use..."
cargo build --profile pgo-use --example bench

$bench = Join-Path $repo 'target/pgo-use/examples/bench.exe'
$args = @('--iters', $Iters)
if ($Quick) {
    $args = @('--quick') + $args
}

Write-Host "[PGO] Running benchmark: $($args -join ' ')"
& $bench @args
