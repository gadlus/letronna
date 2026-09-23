# Capture only the Letronna window, never the whole desktop.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
function Shot($name) {
  $p = Get-Process letronna -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $p) { "no letronna"; return }
  $h = (Get-Process -Id $p.Id).MainWindowHandle
  if ($h -eq 0) {
    # A console child has no window of its own; use its terminal host.
    $h = (Get-Process WindowsTerminal, cmd, powershell -ErrorAction SilentlyContinue |
          Where-Object { $_.MainWindowTitle -match 'Letronna|letronna' } |
          Select-Object -First 1).MainWindowHandle
  }
  if (-not $h -or $h -eq 0) { "no window"; return }
  $r = New-Object Win+RECT
  [void][Win]::GetWindowRect($h, [ref]$r)
  Add-Type -AssemblyName System.Drawing
  $w = $r.Right - $r.Left; $ht = $r.Bottom - $r.Top
  if ($w -le 0 -or $ht -le 0) { "bad rect"; return }
  $bmp = New-Object System.Drawing.Bitmap $w, $ht
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $w, $ht))
  $bmp.Save("$PSScriptRoot\$name.png")
  "$name $w x $ht"
}
