# Records the README screenshots and demo video in a clean Windows Sandbox.
#   cargo build --release --features demo
#   pwsh scripts/demo/record.ps1 [-Scene probe|demo]
# Output: target/demo/out (raw.mkv, marks.txt). Needs Windows Sandbox and ffmpeg 8.1+
# (for the gfxcapture source), on PATH or given with -FFmpeg.
param([string]$Scene = 'demo', [int]$TimeoutSec = 600, [string]$Exe, [string]$FFmpeg)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot/../.."
$work = Join-Path $root 'target/demo'
$exe = if ($Exe) { $Exe } else { Join-Path $root 'target/release/keytab.exe' }
if (-not (Test-Path $exe)) { throw 'Build first: cargo build --release --features demo' }
$ffmpeg = if ($FFmpeg) { $FFmpeg } else { (Get-Command ffmpeg).Source }
# Chocolatey's ffmpeg is a shim; the sandbox needs the real binary.
$real = Join-Path (Split-Path $ffmpeg) '../lib/ffmpeg/tools/ffmpeg/bin/ffmpeg.exe'
if (-not $FFmpeg -and (Test-Path $real)) { $ffmpeg = $real }

Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory "$work/out" | Out-Null
Copy-Item $exe, $ffmpeg, "$PSScriptRoot/guest.ps1", "$PSScriptRoot/scene.ps1" $work
Set-Content "$work/scene.txt" $Scene

@"
<Configuration>
  <VGpu>Enable</VGpu>
  <Networking>Enable</Networking>
  <MappedFolders>
    <MappedFolder><HostFolder>$work</HostFolder><SandboxFolder>C:\demo</SandboxFolder><ReadOnly>false</ReadOnly></MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\demo\guest.ps1</Command>
  </LogonCommand>
</Configuration>
"@ | Set-Content "$work/demo.wsb"

if ((wsb list --raw | ConvertFrom-Json).WindowsSandboxEnvironments) { throw 'Another Windows Sandbox is running; close it first' }
$id = (wsb start --raw --config (Get-Content "$work/demo.wsb" -Raw) | ConvertFrom-Json).Id
try {
    Start-Process wsb -ArgumentList connect, --id, $id  # the logon command needs a desktop session
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while (-not (Test-Path "$work/out/done.txt")) {
        if ((Get-Date) -gt $deadline) { throw 'Timed out waiting for the sandbox' }
        Start-Sleep 2
    }
} finally {
    wsb stop --id $id | Out-Null
}
Get-Content "$work/out/done.txt"
