#requires -Version 7.2
<#
.SYNOPSIS
Checks native Settings startup, page navigation, and close/pipe lifetime regressions.
.DESCRIPTION
Requires an interactive Windows desktop and the installed x64 Windows App Runtime.
Uses a private synthetic backend; no widget, credentials, or settings files are used.
Does not build, publish, install dependencies, or invoke any application actions.
#>
[CmdletBinding()]
param(
    [string]$CompanionDirectory = (Join-Path $PSScriptRoot '../../settings-ui/bin/portable'),
    [string]$TemporaryDirectory = (Join-Path ([IO.Path]::GetTempPath()) 'opencode'),
    [ValidateRange(2, 60)][int]$TimeoutSeconds = 15
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'This smoke test requires an interactive Windows desktop.' }

$companion = (Resolve-Path -LiteralPath $CompanionDirectory).Path
$names = @('SettingsUI.exe', 'Microsoft.WindowsAppRuntime.Bootstrap.dll', 'SettingsUI.pri', 'App.xbf', 'MainWindow.xbf')
$hashes = @{}
foreach ($name in $names) {
    $file = Get-Item -LiteralPath (Join-Path $companion $name)
    if ($file.PSIsContainer -or $file.Length -eq 0) { throw "Missing or empty companion file: $name" }
    $hashes[$name] = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
}

$framework = if ([Environment]::Is64BitProcess) { 'Framework64' } else { 'Framework' }
$automationDirectory = Join-Path $env:WINDIR "Microsoft.NET/$framework/v4.0.30319/WPF"
foreach ($assembly in @('UIAutomationTypes.dll', 'UIAutomationClient.dll')) {
    $path = Join-Path $automationDirectory $assembly
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Windows .NET Framework UI Automation assemblies are required by the smoke client: $path"
    }
    Add-Type -Path $path
}
if (-not ('SettingsUISmokeNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public static class SettingsUISmokeNative {
    delegate bool WindowCallback(IntPtr window, IntPtr parameter);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool GetNamedPipeClientProcessId(SafePipeHandle pipe, out uint pid);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool PeekNamedPipe(SafePipeHandle pipe, IntPtr buffer, uint size,
        IntPtr read, out uint available, IntPtr left);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")]
    public static extern bool IsWindow(IntPtr window);
    [DllImport("user32.dll")]
    static extern bool EnumWindows(WindowCallback callback, IntPtr parameter);
    [DllImport("user32.dll", EntryPoint = "GetWindowTextW", CharSet = CharSet.Unicode)]
    static extern int GetWindowText(IntPtr window, StringBuilder text, int length);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    public static IntPtr FindOwnedWindow(uint pid, string title) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((window, parameter) => {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner == pid && IsWindowVisible(window)) {
                var text = new StringBuilder(512);
                GetWindowText(window, text, text.Capacity);
                if (text.ToString() == title) { found = window; return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
}

$snapshot = @{
    settings = @{
        language = 'en'; startup = $false; widget_visible = $true; settings_pane_open = $true
        show_tray_icon = $true; show_provider_icons = $true
        show_claude_code = $false; show_codex = $true; show_antigravity = $false
        show_session_window = $true; show_weekly_window = $true
        poll_interval_ms = 900000; adaptive_refresh = $true; alert_threshold_percent = 0
        palette = 'system'; bar_style = 'continuous'; bar_thickness = 'standard'; font_size = 'standard'
        claude_accent = $null; codex_accent = $null; monitor_device = 'smoke-display'; anchor_left = $false
    }
    effective_language = 'en'; claude_code_available = $false
    effective_appearance = @{ dark = $false; claude_accent = @(217, 119, 87); codex_accent = @(16, 163, 127) }
    update_busy = $false; update_available = $true; update_status = 'Synthetic update available'; install_channel = 'portable'
    version = 'smoke-test'; languages = @(@{ code = 'en'; name = 'English' })
    monitors = @(@{ device = 'smoke-display'; primary = $true })
}
$controls = @{
    General = 'LanguageSelector'; Monitoring = 'Setting_show_codex'; Appearance = 'Appearance_palette'
    Placement = 'MonitorSelector'; Update = 'CheckForUpdates'; About = 'ThirdPartyNotices'
}
$pages = @('General', 'Monitoring', 'Appearance', 'Placement', 'Update', 'About')
$timeoutMilliseconds = $TimeoutSeconds * 1000
$results = [Collections.Generic.List[object]]::new()

function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Get-ExitDescription([Diagnostics.Process]$Process) {
    $code = $Process.ExitCode
    $unsigned = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$code), 0)
    '{0} (0x{1:X8})' -f $code, $unsigned
}

function Assert-Running([Diagnostics.Process]$Process, [string]$Phase) {
    if ($Process.HasExited) {
        throw "PID $($Process.Id) exited during $Phase with Windows exit code $(Get-ExitDescription $Process)."
    }
}

function Wait-SmokeTask([Threading.Tasks.Task]$Task, [Diagnostics.Process]$Process, [string]$Phase) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while (-not $Task.IsCompleted) {
        Assert-Running $Process $Phase
        if ($clock.ElapsedMilliseconds -ge $timeoutMilliseconds) { throw "Timed out during $Phase (PID $($Process.Id))." }
        Start-Sleep -Milliseconds 20
    }
    $Task.GetAwaiter().GetResult()
}

function Read-Exact([IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process, [int]$Count) {
    $buffer = [byte[]]::new($Count)
    $offset = 0
    while ($offset -lt $Count) {
        $read = Wait-SmokeTask ($Pipe.ReadAsync($buffer, $offset, $Count - $offset)) $Process 'reading the initial request'
        Assert ($read -gt 0) 'The companion disconnected before sending its initial request.'
        $offset += $read
    }
    return ,$buffer
}

function Write-Frame([IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process, [hashtable]$Message) {
    $body = [Text.Encoding]::UTF8.GetBytes(($Message | ConvertTo-Json -Depth 8 -Compress))
    Assert ($body.Length -gt 0 -and $body.Length -le 65536) 'Invalid synthetic frame length.'
    $frame = [byte[]]::new($body.Length + 4)
    [BitConverter]::GetBytes([uint32]$body.Length).CopyTo($frame, 0)
    $body.CopyTo($frame, 4)
    Wait-SmokeTask ($Pipe.WriteAsync($frame, 0, $frame.Length)) $Process 'writing a synthetic response/event' | Out-Null
}

function Find-Control([Windows.Automation.AutomationElement]$Window, [string]$Id) {
    $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    $Window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
}

function Test-Selection([Windows.Automation.AutomationElement]$Control, [string]$Name) {
    $pattern = $Control.GetCurrentPattern([Windows.Automation.SelectionPattern]::Pattern)
    $selection = @($pattern.Current.GetSelection())
    $selection.Count -eq 1 -and $selection[0].Current.Name -ceq $Name
}

function Wait-Page([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process, [string]$Page) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds) {
        Assert-Running $Process "rendering $Page"
        $control = Find-Control $Window $controls[$Page]
        if ($null -ne $control) {
            # These values distinguish the hydrated synthetic snapshot from the pre-connection UI.
            $ready = switch ($Page) {
                General { Test-Selection $control 'English' }
                Monitoring { $control.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq [Windows.Automation.ToggleState]::On }
                Appearance { Test-Selection $control 'Follow Windows' }
                Placement { Test-Selection $control 'Primary display · smoke-display' }
                Update { $control.Current.Name -ceq 'Install available update' }
                About {
                    $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Version smoke-test')
                    $null -ne $Window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
                }
            }
            if ($ready) { return }
        }
        Start-Sleep -Milliseconds 50
    }
    throw "The $Page page did not expose its hydrated principal control ($($controls[$Page]))."
}

function Select-Page([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process, [string]$Page) {
    $item = Find-Control $Window "Navigation$Page"
    Assert ($null -ne $item) "Missing navigation item: $Page"
    $pattern = $null
    if ($item.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) {
        $pattern.Select()
    } elseif ($item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
        $pattern.Invoke()
    } else { throw "The $Page navigation item supports neither SelectionItem nor Invoke." }
    Wait-Page $Window $Process $Page
}

function Assert-NoUnexpectedRequests([IO.Pipes.NamedPipeServerStream]$Pipe) {
    [uint32]$available = 0
    $ok = [SettingsUISmokeNative]::PeekNamedPipe($Pipe.SafePipeHandle, [IntPtr]::Zero, 0, [IntPtr]::Zero, [ref]$available, [IntPtr]::Zero)
    Assert $ok 'Could not inspect the synthetic pipe before closing.'
    Assert ($available -eq 0) 'A UI-only interaction unexpectedly submitted another backend request. No requests were forwarded to the widget.'
}

function Request-WindowClose([IntPtr]$Window, [Diagnostics.Process]$Process) {
    Assert-Running $Process 'requesting ordinary window close'
    [uint32]$owner = 0
    [SettingsUISmokeNative]::GetWindowThreadProcessId($Window, [ref]$owner) | Out-Null
    Assert ($owner -eq $Process.Id) 'Window ownership changed before closing.'
    # SC_CLOSE is the ordinary caption-close path, not a forced process exit.
    Assert ([SettingsUISmokeNative]::PostMessage($Window, 0x0112, [IntPtr]::new(0xF060), [IntPtr]::Zero)) 'Could not request ordinary window close.'
}

function Test-DraftCloseCancellation([Windows.Automation.AutomationElement]$Window,
    [IntPtr]$Handle, [IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process) {
    $input = Find-Control $Window 'Color_codex_accent'
    Assert ($null -ne $input) 'Missing Codex accent draft input.'
    $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('#123456')
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '#123456') 'The unapplied hex draft was not entered.'
    Assert-NoUnexpectedRequests $Pipe
    Request-WindowClose $Handle $Process
    $message = 'Apply or discard each color draft before closing. Your draft text is still available on the Appearance page.'
    $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $message)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $canceled = $false
    while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds) {
        Assert-Running $Process 'canceling close for an unapplied color draft'
        $feedback = $Window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        if ($null -ne $feedback -and -not $feedback.Current.IsOffscreen) { $canceled = $true; break }
        Start-Sleep -Milliseconds 50
    }
    Assert $canceled 'Unapplied draft close did not expose the cancellation feedback.'
    Assert ([SettingsUISmokeNative]::IsWindowVisible($Handle)) 'Canceled close hid or destroyed the settings window.'
    $discard = Find-Control $Window 'DiscardColor_codex_accent'
    Assert ($null -ne $discard) 'Missing draft Discard button.'
    $discard.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $input = Find-Control $Window 'Color_codex_accent'
    Assert ($null -ne $input -and $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '') 'Discard did not restore the synthetic default accent.'

    # Reuse the same pipe after cancellation: a wrongly set Closed flag would ignore this event.
    Write-Frame $Pipe $Process @{ v = 1; event = 'activate'; page = 'general' }
    Wait-Page $Window $Process 'General'
    foreach ($next in $pages) { Select-Page $Window $Process $next }
    Select-Page $Window $Process 'Appearance'
    $input = Find-Control $Window 'Color_codex_accent'
    Assert ($null -ne $input -and $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '') 'Discarded draft returned after navigation.'
    Assert-NoUnexpectedRequests $Pipe
    Write-Host 'Canceled draft close verified: draft discarded, same pipe reactivated General, all six pages navigated, window still usable.'
}

function Open-Notices([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process) {
    $button = Find-Control $Window 'ThirdPartyNotices'
    Assert ($null -ne $button) 'Missing third-party notices button.'
    $button.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $handle = [IntPtr]::Zero
    while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds) {
        Assert-Running $Process 'opening third-party notices'
        $handle = [SettingsUISmokeNative]::FindOwnedWindow([uint32]$Process.Id, 'Third-party licenses and notices')
        if ($handle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 50
    }
    Assert ($handle -ne [IntPtr]::Zero) 'Third-party notices did not create a visible owned window.'
    $noticesWindow = [Windows.Automation.AutomationElement]::FromHandle($handle)
    $textCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::NameProperty, 'Third-party license and notice text')
    $clock.Restart()
    $noticesText = $null
    $noticesReady = $false
    while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds) {
        Assert-Running $Process 'loading third-party notices text'
        $textBox = $noticesWindow.FindFirst([Windows.Automation.TreeScope]::Descendants, $textCondition)
        if ($null -ne $textBox) {
            $noticesText = $textBox.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value
            if ($noticesText.Contains('https://github.com/upstream-ray/codex-usage-monitor') -and
                $noticesText.Contains('Copyright (c) 2025 Craig Constable')) {
                $noticesReady = $true
                break
            }
        }
        Start-Sleep -Milliseconds 50
    }
    Assert $noticesReady "Third-party notices did not expose the upstream project attribution in their text (UIA length $($noticesText.Length))."
    Write-Host ('Third-party notices opened: owned HWND 0x{0:X}; left open for shutdown cleanup.' -f $handle.ToInt64())
    $handle
}

function Invoke-SmokeCase([string]$Page, [string]$CloseMode, [bool]$NavigateAll = $false,
    [bool]$DraftCloseCancellation = $false, [bool]$Notices = $false, [bool]$UpdateAvailable = $true) {
    $process = $null
    $pipe = $null
    $result = [pscustomobject]@{
        Page = $Page; Close = $CloseMode; Status = 'Failed'; PID = $null
        InitialGet = $false; HWND = $null; Navigated = ''; DraftCancel = $false
        NoticesHWND = $null; ExitCode = $null; Error = $null
    }
    try {
        $pipeName = "CodexUsage.Settings.Smoke.$PID.$([Guid]::NewGuid().ToString('N'))"
        $options = [IO.Pipes.PipeOptions]::Asynchronous -bor [IO.Pipes.PipeOptions]::CurrentUserOnly
        $pipe = [IO.Pipes.NamedPipeServerStream]::new($pipeName, [IO.Pipes.PipeDirection]::InOut, 1,
            [IO.Pipes.PipeTransmissionMode]::Byte, $options, 65536, 65536)
        $connection = $pipe.WaitForConnectionAsync()
        $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $companion 'SettingsUI.exe'))
        $info.WorkingDirectory = $workingDirectory
        $info.UseShellExecute = $false
        $info.Environment['LOCALAPPDATA'] = $profileDirectory
        $info.Environment['TEMP'] = $childTemporaryDirectory
        $info.Environment['TMP'] = $childTemporaryDirectory
        foreach ($argument in @('--pipe', ('\\.\pipe\' + $pipeName), '--parent-pid', "$PID", '--page', $Page.ToLowerInvariant())) {
            $info.ArgumentList.Add($argument)
        }
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $process = [Diagnostics.Process]::Start($info)
        $result.PID = $process.Id
        Wait-SmokeTask $connection $process 'connecting to the private pipe' | Out-Null
        [uint32]$clientPid = 0
        Assert ([SettingsUISmokeNative]::GetNamedPipeClientProcessId($pipe.SafePipeHandle, [ref]$clientPid)) 'Could not identify the pipe client.'
        Assert ($clientPid -eq $process.Id) 'The connected pipe client is not the smoke-test companion.'
        $length = [BitConverter]::ToUInt32((Read-Exact $pipe $process 4), 0)
        Assert ($length -gt 0 -and $length -le 65536) "Invalid initial frame length: $length"
        $body = Read-Exact $pipe $process ([int]$length)
        $request = [Text.UTF8Encoding]::new($false, $true).GetString($body) | ConvertFrom-Json -AsHashtable
        Assert ($request -is [hashtable] -and $request.Count -eq 3 -and $request.ContainsKey('v') -and
            $request.ContainsKey('op') -and $request.ContainsKey('id')) 'Unexpected initial request fields.'
        Assert ($request.v -eq 1 -and $request.op -ceq 'get' -and $request.id -is [long] -and
            $request.id -ge 0 -and $request.id -le [uint32]::MaxValue) 'The first request was not a valid protocol-v1 get.'
        $result.InitialGet = $true
        $caseSnapshot = $snapshot.Clone()
        $caseSnapshot.update_available = $UpdateAvailable
        Write-Frame $pipe $process @{ v = 1; id = $request.id; ok = $true; revision = 1; state = $caseSnapshot }

        $windowClock = [Diagnostics.Stopwatch]::StartNew()
        do {
            Assert-Running $process 'creating the settings window'
            $process.Refresh()
            $hwnd = $process.MainWindowHandle
            if ($hwnd -ne [IntPtr]::Zero) { break }
            Start-Sleep -Milliseconds 50
        } while ($windowClock.ElapsedMilliseconds -lt $timeoutMilliseconds)
        Assert ($hwnd -ne [IntPtr]::Zero) 'The companion did not create a real top-level HWND.'
        [uint32]$windowPid = 0
        [SettingsUISmokeNative]::GetWindowThreadProcessId($hwnd, [ref]$windowPid) | Out-Null
        Assert ($windowPid -eq $process.Id -and [SettingsUISmokeNative]::IsWindowVisible($hwnd)) 'The HWND is not a visible window owned by this test process.'
        $result.HWND = '0x{0:X}' -f $hwnd.ToInt64()
        $window = [Windows.Automation.AutomationElement]::FromHandle($hwnd)
        Wait-Page $window $process $Page
        if (-not $UpdateAvailable) {
            $updateNavigation = Find-Control $window 'NavigationUpdate'
            Assert ($null -eq $updateNavigation -or $updateNavigation.Current.IsOffscreen) 'Update navigation appeared without an available update.'
            $manualCheck = Find-Control $window 'CheckForUpdates'
            Assert ($null -ne $manualCheck -and $manualCheck.Current.Name -ceq 'Check for updates') 'About did not offer a manual update check.'
        }
        Write-Host "$Page startup: PID $($process.Id), get received, HWND $($result.HWND), hydrated page ready after $($clock.ElapsedMilliseconds) ms."
        if ($NavigateAll) {
            foreach ($next in $pages) { Select-Page $window $process $next }
            $result.Navigated = $pages -join ', '
            Write-Host "Navigation verified: $($result.Navigated)."
        }
        if ($DraftCloseCancellation) {
            Assert ($Page -ceq 'Appearance') 'Draft cancellation requires the Appearance startup route.'
            Test-DraftCloseCancellation $window $hwnd $pipe $process
            $result.DraftCancel = $true
            $result.Navigated = $pages -join ', '
        }
        $noticesHandle = [IntPtr]::Zero
        if ($Notices) {
            Assert ($Page -ceq 'About') 'Third-party notices require the About startup route.'
            $noticesHandle = Open-Notices $window $process
            $result.NoticesHWND = '0x{0:X}' -f $noticesHandle.ToInt64()
        }
        Assert-NoUnexpectedRequests $pipe
        switch ($CloseMode) {
            Shutdown { Write-Frame $pipe $process @{ v = 1; event = 'shutdown' } }
            ShutdownDisconnect {
                Write-Frame $pipe $process @{ v = 1; event = 'shutdown' }
                $pipe.Dispose(); $pipe = $null
            }
            Disconnect { $pipe.Dispose(); $pipe = $null }
            WindowClose { Request-WindowClose $hwnd $process }
            WindowCloseDisconnect {
                Request-WindowClose $hwnd $process
                $pipe.Dispose(); $pipe = $null
            }
            default { throw "Unknown smoke close mode: $CloseMode" }
        }
        if (-not $process.WaitForExit($timeoutMilliseconds)) {
            $diagnostic = ''
            if ($DraftCloseCancellation) {
                try {
                    $draftInput = Find-Control $window 'Color_codex_accent'
                    $draftValue = if ($null -ne $draftInput) {
                        $draftInput.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value
                    } else { '<not found>' }
                    $closeMessage = $Window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,
                            'Apply or discard each color draft before closing. Your draft text is still available on the Appearance page.'))
                    [uint32]$unread = 0
                    [SettingsUISmokeNative]::PeekNamedPipe($pipe.SafePipeHandle, [IntPtr]::Zero, 0, [IntPtr]::Zero, [ref]$unread, [IntPtr]::Zero) | Out-Null
                    $diagnostic = "; color input value='$draftValue'; draft-close warning visible=$($null -ne $closeMessage -and -not $closeMessage.Current.IsOffscreen); window exists=$([SettingsUISmokeNative]::IsWindow($hwnd)); window visible=$([SettingsUISmokeNative]::IsWindowVisible($hwnd)); pipe unread=$unread"
                } catch { $diagnostic = "; close diagnostics unavailable: $($_.Exception.Message)" }
            }
            throw "$CloseMode did not exit within $TimeoutSeconds seconds$diagnostic."
        }
        $result.ExitCode = Get-ExitDescription $process
        Assert ($process.ExitCode -eq 0) "$CloseMode ended with Windows exit code $($result.ExitCode)."
        if ($Notices) { Assert (-not [SettingsUISmokeNative]::IsWindow($noticesHandle)) 'The notices window survived companion shutdown.' }
        $result.Status = 'Passed'
    } catch {
        $result.Error = $_.Exception.Message
        if ($null -ne $process -and $process.HasExited) { $result.ExitCode = Get-ExitDescription $process }
        $diagnostic = Join-Path $profileDirectory 'CodexUsage-SettingsUI-startup.log'
        if (Test-Path -LiteralPath $diagnostic -PathType Leaf) {
            Write-Host "Startup diagnostic: $([IO.File]::ReadAllText($diagnostic).Trim())"
            [IO.File]::Delete($diagnostic)
        }
    } finally {
        if ($null -ne $pipe) { $pipe.Dispose() }
        if ($null -ne $process) {
            try {
                if (-not $process.HasExited) {
                    Write-Host "Cleanup: stopping only owned smoke-test PID $($process.Id)."
                    $process.Kill()
                    Assert ($process.WaitForExit($timeoutMilliseconds)) 'The owned smoke-test process could not be reaped.'
                }
            } catch { $result.Status = 'Failed'; $result.Error = "$($result.Error) Cleanup failed: $($_.Exception.Message)" }
            finally { $process.Dispose() }
        }
    }
    $result
}

$root = Join-Path $TemporaryDirectory ('settings-ui-smoke-' + [Guid]::NewGuid().ToString('N'))
$workingDirectory = Join-Path $root 'unrelated-working-directory'
$profileDirectory = Join-Path $root 'profile'
$childTemporaryDirectory = Join-Path $root 'temp'
New-Item -ItemType Directory -Path $root | Out-Null
try {
    foreach ($directory in @($workingDirectory, $profileDirectory, $childTemporaryDirectory)) {
        [IO.Directory]::CreateDirectory($directory) | Out-Null
    }
    Write-Host "Companion: $companion"
    Write-Host "SettingsUI.exe SHA-256: $($hashes['SettingsUI.exe'])"
    foreach ($case in @(
        @{ Page = 'General'; CloseMode = 'Shutdown'; NavigateAll = $true },
        @{ Page = 'Monitoring'; CloseMode = 'Disconnect' },
        @{ Page = 'Appearance'; CloseMode = 'WindowClose'; DraftCloseCancellation = $true },
        @{ Page = 'Placement'; CloseMode = 'Shutdown' },
        @{ Page = 'About'; CloseMode = 'WindowClose' },
        @{ Page = 'General'; CloseMode = 'ShutdownDisconnect' },
        @{ Page = 'About'; CloseMode = 'WindowCloseDisconnect' },
        @{ Page = 'About'; CloseMode = 'Shutdown'; Notices = $true },
        @{ Page = 'About'; CloseMode = 'Shutdown'; UpdateAvailable = $false }
    )) { $results.Add((Invoke-SmokeCase @case)) }
    foreach ($name in $names) {
        Assert ((Get-FileHash -LiteralPath (Join-Path $companion $name) -Algorithm SHA256).Hash -ceq $hashes[$name]) "Payload changed during smoke testing: $name. Re-run against a stable published copy."
    }
    $results | Format-Table Page, Close, Status, PID, InitialGet, HWND, DraftCancel, NoticesHWND, ExitCode -AutoSize
    $failed = @($results | Where-Object Status -EQ 'Failed')
    if ($failed.Count) {
        $failed | Format-List Page, Close, PID, Error
        throw "$($failed.Count) Settings UI smoke test(s) failed."
    }
    Write-Host 'Passed: all six startup routes and page navigation; shutdown/disconnect/window close and immediate EOF races with exit code 0; canceled draft close followed by discard/reuse/close; open notices cleaned up on shutdown.'
} finally {
    # Only this invocation's random test root and owned child processes are cleaned up.
    Remove-Item -LiteralPath $root -Recurse -Force
}
