#requires -Version 5.1
<#
.SYNOPSIS
  Writes a corpus of shell-made shortcuts for the Rust .lnk parser check (wsD D7, live check L6).
.DESCRIPTION
  Creates <Out>\generated with -Count .lnk files written by WScript.Shell (the shell's own link
  writer): targets in System32, in Program Files and in a per-user folder (<Out>\targets, small
  placeholder files), targets written as %SystemRoot% and %ProgramFiles% paths, links with
  arguments, links with a working folder, Unicode link names and a target path of 240 characters.
  It writes only under -Out; run it unelevated. Then, at the repository root:
    $env:DOCK_NO_MANIFEST='1'; $env:CARGO_TARGET_DIR='target-test'; $env:DOCK_LNK_CORPUS='<Out>'
    cargo test --locked -- --ignored --nocapture lnk_matches_ishelllink
  Every sub-folder of <Out> holding .lnk files is compared on its own (L6 adds "common" and
  "per-user" next to "generated").
#>
param(
  [Parameter(Mandatory = $true)][string]$Out,
  [int]$Count = 200
)
$ErrorActionPreference = 'Stop'
$gen = Join-Path $Out 'generated'
$targets = Join-Path $Out 'targets'
New-Item -ItemType Directory -Force -Path $gen, $targets | Out-Null

# Per-user targets: placeholder files (the parser reads the link, never its target).
$perUser = @(1..5 | ForEach-Object {
    $p = Join-Path $targets "tool$_.exe"
    Set-Content -LiteralPath $p -Value 'MZ' -NoNewline
    $p
  })

# A target path of exactly 240 characters.
$dir = $targets
while ($dir.Length -lt 200) { $dir = Join-Path $dir ('d' * 20) }
if ($dir.Length -gt 230) { throw "-Out is too long for a 240-character target: $Out" }
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$long = Join-Path $dir (('n' * (240 - $dir.Length - 1 - 4)) + '.exe')
Set-Content -LiteralPath $long -Value 'MZ' -NoNewline

$sysRoot = $env:SystemRoot
$progFiles = $env:ProgramFiles
$sys = @(Get-ChildItem -LiteralPath (Join-Path $sysRoot 'System32') -Filter *.exe -File |
    Select-Object -First 40 | ForEach-Object { $_.FullName })
$pf = @(Get-ChildItem -LiteralPath $progFiles -Filter *.exe -File -Recurse -Depth 3 -ErrorAction SilentlyContinue |
    Select-Object -First 40 | ForEach-Object { $_.FullName })
if ($pf.Count -eq 0) { $pf = $sys }
$envSys = @($sys | ForEach-Object { '%SystemRoot%' + $_.Substring($sysRoot.Length) })
$envPf = @($pf | Where-Object { $_.StartsWith($progFiles, 'OrdinalIgnoreCase') } |
    ForEach-Object { '%ProgramFiles%' + $_.Substring($progFiles.Length) })
if ($envPf.Count -eq 0) { $envPf = $envSys }

# Unicode names, written with char codes so this file stays ASCII.
$uni = @(
  "Citt$([char]0xE0)",
  "$([char]0x65E5)$([char]0x672C)",
  "Gr$([char]0xFC)$([char]0xDF)e",
  "$([char]0x0416)$([char]0x0443)$([char]0x043A)",
  "Caf$([char]0xE9) $([char]0x2603)"
)

$ws = New-Object -ComObject WScript.Shell
$made = 0
for ($i = 0; $i -lt $Count; $i++) {
  $kind = $i % 8
  $n = [int][math]::Floor($i / 8)
  $linkArgs = $null; $work = $null
  switch ($kind) {
    0 { $target = $sys[$n % $sys.Count]; $tag = 'system32' }
    1 { $target = $pf[$n % $pf.Count]; $tag = 'programfiles' }
    2 { $target = $perUser[$n % $perUser.Count]; $tag = 'peruser' }
    3 { $target = $envSys[$n % $envSys.Count]; $tag = 'env-systemroot' }
    4 { $target = $envPf[$n % $envPf.Count]; $tag = 'env-programfiles' }
    5 { $target = $sys[$n % $sys.Count]; $linkArgs = "--flag $i"; $tag = 'arguments' }
    6 { $target = $pf[$n % $pf.Count]; $work = Split-Path -Parent $target; $tag = 'workdir' }
    7 { $target = $long; $tag = 'long' }
  }
  $name = '{0:D3} {1}' -f $i, $tag
  $path = Join-Path $gen "$name.lnk"
  $sc = $ws.CreateShortcut($path)
  $sc.TargetPath = $target
  if ($linkArgs) { $sc.Arguments = $linkArgs }
  if ($work) { $sc.WorkingDirectory = $work }
  $sc.Save()
  # WScript.Shell saves under ANSI names only: a Unicode name is given by a rename.
  if ($i % 5 -eq 0) { Rename-Item -LiteralPath $path -NewName ($uni[[int]($i / 5) % $uni.Count] + " $name.lnk") }
  $made++
}
Write-Host "make-corpus: $made shortcuts in $gen"
