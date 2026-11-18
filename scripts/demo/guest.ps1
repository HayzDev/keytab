# Runs inside Windows Sandbox (started by record.ps1): sets up a clean desktop with sample
# apps, starts keytab, then plays a scripted scene while ffmpeg records the screen.
$ErrorActionPreference = 'Continue'
$demo = 'C:\demo'
$out = "$demo\out"
Start-Transcript "$out\guest.log" | Out-Null
$scene = (Get-Content "$demo\scene.txt").Trim()
function Step([string]$what) { Add-Content "$out\steps.txt" ("{0:HH:mm:ss} {1}" -f (Get-Date), $what) }
Step start

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class K {
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint type);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool repaint);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    static uint Ext(byte vk) { return (vk == 0xA5 || vk == 0xA3 || (vk >= 0x21 && vk <= 0x2E)) ? 1u : 0u; }
    public static void Down(byte vk) { keybd_event(vk, (byte)MapVirtualKey(vk, 0), Ext(vk), UIntPtr.Zero); }
    public static void Up(byte vk) { keybd_event(vk, (byte)MapVirtualKey(vk, 0), Ext(vk) | 2, UIntPtr.Zero); }
}
"@

$VK = @{ RAlt = 0xA5; RShift = 0xA1; LShift = 0xA0; Esc = 0x1B; Enter = 0x0D; Back = 0x08; Down = 0x28; Up = 0x26 }
function Tap([int]$vk, [int]$hold = 70) { [K]::Down($vk); Start-Sleep -Milliseconds $hold; [K]::Up($vk) }
# Types letters/digits/space one by one, like a person.
function Type-Text([string]$text, [int]$gap = 160) {
    foreach ($c in $text.ToUpper().ToCharArray()) { Tap ([int][char]$c); Start-Sleep -Milliseconds $gap }
}
function Wait([double]$s) { Start-Sleep -Milliseconds ([int]($s * 1000)) }
function Mark([string]$name) { Add-Content "$out\marks.txt" ("{0} {1:F2}" -f $name, ((Get-Date) - $script:t0).TotalSeconds) }

Step desktop
# ---- clean desktop: dark mode, transparency, plain wallpaper ----
$p = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize'
New-Item $p -Force | Out-Null
Set-ItemProperty $p AppsUseLightTheme 0 -Type DWord
Set-ItemProperty $p SystemUsesLightTheme 0 -Type DWord
Set-ItemProperty $p EnableTransparency 1 -Type DWord
$adv = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced'
Set-ItemProperty $adv HideIcons 1 -Type DWord
Set-ItemProperty $adv TaskbarAl 1 -Type DWord
$edge = 'HKLM:\SOFTWARE\Policies\Microsoft\Edge'
New-Item $edge -Force | Out-Null
Set-ItemProperty $edge 'HideFirstRunExperience' 1 -Type DWord
Set-ItemProperty $edge 'StartupBoostEnabled' 0 -Type DWord
Set-ItemProperty $edge 'BrowserSignin' 0 -Type DWord
Set-ItemProperty $edge 'SyncDisabled' 1 -Type DWord
Set-ItemProperty $edge 'ShowRecommendationsEnabled' 0 -Type DWord
$wall = 'C:\Windows\Web\Wallpaper\Windows\img19.jpg'
if (-not (Test-Path $wall)) { $wall = (Get-ChildItem 'C:\Windows\Web\Wallpaper' -Recurse -Filter *.jpg | Select-Object -First 1).FullName }
Set-ItemProperty 'HKCU:\Control Panel\Desktop' Wallpaper $wall
Stop-Process -Name explorer -Force
Wait 4
rundll32.exe user32.dll, UpdatePerUserSystemParameters 1, True

Step content
# ---- sample content ----
$docs = "$env:USERPROFILE\Documents"
New-Item -ItemType Directory "$docs\Projects\website", "$docs\Projects\mobile-app", "$docs\Invoices" -Force | Out-Null
Set-Content "$docs\meeting-notes.txt" "Weekly sync`r`n`r`n- Ship onboarding flow by Friday`r`n- Review analytics dashboard`r`n- Plan Q1 roadmap"
Set-Content "$docs\todo.txt" "Today`r`n`r`n[x] Reply to Sam`r`n[ ] Update invoices`r`n[ ] Book flights"

Step webview2
# ---- WebView2 (the sandbox has no Edge, so no WebView2 runtime) ----
$wv = 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application'
if (-not (Test-Path $wv)) {
    Invoke-WebRequest 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile "$env:TEMP\wv2.exe" -UseBasicParsing
    Start-Process "$env:TEMP\wv2.exe" -ArgumentList '/silent', '/install' -Wait
}

Step apps
# ---- apps (the sandbox has only built-in Windows apps: no Notepad or Edge) ----
function Open([string]$file, [string[]]$arguments = @()) {
    if ($arguments.Count) { Start-Process $file -ArgumentList $arguments } else { Start-Process $file }
    Wait 2
}
Open explorer "$docs\Invoices"
Open charmap
Open services.msc
Open regedit
Open taskmgr
Open powershell '-NoExit', '-Command', '$Host.UI.RawUI.WindowTitle = ''PowerShell''; Get-ChildItem ~\Documents'
if (Test-Path "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell_ise.exe") { Open powershell_ise "$docs\meeting-notes.txt" }
Open explorer "$docs\Projects"
Wait 3

Step layout
# Tidy layout: windows cascade across the screen, a maximized Explorer in front.
$w = [K]::GetSystemMetrics(0); $h = [K]::GetSystemMetrics(1)
$i = 0
Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -and $_.ProcessName -ne 'explorer' } | ForEach-Object {
    [K]::MoveWindow($_.MainWindowHandle, [int]($w * 0.08) + 60 * $i, [int]($h * 0.08) + 40 * $i, [int]($w * 0.55), [int]($h * 0.6), $true) | Out-Null
    $i++
}
$shell = New-Object -ComObject Shell.Application
foreach ($win in $shell.Windows()) {
    if ($win.LocationName -eq 'Projects') { [K]::ShowWindow([IntPtr]$win.HWND, 3) | Out-Null; [K]::SetForegroundWindow([IntPtr]$win.HWND) | Out-Null }
}
Step keytab
# Park the pointer in a corner, so hover selection never moves the highlight.
[K]::SetCursorPos($w - 2, 2) | Out-Null
Start-Process "$demo\keytab.exe"
Wait 4
Step warmup
# Warm up: the first open loads the webview, which is slow in a fresh sandbox.
Tap $VK.RAlt; Wait 4; Tap $VK.Esc; Wait 1.5

# ---- record ----
function Start-Recorder([string]$src) {
    $p = New-Object System.Diagnostics.Process
    $p.StartInfo.FileName = "$demo\ffmpeg.exe"
    $p.StartInfo.Arguments = "-y -hide_banner -loglevel warning $src -c:v libx264rgb -crf 0 -preset ultrafast `"$out\raw.mkv`""
    $p.StartInfo.UseShellExecute = $false
    $p.StartInfo.RedirectStandardInput = $true
    $p.StartInfo.RedirectStandardError = $true
    $p.StartInfo.CreateNoWindow = $true
    $p.Start() | Out-Null
    $script:errTask = $p.StandardError.ReadToEndAsync()
    Wait 3
    if ($p.HasExited) { Add-Content "$out\ffmpeg.log" $script:errTask.Result; return $null }
    Add-Content "$out\ffmpeg.log" "recording with: $src"
    $p
}
Step record
# Windows.Graphics.Capture sees composited output (WebView2, blur, DWM thumbnails) and works
# in the sandbox's remote session; Desktop Duplication and GDI are fallbacks.
$ff = Start-Recorder '-f lavfi -i gfxcapture=monitor_idx=0:capture_cursor=0:max_framerate=30,hwdownload,format=bgra -r 30'
if (-not $ff) { $ff = Start-Recorder '-f lavfi -i ddagrab=framerate=30:draw_mouse=0,hwdownload,format=bgra' }
if (-not $ff) { $ff = Start-Recorder '-f gdigrab -framerate 30 -draw_mouse 0 -i desktop' }
$errTask = $script:errTask
$script:t0 = Get-Date

Step scene
switch ($scene) {
    'probe' {
        Mark open; Tap $VK.RAlt; Wait 2.5
        Mark preview; [K]::Down($VK.LShift); Wait 2.5; [K]::Up($VK.LShift); Wait 1
        Mark close; Tap $VK.Esc; Wait 1
        Mark search; [K]::Down($VK.RAlt); Wait 0.1; Tap $VK.RShift; [K]::Up($VK.RAlt); Wait 1.5
        Type-Text 'note'; Wait 2
        Tap $VK.Esc; Wait 1
    }
    default { . "$demo\scene.ps1" }
}

Wait 1
$ff.StandardInput.Write('q'); $ff.StandardInput.Flush()
$ff.WaitForExit(60000) | Out-Null
Set-Content "$out\ffmpeg.log" $errTask.Result
Get-Process | Where-Object { $_.MainWindowTitle } | ForEach-Object { "{0} | {1}" -f $_.ProcessName, $_.MainWindowTitle } | Set-Content "$out\windows.txt"
Stop-Transcript | Out-Null
Set-Content "$out\done.txt" "scene $scene finished, ffmpeg exit $($ff.ExitCode)"
