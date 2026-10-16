#requires -Version 7.2
<#
.SYNOPSIS
Publishes the five portable Settings UI files without overwriting live files.
.NOTES
SourceDir may contain other build outputs; only the five named files are copied.
DestinationDir may contain only allowlisted files. Its parent is created after
source validation if missing; reparse paths are refused before creation.
Directory replacement uses two renames, not an atomic exchange. Ordinary failures
are rolled back; process termination, power loss, and hostile filesystem races
cannot be recovered automatically. A failed rollback retains the backup.
#>
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'Medium')]
param(
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$SourceDir,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$DestinationDir
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$names = [string[]]@('SettingsUI.exe', 'Microsoft.WindowsAppRuntime.Bootstrap.dll', 'SettingsUI.pri', 'App.xbf', 'MainWindow.xbf')
$allowed = [Collections.Generic.HashSet[string]]::new($names, [StringComparer]::Ordinal)

function Get-PathAttributes([string]$Path) {
    try { [IO.File]::GetAttributes($Path) }
    catch [IO.FileNotFoundException] { $null }
    catch [IO.DirectoryNotFoundException] { $null }
}

function Assert-SafePath([string]$Path, [switch]$Directory) {
    $attributes = Get-PathAttributes $Path
    if ($Directory -and ($null -eq $attributes -or -not ($attributes -band [IO.FileAttributes]::Directory))) {
        throw "Required directory does not exist: $Path"
    }
    $current = $Path
    while ($null -ne $current) {
        $attributes = Get-PathAttributes $current
        if ($null -ne $attributes -and ($attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Reparse paths are not allowed: $current"
        }
        $parent = [IO.Directory]::GetParent($current)
        $current = if ($null -eq $parent) { $null } else { $parent.FullName }
    }
}

function Get-PayloadNames([string]$Path) {
    Assert-SafePath $Path -Directory
    foreach ($entry in [IO.Directory]::EnumerateFileSystemEntries($Path)) {
        $name = [IO.Path]::GetFileName($entry)
        $attributes = Get-PathAttributes $entry
        if (-not $allowed.Contains($name) -or
            ($attributes -band ([IO.FileAttributes]::Directory -bor [IO.FileAttributes]::ReparsePoint))) {
            throw "Unexpected portable artifact: $entry. Refusing to replace unrelated files or directories."
        }
        $name
    }
}

function Get-StreamHash([IO.Stream]$Stream) {
    $Stream.Position = 0
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { [Convert]::ToHexString($hasher.ComputeHash($Stream)) }
    finally { $hasher.Dispose() }
}

function Assert-SourceUnchanged {
    Assert-SafePath $sourcePath -Directory
    foreach ($name in $names) {
        $path = Join-Path $sourcePath $name
        Assert-SafePath $path
        $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            if ($stream.Length -ne $snapshots[$name].Length -or
                [IO.File]::GetLastWriteTimeUtc($path) -ne $snapshots[$name].LastWriteTimeUtc -or
                (Get-StreamHash $stream) -cne $snapshots[$name].Hash) {
                throw "Source changed during publication: $path. Rebuild and retry."
            }
        } finally { $stream.Dispose() }
    }
}

function Remove-OwnedDirectory([string]$Path) {
    # Never recursively remove a directory or follow an unexpected entry.
    $entries = @(Get-PayloadNames $Path)
    foreach ($name in $entries) {
        $file = Join-Path $Path $name
        Assert-SafePath $file
        [IO.File]::Delete($file)
    }
    [IO.Directory]::Delete($Path, $false)
}

$sourceStreams = @{}
$targetStreams = @{}
$snapshots = @{}
$stagingOwned = $false
$backupOwned = $false
$committed = $false
$result = $null
try {
    $provider = $null
    $drive = $null
    $sourcePath = [IO.Path]::TrimEndingDirectorySeparator($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($SourceDir, [ref]$provider, [ref]$drive))
    if ($provider.Name -ne 'FileSystem') { throw 'SourceDir must be a filesystem path.' }
    $destinationPath = [IO.Path]::TrimEndingDirectorySeparator($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($DestinationDir, [ref]$provider, [ref]$drive))
    if ($provider.Name -ne 'FileSystem') { throw 'DestinationDir must be a filesystem path.' }
    $comparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    $sourcePrefix = $sourcePath.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $destinationPrefix = $destinationPath.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if ($sourcePath.Equals($destinationPath, $comparison) -or
        $sourcePath.StartsWith($destinationPrefix, $comparison) -or
        $destinationPath.StartsWith($sourcePrefix, $comparison)) {
        throw 'SourceDir and DestinationDir must be separate, non-overlapping directories.'
    }
    $parentPath = [IO.Path]::GetDirectoryName($destinationPath)
    if ([string]::IsNullOrEmpty($parentPath)) { throw 'DestinationDir cannot be a filesystem root.' }
    Assert-SafePath $sourcePath -Directory
    Assert-SafePath $parentPath
    $parentAttributes = Get-PathAttributes $parentPath
    if ($null -ne $parentAttributes -and -not ($parentAttributes -band [IO.FileAttributes]::Directory)) {
        throw "Destination parent is not a directory: $parentPath"
    }
    Assert-SafePath $destinationPath
    $targetExisted = $null -ne (Get-PathAttributes $destinationPath)
    $targetNames = @(if ($targetExisted) { Get-PayloadNames $destinationPath })

    # Hold all sources open before hashing or copying. Windows denies writes and
    # replacement through these handles; the final check also detects Unix writes.
    foreach ($name in $names) {
        $path = Join-Path $sourcePath $name
        Assert-SafePath $path
        $sourceStreams[$name] = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        if ($sourceStreams[$name].Length -eq 0) { throw "Empty source file: $path" }
    }
    foreach ($name in $names) {
        $snapshots[$name] = @{
            Length = $sourceStreams[$name].Length
            LastWriteTimeUtc = [IO.File]::GetLastWriteTimeUtc((Join-Path $sourcePath $name))
            Hash = Get-StreamHash $sourceStreams[$name]
        }
    }

    $unchanged = $targetNames.Count -eq $names.Count
    if ($unchanged) {
        foreach ($name in $names) {
            $stream = $null
            try {
                $stream = [IO.File]::Open((Join-Path $destinationPath $name), [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
                if ((Get-StreamHash $stream) -cne $snapshots[$name].Hash) { $unchanged = $false }
            } catch {
                throw "Cannot read the existing portable files. Close the existing Settings window and retry. $($_.Exception.Message)"
            } finally { if ($null -ne $stream) { $stream.Dispose() } }
        }
    }
    if ($unchanged) {
        Assert-SourceUnchanged
        $result = [pscustomobject]@{ Status = 'Unchanged'; DestinationDir = $destinationPath; FileCount = $names.Count; BackupDirectory = $null }
    } elseif (-not $PSCmdlet.ShouldProcess($destinationPath, 'Replace the complete portable Settings UI directory')) {
        $result = [pscustomobject]@{ Status = 'Skipped'; DestinationDir = $destinationPath; FileCount = $names.Count; BackupDirectory = $null }
    } else {
        # Preflight descendant locks without writing any bytes. Directory rename
        # behavior varies by filesystem and sharing mode; even delete-sharing
        # child handles can block it, so release our handles just before renaming.
        foreach ($name in $targetNames) {
            try {
                $targetStreams[$name] = [IO.File]::Open((Join-Path $destinationPath $name), [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::Delete)
            } catch {
                throw "Cannot replace the existing portable files. Close the existing Settings window and retry. The destination was not changed. $($_.Exception.Message)"
            }
        }
        Assert-SafePath $parentPath
        [IO.Directory]::CreateDirectory($parentPath) | Out-Null
        Assert-SafePath $parentPath -Directory
        $prefix = '.' + [IO.Path]::GetFileName($destinationPath) + '.publish-' + [Guid]::NewGuid().ToString('N')
        $stagingPath = Join-Path $parentPath ($prefix + '.staging')
        $backupPath = Join-Path $parentPath ($prefix + '.backup')
        if ($null -ne (Get-PathAttributes $stagingPath) -or $null -ne (Get-PathAttributes $backupPath)) {
            throw 'Temporary publication paths already exist. Retry publication.'
        }
        [IO.Directory]::CreateDirectory($stagingPath) | Out-Null
        $stagingOwned = $true
        foreach ($name in $names) {
            $path = Join-Path $stagingPath $name
            $output = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $sourceStreams[$name].Position = 0
                $sourceStreams[$name].CopyTo($output)
                $output.Flush($true)
            } finally { $output.Dispose() }
            if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -cne $snapshots[$name].Hash) {
                throw "Staged file does not match its source: $name"
            }
        }
        if (@(Get-PayloadNames $stagingPath).Count -ne $names.Count) { throw 'Incomplete staging directory.' }
        Assert-SourceUnchanged
        Assert-SafePath $parentPath -Directory
        Assert-SafePath $destinationPath
        if ($targetExisted) {
            $currentNames = @(Get-PayloadNames $destinationPath)
            if ($currentNames.Count -ne $targetNames.Count -or @($currentNames | Where-Object { $_ -cnotin $targetNames }).Count -ne 0) {
                throw 'Destination changed during publication. Retry publication.'
            }
        } elseif ($null -ne (Get-PathAttributes $destinationPath)) {
            throw 'Destination appeared during publication. Refusing to replace it.'
        }
        foreach ($stream in $targetStreams.Values) { $stream.Dispose() }
        $targetStreams.Clear()

        try {
            if ($targetExisted) {
                [IO.Directory]::Move($destinationPath, $backupPath)
                $backupOwned = $true
            }
            [IO.Directory]::Move($stagingPath, $destinationPath)
            $stagingOwned = $false
            $committed = $true
        } catch {
            $publishError = $_.Exception.Message
            if ($backupOwned) {
                try {
                    Assert-SafePath $parentPath -Directory
                    if ($null -ne (Get-PathAttributes $destinationPath)) { throw 'Destination was occupied by another operation.' }
                    Assert-SafePath $backupPath -Directory
                    [IO.Directory]::Move($backupPath, $destinationPath)
                    $backupOwned = $false
                } catch {
                    throw "Publication and rollback failed. Original files are retained at '$backupPath'; restore that directory to '$destinationPath' after resolving the conflict. Rollback: $($_.Exception.Message) Publication: $publishError"
                }
            }
            throw "Cannot replace the portable directory. Close the existing Settings window and retry. Original destination was preserved or restored. $publishError"
        }
        $result = [pscustomobject]@{ Status = 'Published'; DestinationDir = $destinationPath; FileCount = $names.Count; BackupDirectory = $null }
    }
} catch {
    throw "Settings publish failed: $($_.Exception.Message)"
} finally {
    foreach ($stream in $targetStreams.Values) { $stream.Dispose() }
    foreach ($stream in $sourceStreams.Values) { $stream.Dispose() }
    if ($stagingOwned) {
        try { Remove-OwnedDirectory $stagingPath }
        catch { Write-Warning "Retained staging directory '$stagingPath': $($_.Exception.Message)" }
    }
    if ($committed -and $backupOwned) {
        try { Remove-OwnedDirectory $backupPath }
        catch {
            $result.BackupDirectory = $backupPath
            Write-Warning "Publication succeeded; retained backup '$backupPath': $($_.Exception.Message)"
        }
    }
}
$result
