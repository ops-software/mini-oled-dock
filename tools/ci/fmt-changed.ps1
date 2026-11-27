#requires -Version 7
# fmt on the lines a pull request writes: rustfmt must not want to touch any line the change wrote.
# A '+' line whose exact text the same diff also removes (a line moved between files, or inside one) is not judged:
# a move keeps its text byte for byte (plans P1-workspace-v2, rule 4), so rustfmt's view of it is older than the change.
# A whole-tree report is printed as a notice and is not gated (the tree is not rustfmt-clean today).
param([string]$BaseRef = 'origin/main', [switch]$SelfTest)   # pull request: origin/<base>; push to main: HEAD~1
$ErrorActionPreference = 'Stop'

function Rel([string]$p) { $r = ($p -replace '\\', '/') -replace '^//\?/', ''; $root = ((Get-Location).Path -replace '\\', '/').TrimEnd('/') + '/'; return $r.Replace($root, '') }

# 1. Written lines: the '+' lines of `git diff -U0`, per file and new line number, minus the moved ones.
function Get-WrittenLines([string[]]$diff) {
  $plus = [System.Collections.Generic.List[object]]::new()
  $minus = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
  $file = $null; $ln = 0; $inHunk = $false
  foreach ($l in $diff) {
    if ($l.StartsWith('diff --git ')) { $file = $null; $inHunk = $false; continue }
    if ($l -match '^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@') { $ln = [int]$Matches[1]; $inHunk = $true; continue }
    if (-not $inHunk) {
      if ($l -match '^\+\+\+ b/(.+)$') { $file = $Matches[1] } elseif ($l.StartsWith('+++ ')) { $file = $null }   # /dev/null: deleted
      continue
    }
    if ($l.StartsWith('+')) { if ($file) { $plus.Add(@($file, $ln, $l.Substring(1))) }; $ln++ }
    elseif ($l.StartsWith('-')) { [void]$minus.Add($l.Substring(1)) }
  }
  $written = @{}
  foreach ($p in $plus) {
    if ($minus.Contains($p[2])) { continue }                       # moved, not written
    if (-not $written.ContainsKey($p[0])) { $written[$p[0]] = [System.Collections.Generic.HashSet[int]]::new() }
    [void]$written[$p[0]].Add([int]$p[1])
  }
  return $written
}

function Test-Written($written, [string]$path, [int]$line) {
  $p = (($path -replace '\\', '/') -replace '^//\?/', '') -replace '^\./', ''
  foreach ($k in $written.Keys) { if (($p -eq $k -or $p.EndsWith("/$k")) -and $written[$k].Contains($line)) { return $true } }
  return $false
}

# 2. rustfmt hunks: "Diff in <path>:<N>:" then lines starting with ' ', '-' or '+'. Original line numbers advance on
#    ' ' and '-' lines. A hunk is the change's if one of its '-' lines, or for a pure-addition hunk the context line
#    before the first '+', is a written line.
function Get-BadHunks([string]$fmt, $written) {
  $bad = [System.Collections.Generic.List[string]]::new()
  $path = $null; $cur = 0; $prev = 0; $hit = $null; $sawMinus = $false; $open = $false
  foreach ($l in ($fmt -split "`r?`n")) {
    if ($l -match '^Diff in (.+?):(\d+):?$') {
      if ($open -and $hit) { $bad.Add($hit) }
      $path = $Matches[1]; $cur = [int]$Matches[2]; $prev = 0; $hit = $null; $sawMinus = $false; $open = $true
    } elseif ($open -and $l.Length -gt 0) {
      $c = $l.Substring(0, 1)
      if ($c -ceq ' ') { $prev = $cur; $cur++ }
      elseif ($c -ceq '-') {
        $sawMinus = $true
        if (-not $hit -and (Test-Written $written $path $cur)) { $hit = "$(Rel $path):$cur is not rustfmt-formatted (a line you wrote)" }
        $cur++
      } elseif ($c -ceq '+') {
        if (-not $sawMinus -and -not $hit -and $prev -gt 0 -and (Test-Written $written $path $prev)) { $hit = "$(Rel $path):$prev is not rustfmt-formatted (a line you wrote)" }
      }
    }
  }
  if ($open -and $hit) { $bad.Add($hit) }
  return ,$bad
}

if ($SelfTest) {
  # One file loses two lines, another gains one of them verbatim (moved), one edited copy and one new line.
  $diff = @(
    'diff --git a/src/old.rs b/src/old.rs', 'index 1111111..2222222 100644', '--- a/src/old.rs', '+++ b/src/old.rs',
    '@@ -3,2 +2,0 @@', '-fn moved( a: u8 ) {}', '-fn edited() {}',
    'diff --git a/crates/x/src/new.rs b/crates/x/src/new.rs', 'index 3333333..4444444 100644',
    '--- a/crates/x/src/new.rs', '+++ b/crates/x/src/new.rs',
    '@@ -1 +1,3 @@', '-//! stub', '+fn moved( a: u8 ) {}', '+fn edited( ) {}', '+fn added( ) {}'
  )
  $w = Get-WrittenLines $diff
  $fmt = "Diff in \\?\C:\r\crates\x\src\new.rs:1:`n-fn moved( a: u8 ) {}`n+fn moved(a: u8) {}`n" +
         "Diff in \\?\C:\r\crates\x\src\new.rs:2:`n-fn edited( ) {}`n+fn edited() {}`n" +
         "Diff in \\?\C:\r\src\other.rs:9:`n-fn  old() {}`n+fn old() {}`n"
  $bad = Get-BadHunks $fmt $w
  $ok = -not (Test-Written $w 'crates/x/src/new.rs' 1) -and (Test-Written $w 'crates/x/src/new.rs' 2) -and
        (Test-Written $w 'crates/x/src/new.rs' 3) -and $bad.Count -eq 1 -and $bad[0] -like '*new.rs:2 is not*'
  if ($ok) { Write-Host 'fmt-changed self-test: ok'; exit 0 }
  Write-Host "fmt-changed self-test: FAILED ($($bad -join '; '))"; exit 1
}

$ErrorActionPreference = 'Continue'                                # native stderr
$fmt = cargo fmt --all --check -- --color never 2>&1 | Out-String  # read-only; its exit code is ignored on purpose
$diff = @(git diff -U0 "$BaseRef...HEAD" -- '*.rs')
$ErrorActionPreference = 'Stop'
$bad = Get-BadHunks $fmt (Get-WrittenLines $diff)
Write-Host "whole tree: $(([regex]::Matches($fmt, '(?m)^Diff in ')).Count) hunks (not gated)"
if ($bad.Count -gt 0) { $bad | ForEach-Object { Write-Host $_ }; exit 1 }
exit 0
