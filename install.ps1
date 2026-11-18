# keytab installer for Windows:
#   irm https://raw.githubusercontent.com/HayzDev/keytab/main/install.ps1 | iex
# Downloads the latest release, installs it for the current user (no admin), and starts it.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$url = 'https://github.com/HayzDev/keytab/releases/latest/download/keytab-setup.exe'
$setup = Join-Path $env:TEMP 'keytab-setup.exe'
Write-Host 'Downloading keytab...'
Invoke-WebRequest $url -OutFile $setup
Write-Host 'Installing...'
Get-Process keytab -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Process $setup -ArgumentList '/S' -Wait
Remove-Item $setup
Start-Process (Join-Path $env:LOCALAPPDATA 'keytab\keytab.exe')
Write-Host 'keytab is running. Press Right Alt to switch windows (Right Alt + Right Shift to search).'
