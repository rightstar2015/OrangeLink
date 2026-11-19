# Saves a PNG of the OrangeLink window only (never the whole screen).
#
#   .\scripts\capture_window.ps1 -Out shot.png
#   .\scripts\capture_window.ps1 -Out frames\f -Frames 60 -IntervalMs 100   # numbered PNGs for a GIF
#
# Brings the window to the front first (unless -NoActivate), so nothing that overlaps it is captured.
# Only meant for a sandbox instance (scripts\dev-sandbox.ps1).
param(
    [Parameter(Mandatory)][string]$Out,
    [int]$Frames = 1,
    [int]$IntervalMs = 100,
    [int]$X = -1, [int]$Y = -1, [int]$Width = 1280, [int]$Height = 800,
    [switch]$NoActivate   # leave the window alone (keeps keyboard focus while typing)
)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Win {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
}
'@
[void][Win]::SetProcessDPIAware()
$p = Get-Process orangelink -ErrorAction Stop | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { throw 'no OrangeLink window' }
$h = $p.MainWindowHandle
if (-not $NoActivate) {
    [void][Win]::ShowWindow($h, 9)
    if ($X -ge 0) { [void][Win]::MoveWindow($h, $X, $Y, $Width, $Height, $true) }
    [void][Win]::SetForegroundWindow($h)
    Start-Sleep -Milliseconds 400
}
$Out = [IO.Path]::GetFullPath([IO.Path]::Combine((Get-Location).Path, $Out))
New-Item -ItemType Directory -Force (Split-Path -Parent $Out) | Out-Null
for ($i = 0; $i -lt $Frames; $i++) {
    $r = New-Object Win+RECT
    [void][Win]::DwmGetWindowAttribute($h, 9, [ref]$r, 16)   # DWMWA_EXTENDED_FRAME_BOUNDS
    $w = $r.R - $r.L; $hh = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $hh
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $g.Dispose()
    $path = if ($Frames -gt 1) { '{0}{1:D3}.png' -f $Out, $i } else { $Out }
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    if ($Frames -gt 1) { Start-Sleep -Milliseconds $IntervalMs }
}
