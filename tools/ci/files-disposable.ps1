#requires -Version 7
# The machine of the files-disposable CI job (plans P1-files-engine, FZ4; written first on the FS-3 spike branch, from
# which FZ4 takes it): an SMB share on localhost, a FAT32 VHD and two NTFS VHDs whose Recycle Bin values are set per
# volume. It changes the machine (a share, disks, HKCU values), so it refuses to run anywhere but a GitHub-hosted
# runner, the throwaway machine of the recycle, dialog and registration cases; never on a developer's PC.
#   pwsh tools/ci/files-disposable.ps1 -Setup               exports FILES_DISP_UNC, FILES_DISP_FAT, FILES_DISP_NUKE,
#                                                            FILES_DISP_QUOTA and FILES_TEST_ROOT to $env:GITHUB_ENV
#   pwsh tools/ci/files-disposable.ps1 -Register -Dll <p>   registers the fault DLL's three classes for .modfault (HKCU)
#   pwsh tools/ci/files-disposable.ps1 -Hotplug attach      a 64 MiB NTFS VHD appears with a drive letter; prints "letter <X>"
#   pwsh tools/ci/files-disposable.ps1 -Hotplug detach      that VHD is detached and its file deleted (the drive goes away)
param([switch]$Setup, [switch]$Register, [string]$Dll = '', [ValidateSet('', 'attach', 'detach')][string]$Hotplug = '')
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
  Write-Host 'files-disposable.ps1 runs only on a GitHub-hosted runner (GITHUB_ACTIONS=true, RUNNER_ENVIRONMENT=github-hosted)'
  exit 2
}

function Export-Var([string]$Name, [string]$Value) {
  "$Name=$Value" | Out-File -Append -Encoding utf8 $env:GITHUB_ENV
  Set-Item "Env:$Name" $Value
  Write-Host "$Name=$Value"
}

# A VHD made with diskpart (create, attach, one primary partition, quick format, a drive letter); returns its volume.
function New-TestVhd([string]$Path, [int]$MaxMb, [string]$Fs, [string]$Label) {
  $script = Join-Path (Split-Path $Path) "$Label-diskpart.txt"
  @(
    "create vdisk file=`"$Path`" maximum=$MaxMb type=expandable",
    "select vdisk file=`"$Path`"",
    'attach vdisk',
    'create partition primary',
    "format fs=$Fs quick label=$Label",
    'assign'
  ) | Set-Content -Encoding ascii $script
  $out = diskpart /s $script
  if ($LASTEXITCODE -ne 0) { $out | Write-Host; throw "diskpart failed for $Label" }
  $vol = Get-DiskImage -ImagePath $Path | Get-Disk | Get-Partition | Get-Volume | Where-Object DriveLetter | Select-Object -First 1
  if (-not $vol) { throw "no drive letter for $Label" }
  Write-Host "$Label on $($vol.DriveLetter): ($Fs, $MaxMb MiB)"
  return $vol
}

# Per-volume Recycle Bin values. The value names are unverified (F 5.5.9): when a probe sees no effect, its row is
# "not reproduced" and the classifier's safe policy stands.
function Set-BinValue($Vol, [string]$Name, [int]$Value) {
  if ($Vol.UniqueId -notmatch '\{[0-9a-fA-F-]+\}') { throw "no volume GUID in $($Vol.UniqueId)" }
  $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\$($Matches[0])"
  New-Item -Path $key -Force | Out-Null
  New-ItemProperty -Path $key -Name $Name -PropertyType DWord -Value $Value -Force | Out-Null
  Write-Host "$key $Name=$Value"
}

if ($Hotplug) {
  # One drive that comes and goes on demand: the files app's drive hot-plug test (F4.11) times how fast the sidebar follows.
  $dir = Join-Path $env:RUNNER_TEMP 'mod-files-disp'
  New-Item -ItemType Directory -Force $dir | Out-Null
  $vhd = Join-Path $dir 'hot.vhd'
  if ($Hotplug -eq 'attach') {
    $vol = New-TestVhd $vhd 64 'ntfs' 'MODHOT'
    "letter $($vol.DriveLetter)"
  } else {
    $script = Join-Path $dir 'hot-detach.txt'
    @("select vdisk file=`"$vhd`"", 'detach vdisk') | Set-Content -Encoding ascii $script
    $out = diskpart /s $script
    if ($LASTEXITCODE -ne 0) { $out | Write-Host; throw 'diskpart detach failed' }
    Remove-Item -Force -ErrorAction SilentlyContinue $vhd
    'detached'
  }
}

if ($Setup) {
  $base = Join-Path $env:RUNNER_TEMP 'mod-files-disp'
  foreach ($d in 'share', 'fat', 'nuke', 'quota') { New-Item -ItemType Directory -Force (Join-Path $base $d) | Out-Null }
  $me = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
  New-SmbShare -Name modfiles -Path (Join-Path $base 'share') -FullAccess $me | Out-Null
  $unc = '\\localhost\modfiles'
  if (-not (Test-Path -LiteralPath $unc)) { throw "$unc is not reachable" }
  $fat = New-TestVhd (Join-Path $base 'fat\fat.vhd') 64 'fat32' 'MODFAT'
  # Bin values are per volume: one volume with the bin turned off, another with a 1 MiB bin.
  $nuke = New-TestVhd (Join-Path $base 'nuke\nuke.vhd') 128 'ntfs' 'MODNUKE'
  $quota = New-TestVhd (Join-Path $base 'quota\quota.vhd') 128 'ntfs' 'MODQUOTA'
  Set-BinValue $nuke 'NukeOnDelete' 1
  Set-BinValue $quota 'MaxCapacity' 1
  $vars = [ordered]@{
    FILES_DISP_UNC   = $unc
    FILES_DISP_FAT   = "$($fat.DriveLetter):\"
    FILES_DISP_NUKE  = "$($nuke.DriveLetter):\"
    FILES_DISP_QUOTA = "$($quota.DriveLetter):\"
  }
  foreach ($k in $vars.Keys) { Export-Var $k $vars[$k] }
  Export-Var 'FILES_TEST_ROOT' ($vars.Values -join ';')
}

if ($Register) {
  # The fault DLL (the files_faultext example) for files of type .modfault, in HKCU only: a context-menu handler, an
  # icon handler and a thumbnail handler. The CLSIDs are pinned in the DLL and in the shell host (fault_clsids_are_pinned).
  if (-not $Dll -or -not (Test-Path $Dll)) { throw "the fault DLL is missing: '$Dll'" }
  $path = (Resolve-Path $Dll).Path
  $clsid = [ordered]@{
    FaultMenu  = '{08f7748e-e7c9-4950-ac48-6ebab3ca29dc}'
    FaultIcon  = '{e3fec5ef-f489-427f-879f-ef723f66d403}'
    FaultThumb = '{5a4db2eb-6c01-444f-be94-ebff37f11b35}'
  }
  $root = 'HKCU:\Software\Classes'
  function Set-Default([string]$Key, [string]$Value) {
    New-Item -Force -Path $Key | Out-Null
    Set-ItemProperty -Path $Key -Name '(default)' -Value $Value
  }
  Set-Default "$root\.modfault" 'ModFault.File'
  Set-Default "$root\ModFault.File\DefaultIcon" '%1'
  Set-Default "$root\ModFault.File\shellex\ContextMenuHandlers\ModFault" $clsid.FaultMenu
  Set-Default "$root\ModFault.File\shellex\IconHandler" $clsid.FaultIcon
  # The thumbnail-handler key under the extension: an unverified name; the disposable test records whether it loads.
  Set-Default "$root\.modfault\ShellEx\{e357fccd-a995-4576-b01f-234630154e96}" $clsid.FaultThumb
  foreach ($name in $clsid.Keys) {
    $key = "$root\CLSID\$($clsid[$name])"
    Set-Default $key "files test $name"
    Set-Default "$key\InprocServer32" $path
    Set-ItemProperty -Path "$key\InprocServer32" -Name 'ThreadingModel' -Value 'Apartment'
  }
  # A thumbnail provider initialized with IInitializeWithItem loads in-process only with process isolation off.
  New-ItemProperty -Path "$root\CLSID\$($clsid.FaultThumb)" -Name 'DisableProcessIsolation' -PropertyType DWord -Value 1 -Force | Out-Null
  Write-Host "registered FaultMenu, FaultIcon and FaultThumb for .modfault from $path"
}
