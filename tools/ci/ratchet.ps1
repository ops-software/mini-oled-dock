#requires -Version 7
# Test-count ratchet: the number of tests (ignored ones included) may not fall below tests/ratchet.txt,
# and the floor itself may not be lowered against the base ref.
param([string]$BaseRef = $env:RATCHET_BASE)         # e.g. origin/main on a pull request
$ErrorActionPreference = 'Stop'
$env:DOCK_NO_MANIFEST = '1'; if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'target-test' }
$ErrorActionPreference = 'Continue'      # cargo writes its progress to stderr
$out = cargo test --locked --workspace -- --list 2>&1 | Out-String; $code = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
if ($code -ne 0) { throw "ratchet: cargo test --list failed ($code)`n$out" }
$total = 0; foreach ($m in [regex]::Matches($out, '(?m)^(\d+) tests?, \d+ benchmarks?')) { $total += [int]$m.Groups[1].Value }
if ($total -eq 0) { throw 'ratchet: no test summary lines found (cargo output changed?)' }
$floor = [int](Get-Content tests/ratchet.txt -TotalCount 1)
Write-Host "tests: $total, floor: $floor"
if ($total -lt $floor) { Write-Error "ratchet: $total tests < floor ${floor}: a test was deleted"; exit 1 }
if ($BaseRef) {
  $base = 0; $t = git show "${BaseRef}:tests/ratchet.txt" 2>$null; if ($LASTEXITCODE -eq 0 -and $t) { $base = [int]$t }
  if ($floor -lt $base) { Write-Error "ratchet: floor lowered ($base -> $floor)"; exit 1 }
}
if ($total -gt $floor) { Write-Host "::notice::tests rose to $total; raise tests/ratchet.txt in this PR" }
