<#
.SYNOPSIS
    Vitrobar performance budget check: memory, idle CPU, exe size (and start-up time).

.DESCRIPTION
    Starts Vitrobar the way a user does (the watchdog, no arguments, so the watchdog and the
    bar are both counted) with a throw-away settings file, opens 10 app windows, waits 10 s,
    then samples for 30 s:
      - memory: sum of PrivateMemorySize64 of the Vitrobar processes (the largest sample);
      - CPU: total CPU time used by the Vitrobar processes over the 30 s;
      - size: of the exe.
    Budgets: memory < 40 MB, CPU <= 50 ms in 30 s, exe < 5 MB (and with -MeasureStartup,
    bar window on screen < 300 ms after the launch).
    With -StartPanel the check runs a different scenario: Vitrobar starts with --debug-state,
    the check posts the debug message "Vitrobar.Debug.OpenStart" to its VitrobarNotify window
    (honoured only with --debug-state; it toggles Start on the primary monitor), times the
    post until a visible VitrobarPanel window exists (polled every 5 ms, 2 s at most), samples
    the memory while the panel is open, toggles it closed with the same message, waits 3 s
    and samples again. Budgets: open < 150 ms, memory open < 60 MB, 3 s after close < 40 MB
    (and the exe size). The idle CPU figure is not measured in this mode.
    Prints one line of JSON and exits 1 when a budget is missed.

    Desktop safety: it only closes the windows and processes it started itself, never uses
    Notepad (it restores unsaved tabs), points VITROBAR_SETTINGS at a temp file (the real
    settings.toml is never created or touched; "Start with Windows" stays as it is), and
    always closes Vitrobar, runs `vitrobar.exe --restore` and checks the stock taskbar at
    the end, also when something fails.

    How it knows which windows are its own:
      - Character Map, Paint, its own test window: the process it started (Start-Process
        -PassThru) and that process's children.
      - Calculator: calc.exe only asks Windows to start the packaged CalculatorApp, so the
        check takes the first CalculatorApp process that appears after its launch.
      - File Explorer windows share Explorer's process: each opens a temp folder with a
        unique name and is recognised by that title.
    So do not open Calculator yourself while the check runs. The memory figure is the bar
    with everything that is open on the desktop: run it on a quiet desktop (no other apps
    beyond the usual tray ones) for a number comparable with the budget.

.PARAMETER Exe
    The vitrobar.exe to check (normally target\release\vitrobar.exe).
.PARAMETER Apps
    How many app windows to open (default 10, at most 10).
.PARAMETER MeasureStartup
    Also time the launch until the bar window (class VitrobarBar) is visible.
.PARAMETER StartPanel
    Measure the Start panel (open time, memory open and after closing) instead of the idle
    figures; see above.
.PARAMETER MemoryReportOnly
    Report the memory but do not fail on it (CI: see .github/workflows/ci.yml).
.PARAMETER Widgets
    Run with both widgets on (Music and Claude limits) in the temp settings file; the idle
    budgets stay the same (spec: < 40 MB idle with both widgets on). Turning them on this
    way only starts their watchers: Claude Code's settings are never touched.
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [ValidateRange(0, 10)][int]$Apps = 10,
    [switch]$MeasureStartup,
    [switch]$StartPanel,
    [switch]$MemoryReportOnly,
    [switch]$Widgets
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$MemoryBudgetMB = 40
$CpuBudgetMs = 50
$ExeBudgetMB = 5
$StartupBudgetMs = 300
$SettleSeconds = 10
$SampleSeconds = 30
$StartOpenBudgetMs = 150
$StartOpenMemoryBudgetMB = 60
$StartClosedMemoryBudgetMB = 40

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class VitroPerf {
    delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern int GetClassNameW(IntPtr hwnd, StringBuilder name, int max);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool PostMessageW(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr hwnd);
    [DllImport("winmm.dll")] static extern uint timeBeginPeriod(uint ms);
    [DllImport("winmm.dll")] static extern uint timeEndPeriod(uint ms);
    const uint WM_CLOSE = 0x0010;

    public class Win { public IntPtr Hwnd; public uint Pid; public string Class; public bool Visible; }

    // Top-level windows (EnumWindows works across processes where FindWindow on a class
    // registered by another process sometimes does not).
    public static List<Win> Windows() {
        var list = new List<Win>();
        var sb = new StringBuilder(256);
        EnumWindows((h, l) => {
            sb.Length = 0;
            GetClassNameW(h, sb, sb.Capacity);
            uint pid; GetWindowThreadProcessId(h, out pid);
            list.Add(new Win { Hwnd = h, Pid = pid, Class = sb.ToString(), Visible = IsWindowVisible(h) });
            return true;
        }, IntPtr.Zero);
        return list;
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern int InternalGetWindowText(IntPtr hwnd, StringBuilder text, int max);
    // Never sends a message to the window's thread (a hung window cannot block the check).
    public static string Title(IntPtr hwnd) {
        var sb = new StringBuilder(512);
        InternalGetWindowText(hwnd, sb, sb.Capacity);
        return sb.ToString();
    }

    public static IntPtr[] WindowsOfClass(string cls) {
        var r = new List<IntPtr>();
        foreach (var w in Windows()) if (w.Class == cls) r.Add(w.Hwnd);
        return r.ToArray();
    }

    // True when a visible window of `cls` belongs to a process other than `excludePid`
    // (the watchdog has no window, so any visible VitrobarBar is the new bar's).
    static bool VisibleOfClass(string cls, HashSet<uint> pids) {
        foreach (var w in Windows()) if (w.Visible && w.Class == cls && !pids.Contains(w.Pid)) return true;
        return false;
    }

    // Starts `exe` and returns {pid, ms until a visible `cls` window exists (-1 on timeout)}.
    // Polls every ~1-5 ms with a 1 ms timer resolution.
    public static long[] StartAndWait(string exe, string args, string cls, int timeoutMs, bool wait) {
        var before = new HashSet<uint>();
        foreach (var w in Windows()) if (w.Class == cls) before.Add(w.Pid);
        timeBeginPeriod(1);
        try {
            var sw = Stopwatch.StartNew();
            var p = Process.Start(new ProcessStartInfo(exe, args) { UseShellExecute = false });
            long ms = -1;
            if (wait) {
                while (sw.ElapsedMilliseconds < timeoutMs) {
                    if (VisibleOfClass(cls, before)) { ms = sw.ElapsedMilliseconds; break; }
                    Thread.Sleep(2);
                }
            }
            return new long[] { p.Id, ms };
        } finally { timeEndPeriod(1); }
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern uint RegisterWindowMessageW(string s);

    static bool PanelVisible(HashSet<uint> pids) {
        foreach (var w in Windows()) if (w.Visible && w.Class == "VitrobarPanel" && pids.Contains(w.Pid)) return true;
        return false;
    }

    // Posts the debug "toggle Start" message to the VitrobarNotify window of one of `pids`.
    // Returns the stopwatch start tick (or -1 when there is no such window), then waits until
    // a visible VitrobarPanel of those processes exists (want = true) or none does (want =
    // false); returns the elapsed ms (-1 on timeout, -2 when the notify window is missing).
    public static long ToggleStart(uint[] pidList, bool want, int timeoutMs) {
        var pids = new HashSet<uint>(pidList);
        IntPtr target = IntPtr.Zero;
        foreach (var w in Windows()) if (w.Class == "VitrobarNotify" && pids.Contains(w.Pid)) target = w.Hwnd;
        if (target == IntPtr.Zero) return -2;
        uint msg = RegisterWindowMessageW("Vitrobar.Debug.OpenStart");
        timeBeginPeriod(1);
        try {
            var sw = Stopwatch.StartNew();
            PostMessageW(target, msg, IntPtr.Zero, IntPtr.Zero);
            while (sw.ElapsedMilliseconds < timeoutMs) {
                if (PanelVisible(pids) == want) return sw.ElapsedMilliseconds;
                Thread.Sleep(5);
            }
            return -1;
        } finally { timeEndPeriod(1); }
    }

    public static void Close(IntPtr hwnd) { PostMessageW(hwnd, WM_CLOSE, IntPtr.Zero, IntPtr.Zero); }
    public static bool Alive(IntPtr hwnd) { return IsWindow(hwnd); }

    public static uint TrayOwner() {
        IntPtr h = FindWindowW("Shell_TrayWnd", null);
        if (h == IntPtr.Zero) return 0;
        uint pid; GetWindowThreadProcessId(h, out pid);
        return pid;
    }
    public static int TrayCount() {
        int n = 0;
        foreach (var w in Windows()) if (w.Class == "Shell_TrayWnd") n++;
        return n;
    }
    public static bool TrayVisible() {
        IntPtr h = FindWindowW("Shell_TrayWnd", null);
        return h != IntPtr.Zero && IsWindowVisible(h);
    }

    // A plain Win32 window on its own thread inside this process (one more app with no user
    // state; closing it is ours to do).
    delegate IntPtr WndProc(IntPtr h, uint m, IntPtr w, IntPtr l);
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct WNDCLASSW {
        public uint style; public WndProc proc; public int cls; public int wnd; public IntPtr inst;
        public IntPtr icon; public IntPtr cursor; public IntPtr bg; public string menu; public string name;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct MSG { public IntPtr h; public uint m; public IntPtr w; public IntPtr l; public uint t; public int x; public int y; }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern ushort RegisterClassW(ref WNDCLASSW c);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr CreateWindowExW(uint ex, string cls, string title, uint style, int x, int y, int w, int h,
        IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] static extern IntPtr DefWindowProcW(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] static extern void PostQuitMessage(int code);
    [DllImport("user32.dll")] static extern int GetMessageW(out MSG m, IntPtr h, uint a, uint b);
    [DllImport("user32.dll")] static extern bool TranslateMessage(ref MSG m);
    [DllImport("user32.dll")] static extern IntPtr DispatchMessageW(ref MSG m);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("kernel32.dll")] static extern IntPtr GetModuleHandleW(IntPtr name);
    static WndProc proc = (h, m, w, l) => {
        if (m == 0x0010) { DestroyWindow(h); return IntPtr.Zero; }      // WM_CLOSE
        if (m == 0x0002) { PostQuitMessage(0); return IntPtr.Zero; }    // WM_DESTROY
        return DefWindowProcW(h, m, w, l);
    };
    public static IntPtr OpenWindow(string title) {
        var ready = new ManualResetEvent(false);
        IntPtr hwnd = IntPtr.Zero;
        var t = new Thread(() => {
            var wc = new WNDCLASSW { proc = proc, inst = GetModuleHandleW(IntPtr.Zero), bg = (IntPtr)6, name = "VitroPerfTestWindow" };
            RegisterClassW(ref wc);
            // WS_OVERLAPPEDWINDOW, CW_USEDEFAULT position.
            hwnd = CreateWindowExW(0, "VitroPerfTestWindow", title, 0x00CF0000, unchecked((int)0x80000000), 0, 360, 200,
                IntPtr.Zero, IntPtr.Zero, wc.inst, IntPtr.Zero);
            if (hwnd != IntPtr.Zero) ShowWindow(hwnd, 4); // SW_SHOWNOACTIVATE
            ready.Set();
            MSG msg;
            while (hwnd != IntPtr.Zero && GetMessageW(out msg, IntPtr.Zero, 0, 0) > 0) { TranslateMessage(ref msg); DispatchMessageW(ref msg); }
        });
        t.IsBackground = true;
        t.Start();
        ready.WaitOne(10000);
        return hwnd;
    }
}
'@

function Write-Note([string]$msg) { [Console]::Error.WriteLine("perf-check: $msg") }

$exePath = (Resolve-Path -LiteralPath $Exe).Path
if (Get-Process -Name vitrobar -ErrorAction SilentlyContinue) {
    # The check could not tell its processes from the user's, and must never stop the user's.
    Write-Note 'a Vitrobar is already running: close it first'
    exit 2
}

$tag = "vitrobar-perf-$PID-$([Environment]::TickCount)"
$temp = Join-Path ([IO.Path]::GetTempPath()) $tag
New-Item -ItemType Directory -Path $temp | Out-Null
$settings = Join-Path $temp 'settings.toml'
# No first-run import, and "Start with Windows" off so nothing is written to HKCU\...\Run.
$settingsText = "first-run-done = true`nstart-with-windows = false"
if ($Widgets) { $settingsText += "`nwidget-music = true`nwidget-claude = true" }
Set-Content -LiteralPath $settings -Encoding utf8 -Value $settingsText
$oldSettingsEnv = $env:VITROBAR_SETTINGS
$env:VITROBAR_SETTINGS = $settings

$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runBefore = (Get-ItemProperty -LiteralPath $runKey -Name Vitrobar -ErrorAction SilentlyContinue)
$runBefore = if ($runBefore) { $runBefore.Vitrobar } else { $null }

$ownPids = [System.Collections.Generic.List[int]]::new()      # processes we started
$ownWindows = [System.Collections.Generic.List[IntPtr]]::new() # windows we opened in shared processes
$vitroPids = [System.Collections.Generic.HashSet[int]]::new()
$result = [ordered]@{}
$failures = [System.Collections.Generic.List[string]]::new()
$opened = 0

function Get-VitroProcesses {
    # Every vitrobar process started by this run: the watchdog, its bar, a possible adopter.
    foreach ($p in Get-CimInstance Win32_Process -Filter "Name = 'vitrobar.exe'") {
        if ($vitroPids.Contains([int]$p.ProcessId) -or $vitroPids.Contains([int]$p.ParentProcessId)) {
            [void]$vitroPids.Add([int]$p.ProcessId)
        }
    }
    foreach ($id in @($vitroPids)) { Get-Process -Id $id -ErrorAction SilentlyContinue }
}

function Start-Tracked([string]$file, [string]$handOff) {
    # The started process owns its window (Win32 apps). `handOff`: the process name a
    # launcher hands over to when it only asks Windows to start a packaged app (calc.exe ->
    # CalculatorApp, started by the system, not as our child); then the first process of
    # that name that appears after the launch is ours. Looked for only until it appears.
    $before = if ($handOff) { @(Get-Process -Name $handOff -ErrorAction SilentlyContinue | ForEach-Object Id) } else { @() }
    $t0 = Get-Date
    try { $p = Start-Process -FilePath $file -PassThru }
    catch {
        Write-Note "could not start $file`: $($_.Exception.Message)"
        return $false
    }
    if (-not $handOff) {
        if ($p.HasExited) { return $false }
        $ownPids.Add($p.Id)
        return $true
    }
    $deadline = $t0.AddSeconds(8)
    while ((Get-Date) -lt $deadline) {
        $new = @(Get-Process -Name $handOff -ErrorAction SilentlyContinue |
            Where-Object { $before -notcontains $_.Id -and $_.StartTime -ge $t0.AddSeconds(-1) } |
            Sort-Object StartTime)
        if ($new.Count -gt 0) {
            $ownPids.Add($new[0].Id)
            return $true
        }
        if (-not $p.HasExited -and ((Get-Date) - $t0).TotalSeconds -ge 3) {
            # No hand-over (classic calc on Windows Server): the launched process is the app.
            $ownPids.Add($p.Id)
            return $true
        }
        Start-Sleep -Milliseconds 100
    }
    return $false
}

function Test-OurFolderTitle([string]$title, [string]$name) {
    # File Explorer titles are "<folder>" or "<folder> - File Explorer" (localized suffix);
    # with "Display the full path in the title bar" on, the folder comes after its path. So
    # the name may sit anywhere, but must end the title or be followed by a space (so that
    # "<tag>-1" never matches "<tag>-10").
    $i = $title.IndexOf($name, [StringComparison]::OrdinalIgnoreCase)
    if ($i -lt 0) { return $false }
    $end = $i + $name.Length
    return $end -eq $title.Length -or $title[$end] -eq ' '
}

function Open-Apps([int]$count) {
    $plan = @(
        @('charmap.exe', ''), @('charmap.exe', ''), @('charmap.exe', ''),
        @('mspaint.exe', ''), @('mspaint.exe', ''), @('mspaint.exe', ''),
        @('explorer', ''), @('explorer', ''),
        @('calc.exe', 'CalculatorApp'),
        @('form', '')
    )
    $n = 0
    $folders = 0
    foreach ($app in $plan | Select-Object -First $count) {
        $ok = $false
        if ($app[0] -eq 'explorer' -and -not (Get-Process -Name explorer -ErrorAction SilentlyContinue)) {
            # No shell running (a CI runner): explorer.exe would start one. Another app instead.
            $app = @('charmap.exe', '')
        }
        switch ($app[0]) {
            'explorer' {
                # File Explorer windows share Explorer's process: open a folder with a unique
                # name and take only the new window that shows it.
                $folders++
                $name = "$tag-$folders"
                $dir = Join-Path $temp $name
                New-Item -ItemType Directory -Path $dir -Force | Out-Null
                $before = [VitroPerf]::WindowsOfClass('CabinetWClass')
                Start-Process -FilePath explorer.exe -ArgumentList "/n,`"$dir`""
                $deadline = (Get-Date).AddSeconds(8)
                do {
                    Start-Sleep -Milliseconds 200
                    $new = @([VitroPerf]::WindowsOfClass('CabinetWClass') |
                        Where-Object { $before -notcontains $_ -and (Test-OurFolderTitle ([VitroPerf]::Title($_)) $name) })
                } while ($new.Count -eq 0 -and (Get-Date) -lt $deadline)
                foreach ($h in $new) { $ownWindows.Add($h) }
                $ok = $new.Count -gt 0
            }
            'form' {
                # A plain Win32 window on a thread of this script.
                $h = [VitroPerf]::OpenWindow($tag)
                if ($h -ne [IntPtr]::Zero) { $ownWindows.Add($h); $ok = $true }
            }
            default {
                $ok = Start-Tracked $app[0] $app[1]
                if (-not $ok -and $app[1]) {
                    # The packaged app is not installed (Calculator on a Windows Server
                    # runner): its launcher exits without a window. Another app instead.
                    Write-Note "no window for $($app[0]); charmap.exe instead"
                    $ok = Start-Tracked 'charmap.exe' ''
                }
            }
        }
        if ($ok) { $n++ } else { Write-Note "no window for $($app[0])" }
    }
    return $n
}

function Stop-Vitrobar {
    $procs = @(Get-VitroProcesses)
    if (-not $procs) { return }
    # A clean quit: WM_CLOSE to the bar's own (hidden) notify window; the bar gives the shell
    # back and exits 0, which ends the watchdog.
    foreach ($w in [VitroPerf]::Windows()) {
        if ($w.Class -eq 'VitrobarNotify' -and $vitroPids.Contains([int]$w.Pid)) { [VitroPerf]::Close($w.Hwnd) }
    }
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline -and @(Get-VitroProcesses).Count -gt 0) { Start-Sleep -Milliseconds 100 }
    foreach ($p in @(Get-VitroProcesses)) {
        Write-Note "vitrobar $($p.Id) did not quit: stopping it"
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    }
}

function Close-Apps {
    # Children the started apps spawned since are theirs, so ours too.
    foreach ($c in @(Get-CimInstance Win32_Process | Where-Object { $ownPids -contains [int]$_.ParentProcessId })) {
        if ($ownPids -notcontains [int]$c.ProcessId) { $ownPids.Add([int]$c.ProcessId) }
    }
    # A File Explorer window on one of this run's folders that showed up too late to be
    # tracked is still ours (the folder names are unique to this run; Contains, not
    # StartsWith: the title may begin with the folder's full path).
    foreach ($h in [VitroPerf]::WindowsOfClass('CabinetWClass')) {
        if ($ownWindows -notcontains $h -and [VitroPerf]::Title($h).Contains("$tag-")) {
            $ownWindows.Add($h)
            $failures.Add('explorer_window_tracked_late')
        }
    }
    foreach ($h in $ownWindows) { if ([VitroPerf]::Alive($h)) { [VitroPerf]::Close($h) } }
    foreach ($id in $ownPids) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
    $deadline = (Get-Date).AddSeconds(5)
    while ((Get-Date) -lt $deadline -and @($ownWindows | Where-Object { [VitroPerf]::Alive($_) }).Count -gt 0) {
        Start-Sleep -Milliseconds 100
    }
    $left = @($ownWindows | Where-Object { [VitroPerf]::Alive($_) }).Count +
        @($ownPids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }).Count
    if ($left -gt 0) { $failures.Add("test_apps_left_open:$left") }
}

$exitCode = 1
try {
    $exeMB = (Get-Item -LiteralPath $exePath).Length / 1MB
    $result.exe_mb = [math]::Round($exeMB, 2)
    if ($exeMB -ge $ExeBudgetMB) { $failures.Add('exe_size') }

    $launchArgs = if ($StartPanel) { '--debug-state' } else { '' }
    $launch = [VitroPerf]::StartAndWait($exePath, $launchArgs, 'VitrobarBar', 5000, [bool]$MeasureStartup)
    [void]$vitroPids.Add([int]$launch[0])
    if ($MeasureStartup) {
        $result.startup_ms = $launch[1]
        if ($launch[1] -lt 0 -or $launch[1] -ge $StartupBudgetMs) { $failures.Add('startup') }
    }
    Start-Sleep -Milliseconds 1500
    if (@(Get-VitroProcesses).Count -lt 2) { $failures.Add('bar_not_running') }

    $opened = Open-Apps $Apps
    $result.apps = $opened
    if ($opened -lt $Apps) { $failures.Add("apps_opened_$opened`_of_$Apps") }

    Start-Sleep -Seconds $SettleSeconds

    if ($StartPanel) {
        $ids = [uint32[]]@($vitroPids | ForEach-Object { [uint32]$_ })
        $openMs = [VitroPerf]::ToggleStart($ids, $true, 2000)
        $result.start_open_ms = $openMs
        if ($openMs -lt 0 -or $openMs -ge $StartOpenBudgetMs) { $failures.Add('start_open_time') }
        if ($openMs -ge 0) {
            Start-Sleep -Milliseconds 1500   # let the data workers and the first search settle
            $openSamples = [System.Collections.Generic.List[double]]::new()
            for ($i = 0; $i -lt 5; $i++) {
                $openSamples.Add(((@(Get-VitroProcesses) | Measure-Object -Property PrivateMemorySize64 -Sum).Sum) / 1MB)
                Start-Sleep -Milliseconds 200
            }
            $result.start_open_memory_mb = [math]::Round(($openSamples | Measure-Object -Maximum).Maximum, 2)
            if ($result.start_open_memory_mb -ge $StartOpenMemoryBudgetMB) { $failures.Add('start_open_memory') }
            $closeMs = [VitroPerf]::ToggleStart($ids, $false, 2000)
            $result.start_close_ms = $closeMs
            if ($closeMs -lt 0) { $failures.Add('start_not_closed') }
        }
        Start-Sleep -Seconds 3
        $procs = @(Get-VitroProcesses)
        $result.processes = $procs.Count
        $result.start_closed_memory_mb = [math]::Round((($procs | Measure-Object -Property PrivateMemorySize64 -Sum).Sum) / 1MB, 2)
        $result.memory_detail = (@($procs | Sort-Object Id | ForEach-Object { "$($_.Id):$([math]::Round($_.PrivateMemorySize64 / 1MB, 2))" }) -join ' ')
        if ($result.start_closed_memory_mb -ge $StartClosedMemoryBudgetMB) { $failures.Add('start_closed_memory') }
    } else {
        $procs = @(Get-VitroProcesses)
        $cpu0 = ($procs | Measure-Object -Property { $_.TotalProcessorTime.TotalMilliseconds } -Sum).Sum
        $samples = [System.Collections.Generic.List[double]]::new()
        $end = (Get-Date).AddSeconds($SampleSeconds)
        while ((Get-Date) -lt $end) {
            $procs = @(Get-VitroProcesses)
            $samples.Add((($procs | Measure-Object -Property PrivateMemorySize64 -Sum).Sum) / 1MB)
            Start-Sleep -Seconds 1
        }
        $procs = @(Get-VitroProcesses)
        $cpu1 = ($procs | Measure-Object -Property { $_.TotalProcessorTime.TotalMilliseconds } -Sum).Sum
        $result.processes = $procs.Count
        $result.memory_mb = [math]::Round(($samples | Measure-Object -Maximum).Maximum, 2)
        $result.memory_detail = (@($procs | Sort-Object Id | ForEach-Object { "$($_.Id):$([math]::Round($_.PrivateMemorySize64 / 1MB, 2))" }) -join ' ')
        $result.cpu_ms = [math]::Round($cpu1 - $cpu0, 1)
        if ($result.memory_mb -ge $MemoryBudgetMB) {
            if ($MemoryReportOnly) { $result.memory_enforced = $false } else { $failures.Add('memory') }
        }
        if ($result.cpu_ms -gt $CpuBudgetMs) { $failures.Add('cpu') }
    }
} catch {
    $failures.Add("error: $($_.Exception.Message)")
} finally {
    # Every step on its own: a failing one never skips the ones after it, and the JSON is
    # always printed.
    try { Stop-Vitrobar } catch { $failures.Add("stop_vitrobar: $($_.Exception.Message)") }
    try { Close-Apps } catch { $failures.Add("close_apps: $($_.Exception.Message)") }
    try {
        # The safety net, whatever happened above.
        $r = Start-Process -FilePath $exePath -ArgumentList '--restore' -Wait -PassThru
        if ($r.ExitCode -ne 0) { $failures.Add("restore_exit_$($r.ExitCode)") }
    } catch { $failures.Add("restore_failed: $($_.Exception.Message)") }
    $env:VITROBAR_SETTINGS = $oldSettingsEnv
    try {
        $runAfter = (Get-ItemProperty -LiteralPath $runKey -Name Vitrobar -ErrorAction SilentlyContinue)
        $runAfter = if ($runAfter) { $runAfter.Vitrobar } else { $null }
        if ($runAfter -ne $runBefore) {
            if ($null -eq $runBefore) { Remove-ItemProperty -LiteralPath $runKey -Name Vitrobar -ErrorAction SilentlyContinue }
            else { Set-ItemProperty -LiteralPath $runKey -Name Vitrobar -Value $runBefore }
            $failures.Add('autostart_changed')
        }
    } catch { $failures.Add("run_key: $($_.Exception.Message)") }
    try {
        $explorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | ForEach-Object Id)
        $owner = [VitroPerf]::TrayOwner()
        if ($owner -ne 0) {
            $shellOk = ($explorer -contains [int]$owner) -and ([VitroPerf]::TrayCount() -eq 1) -and [VitroPerf]::TrayVisible()
            $result.shell_ok = $shellOk
            if (-not $shellOk) { $failures.Add('shell_not_restored') }
        } else {
            $result.shell_ok = $null   # no Explorer taskbar on this machine (e.g. a CI runner)
        }
    } catch { $failures.Add("shell_check: $($_.Exception.Message)") }
    try { Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction Stop }
    catch { Write-Note "could not remove $temp`: $($_.Exception.Message)" }
    $result.failures = @($failures)
    $result.pass = ($failures.Count -eq 0)
    $result | ConvertTo-Json -Compress
    $exitCode = if ($result.pass) { 0 } else { 1 }
}
exit $exitCode
