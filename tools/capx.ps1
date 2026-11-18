param(
  [string]$Name = "ref",
  [int]$X = 40, [int]$Y = 160, [int]$W = 1240, [int]$H = 800,
  [string]$Proto = "", [int]$PX = 1300,
  [int]$Select = 2, [int]$PSelect = 1, [int]$PHover = -1,
  [switch]$NoReal
)
# Opens a separate Explorer window on Downloads without activating it, makes it topmost at
# a known visible rect, times it, optionally runs the prototype beside it, captures the
# screen, then restores the window placement and closes it.
$ErrorActionPreference = "Stop"
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;
public static class X {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc f, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int s);
  [StructLayout(LayoutKind.Sequential)] public struct WINDOWPLACEMENT { public int length, flags, showCmd; public int a,b,c,d; public RECT rc; }
  [DllImport("user32.dll")] public static extern bool GetWindowPlacement(IntPtr h, ref WINDOWPLACEMENT p);
  [DllImport("user32.dll")] public static extern bool SetWindowPlacement(IntPtr h, ref WINDOWPLACEMENT p);
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct SI { public int cb; public string r, d, t; public int x,y,xs,ys,xc,yc,fill,flags; public short show, r2; public IntPtr r3, i, o, e; }
  [StructLayout(LayoutKind.Sequential)] public struct PI { public IntPtr hp, ht; public int pid, tid; }
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern bool CreateProcess(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inh, int flags, IntPtr env, string dir, ref SI si, out PI pi);

  public static string Cls(IntPtr h) { var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
  public static List<IntPtr> Top(string cls) {
    var l = new List<IntPtr>();
    EnumWindows((h, p) => { if (Cls(h) == cls) l.Add(h); return true; }, IntPtr.Zero);
    return l;
  }
  public static bool HasChild(IntPtr parent, string cls) {
    bool f = false;
    EnumChildWindows(parent, (h, p) => { if (Cls(h) == cls && IsWindowVisible(h)) { f = true; return false; } return true; }, IntPtr.Zero);
    return f;
  }
  public static int Launch(string cmd) {
    var si = new SI(); si.cb = Marshal.SizeOf(si); si.flags = 1; si.show = 4; // STARTF_USESHOWWINDOW, SW_SHOWNOACTIVATE
    PI pi; CreateProcess(null, new StringBuilder(cmd), IntPtr.Zero, IntPtr.Zero, false, 0, IntPtr.Zero, null, ref si, out pi);
    return pi.pid;
  }
  public static RECT Frame(IntPtr h) { RECT r; DwmGetWindowAttribute(h, 9, out r, 16); return r; }
  // Moves h so its visible frame (DWM extended frame bounds) is exactly x,y,w,h, topmost, no activation.
  public static void Place(IntPtr h, int x, int y, int w, int hh) {
    SetWindowPos(h, new IntPtr(-1), x, y, w, hh, 0x0010 | 0x0040);
    RECT wr; GetWindowRect(h, out wr); RECT f = Frame(h);
    int l = f.L - wr.L, t = f.T - wr.T, r = wr.R - f.R, b = wr.B - f.B;
    SetWindowPos(h, new IntPtr(-1), x - l, y - t, w + l + r, hh + t + b, 0x0010 | 0x0040);
  }
  public static long Hash(int x, int y, int w, int h) {
    using (var bmp = new Bitmap(w, h, PixelFormat.Format32bppArgb)) {
      using (var g = Graphics.FromImage(bmp)) g.CopyFromScreen(x, y, 0, 0, bmp.Size);
      var d = bmp.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.ReadOnly, bmp.PixelFormat);
      var buf = new byte[d.Stride * h]; Marshal.Copy(d.Scan0, buf, 0, buf.Length); bmp.UnlockBits(d);
      long a = 1469598103934665603; foreach (var v in buf) { a ^= v; a *= 1099511628211; } return a;
    }
  }
  public static void Shot(string path) {
    int w = Screen(0), h = Screen(1);
    using (var bmp = new Bitmap(w, h)) { using (var g = Graphics.FromImage(bmp)) g.CopyFromScreen(0, 0, 0, 0, bmp.Size); bmp.Save(path, ImageFormat.Png); }
  }
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  // Renders the window itself (PW_RENDERFULLCONTENT), whatever covers it, cropped to its visible frame.
  public static void PW(IntPtr h, string path) {
    RECT wr; GetWindowRect(h, out wr); RECT f = Frame(h);
    using (var bmp = new Bitmap(wr.R - wr.L, wr.B - wr.T, PixelFormat.Format32bppArgb)) {
      using (var g = Graphics.FromImage(bmp)) { IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); }
      using (var c = bmp.Clone(new Rectangle(f.L - wr.L, f.T - wr.T, f.R - f.L, f.B - f.T), PixelFormat.Format24bppRgb)) c.Save(path, ImageFormat.Png);
    }
  }
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int s);
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x, y; }
  [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint f);
  // True when the window owns the top-level window at its centre and four inner corners.
  public static string Why = "";
  public static bool OnTop(IntPtr h) {
    RECT f = Frame(h);
    int[][] pts = { new[]{(f.L+f.R)/2,(f.T+f.B)/2}, new[]{f.L+20,f.T+20}, new[]{f.R-20,f.T+20}, new[]{f.L+20,f.B-20}, new[]{f.R-60,f.B-60} };
    foreach (var q in pts) { var p = new POINT(); p.x = q[0]; p.y = q[1]; IntPtr at = GetAncestor(WindowFromPoint(p), 2); if (at != h) { uint pid; GetWindowThreadProcessId(at, out pid); Why = "covered at " + q[0] + "," + q[1] + " by " + at + " " + Cls(at) + " pid=" + pid + " ex=0x" + GetWindowLong(at, -20).ToString("x"); return false; } }
    return true;
  }
  public static IntPtr Find(string cls) { var l = Top(cls); foreach (var h in l) if (IsWindowVisible(h)) return h; return IntPtr.Zero; }
  public static string Info(IntPtr h, int px, int py) {
    int c; DwmGetWindowAttribute(h, 14, out c, 4);
    var p = new POINT(); p.x = px; p.y = py; IntPtr at = GetAncestor(WindowFromPoint(p), 2);
    return "cloaked=" + c + " exstyle=0x" + GetWindowLong(h, -20).ToString("x") + " at_point=" + at + " " + Cls(at) + " exstyle_at=0x" + GetWindowLong(at, -20).ToString("x");
  }
  public static int Screen(int i) { return GetSystemMetrics(i); }
}
"@
[X]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
$log = @()
$fg0 = [X]::GetForegroundWindow()
$log += "fg_before=$fg0 fg_class=$([X]::Cls($fg0))"
$hwnd = [IntPtr]::Zero
if (-not $NoReal) {
  $pre = [X]::Top("CabinetWClass")
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $pid2 = [X]::Launch("explorer.exe /separate,$([Environment]::GetFolderPath('UserProfile'))\Downloads")
  while ($sw.ElapsedMilliseconds -lt 8000) {
    $new = [X]::Top("CabinetWClass") | ? { $pre -notcontains $_ -and [X]::IsWindowVisible($_) }
    if ($new) { $hwnd = @($new)[0]; break }
    Start-Sleep -Milliseconds 5
  }
  $tWin = $sw.ElapsedMilliseconds
  if ($hwnd -eq [IntPtr]::Zero) { $log += "no new window after 8 s"; $log | Out-File "$PSScriptRoot\$Name.txt" -Encoding ascii; exit 1 }
  $pl = New-Object X+WINDOWPLACEMENT; $pl.length = 44; [X]::GetWindowPlacement($hwnd, [ref]$pl) | Out-Null
  [X]::Place($hwnd, $X, $Y, $W, $H)
  $fg1 = [X]::GetForegroundWindow()
  $stole = ($fg1 -eq $hwnd)
  if ($stole) { $log += "explorer took foreground; restore=" + [X]::SetForegroundWindow($fg0) }
  while (-not [X]::HasChild($hwnd, "SHELLDLL_DefView") -and $sw.ElapsedMilliseconds -lt 8000) { Start-Sleep -Milliseconds 5 }
  $tView = $sw.ElapsedMilliseconds
  $log += "after place: " + [X]::Info($hwnd, $X + 300, $Y + 300)
  [X]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010) | Out-Null
  $log += "after 2nd topmost: " + [X]::Info($hwnd, $X + 300, $Y + 300)
  $last = 0; $stableSince = $sw.ElapsedMilliseconds; $tStable = -1
  while ($sw.ElapsedMilliseconds -lt 9000) {
    $hsh = [X]::Hash($X, $Y, $W, $H)
    if ($hsh -ne $last) { $last = $hsh; $stableSince = $sw.ElapsedMilliseconds }
    elseif ($sw.ElapsedMilliseconds - $stableSince -ge 700) { $tStable = $stableSince; break }
    Start-Sleep -Milliseconds 20
  }
  $log += "explorer pid=$pid2 window_ms=$tWin defview_ms=$tView visually_stable_ms=$tStable stole_focus=$stole"
  try {
    $sh = New-Object -ComObject Shell.Application
    $win = $sh.Windows() | ? { $_.HWND -eq $hwnd.ToInt64() } | select -first 1
    $doc = $win.Document
    $log += "view_mode=$($doc.CurrentViewMode) sort=$($doc.SortColumns) group=$($doc.GroupBy) icon=$($doc.IconSize) count=$($doc.Folder.Items().Count)"
    if ($Select -ge 0) { $doc.SelectItem($doc.Folder.Items().Item($Select), 1 + 4) }
  } catch { $log += "shell COM: $_" }
}
$protoPid = 0
if ($Proto) {
  Start-Sleep -Milliseconds 300
  $protoPid = [X]::Launch("`"$Proto`" --x $PX --y $Y --w $W --h $H --secs 9 --select $PSelect --hover $PHover")
  $sw2 = [Diagnostics.Stopwatch]::StartNew()
  while (-not ([X]::Top("ExplorustExplorerProto") | ? { [X]::IsWindowVisible($_) }) -and $sw2.ElapsedMilliseconds -lt 6000) { Start-Sleep -Milliseconds 5 }
  $log += "proto pid=$protoPid window_seen_ms=$($sw2.ElapsedMilliseconds)"
  Start-Sleep -Milliseconds 2500
} else { Start-Sleep -Milliseconds 600 }
$ph = if ($Proto) { [X]::Find("ExplorustExplorerProto") } else { [IntPtr]::Zero }
$ok = $false
for ($a = 1; $a -le 2 -and -not $ok; $a++) {
  if ($hwnd -ne [IntPtr]::Zero) { [X]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010) | Out-Null }
  Start-Sleep -Milliseconds 250
  $ok = (($hwnd -eq [IntPtr]::Zero) -or [X]::OnTop($hwnd)) -and (($ph -eq [IntPtr]::Zero) -or [X]::OnTop($ph))
  if ($ok) { [X]::Shot("$PSScriptRoot\$Name.png"); $ok = (($hwnd -eq [IntPtr]::Zero) -or [X]::OnTop($hwnd)) -and (($ph -eq [IntPtr]::Zero) -or [X]::OnTop($ph)) }
  $log += "attempt $a on_top=$ok $([X]::Why)"
}
if (-not $ok) { [X]::Shot("$PSScriptRoot\$Name.png") }
if ($hwnd -ne [IntPtr]::Zero) { [X]::PW($hwnd, "$PSScriptRoot\$Name-real-pw.png") }
if ($ph -ne [IntPtr]::Zero) { [X]::PW($ph, "$PSScriptRoot\$Name-proto-pw.png") }
$log += "fg_at_shot=$([X]::GetForegroundWindow()) screen=$([X]::Screen(0))x$([X]::Screen(1))"
if ($hwnd -ne [IntPtr]::Zero) {
  $f = [X]::Frame($hwnd); $log += "frame=$($f.L),$($f.T),$($f.R),$($f.B)"
  [X]::SetWindowPos($hwnd, [IntPtr](-2), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010) | Out-Null
  [X]::SetWindowPlacement($hwnd, [ref]$pl) | Out-Null
  [X]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
}
$log += "fg_after=$([X]::GetForegroundWindow())"
$log | Out-File "$PSScriptRoot\$Name.txt" -Encoding ascii
