#requires -Version 7.2
[CmdletBinding()]
param([string]$TemporaryDirectory = [IO.Path]::GetTempPath())
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$publisher = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../publish-settings.ps1'))
$names = @('SettingsUI.exe', 'Microsoft.WindowsAppRuntime.Bootstrap.dll', 'SettingsUI.pri', 'App.xbf', 'MainWindow.xbf')
$root = Join-Path $TemporaryDirectory ('settings-publish-tests-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($root) | Out-Null
$results = [Collections.Generic.List[object]]::new()

function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function New-Fixture {
    $path = Join-Path $root ([Guid]::NewGuid().ToString('N'))
    $source = Join-Path $path 'source build'
    $destination = Join-Path $path 'portable output'
    [IO.Directory]::CreateDirectory($source) | Out-Null
    [IO.Directory]::CreateDirectory($destination) | Out-Null
    foreach ($name in $names) {
        [IO.File]::WriteAllText((Join-Path $source $name), "new payload: $name", [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText((Join-Path $destination $name), "old payload: $name", [Text.UTF8Encoding]::new($false))
    }
    @{ Root = $path; Source = $source; Destination = $destination }
}

function Get-Hashes([string]$Path) {
    $hashes = @{}
    foreach ($name in $names) { $hashes[$name] = (Get-FileHash -LiteralPath (Join-Path $Path $name) -Algorithm SHA256).Hash }
    $hashes
}

function Assert-Hashes([string]$Path, [hashtable]$Expected) {
    Assert ([IO.Directory]::Exists($Path)) "Destination is missing: $Path"
    foreach ($name in $names) {
        Assert ((Get-FileHash -LiteralPath (Join-Path $Path $name) -Algorithm SHA256).Hash -ceq $Expected[$name]) "Content changed unexpectedly: $name"
    }
}

function Assert-NoTemporaryDirectories([string]$Path) {
    Assert (@(Get-ChildItem -LiteralPath $Path -Force | Where-Object Name -Like '*.publish-*').Count -eq 0) 'Publication left a staging or backup directory.'
}

function Assert-Refusal([scriptblock]$Action, [string]$Pattern) {
    try { & $Action | Out-Null }
    catch {
        Assert ($_.Exception.Message -match $Pattern) "Unexpected failure: $($_.Exception.Message)"
        return
    }
    throw 'Expected publication to fail.'
}

function Test-Case([string]$Name, [scriptblock]$Action) {
    try {
        & $Action
        $results.Add([pscustomobject]@{ Test = $Name; Status = 'Passed' })
    } catch {
        $results.Add([pscustomobject]@{ Test = $Name; Status = 'Failed'; Error = $_.Exception.Message })
    }
}

try {
    Test-Case 'New destination contains exactly five files; unrelated build outputs stay excluded' {
        $fixture = New-Fixture
        Remove-Item -LiteralPath $fixture.Destination -Recurse -Force
        $fixture.Destination = Join-Path $fixture.Root 'new parent/nested/portable output'
        $parent = [IO.Path]::GetDirectoryName($fixture.Destination)
        Assert (-not [IO.Directory]::Exists($parent)) 'Destination parent already existed before publication.'
        [IO.File]::WriteAllText((Join-Path $fixture.Source 'SettingsUI.pdb'), 'not portable')
        [IO.Directory]::CreateDirectory((Join-Path $fixture.Source 'runtime bundle')) | Out-Null
        $result = & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination
        Assert ($result.Status -eq 'Published') 'New destination was not published.'
        Assert (@(Get-ChildItem -LiteralPath $fixture.Destination -Force).Count -eq 5) 'Published payload was not exactly five flat files.'
        Assert-Hashes $fixture.Destination (Get-Hashes $fixture.Source)
        Assert-NoTemporaryDirectories $parent
    }
    Test-Case 'Whole-directory replacement and identical-content fast path' {
        $fixture = New-Fixture
        $result = & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination
        Assert ($result.Status -eq 'Published') 'Replacement did not publish.'
        Assert-Hashes $fixture.Destination (Get-Hashes $fixture.Source)
        $timestamps = @{}
        foreach ($name in $names) { $timestamps[$name] = [IO.File]::GetLastWriteTimeUtc((Join-Path $fixture.Destination $name)) }
        $result = & $publisher -SourceDir $fixture.Source -DestinationDir ($fixture.Destination + [IO.Path]::DirectorySeparatorChar)
        Assert ($result.Status -eq 'Unchanged') 'Identical content was not recognized.'
        foreach ($name in $names) { Assert ([IO.File]::GetLastWriteTimeUtc((Join-Path $fixture.Destination $name)) -eq $timestamps[$name]) "Repeat publication rewrote $name" }
        Assert-NoTemporaryDirectories $fixture.Root
    }
    Test-Case 'An incomplete but allowlisted old destination can be repaired' {
        $fixture = New-Fixture
        [IO.File]::Delete((Join-Path $fixture.Destination 'MainWindow.xbf'))
        $result = & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination
        Assert ($result.Status -eq 'Published') 'Incomplete destination was not repaired.'
        Assert-Hashes $fixture.Destination (Get-Hashes $fixture.Source)
        Assert-NoTemporaryDirectories $fixture.Root
    }
    Test-Case 'Missing and empty source files fail before changing the old destination' {
        foreach ($empty in @($false, $true)) {
            $fixture = New-Fixture
            $old = Get-Hashes $fixture.Destination
            $file = Join-Path $fixture.Source 'MainWindow.xbf'
            if ($empty) { [IO.File]::WriteAllBytes($file, [byte[]]@()) } else { [IO.File]::Delete($file) }
            Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination } 'Settings publish failed:'
            Assert-Hashes $fixture.Destination $old
            Assert-NoTemporaryDirectories $fixture.Root
            $missingParent = Join-Path $fixture.Root 'absent parent'
            Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir (Join-Path $missingParent 'portable') } 'Settings publish failed:'
            Assert (-not [IO.Directory]::Exists($missingParent)) 'Invalid source created a destination parent.'
        }
    }
    Test-Case 'WhatIf leaves the complete old payload untouched' {
        $fixture = New-Fixture
        $old = Get-Hashes $fixture.Destination
        $result = & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination -WhatIf
        Assert ($result.Status -eq 'Skipped') 'WhatIf did not skip publication.'
        Assert-Hashes $fixture.Destination $old
        Assert-NoTemporaryDirectories $fixture.Root
        $missingParent = Join-Path $fixture.Root 'absent parent'
        $result = & $publisher -SourceDir $fixture.Source -DestinationDir (Join-Path $missingParent 'portable') -WhatIf
        Assert ($result.Status -eq 'Skipped') 'WhatIf did not skip a new destination.'
        Assert (-not [IO.Directory]::Exists($missingParent)) 'WhatIf created a destination parent.'
    }
    Test-Case 'Unknown destination files and directories are preserved and refused' {
        foreach ($directory in @($false, $true)) {
            $fixture = New-Fixture
            $old = Get-Hashes $fixture.Destination
            $extra = Join-Path $fixture.Destination 'user data'
            if ($directory) { [IO.Directory]::CreateDirectory($extra) | Out-Null } else { [IO.File]::WriteAllText($extra, 'user-owned') }
            Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination } 'Unexpected portable artifact'
            Assert (Test-Path -LiteralPath $extra) 'Unrelated item was removed.'
            if (-not $directory) { Assert ([IO.File]::ReadAllText($extra) -ceq 'user-owned') 'Unrelated file content changed.' }
            Assert-Hashes $fixture.Destination $old
            Assert-NoTemporaryDirectories $fixture.Root
        }
    }
    Test-Case 'Source and destination overlap is refused' {
        $fixture = New-Fixture
        $old = Get-Hashes $fixture.Source
        foreach ($destination in @($fixture.Source, (Join-Path $fixture.Source 'nested'))) {
            Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $destination } 'non-overlapping'
        }
        Assert-Hashes $fixture.Source $old
        Assert-NoTemporaryDirectories $fixture.Root
    }
    Test-Case 'Reparse source, destination, and ancestor paths are refused' {
        $fixture = New-Fixture
        $old = Get-Hashes $fixture.Destination
        $link = Join-Path $fixture.Root 'linked directory'
        $linkType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        New-Item -ItemType $linkType -Path $link -Target $fixture.Source | Out-Null
        Assert-Refusal { & $publisher -SourceDir $link -DestinationDir $fixture.Destination } 'Reparse paths'
        Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir (Join-Path $link 'nested destination') } 'Reparse paths|non-overlapping'
        Remove-Item -LiteralPath $link -Force
        New-Item -ItemType $linkType -Path $link -Target $fixture.Destination | Out-Null
        Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $link } 'Reparse paths'
        Assert-Hashes $fixture.Destination $old
        Assert-NoTemporaryDirectories $fixture.Root
    }
    Test-Case 'A reparse entry named as a payload file is refused' {
        $fixture = New-Fixture
        $old = Get-Hashes $fixture.Destination
        $entry = Join-Path $fixture.Source 'App.xbf'
        [IO.File]::Delete($entry)
        $linkType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        New-Item -ItemType $linkType -Path $entry -Target $fixture.Destination | Out-Null
        Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination } 'Reparse paths'
        Assert-Hashes $fixture.Destination $old
        Assert-NoTemporaryDirectories $fixture.Root
    }
    Test-Case 'Source timestamp modification during copying fails with all old hashes intact' {
        $fixture = New-Fixture
        $old = Get-Hashes $fixture.Destination
        $lines = [IO.File]::ReadAllLines($publisher)
        $line = @(for ($i = 0; $i -lt $lines.Length; $i++) { if ($lines[$i].Trim() -ceq '$sourceStreams[$name].CopyTo($output)') { $i + 1 } })
        Assert ($line.Count -eq 1) 'Could not locate copying for fault injection.'
        $global:SettingsPublishRegressionSourceChanged = $false
        $breakpoint = Set-PSBreakpoint -Script $publisher -Line $line[0] -Action {
            if ($name -ceq 'MainWindow.xbf') {
                $path = Join-Path $sourcePath 'App.xbf'
                [IO.File]::SetLastWriteTimeUtc($path, [IO.File]::GetLastWriteTimeUtc($path).AddMinutes(1))
                $global:SettingsPublishRegressionSourceChanged = $true
            }
        }
        try {
            Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination } 'Source changed during publication'
            Assert $global:SettingsPublishRegressionSourceChanged 'Source modification fault was not injected.'
        } finally {
            Remove-PSBreakpoint -Breakpoint $breakpoint
            Remove-Variable -Name SettingsPublishRegressionSourceChanged -Scope Global
        }
        Assert-Hashes $fixture.Destination $old
        Assert-NoTemporaryDirectories $fixture.Root
    }

    if ($IsWindows) {
        Test-Case 'Locked old file (FileShare.None) refuses publication and preserves all old hashes' {
            $fixture = New-Fixture
            $old = Get-Hashes $fixture.Destination
            $lock = [IO.File]::Open((Join-Path $fixture.Destination 'SettingsUI.exe'), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
            try { Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination } 'Close the existing Settings window' }
            finally { $lock.Dispose() }
            Assert-Hashes $fixture.Destination $old
            Assert-NoTemporaryDirectories $fixture.Root
        }
        Test-Case 'Readable old file without write/delete sharing also refuses replacement' {
            $fixture = New-Fixture
            $old = Get-Hashes $fixture.Destination
            $lock = [IO.File]::Open((Join-Path $fixture.Destination 'MainWindow.xbf'), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
            try { Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination } 'Close the existing Settings window' }
            finally { $lock.Dispose() }
            Assert-Hashes $fixture.Destination $old
            Assert-NoTemporaryDirectories $fixture.Root
        }
        Test-Case 'Locked destination directory fails its rename without changing old hashes' {
            if (-not ('SettingsPublishTestLocks' -as [type])) {
                Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class SettingsPublishTestLocks {
    [DllImport("kernel32.dll", EntryPoint = "CreateFileW", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFile(string path, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    public static SafeFileHandle OpenDirectory(string path) {
        var handle = CreateFile(path, 0x80000000, 3, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
        if (handle.IsInvalid) {
            int error = Marshal.GetLastWin32Error();
            handle.Dispose();
            throw new Win32Exception(error);
        }
        return handle;
    }
}
'@
            }
            $fixture = New-Fixture
            $old = Get-Hashes $fixture.Destination
            $lock = [SettingsPublishTestLocks]::OpenDirectory($fixture.Destination)
            try { Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination } 'Close the existing Settings window' }
            finally { $lock.Dispose() }
            Assert-Hashes $fixture.Destination $old
            Assert-NoTemporaryDirectories $fixture.Root
        }
        Test-Case 'Failure of the staging rename restores the backup and safely retains locked staging' {
            $fixture = New-Fixture
            $old = Get-Hashes $fixture.Destination
            $lines = [IO.File]::ReadAllLines($publisher)
            $line = @(for ($i = 0; $i -lt $lines.Length; $i++) { if ($lines[$i].Trim() -ceq '[IO.Directory]::Move($stagingPath, $destinationPath)') { $i + 1 } })
            Assert ($line.Count -eq 1) 'Could not locate the staging rename for fault injection.'
            $global:SettingsPublishRegressionStageLock = $null
            $global:SettingsPublishRegressionStagePath = $null
            $breakpoint = Set-PSBreakpoint -Script $publisher -Line $line[0] -Action {
                $global:SettingsPublishRegressionStagePath = $stagingPath
                $global:SettingsPublishRegressionStageLock = [SettingsPublishTestLocks]::OpenDirectory($stagingPath)
            }
            try {
                Assert-Refusal { & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination -WarningAction SilentlyContinue } 'Original destination was preserved or restored'
                Assert ($null -ne $global:SettingsPublishRegressionStageLock) 'Staging rename fault was not injected.'
                Assert-Hashes $fixture.Destination $old
                Assert (@(Get-ChildItem -LiteralPath $fixture.Root -Force | Where-Object Name -Like '*.backup').Count -eq 0) 'Backup was not restored to the destination.'
            } finally {
                Remove-PSBreakpoint -Breakpoint $breakpoint
                if ($null -ne $global:SettingsPublishRegressionStageLock) { $global:SettingsPublishRegressionStageLock.Dispose() }
                if ($null -ne $global:SettingsPublishRegressionStagePath -and [IO.Directory]::Exists($global:SettingsPublishRegressionStagePath)) {
                    [IO.Directory]::Delete($global:SettingsPublishRegressionStagePath, $false)
                }
                Remove-Variable -Name SettingsPublishRegressionStageLock, SettingsPublishRegressionStagePath -Scope Global
            }
            Assert-NoTemporaryDirectories $fixture.Root
        }
        Test-Case 'Backup cleanup failure retains the backup without losing the committed payload' {
            $fixture = New-Fixture
            $expected = Get-Hashes $fixture.Source
            $lines = [IO.File]::ReadAllLines($publisher)
            $line = @(for ($i = 0; $i -lt $lines.Length; $i++) { if ($lines[$i].Trim() -ceq 'try { Remove-OwnedDirectory $backupPath }') { $i + 1 } })
            Assert ($line.Count -eq 1) 'Could not locate backup cleanup for fault injection.'
            $global:SettingsPublishRegressionBackupLock = $null
            $breakpoint = Set-PSBreakpoint -Script $publisher -Line $line[0] -Action {
                $global:SettingsPublishRegressionBackupLock = [SettingsPublishTestLocks]::OpenDirectory($backupPath)
            }
            try {
                $result = & $publisher -SourceDir $fixture.Source -DestinationDir $fixture.Destination -WarningAction SilentlyContinue
                Assert ($null -ne $global:SettingsPublishRegressionBackupLock) 'Backup cleanup fault was not injected.'
                Assert ($result.Status -eq 'Published') 'Cleanup failure incorrectly failed publication.'
                Assert ($null -ne $result.BackupDirectory -and [IO.Directory]::Exists($result.BackupDirectory)) 'Retained backup was not reported.'
                Assert-Hashes $fixture.Destination $expected
            } finally {
                Remove-PSBreakpoint -Breakpoint $breakpoint
                if ($null -ne $global:SettingsPublishRegressionBackupLock) { $global:SettingsPublishRegressionBackupLock.Dispose() }
                Remove-Variable -Name SettingsPublishRegressionBackupLock -Scope Global
            }
            [IO.Directory]::Delete($result.BackupDirectory, $false)
            Assert-NoTemporaryDirectories $fixture.Root
        }
    } else {
        $results.Add([pscustomobject]@{ Test = 'Windows file and directory lock regressions'; Status = 'Skipped' })
    }

    $results | Format-Table -AutoSize
    $failed = @($results | Where-Object Status -EQ 'Failed')
    if ($failed.Count) {
        $failed | Format-List Test, Error
        throw "$($failed.Count) publish-settings regression test(s) failed."
    }
    Write-Host "Passed: $(@($results | Where-Object Status -EQ 'Passed').Count); skipped: $(@($results | Where-Object Status -EQ 'Skipped').Count)."
} finally {
    # This random test root and every fixture below it belong to this invocation.
    Remove-Item -LiteralPath $root -Recurse -Force
}
