# Click inside the wwd window's client area: scripts/click.ps1 -X 640 -Y 455
param([int]$X, [int]$Y, [int]$Hold = 120)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class WwdClick {
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@
$p = Get-Process wwd -ErrorAction Stop | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
$h = $p.MainWindowHandle
$l = [IntPtr](($Y -shl 16) -bor ($X -band 0xffff))
[WwdClick]::PostMessage($h, 0x0200, [IntPtr]0, $l) | Out-Null   # WM_MOUSEMOVE
Start-Sleep -Milliseconds 50
[WwdClick]::PostMessage($h, 0x0201, [IntPtr]1, $l) | Out-Null   # WM_LBUTTONDOWN
Start-Sleep -Milliseconds ([math]::Max(1, $Hold / 2))
$l2 = [IntPtr](($Y -shl 16) -bor (($X + 1) -band 0xffff))
[WwdClick]::PostMessage($h, 0x0200, [IntPtr]1, $l2) | Out-Null  # drag 1px
Start-Sleep -Milliseconds ([math]::Max(1, $Hold / 2))
[WwdClick]::PostMessage($h, 0x0202, [IntPtr]0, $l2) | Out-Null  # WM_LBUTTONUP
"clicked $X,$Y"
