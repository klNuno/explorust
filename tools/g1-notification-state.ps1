# Run in the interactive session. Pass -Observe to record Explorer's answers.
param([switch]$Observe)
$ErrorActionPreference = 'Stop'
$log = Join-Path $PSScriptRoot 'notification-state.log'
$lines = [System.Collections.Generic.List[string]]::new()
try {
    Add-Type @'
using System;
using System.Runtime.InteropServices;

public static class NotificationStateProbe {
    [DllImport("shell32.dll")]
    public static extern int SHQueryUserNotificationState(out int state);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr CreateEvent(IntPtr attributes, bool manual, bool initial, string name);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr CreateMutex(IntPtr attributes, bool owned, string name);
    [DllImport("kernel32.dll")]
    public static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll")]
    public static extern bool ReleaseMutex(IntPtr handle);
    [DllImport("kernel32.dll")]
    public static extern bool CloseHandle(IntPtr handle);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowEx(uint ex, string cls, string title, uint style,
        int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")]
    public static extern bool DestroyWindow(IntPtr window);
    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int index);

    public static IntPtr ShowFixture(bool fullscreen) {
        int width = GetSystemMetrics(0);
        int height = GetSystemMetrics(1);
        IntPtr window = CreateWindowEx(0, "STATIC", "Notification state probe", 0x90000000,
            0, 0, fullscreen ? width : width / 2, fullscreen ? height : height / 2,
            IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        SetForegroundWindow(window);
        if (window == IntPtr.Zero || GetForegroundWindow() != window) {
            DestroyWindow(window);
            throw new InvalidOperationException("Could not activate the notification state fixture");
        }
        return window;
    }
}
'@
    $script:failures = 0
    function Check([string]$Case, [int]$Expected) {
        Start-Sleep -Milliseconds 150
        $state = 0
        $hr = [NotificationStateProbe]::SHQueryUserNotificationState([ref]$state)
        $pass = $hr -eq 0 -and $state -eq $Expected
        if (-not $pass) { $script:failures++ }
        $label = if ($Observe) { 'OBSERVED' } elseif ($pass) { 'PASS' } else { 'FAIL' }
        $lines.Add("$label $Case HRESULT=$hr state=$state expected=$Expected")
    }
    Check 'desktop' 5
    $event = [NotificationStateProbe]::CreateEvent([IntPtr]::Zero, $true, $false, '{A1965210-3A9D-4bca-822B-433645B3F5A2}')
    if ($event -eq [IntPtr]::Zero) { throw 'Could not create presentation mode event' }
    try { Check 'presentation' 4 }
    finally { [void][NotificationStateProbe]::CloseHandle($event) }
    Check 'after presentation' 5
    $gate = [NotificationStateProbe]::CreateMutex([IntPtr]::Zero, $false, 'Local\__DDrawCheckExclMode__')
    $exclusive = [NotificationStateProbe]::CreateMutex([IntPtr]::Zero, $false, 'Local\__DDrawExclMode__')
    try {
        if ($gate -eq [IntPtr]::Zero -or $exclusive -eq [IntPtr]::Zero) { throw 'Could not create DirectDraw mutexes' }
        if ([NotificationStateProbe]::WaitForSingleObject($exclusive, 0) -notin 0, 128) { throw 'Could not acquire DirectDraw exclusive mutex' }
        try { Check 'exclusive fullscreen' 3 }
        finally { [void][NotificationStateProbe]::ReleaseMutex($exclusive) }
        Check 'released exclusive mutex' 5
        foreach ($mutex in @($gate, $exclusive)) {
            if ([NotificationStateProbe]::WaitForSingleObject($mutex, 0) -notin 0, 128) { throw 'The shell left a DirectDraw mutex owned' }
            [void][NotificationStateProbe]::ReleaseMutex($mutex)
        }
    } finally {
        if ($exclusive -ne [IntPtr]::Zero) { [void][NotificationStateProbe]::CloseHandle($exclusive) }
        if ($gate -ne [IntPtr]::Zero) { [void][NotificationStateProbe]::CloseHandle($gate) }
    }
    $window = [NotificationStateProbe]::ShowFixture($false)
    try { Check 'windowed' 5 }
    finally { [void][NotificationStateProbe]::DestroyWindow($window) }
    $window = [NotificationStateProbe]::ShowFixture($true)
    try { Check 'borderless fullscreen' 2 }
    finally { [void][NotificationStateProbe]::DestroyWindow($window) }
    Check 'after borderless fullscreen' 5
    if ($script:failures -and -not $Observe) { exit 1 }
} catch {
    $lines.Add("ERROR $_")
    exit 2
} finally {
    $lines.Add('DONE')
    $lines | Set-Content "$log.tmp"
    Move-Item "$log.tmp" $log
}
