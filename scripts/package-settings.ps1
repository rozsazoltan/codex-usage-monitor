#requires -Version 7.2
param(
    [string]$InputDirectory = "$PSScriptRoot/../settings-ui/bin/portable",
    [string]$OutputDirectory = "$PSScriptRoot/../target/settings-payload"
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The payload file list is settings-ui/portable-files.txt: exact relative paths
# with '/' separators in ordinal sort order. It pins the native companion plus
# the self-contained Windows App SDK files and the single en-us XAML satellite
# (without it the companion crashes at startup with 0xC000027B). Only explicit
# native project XAML items may add *.xbf resources to this list; framework
# files change only with the pinned SDK and must be reviewed then.
$listPath = "$PSScriptRoot/../settings-ui/portable-files.txt"
$names = [Collections.Generic.List[string]]::new()
foreach ($line in (Get-Content -LiteralPath $listPath -Encoding utf8NoBOM)) {
    if ($line -ne '') { $names.Add($line) }
}
foreach ($entry in $names) {
    if ($entry -cnotmatch '^[A-Za-z0-9_][A-Za-z0-9_.-]*(/[A-Za-z0-9_.-]+)?$' -or $entry.Contains('\') -or $entry.Contains('..')) {
        throw "Unsupported payload path: $entry"
    }
}
$sorted = $names.ToArray()
[Array]::Sort($sorted, [StringComparer]::Ordinal)
for ($i = 0; $i -lt $sorted.Count; $i++) {
    if ($sorted[$i] -cne $names[$i]) { throw "portable-files.txt must be ordinal-sorted: $($names[$i])" }
}
$unique = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($name in $sorted) {
    if (-not $unique.Add($name)) { throw "Duplicate payload name: $name" }
}
# Every XBF in the list must come from a native project XAML item.
[xml]$project = Get-Content "$PSScriptRoot/../settings-ui/SettingsUI.vcxproj" -Raw
$xaml = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($item in $project.SelectNodes("//*[local-name()='ApplicationDefinition' or local-name()='Page']")) {
    $xaml.Add([IO.Path]::ChangeExtension($item.GetAttribute('Include'), '.xbf')) | Out-Null
}
foreach ($name in $sorted) {
    if ($name -clike '*.xbf' -and -not $xaml.Contains($name)) { throw "XAML resource is not a project item: $name" }
}
foreach ($required in @('App.xbf', 'MainWindow.xbf')) {
    if ($required -cnotin $sorted) { throw "Missing required XAML item: $required" }
}
$inputPath = (Resolve-Path $InputDirectory).Path
$ancestor = [IO.DirectoryInfo]::new($inputPath)
while ($null -ne $ancestor) {
    if ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse input directory: $($ancestor.FullName)" }
    $ancestor = $ancestor.Parent
}
foreach ($file in Get-ChildItem $inputPath -Force) {
    if (($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Unexpected portable artifact: $($file.Name). Publish a clean allowlisted folder."
    }
    if ($file.PSIsContainer) {
        if ($file.Name -cne 'en-us') {
            throw "Unexpected portable artifact: $($file.Name). Publish a clean allowlisted folder."
        }
        foreach ($child in Get-ChildItem $file.FullName -Force) {
            $relative = 'en-us/' + $child.Name
            if ($child.PSIsContainer -or ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $relative -cnotin $sorted) {
                throw "Unexpected portable artifact: $relative. Publish a clean allowlisted folder."
            }
        }
        continue
    }
    if ($file.Name -cnotin $sorted) {
        throw "Unexpected portable artifact: $($file.Name). Publish a clean allowlisted folder."
    }
}
if (-not ('SettingsCompression' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class SettingsCompression {
    [DllImport("cabinet.dll", SetLastError=true)] static extern bool CreateCompressor(uint algorithm, IntPtr allocation, out IntPtr compressor);
    [DllImport("cabinet.dll", SetLastError=true)] static extern bool Compress(IntPtr compressor, byte[] source, UIntPtr size, byte[] destination, UIntPtr capacity, out UIntPtr written);
    [DllImport("cabinet.dll")] static extern bool CloseCompressor(IntPtr compressor);
    public static byte[] Encode(byte[] source) {
        IntPtr handle;
        if (!CreateCompressor(4, IntPtr.Zero, out handle)) throw new Win32Exception();
        try {
            UIntPtr needed;
            Compress(handle, source, (UIntPtr)source.Length, null, UIntPtr.Zero, out needed);
            if (needed.ToUInt64() == 0 || needed.ToUInt64() > 67108864) throw new Win32Exception();
            byte[] result = new byte[(int)needed.ToUInt64()];
            if (!Compress(handle, source, (UIntPtr)source.Length, result, (UIntPtr)result.Length, out needed)) throw new Win32Exception();
            Array.Resize(ref result, (int)needed.ToUInt64());
            return result;
        } finally { CloseCompressor(handle); }
    }
}
'@
}
$stream = [IO.MemoryStream]::new()
$entries = @()
$total = 0L
try {
    foreach ($name in $sorted) {
        $bytes = [IO.File]::ReadAllBytes((Join-Path $inputPath $name))
        if ($bytes.Length -eq 0 -or $bytes.Length -gt 64MB) { throw "Invalid payload size: $name" }
        $compressed = [SettingsCompression]::Encode($bytes)
        $entries += [ordered]@{ name = $name; size = $bytes.Length; sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant(); offset = $stream.Length; compressed_size = $compressed.Length }
        $stream.Write($compressed, 0, $compressed.Length)
        $total += $bytes.Length
        if ($total -gt 64MB -or $stream.Length -gt 64MB) { throw 'Settings payload exceeds the 64 MiB limit' }
    }
    $manifest = [ordered]@{ schema = 1; compression = 'xpress-huff'; files = @($entries) } | ConvertTo-Json -Depth 5 -Compress
    New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $OutputDirectory 'payload.bin'), $stream.ToArray())
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'manifest.json'), $manifest, [Text.UTF8Encoding]::new($false))
    Write-Host "Settings payload: $($entries.Count) files, $total uncompressed bytes, $($stream.Length) compressed bytes, $([Text.Encoding]::UTF8.GetByteCount($manifest)) manifest bytes."
} finally { $stream.Dispose() }
