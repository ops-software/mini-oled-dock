#requires -Version 7
# Refreshes docs/screenshots from the offscreen preview renders (the legacy, non-matrix run of dock.exe --preview).
# pwsh tools/screenshots.ps1   (run it before tagging a release, then commit the refreshed screenshots)
# Fails when a PNG the README names is not produced.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)
$names = [regex]::Matches((Get-Content README.md -Raw), 'docs/screenshots/([A-Za-z0-9_\-]+\.png)') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
if (-not $names) { throw 'README.md names no docs/screenshots/*.png' }
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('dock-shots-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
  $env:DOCK_NO_MANIFEST = '1'; $env:CARGO_TARGET_DIR = 'target-test'
  & cargo build --locked
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
  $p = Start-Process -FilePath 'target-test\debug\dock.exe' -ArgumentList '--preview', $tmp -Wait -PassThru -WindowStyle Hidden
  if ($p.ExitCode -ne 0) { throw "dock.exe --preview exited $($p.ExitCode)" }
  $missing = @($names | Where-Object { -not (Test-Path (Join-Path $tmp $_)) })
  if ($missing) { throw "the preview did not render: $($missing -join ', ')" }
  New-Item -ItemType Directory -Force docs/screenshots | Out-Null
  foreach ($n in $names) { Copy-Item (Join-Path $tmp $n) (Join-Path docs/screenshots $n) -Force }
  "refreshed $($names.Count) screenshots"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
