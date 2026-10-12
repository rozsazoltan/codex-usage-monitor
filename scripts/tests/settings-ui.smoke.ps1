#requires -Version 7.2
<#
.SYNOPSIS
Checks native Settings startup, bitmap previews, page navigation, and close/pipe lifetime regressions.
.DESCRIPTION
Requires an interactive Windows desktop and the installed x64 Windows App Runtime.
Uses a private synthetic backend; no widget, credentials, or settings files are used.
Does not build, publish, install dependencies, or connect to the real widget.
Preview and explicitly authorized setting edits use only an in-memory synthetic backend.
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
Add-Type -AssemblyName System.Drawing
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
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool SetForegroundWindow(IntPtr window);
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
        window_label_font_size = 9; show_provider_icon = $true
        show_percentage = $true; show_bar = $true; show_reset_time = $true; usage_text_color = $null
        claude_accent = $null; codex_accent = $null; monitor_device = 'smoke-display'; anchor_left = $false
    }
    effective_language = 'en'; claude_code_available = $false
    effective_appearance = @{ dark = $false; claude_accent = @(217, 119, 87); codex_accent = @(15, 143, 112) }
    update_busy = $false; update_available = $true; update_status = 'Synthetic update available'; install_channel = 'portable'
    version = 'smoke-test'; languages = @(@{ code = 'en'; name = 'English' })
    monitors = @(@{ device = 'smoke-display'; primary = $true })
}
$controls = @{
    General = 'LanguageSelector'; Monitoring = 'Setting_show_codex'; Appearance = 'Appearance_palette'
    Placement = 'MonitorSelector'; Update = 'CheckForUpdates'; About = 'ThirdPartyNotices'
    Experimental = 'Setting_allow_unlimited_providers'
}
$pages = @('General', 'Monitoring', 'Appearance', 'Placement', 'Update', 'About', 'Experimental')
$timeoutMilliseconds = $TimeoutSeconds * 1000
$results = [Collections.Generic.List[object]]::new()
$script:smokeBackend = $null
$script:servicingPipe = $false

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
    if ($null -ne $script:smokeBackend -and -not $script:servicingPipe) { Receive-SmokeRequests }
}

function New-PreviewSnapshot([hashtable]$Canonical, [hashtable]$Changes = @{}) {
    $candidate = $Canonical.settings.Clone()
    foreach ($key in $Changes.Keys) { $candidate[$key] = $Changes[$key] }
    if ($Changes.ContainsKey('show_provider_icons')) { $candidate.show_provider_icon = $Changes.show_provider_icons }
    $width = 160
    $height = 24 + 2 * [int]$candidate.window_label_font_size
    $bitmap = [Drawing.Bitmap]::new($width, $height, [Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $stream = [IO.MemoryStream]::new()
    $brushes = [Collections.Generic.List[Drawing.SolidBrush]]::new()
    try {
        $graphics.Clear([Drawing.Color]::FromArgb(255, 243, 243, 243))
        $rgbColor = {
            param($Rgb, [string]$Fallback)
            if ($null -ne $Rgb) { return [Drawing.Color]::FromArgb(255, [int]$Rgb[0], [int]$Rgb[1], [int]$Rgb[2]) }
            [Drawing.ColorTranslator]::FromHtml($Fallback)
        }
        $claude = [Drawing.SolidBrush]::new((& $rgbColor $candidate.claude_accent '#D97757'))
        $codex = [Drawing.SolidBrush]::new((& $rgbColor $candidate.codex_accent '#0F8F70'))
        $text = [Drawing.SolidBrush]::new((& $rgbColor $candidate.usage_text_color '#404040'))
        $icon = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml('#8030C0'))
        foreach ($brush in @($claude, $codex, $text, $icon)) { $brushes.Add($brush) }
        # Deliberately simple, opaque markers test bitmap transport and candidate
        # state, not the Rust renderer's typography or layout implementation.
        if ($candidate.show_bar) {
            $graphics.FillRectangle($claude, 6, 6, 70, 8)
            $graphics.FillRectangle($codex, 6, $height - 14, 96, 8)
        }
        if ($candidate.show_percentage) { $graphics.FillRectangle($text, 108, 6, 30, 10) }
        if ($candidate.show_reset_time) { $graphics.FillRectangle($text, 108, $height - 14, 30, 8) }
        if ($candidate.show_provider_icon) { $graphics.FillRectangle($icon, 142, 6, 12, 12) }
        $bitmap.Save($stream, [Drawing.Imaging.ImageFormat]::Bmp)
        $result = $Canonical.Clone()
        $result.widget_preview_image = 'data:image/bmp;base64,' + [Convert]::ToBase64String($stream.ToArray())
        $result.widget_preview_width = $width
        $result.widget_preview_height = $height
        return $result
    } finally {
        foreach ($brush in $brushes) { $brush.Dispose() }
        $stream.Dispose(); $graphics.Dispose(); $bitmap.Dispose()
    }
}

function Receive-SmokeRequests {
    $backend = $script:smokeBackend
    if ($null -eq $backend -or $script:servicingPipe -or -not $backend.Pipe.IsConnected) { return }
    $script:servicingPipe = $true
    try {
        while ($true) {
            [uint32]$available = 0
            $ok = [SettingsUISmokeNative]::PeekNamedPipe($backend.Pipe.SafePipeHandle, [IntPtr]::Zero, 0, [IntPtr]::Zero, [ref]$available, [IntPtr]::Zero)
            Assert $ok 'Could not inspect the synthetic pipe.'
            if ($available -eq 0) { break }
            $length = [BitConverter]::ToUInt32((Read-Exact $backend.Pipe $backend.Process 4), 0)
            Assert ($length -gt 0 -and $length -le 65536) 'Invalid synthetic request frame length.'
            $request = [Text.UTF8Encoding]::new($false, $true).GetString((Read-Exact $backend.Pipe $backend.Process ([int]$length))) | ConvertFrom-Json -AsHashtable
            Assert ($request.v -eq 1 -and $request.id -is [long]) 'Invalid synthetic request envelope.'
            switch -CaseSensitive ($request.op) {
                preview {
                    Assert ($request.Count -eq 4 -and $request.changes -is [hashtable]) 'Preview must contain a changes object.'
                    foreach ($key in $request.changes.Keys) {
                        Assert ($key -in @('palette', 'bar_style', 'bar_thickness', 'font_size', 'window_label_font_size',
                            'claude_accent', 'codex_accent', 'usage_text_color', 'show_provider_icon', 'show_provider_icons',
                            'show_percentage', 'show_bar', 'show_reset_time')) "Unexpected preview candidate key: $key"
                    }
                    $backend.Previews.Add($request)
                    $response = @{ v = 1; id = $request.id; ok = $true; revision = $backend.Revision;
                        state = (New-PreviewSnapshot $backend.State $request.changes) }
                    $backend.LastPreviewResponse = $response
                    if ($backend.HoldNextPreview) {
                        $backend.HoldNextPreview = $false
                        $backend.HeldPreview = $response
                    } else { Write-Frame $backend.Pipe $backend.Process $response }
                }
                set_batch {
                    Assert ($request.Count -eq 4 -and $request.changes -is [array] -and $request.changes.Count -gt 0) 'Invalid synthetic setting batch.'
                    foreach ($change in $request.changes) {
                        Assert ($backend.AllowedChanges.ContainsKey($change.key)) "Unexpected setting mutation: $($change.key)."
                        Assert (($change.value | ConvertTo-Json -Compress) -ceq ($backend.AllowedChanges[$change.key] | ConvertTo-Json -Compress)) "Unexpected value for $($change.key)."
                        $backend.State.settings[$change.key] = $change.value
                        if ($change.key -in @('show_provider_icon', 'show_provider_icons')) {
                            $backend.State.settings.show_provider_icon = $change.value
                            $backend.State.settings.show_provider_icons = $change.value
                        }
                    }
                    Assert ($backend.State.settings.show_percentage -or $backend.State.settings.show_bar -or $backend.State.settings.show_reset_time) 'Native UI submitted an all-hidden usage configuration.'
                    $backend.Commits.Add($request)
                    $backend.Revision++
                    Write-Frame $backend.Pipe $backend.Process @{ v = 1; id = $request.id; ok = $true;
                        revision = $backend.Revision; state = (New-PreviewSnapshot $backend.State) }
                }
                default { throw "Unexpected backend operation: $($request.op). No requests were forwarded to the widget." }
            }
        }
    } finally { $script:servicingPipe = $false }
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
                Experimental { $control.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -in @([Windows.Automation.ToggleState]::On, [Windows.Automation.ToggleState]::Off) }
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
    Assert ($null -ne $script:smokeBackend -and $Pipe -eq $script:smokeBackend.Pipe) 'No active synthetic backend.'
    $commits = $script:smokeBackend.Commits.Count
    # Cover the 180 ms native preview coalescing interval and pending UI dispatch.
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-Running $script:smokeBackend.Process 'settling preview-only requests'
        Start-Sleep -Milliseconds 20
    } while ($clock.ElapsedMilliseconds -lt 400)
    Receive-SmokeRequests
    Assert ($script:smokeBackend.Commits.Count -eq $commits) 'A preview-only interaction unexpectedly committed settings.'
}

function Show-SmokeControl([Windows.Automation.AutomationElement]$Control, [int]$BudgetMs = -1) {
    # Invoke does not reliably scroll WinUI controls into view. Keep screen-color checks
    # inside the actual ScrollViewer viewport rather than sampling clipped content.
    $walker = [Windows.Automation.TreeWalker]::ControlViewWalker
    $ancestor = $walker.GetParent($Control)
    $scroll = $null
    while ($null -ne $ancestor) {
        if ($ancestor.TryGetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern, [ref]$scroll) -and
            $scroll.Current.VerticallyScrollable) { break }
        $ancestor = $walker.GetParent($ancestor)
    }
    if ($null -eq $ancestor) {
        Assert (-not $Control.Current.IsOffscreen) 'The requested smoke control is offscreen without a scrollable ancestor.'
        return
    }
    $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
    Start-Sleep -Milliseconds 50
    if ($BudgetMs -lt 0) { $BudgetMs = $timeoutMilliseconds }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        $bounds = $Control.Current.BoundingRectangle
        $viewport = $ancestor.Current.BoundingRectangle
        if (-not $Control.Current.IsOffscreen -and $bounds.Height -gt 0 -and
            $bounds.Top -ge $viewport.Top -and $bounds.Bottom -le $viewport.Bottom) { return }
        $scroll.Scroll([Windows.Automation.ScrollAmount]::NoAmount, [Windows.Automation.ScrollAmount]::SmallIncrement)
        Start-Sleep -Milliseconds 50
    } while ($clock.ElapsedMilliseconds -lt $BudgetMs)
    $bounds = $Control.Current.BoundingRectangle
    throw "Could not scroll $($Control.Current.AutomationId) fully into view. Offscreen=$($Control.Current.IsOffscreen) Bounds=$($bounds.Left),$($bounds.Top),$($bounds.Width)x$($bounds.Height)."
}

function Wait-PreviewColor([Windows.Automation.AutomationElement]$Window,
    [Diagnostics.Process]$Process, [string]$Hex, [bool]$Present = $true) {
    # Refetch the preview element on every poll: native page rebuilds can
    # invalidate a previously returned element (Infinity bounds) mid-test.
    $color = [Drawing.ColorTranslator]::FromHtml($Hex)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $script:lastPreviewMatches = -1
    $script:lastPreviewBounds = 'none'
    $script:lastPreviewGate = 'no-poll'
    $script:lastPreviewStatus = 'n/a'
    $firstPoll = $true
    do {
        Assert-Running $Process 'rendering the local accent preview'
        $image = Find-Control $Window 'WidgetPreviewImage'
        $status = Find-Control $Window 'WidgetPreviewStatus'
        if ($firstPoll) {
            $firstPoll = $false
            $script:firstPreviewRequests = $script:smokeBackend.Previews.Count
            $statusName = if ($null -ne $status) { $status.Current.Name } else { '<missing>' }
            $script:lastPreviewStatus = $statusName
            Write-Host "Preview first poll: color=$Hex present=$Present reqCount=$($script:smokeBackend.Previews.Count) imageFound=$($null -ne $image) statusFound=$($null -ne $status) status='$statusName'"
        }
        if ($null -eq $image) { $script:lastPreviewGate = 'image-missing'; Start-Sleep -Milliseconds 50; continue }
        if ($null -eq $status) { $script:lastPreviewGate = 'status-missing'; Start-Sleep -Milliseconds 50; continue }
        if ($status.Current.Name -cne 'Sample values rendered by the widget.') { $script:lastPreviewGate = "status-mismatch:$($status.Current.Name)"; Start-Sleep -Milliseconds 50; continue }
        $bounds = $image.Current.BoundingRectangle
        if ([double]::IsInfinity($bounds.Width) -or $bounds.Width -le 0) { $script:lastPreviewGate = "bad-bounds:$($bounds.Width)x$($bounds.Height)"; Start-Sleep -Milliseconds 50; continue }
        try { Show-SmokeControl $image 1500 } catch { $script:lastPreviewGate = "scroll-error:$($_.Exception.Message)"; Start-Sleep -Milliseconds 100; continue }
        $bounds = $image.Current.BoundingRectangle
        if ([double]::IsInfinity($bounds.Width)) { $script:lastPreviewGate = 'bad-bounds-after-scroll'; continue }
        # Focus a real control rather than the non-focusable top-level UIA window
        # so CopyFromScreen samples this smoke-owned window, not another app.
        try { (Find-Control $Window 'NavigationAppearance').SetFocus() } catch {}
        [SettingsUISmokeNative]::SetForegroundWindow([IntPtr]$Window.Current.NativeWindowHandle) | Out-Null
        # UIA status changes when the bitmap is assigned; allow DWM to present
        # that frame before sampling the desktop pixels.
        Start-Sleep -Milliseconds 100
        $bitmap = [Drawing.Bitmap]::new([int][Math]::Ceiling($bounds.Width), [int][Math]::Ceiling($bounds.Height))
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen([int]$bounds.Left, [int]$bounds.Top, 0, 0, $bitmap.Size)
            # The continuous preview bar has a solid fill; require multiple pixels
            # so an unrelated single text pixel cannot satisfy the preview assertion.
            $matches = 0
            for ($y = 0; $y -lt $bitmap.Height; $y += 2) {
                for ($x = 0; $x -lt $bitmap.Width; $x += 2) {
                    if ($bitmap.GetPixel($x, $y).ToArgb() -eq $color.ToArgb()) { $matches++ }
                }
            }
            if (($Present -and $matches -ge 20) -or (-not $Present -and $matches -eq 0)) { return }
            $script:lastPreviewMatches = $matches
            $script:lastPreviewBounds = "$([int]$bounds.Left),$([int]$bounds.Top),$([int]$bounds.Width)x$([int]$bounds.Height)"
            $script:lastPreviewGate = 'counted'
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
        Start-Sleep -Milliseconds 50
    } while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds)
    $treeIds = @()
    try {
        $all = $Window.FindAll([Windows.Automation.TreeScope]::Descendants,
            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::IsControlElementProperty, $true))
        foreach ($el in $all) {
            try { $treeIds += $el.Current.AutomationId } catch { $treeIds += '<unavailable>' }
            if ($treeIds.Count -ge 150) { break }
        }
    } catch { $treeIds = @("tree-dump-failed:$($_.Exception.Message)") }
    if ($script:smokeBackend.Previews.Count -gt 0) {
        $latest = $script:smokeBackend.Previews[$script:smokeBackend.Previews.Count - 1]
        Write-Host "Preview failure diagnostic: lastChanges=$($latest.changes | ConvertTo-Json -Compress -Depth 4); savedTextColor=$($script:smokeBackend.State.settings.usage_text_color | ConvertTo-Json -Compress -Depth 4)"
    }
    $fixturePixel = '<no preview response>'
    if ($script:smokeBackend.LastPreviewResponse) {
        $data = $script:smokeBackend.LastPreviewResponse.state.widget_preview_image -replace '^data:image/bmp;base64,', ''
        $stream = [IO.MemoryStream]::new([Convert]::FromBase64String($data))
        $fixtureBitmap = [Drawing.Bitmap]::new($stream)
        try { $fixturePixel = $fixtureBitmap.GetPixel(108, 6).ToArgb().ToString('X8') }
        finally { $fixtureBitmap.Dispose(); $stream.Dispose() }
    }
    Write-Host "Preview response diagnostic: fixturePixelAtPercentage=$fixturePixel"
    throw "The widget bitmap preview did not reach color $Hex presence=$Present (last matches=$($script:lastPreviewMatches) bounds=$($script:lastPreviewBounds) gate=$($script:lastPreviewGate) firstStatus='$($script:lastPreviewStatus)' previewRequests=$($script:firstPreviewRequests)->$($script:smokeBackend.Previews.Count) heldPreview=$($null -ne $script:smokeBackend.HeldPreview) holdNext=$($script:smokeBackend.HoldNextPreview)). Keep the interactive smoke window unobscured. TreeIds=$($treeIds -join ',')"
}

function Find-PickerRoot([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process,
    [string]$Key = 'codex_accent') {
    $picker = Find-Control $Window "ColorPicker_$Key"
    if ($null -eq $picker) {
        # WinUI flyouts may be hosted in a separate popup HWND. Scope desktop lookup
        # to the child process rather than matching another application's picker.
        $condition = [Windows.Automation.AndCondition]::new(
            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, $Process.Id),
            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, "ColorPicker_$Key"))
        $picker = [Windows.Automation.AutomationElement]::RootElement.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
    }
    if ($null -ne $picker -and -not $picker.Current.IsOffscreen) {
        $title = switch ($Key) {
            'claude_accent' { 'Claude Code bar accent' }
            'codex_accent' { 'Codex bar accent' }
            default { 'Provider logo and percentage color' }
        }
        Assert ($picker.Current.Name -ceq "$title RGB color") 'The native picker has an unexpected accessible name.'
        return [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($picker)
    }
}

function Open-ColorPicker([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process,
    [string]$Key = 'codex_accent') {
    $choose = Find-Control $Window "ChooseColor_$Key"
    Assert ($null -ne $choose -and $choose.Current.Name -ceq 'Choose color…') 'Missing native Choose color button.'
    Show-SmokeControl $choose
    $choose.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-Running $Process 'opening the native RGB picker'
        $root = Find-PickerRoot $Window $Process $Key
        if ($null -ne $root) { return $root }
        Start-Sleep -Milliseconds 50
    } while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds)
    throw "Choose color did not expose the native $Key RGB picker."
}

function Invoke-PickerAction([Windows.Automation.AutomationElement]$Root, [string]$Name,
    [Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process, [string]$Key = 'codex_accent') {
    $condition = [Windows.Automation.AndCondition]::new(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Name),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Button))
    $button = $Root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
    Assert ($null -ne $button) "The scoped color picker has no $Name button."
    $button.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-Running $Process "dismissing the color picker with $Name"
        if ($null -eq (Find-PickerRoot $Window $Process $Key)) { return }
        Start-Sleep -Milliseconds 50
    } while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds)
    throw "The color picker stayed open after $Name."
}

function Enter-CodexDetail([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process) {
    $pencil = Find-Control $Window 'EditProvider_show_codex'
    Assert ($null -ne $pencil) 'Missing Codex pencil button.'
    Show-SmokeControl $pencil
    $pencil.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-Running $Process 'opening the Codex appearance detail'
        if ($null -ne (Find-Control $Window 'Color_codex_accent')) { return }
        Start-Sleep -Milliseconds 50
    } while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds)
    throw 'Codex appearance detail did not render its accent input.'
}

function Exit-CodexDetail([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process) {
    $back = Find-Control $Window 'ProviderDetailBack'
    Assert ($null -ne $back) 'Missing detail Back button.'
    Show-SmokeControl $back
    $back.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-Running $Process 'returning to the Appearance provider list'
        if ($null -ne (Find-Control $Window 'EditProvider_show_codex')) { return }
        Start-Sleep -Milliseconds 50
    } while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds)
    throw 'Appearance provider list did not return after Back.'
}

function Test-AppearanceDraftPreview([Windows.Automation.AutomationElement]$Window,
    [IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process) {
    foreach ($entry in @{
        Appearance_palette = 'Follow Windows'
        Appearance_bar_style = 'Continuous'
        Appearance_bar_thickness = 'Standard'
        Appearance_font_size = 'Standard'
    }.GetEnumerator()) {
        $control = Find-Control $Window $entry.Key
        Assert ($null -ne $control -and (Test-Selection $control $entry.Value)) "Incorrect hydrated Appearance selection: $($entry.Key)"
    }
    foreach ($entry in @{
        AppearancePreview = 'Live widget preview · sample values'
        AppearancePreviewSummary = 'This image is rendered by the widget using sample usage data.'
    }.GetEnumerator()) {
        $control = Find-Control $Window $entry.Key
        Assert ($null -ne $control -and $control.Current.Name -ceq $entry.Value) "Missing or incorrect Appearance preview: $($entry.Key)"
    }
    Wait-BitmapPreview $Window $Process | Out-Null
    Write-Host 'Appearance phase: default bitmap preview'
    Wait-PreviewColor $Window $Process '#0F8F70'
    Enter-CodexDetail $Window $Process
    Wait-PreviewColor $Window $Process '#0F8F70'
    $input = Find-Control $Window 'Color_codex_accent'
    # Preserve non-normalized casing across Cancel; accidentally committing the
    # picker's normalized RGB value must not pass as an unchanged draft.
    $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('#12ab56')
    Wait-PreviewColor $Window $Process '#12AB56'
    Assert-NoUnexpectedRequests $Pipe

    $pickerRoot = Open-ColorPicker $Window $Process
    Invoke-PickerAction $pickerRoot 'Cancel' $Window $Process
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '#12ab56') 'Picker Cancel mutated the existing hex draft.'
    Assert-NoUnexpectedRequests $Pipe

    $discard = Find-Control $Window 'DiscardColor_codex_accent'
    $discard.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $input = Find-Control $Window 'Color_codex_accent'
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '') 'Discard did not restore the saved default before picker use.'
    $pickerRoot = Open-ColorPicker $Window $Process
    Invoke-PickerAction $pickerRoot 'Use color' $Window $Process
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '#0F8F70') 'Use color did not create the default picker RGB draft.'
    Assert-NoUnexpectedRequests $Pipe
    $discard = Find-Control $Window 'DiscardColor_codex_accent'
    $discard.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $input = Find-Control $Window 'Color_codex_accent'
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '') 'Discard did not restore the saved default after picker use.'
    Wait-PreviewColor $Window $Process '#0F8F70'
    Exit-CodexDetail $Window $Process
    Assert-NoUnexpectedRequests $Pipe
    Assert ($script:smokeBackend.Previews.Count -gt 0) 'Draft editing never requested an actual protocol-v1 preview.'
    Test-StaleBitmapPreview $Window $Pipe $Process
    Test-UsageTextColor $Window $Pipe $Process
    Test-WidgetControls $Window $Pipe $Process
    Write-Host 'Appearance verified: BMP transport, preview-only drafts, stale preview response, RGB picker, usage-text Apply/Discard, label font sizes, provider marks and visibility guard.'
}

function Wait-BitmapPreview([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-Running $Process 'loading the widget bitmap preview'
        $image = Find-Control $Window 'WidgetPreviewImage'
        $status = Find-Control $Window 'WidgetPreviewStatus'
        if ($null -ne $image -and $null -ne $status -and
            $status.Current.Name -ceq 'Sample values rendered by the widget.' -and
            -not [double]::IsInfinity($image.Current.BoundingRectangle.Width) -and $image.Current.BoundingRectangle.Width -gt 0) {
            Assert ($image.Current.Name -ceq 'Widget appearance preview with sample usage values') 'Incorrect bitmap accessible name.'
            return $image
        }
        Start-Sleep -Milliseconds 20
    } while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds)
    throw 'The WidgetPreviewImage did not load with successful WidgetPreviewStatus feedback.'
}

function Wait-SmokeCommit([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process, [int]$Before) {
    Assert-Running $Process 'saving the staged setting change'
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $saveBar = $null
    while ($null -eq $saveBar -and $clock.ElapsedMilliseconds -lt $timeoutMilliseconds) {
        Assert-Running $Process 'showing the manual save action'
        $saveBar = Find-Control $Window 'SavePendingChanges'
        if ($null -eq $saveBar) { Start-Sleep -Milliseconds 20 }
    }
    Assert ($null -ne $saveBar) 'Missing manual save button for the staged setting change.'
    $saveBar.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $clock.Restart()
    do {
        Assert-Running $Process 'acknowledging the authorized synthetic setting change'
        $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Saved. Changes are now in effect.')
        if ($script:smokeBackend.Commits.Count -gt $Before -and
            $null -ne $Window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)) {
            Start-Sleep -Milliseconds 100
            $script:smokeBackend.AllowedChanges.Clear()
            return
        }
        Start-Sleep -Milliseconds 20
    } while ($clock.ElapsedMilliseconds -lt $timeoutMilliseconds)
    throw 'The native UI did not acknowledge the authorized setting batch.'
}

function Test-StaleBitmapPreview([Windows.Automation.AutomationElement]$Window,
    [IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process) {
    Enter-CodexDetail $Window $Process
    # Let the detail page's first render finish before holding the draft response.
    Wait-PreviewColor $Window $Process '#0F8F70'
    $script:smokeBackend.HoldNextPreview = $true
    $input = Find-Control $Window 'Color_codex_accent'
    $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('#654321')
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($null -eq $script:smokeBackend.HeldPreview -and $clock.ElapsedMilliseconds -lt $timeoutMilliseconds) {
        Assert-Running $Process 'holding the first color preview response'
        Start-Sleep -Milliseconds 20
    }
    Assert ($null -ne $script:smokeBackend.HeldPreview) 'The synthetic backend did not receive a coalesced preview.'
    $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('#234567')
    $first = $script:smokeBackend.HeldPreview
    $script:smokeBackend.HeldPreview = $null
    $script:smokeBackend.HoldNextPreview = $true
    Write-Frame $Pipe $Process $first
    $clock.Restart()
    while ($null -eq $script:smokeBackend.HeldPreview -and $clock.ElapsedMilliseconds -lt $timeoutMilliseconds) {
        Assert-Running $Process 'holding the latest preview to inspect stale-response rejection'
        Start-Sleep -Milliseconds 20
    }
    Assert ($null -ne $script:smokeBackend.HeldPreview) 'No replacement preview followed the stale response.'
    # Leave the replacement in flight: an incorrectly accepted old bitmap cannot
    # be hidden by a quickly rendered newer response during this assertion.
    Start-Sleep -Milliseconds 200
    Wait-PreviewColor $Window $Process '#0F8F70'
    Write-Frame $Pipe $Process $script:smokeBackend.HeldPreview
    $script:smokeBackend.HeldPreview = $null
    Wait-PreviewColor $Window $Process '#234567'
    Assert ($script:smokeBackend.State.settings.codex_accent -eq $null) 'Preview responses mutated the canonical saved accent.'
    (Find-Control $Window 'DiscardColor_codex_accent').GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-PreviewColor $Window $Process '#0F8F70'
    Exit-CodexDetail $Window $Process
    Assert-NoUnexpectedRequests $Pipe
}

function Test-UsageTextColor([Windows.Automation.AutomationElement]$Window,
    [IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process) {
    $input = Find-Control $Window 'Color_usage_text_color'
    Assert ($null -ne $input) 'Missing provider logo and percentage color input.'
    $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('#aabbcc')
    Wait-PreviewColor $Window $Process '#AABBCC'
    $root = Open-ColorPicker $Window $Process 'usage_text_color'
    Invoke-PickerAction $root 'Cancel' $Window $Process 'usage_text_color'
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '#aabbcc') 'Usage-text picker Cancel changed the draft.'
    $root = Open-ColorPicker $Window $Process 'usage_text_color'
    Invoke-PickerAction $root 'Use color' $Window $Process 'usage_text_color'
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '#AABBCC') 'Usage-text picker did not normalize its draft.'
    Assert-NoUnexpectedRequests $Pipe
    $before = $script:smokeBackend.Commits.Count
    $script:smokeBackend.AllowedChanges.usage_text_color = @(170, 187, 204)
    (Find-Control $Window 'ApplyColor_usage_text_color').GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-SmokeCommit $Window $Process $before
    $input = Find-Control $Window 'Color_usage_text_color'
    $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('#112233')
    Wait-PreviewColor $Window $Process '#112233'
    (Find-Control $Window 'DiscardColor_usage_text_color').GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $input = Find-Control $Window 'Color_usage_text_color'
    Assert ($input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq '#AABBCC') 'Usage-text Discard did not restore the saved RGB value.'
    Wait-PreviewColor $Window $Process '#AABBCC'
    Assert-NoUnexpectedRequests $Pipe
}

function Set-SmokeToggle([Windows.Automation.AutomationElement]$Window, [Diagnostics.Process]$Process,
    [string]$Id, [string]$Key, [bool]$Value) {
    $control = Find-Control $Window $Id
    Assert ($null -ne $control) "Missing widget toggle: $Id"
    Show-SmokeControl $control
    $pattern = $control.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
    Assert (($pattern.Current.ToggleState -eq [Windows.Automation.ToggleState]::On) -ne $Value) "Toggle $Id already has the target value."
    $before = $script:smokeBackend.Commits.Count
    $script:smokeBackend.AllowedChanges[$Key] = $Value
    if ($Key -in @('show_provider_icon', 'show_provider_icons')) {
        $script:smokeBackend.AllowedChanges.show_provider_icon = $Value
        $script:smokeBackend.AllowedChanges.show_provider_icons = $Value
    }
    $pattern.Toggle()
    Wait-SmokeCommit $Window $Process $before
}

function Test-WidgetControls([Windows.Automation.AutomationElement]$Window,
    [IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process) {
    Write-Host 'Appearance phase: label font sizes'
    foreach ($size in @(8, 14, 9)) {
        $combo = Find-Control $Window 'Appearance_window_label_font_size'
        Assert ($null -ne $combo) 'Missing usage window label font size selector.'
        Show-SmokeControl $combo
        $combo.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, "$size pt")
        $item = $combo.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        Assert ($null -ne $item) "Missing supported label font size: $size pt"
        $before = $script:smokeBackend.Commits.Count
        $script:smokeBackend.AllowedChanges.window_label_font_size = $size
        $item.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
        $combo.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
        Wait-SmokeCommit $Window $Process $before
        $image = Wait-BitmapPreview $Window $Process
        $bounds = $image.Current.BoundingRectangle
        Assert ([Math]::Abs($bounds.Width - 160) -le 2 -and [Math]::Abs($bounds.Height - (24 + 2 * $size)) -le 2) 'Bitmap physical dimensions did not match the selected label size.'
    }
    Select-Page $Window $Process 'General'
    Set-SmokeToggle $Window $Process 'Setting_show_provider_icons' 'show_provider_icons' $false
    Select-Page $Window $Process 'Appearance'
    Write-Host 'Appearance phase: provider icon hidden'
    Wait-PreviewColor $Window $Process '#8030C0' $false
    Select-Page $Window $Process 'General'
    Set-SmokeToggle $Window $Process 'Setting_show_provider_icons' 'show_provider_icons' $true
    Select-Page $Window $Process 'Monitoring'
    Set-SmokeToggle $Window $Process 'Setting_show_percentage' 'show_percentage' $false
    Set-SmokeToggle $Window $Process 'Setting_show_bar' 'show_bar' $false
    $last = Find-Control $Window 'Setting_show_reset_time'
    Show-SmokeControl $last
    $last.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Toggle()
    Assert-NoUnexpectedRequests $Pipe
    $last = Find-Control $Window 'Setting_show_reset_time'
    Assert ($last.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq [Windows.Automation.ToggleState]::On) 'Native UI allowed all three usage values to be hidden.'
    $message = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,
        'Keep at least one of percentage, progress bar, or reset time visible.')
    Assert ($null -ne $Window.FindFirst([Windows.Automation.TreeScope]::Descendants, $message)) 'Visibility guard did not expose durable feedback.'
    $feedback = Find-Control $Window 'SettingsFeedback'
    Assert ($null -ne $feedback -and -not $feedback.Current.IsOffscreen -and $feedback.Current.BoundingRectangle.Height -gt 0) 'Settings InfoBar was not visibly presented.'
    Select-Page $Window $Process 'Appearance'
    Write-Host 'Appearance phase: usage values hidden'
    Wait-PreviewColor $Window $Process '#0F8F70' $false
    Wait-PreviewColor $Window $Process '#AABBCC'
    Select-Page $Window $Process 'Monitoring'
    Set-SmokeToggle $Window $Process 'Setting_show_percentage' 'show_percentage' $true
    Set-SmokeToggle $Window $Process 'Setting_show_bar' 'show_bar' $true
    Select-Page $Window $Process 'Appearance'
    Write-Host 'Appearance phase: usage values restored'
    Wait-PreviewColor $Window $Process '#0F8F70'
    Wait-PreviewColor $Window $Process '#8030C0'
    Assert-NoUnexpectedRequests $Pipe
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
    [IntPtr]$Handle, [IO.Pipes.NamedPipeServerStream]$Pipe, [Diagnostics.Process]$Process,
    [string]$Key = 'codex_accent') {
    if ($Key -ceq 'codex_accent') { Enter-CodexDetail $Window $Process }
    $input = Find-Control $Window "Color_$Key"
    Assert ($null -ne $input) "Missing $Key draft input."
    $savedText = $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value
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
    $discard = Find-Control $Window "DiscardColor_$Key"
    Assert ($null -ne $discard) 'Missing draft Discard button.'
    $discard.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    $input = Find-Control $Window "Color_$Key"
    Assert ($null -ne $input -and $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq $savedText) 'Discard did not restore the synthetic saved color.'

    # Reuse the same pipe after cancellation: a wrongly set Closed flag would ignore this event.
    Write-Frame $Pipe $Process @{ v = 1; event = 'activate'; page = 'general' }
    Wait-Page $Window $Process 'General'
    foreach ($next in $pages) { Select-Page $Window $Process $next }
    Select-Page $Window $Process 'Appearance'
    if ($Key -ceq 'codex_accent') { Enter-CodexDetail $Window $Process }
    $input = Find-Control $Window "Color_$Key"
    Assert ($null -ne $input -and $input.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq $savedText) 'Discarded draft returned after navigation.'
    Assert-NoUnexpectedRequests $Pipe
    if ($Key -ceq 'codex_accent') { Exit-CodexDetail $Window $Process }
    Write-Host "Canceled $Key draft close verified: draft discarded, same pipe reactivated General, all pages navigated, window still usable."
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
    $script:smokeBackend = $null
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
        $caseSnapshot.settings = $snapshot.settings.Clone()
        $caseSnapshot.update_available = $UpdateAvailable
        $script:smokeBackend = @{
            Pipe = $pipe; Process = $process; State = $caseSnapshot; Revision = 1
            AllowedChanges = @{}; Commits = [Collections.Generic.List[object]]::new()
            Previews = [Collections.Generic.List[object]]::new(); HoldNextPreview = $false; HeldPreview = $null
        }
        Write-Frame $pipe $process @{ v = 1; id = $request.id; ok = $true; revision = 1; state = (New-PreviewSnapshot $caseSnapshot) }

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
            Test-AppearanceDraftPreview $window $pipe $process
            Test-DraftCloseCancellation $window $hwnd $pipe $process
            Test-DraftCloseCancellation $window $hwnd $pipe $process 'usage_text_color'
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
        $script:smokeBackend = $null
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
        $script:smokeBackend = $null
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
