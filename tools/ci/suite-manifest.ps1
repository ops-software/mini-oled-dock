#requires -Version 7
# The release's suite.json (plan P2-plumbing F-P6), written with string formatting so that its text equals the dock's
# encoder (encode_manifest) byte for byte: UTF-8 without BOM, LF line ends and a final LF, two-space indent, one
# component per line in the dock's component order (ComponentId::ALL), keys in F-P6's order, "file" with '/',
# SHA-256 as 64 lower-case hex digits, sizes in decimal. Never ConvertTo-Json: its spacing and escapes are its own.
#   pwsh tools/ci/suite-manifest.ps1 -Version 1.8.0 -Dist dist -Stage beta   # writes dist/suite.json (release.yml)
#   pwsh tools/ci/suite-manifest.ps1 -SelfTest                             # two dummy files against the fixture (ci.yml)
param([string]$Version = '', [string]$Dist = 'dist', [ValidateSet('beta', 'stable')][string]$Stage = 'beta', [switch]$SelfTest)
$ErrorActionPreference = 'Stop'

# The dock's ComponentId table: id, release asset, install-relative file, and whether -Stage applies (the apps; the dock
# and the language file are stable).
$Components = @(
  @{ Id = 'dock'; Asset = 'dock.exe'; File = 'dock.exe'; App = $false },
  @{ Id = 'tasks'; Asset = 'tasks.exe'; File = 'tasks.exe'; App = $true },
  @{ Id = 'tasks-admin'; Asset = 'tasks-admin.exe'; File = 'tasks-admin.exe'; App = $true },
  @{ Id = 'files'; Asset = 'files.exe'; File = 'files.exe'; App = $true },
  @{ Id = 'lang-it'; Asset = 'lang-it.lang'; File = 'lang/it.lang'; App = $false }
)
$ComponentMaxBytes = 32MB   # COMPONENT_MAX_BYTES
$Utf8 = [System.Text.UTF8Encoding]::new($false)

# The manifest's text for the assets present in $dist (dock.exe required).
function Get-SuiteText([string]$version, [string]$dist, [string]$stage) {
  if ($version -cnotmatch '^(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})$') { throw "suite-manifest: '$version' is not X.Y.Z" }
  $lines = [System.Collections.Generic.List[string]]::new()
  foreach ($c in $Components) {
    $path = Join-Path $dist $c.Asset
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
      if ($c.Id -eq 'dock') { throw "suite-manifest: no dock.exe in $dist" }
      continue
    }
    $size = (Get-Item -LiteralPath $path).Length
    if ($size -lt 1 -or $size -gt $ComponentMaxBytes) { throw "suite-manifest: $($c.Asset) is $size bytes" }
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    $st = if ($c.App) { $stage } else { 'stable' }
    $lines.Add([string]::Format([System.Globalization.CultureInfo]::InvariantCulture,
        '    {{"id": "{0}", "file": "{1}", "version": "{2}", "sha256": "{3}", "size": {4}, "stage": "{5}"}}',
        $c.Id, $c.File, $version, $hash, $size, $st))
  }
  return "{`n  `"schema`": 1,`n  `"version`": `"$version`",`n  `"components`": [`n" + ($lines -join ",`n") + "`n  ]`n}`n"
}

if ($SelfTest) {
  $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("suite-manifest-" + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory $tmp | Out-Null
  try {
    [System.IO.File]::WriteAllText((Join-Path $tmp 'dock.exe'), 'abc', $Utf8)
    [System.IO.File]::WriteAllText((Join-Path $tmp 'tasks.exe'), 'a made-up tasks.exe', $Utf8)
    $got = $Utf8.GetBytes((Get-SuiteText '1.8.0' $tmp 'beta'))
    $want = [System.IO.File]::ReadAllBytes((Join-Path $PSScriptRoot '..\..\tests\fixtures\update\suite-selftest.json'))
    if ([Convert]::ToBase64String($got) -ne [Convert]::ToBase64String($want)) {
      $Utf8.GetString($got) | Write-Host
      throw 'suite-manifest self-test: the text differs from tests/fixtures/update/suite-selftest.json'
    }
    'suite-manifest self-test: ok'
  } finally {
    Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
  }
  exit 0
}

if (-not $Version) { throw 'suite-manifest: -Version X.Y.Z is required' }
$out = Join-Path $Dist 'suite.json'
[System.IO.File]::WriteAllText($out, (Get-SuiteText $Version $Dist $Stage), $Utf8)
if ([System.IO.File]::ReadAllBytes($out) -contains 13) { throw 'suite-manifest: CR in suite.json' }
"suite-manifest: $out written"
