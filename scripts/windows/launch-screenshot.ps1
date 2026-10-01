# Launches the built Windows app on an example with its diagnostic log on,
# checks it is still running after a while (it did not crash on start),
# and photographs its window alone. CI's "Launch" step runs it
# (.github/workflows/windows-app.yml); it works the same on a desktop.
#
#   pwsh scripts/windows/launch-screenshot.ps1 -Exe PATH -Out DIR -Name NAME
#        [-Example ID] [-Panel customizer|check|measure]
#
# `-Panel` opens that side panel at start (the app's `--panel`), so the
# picture shows it rendered: CI takes one of the customizer on an example
# with parameters.
#
# Writes DIR/NAME.png (the window) and DIR/NAME.log (the app's --log
# file, NeoSCAD.Host/AppLog.cs), and fails if the app exited early.
#
# Capture. The window is sized, restored and brought forward, then drawn
# with PrintWindow(PW_RENDERFULLCONTENT), which asks DWM for the window's
# own content, including its DirectComposition layers (the XAML tree, the
# DX12 SwapChainPanel, WebView2's hosted visual), whatever lies on top of
# it. A screen grab showed the runner's console over the window on the
# x64 runner and the Windows first-run privacy screen over everything on
# windows-11-arm. Should PrintWindow fail, the window's rectangle is
# copied from the screen instead, and the log says which was used.
param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $Out,
    [Parameter(Mandatory)] [string] $Name,
    [string] $Example = "csg",
    [string] $Panel = "",
    [int] $Seconds = 25,
    [int] $Width = 1400,
    [int] $Height = 900
)
$ErrorActionPreference = "Stop"

# Only the Win32 calls are compiled here; the bitmap work stays in
# PowerShell, because System.Drawing in .NET 10 spans several assemblies
# (System.Drawing.Common, System.Private.Windows.GdiPlus, ...) that
# Add-Type would each need named.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class NeoScadWindow
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);

    const uint SWP_SHOWWINDOW = 0x0040;
    const int SW_RESTORE = 9;
    const uint PW_RENDERFULLCONTENT = 0x2;

    // Physical pixels throughout, as the app (per-monitor aware) sizes
    // itself: DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2.
    static void PhysicalPixels() { SetThreadDpiAwarenessContext(new IntPtr(-4)); }

    public static void Place(IntPtr hwnd, int width, int height)
    {
        PhysicalPixels();
        ShowWindow(hwnd, SW_RESTORE);
        SetWindowPos(hwnd, IntPtr.Zero, 0, 0, width, height, SWP_SHOWWINDOW);
        SetForegroundWindow(hwnd);
    }

    public static Rect Bounds(IntPtr hwnd)
    {
        PhysicalPixels();
        Rect r;
        if (!GetWindowRect(hwnd, out r)) throw new InvalidOperationException("GetWindowRect failed");
        return r;
    }

    public static bool Print(IntPtr hwnd, IntPtr hdc)
    {
        PhysicalPixels();
        return PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT);
    }
}
'@

function Save-Window([IntPtr] $hwnd, [string] $path) {
    Add-Type -AssemblyName System.Drawing
    $r = [NeoScadWindow]::Bounds($hwnd)
    $w = $r.Right - $r.Left
    $h = $r.Bottom - $r.Top
    if ($w -le 0 -or $h -le 0) { throw "the window has no size: ${w}x${h}" }
    $bitmap = New-Object System.Drawing.Bitmap $w, $h
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $hdc = $graphics.GetHdc()
        try { $printed = [NeoScadWindow]::Print($hwnd, $hdc) } finally { $graphics.ReleaseHdc($hdc) }
        if ($printed) {
            $how = "PrintWindow(PW_RENDERFULLCONTENT)"
        } else {
            $graphics.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $w, $h))
            $how = "screen copy (PrintWindow failed)"
        }
        $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
    "$how, ${w}x${h}"
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$png = Join-Path (Resolve-Path $Out) "$Name.png"
$log = Join-Path (Resolve-Path $Out) "$Name.log"

$arguments = @('--example', $Example, '--log', "`"$log`"")
if ($Panel) { $arguments += @('--panel', $Panel) }
$app = Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru
$started = Get-Date

# The main window's handle appears once the window is created.
$hwnd = [IntPtr]::Zero
while (((Get-Date) - $started).TotalSeconds -lt $Seconds -and -not $app.HasExited) {
    $app.Refresh()
    if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $app.MainWindowHandle; break }
    Start-Sleep -Milliseconds 250
}
if ($hwnd -ne [IntPtr]::Zero) {
    [NeoScadWindow]::Place($hwnd, $Width, $Height)
    Write-Host "window $hwnd placed at ${Width}x${Height}"
}

# Time to start WebView2, load the editor and run the first preview.
$left = $Seconds - ((Get-Date) - $started).TotalSeconds
if ($left -gt 0) { Start-Sleep -Seconds $left }

$alive = -not $app.HasExited
if ($alive -and $hwnd -ne [IntPtr]::Zero) {
    # Forward again: something may have taken the foreground meanwhile.
    [NeoScadWindow]::Place($hwnd, $Width, $Height)
    Start-Sleep -Milliseconds 500
    try {
        $how = Save-Window $hwnd $png
        Write-Host "screenshot: $how -> $png"
        Add-Content -Path $log -Value "(launch-screenshot.ps1) screenshot: $how"
    } catch {
        Write-Host "::warning::no screenshot: $_"
    }
} elseif ($alive) {
    Write-Host "::warning::the app is running but no main window was found in $Seconds s"
}

if (Test-Path $log) {
    Write-Host "--- $log"
    Get-Content $log | ForEach-Object { Write-Host $_ }
} else {
    Write-Host "::warning::the app wrote no log"
}

if ($alive) {
    Stop-Process -Id $app.Id -Force
} else {
    throw "NeoSCAD.exe exited within $Seconds s (exit code $($app.ExitCode))"
}
