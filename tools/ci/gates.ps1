#requires -Version 7
# Local gates as CI runs them (plans P1-workspace-v2). pwsh tools/ci/gates.ps1 [-Only lint,test,crates,size,ab-base,ab-head,imports] [-BaseRef origin/main] [-ImportsBase <file>]
param([string[]]$Only = @('lint', 'test'), [string]$BaseRef = 'origin/main', [string]$ImportsBase = '')
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force scratch | Out-Null                    # git-ignored (.gitignore: scratch/)
$T = @{ DOCK_NO_MANIFEST = '1'; CARGO_TARGET_DIR = 'target-test' }        # tests: asInvoker manifest, own target dir
$R = @{ DOCK_NO_MANIFEST = $null; CARGO_TARGET_DIR = 'target-release-check' } # release exe: admin manifest
$V = @{ DOCK_NO_MANIFEST = '1'; CARGO_TARGET_DIR = 'target-preview' }     # preview renders
function Set-EnvVar([string]$k, $v) { if ([string]::IsNullOrEmpty($v)) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue } else { Set-Item "Env:$k" $v } }   # an empty value is not an unset one
function Use-Env([hashtable]$e, [scriptblock]$b) {
  $old = @{}; foreach ($k in $e.Keys) { $old[$k] = [Environment]::GetEnvironmentVariable($k); Set-EnvVar $k $e[$k] }
  try { & $b } finally { foreach ($k in $old.Keys) { Set-EnvVar $k $old[$k] } }
}
function Invoke-Cargo { $ErrorActionPreference = 'Continue'; & cargo @args; if ($LASTEXITCODE -ne 0) { throw "cargo $($args -join ' ') failed ($LASTEXITCODE)" } }
function Invoke-Render([string]$label) {
  Use-Env $V { Invoke-Cargo build --release --locked }
  foreach ($lang in 'en', 'it') { foreach ($dpi in 96, 120, 144, 192) { foreach ($nt in $false, $true) {
    $dir = "scratch\ab-$label\$lang-$dpi" + $(if ($nt) { '-notext' } else { '' })
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue; New-Item -ItemType Directory -Force $dir | Out-Null
    Use-Env @{ DOCK_PREVIEW_DPI = "$dpi"; DOCK_PREVIEW_LANG = $lang; DOCK_PREVIEW_NOTEXT = $(if ($nt) { '1' } else { $null }) } {
      $p = Start-Process -FilePath 'target-preview\release\dock.exe' -ArgumentList '--preview', $dir -Wait -PassThru -WindowStyle Hidden
      if ($p.ExitCode -ne 0) { throw "preview $lang-$dpi exited $($p.ExitCode)" } } } } }
}
foreach ($step in $Only) { switch ($step) {
  'lint' { pwsh tools/ci/fmt-changed.ps1 -SelfTest; if ($LASTEXITCODE -ne 0) { throw 'fmt-changed self-test failed' }
           Use-Env $T { Invoke-Cargo clippy --locked --workspace --all-targets '--' -D warnings
                         Invoke-Cargo clippy --locked --all-targets --features owner-probe '--' -D warnings
                         Invoke-Cargo clippy --locked --all-targets --features wake-probe '--' -D warnings }
           pwsh tools/ci/fmt-changed.ps1 -BaseRef $BaseRef; if ($LASTEXITCODE -ne 0) { throw 'fmt-changed failed' } }
  'test' { Use-Env $T { Invoke-Cargo test --locked --workspace; Invoke-Cargo test --locked --features owner-probe; Invoke-Cargo test --locked --features wake-probe }
           Use-Env $T { pwsh tools/ci/ratchet.ps1 -BaseRef $BaseRef }; if ($LASTEXITCODE -ne 0) { throw 'ratchet failed' } }
  'crates' { Use-Env $T { $m = (cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json).packages.name
             foreach ($n in $m) { Invoke-Cargo check --locked -p $n --all-targets } } }
  'size' { Use-Env $R { Invoke-Cargo build --release --locked }
           $n = (Get-Item target-release-check\release\dock.exe).Length; "dock.exe: $n bytes"
           Use-Env ($T + @{ DOCK_EXE = 'target-release-check\release\dock.exe' }) { Invoke-Cargo test --locked --test budgets '--' --ignored exe_size_within_ledger } }
  'ab-base' { Invoke-Render 'base' }
  'ab-head' { Invoke-Render 'head'; $bad = 0
              Get-ChildItem scratch\ab-base -Recurse -Filter *.png | ForEach-Object {
                $o = $_.FullName.Replace('\ab-base\', '\ab-head\')
                if (-not (Test-Path $o) -or (Get-FileHash $_.FullName).Hash -ne (Get-FileHash $o).Hash) { "DIFF $($_.FullName)"; $bad++ } }
              if ($bad) { throw "$bad PNG differ" } else { 'ab: identical' } }
  'imports' { $vw = Join-Path ([Environment]::GetEnvironmentVariable('ProgramFiles(x86)')) 'Microsoft Visual Studio\Installer\vswhere.exe'
              $db = & $vw -latest -find 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
              $raw = & $db /nologo /dependents target-release-check\release\dock.exe; $lists = @{ load = @(); delay = @() }; $cur = $null
              foreach ($l in $raw) { $t = $l.Trim()                          # keep the DLL names of the two lists; drop the Summary
                if ($t -eq 'Image has the following dependencies:') { $cur = 'load' }
                elseif ($t -eq 'Image has the following delay load dependencies:') { $cur = 'delay' }
                elseif ($t -eq 'Summary') { $cur = $null }
                elseif ($cur -and $t -match '^\S+\.dll$') { $lists[$cur] += $t } }
              $norm = @('load: ' + (($lists.load | Sort-Object -CaseSensitive) -join ' '), 'delay: ' + (($lists.delay | Sort-Object -CaseSensitive) -join ' '))
              $f = "scratch\imports-$(git rev-parse --short HEAD).txt"; $norm | Set-Content $f; $norm
              if ($ImportsBase) { if (Compare-Object (Get-Content $ImportsBase) $norm -CaseSensitive) { throw "imports differ from $ImportsBase" } else { 'imports: identical' } }
              else { "imports in $f (on the head: -ImportsBase $f)" } }
} }
