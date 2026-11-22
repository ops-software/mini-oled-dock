#requires -Version 7
# fmt on the lines a pull request changes: rustfmt must not want to touch any added or modified line.
# A whole-tree report is printed as a notice and is not gated (the tree is not rustfmt-clean today).
param([string]$BaseRef = 'origin/main')          # pull request: origin/<base>; push to main: HEAD~1
$ErrorActionPreference = 'Continue'              # native stderr
$fmt = cargo fmt --check -- --color never 2>&1 | Out-String       # read-only; its exit code is ignored on purpose
$ErrorActionPreference = 'Stop'

# 1. changed (added or modified) line ranges per .rs file
$changed = @{}
$file = $null
foreach ($l in (git diff -U0 "$BaseRef...HEAD" -- '*.rs')) {
  if ($l -match '^\+\+\+ b/(.+)$') { $file = $Matches[1]; if (-not $changed.ContainsKey($file)) { $changed[$file] = @() } }
  elseif ($l -match '^\+\+\+ ') { $file = $null }           # +++ /dev/null: a deleted file
  elseif ($file -and $l -match '^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@') {
    $n = if ($Matches[2] -ne $null -and $Matches[2] -ne '') { [int]$Matches[2] } else { 1 }
    $start = [int]$Matches[1]
    if ($n -gt 0) { $changed[$file] += ,@($start, ($start + $n - 1)) }
  }
}
function Rel([string]$p) { $r = ($p -replace '\\', '/') -replace '^//\?/', ''; $root = ((Get-Location).Path -replace '\\', '/').TrimEnd('/') + '/'; return $r.Replace($root, '') }
function Test-Changed([string]$path, [int]$line) {
  $p = ($path -replace '\\', '/') -replace '^\./', ''
  foreach ($k in $changed.Keys) {
    if ($p -eq $k -or $p.EndsWith("/$k")) { foreach ($r in $changed[$k]) { if ($line -ge $r[0] -and $line -le $r[1]) { return $true } } }
  }
  return $false
}

# 2. rustfmt hunks: "Diff in <path>:<N>:" then lines starting with ' ', '-' or '+'.
#    Original line numbers advance on ' ' and '-' lines. A hunk touches a changed range if one of its '-' lines,
#    or for a pure-addition hunk the context line before the first '+', lies in it.
$hunks = 0; $files = @{}; $bad = New-Object System.Collections.Generic.List[string]
$path = $null; $cur = 0; $prev = 0; $hit = $false; $sawMinus = $false; $open = $false
function Close-Hunk { if ($script:open -and $script:hit) { $script:bad.Add($script:hitText) }; $script:open = $false }
$hitText = ''
foreach ($l in ($fmt -split "`r?`n")) {
  if ($l -match '^Diff in (.+?):(\d+):?$') {
    Close-Hunk
    $path = $Matches[1]; $cur = [int]$Matches[2]; $prev = 0; $hit = $false; $sawMinus = $false; $open = $true
    $hunks++; $files[$path] = $true
  }
  elseif ($open -and $l.Length -gt 0 -and $l[0] -eq ' ') { $prev = $cur; $cur++ }
  elseif ($open -and $l.Length -gt 0 -and $l[0] -eq '-') {
    $sawMinus = $true
    if (-not $hit -and (Test-Changed $path $cur)) { $hit = $true; $hitText = "$(Rel $path):$cur is not rustfmt-formatted (a line you changed)" }
    $cur++
  }
  elseif ($open -and $l.Length -gt 0 -and $l[0] -eq '+') {
    # pure addition: judged by the context line before the first '+' (only while no '-' line was seen)
    if (-not $sawMinus -and -not $hit -and $prev -gt 0 -and (Test-Changed $path $prev)) { $hit = $true; $hitText = "$(Rel $path):$prev is not rustfmt-formatted (a line you changed)" }
  }
}
Close-Hunk
Write-Host "whole tree: $hunks hunks in $($files.Count) files (not gated)"
if ($bad.Count -gt 0) { $bad | ForEach-Object { Write-Host $_ }; exit 1 }
exit 0
