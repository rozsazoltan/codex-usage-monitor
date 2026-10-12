#requires -Version 7.2
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$MSBuild = $env:CODEX_SETTINGS_MSBUILD
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $MSBuild) {
    $vswhere = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        throw 'Install Visual Studio 2022 C++ Build Tools, or set CODEX_SETTINGS_MSBUILD to the full MSBuild.exe path. See settings-ui/BUILDING.md.'
    }
    $candidates = @(& $vswhere -latest -version '[17.0,18.0)' -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'MSBuild\Current\Bin\MSBuild.exe')
    if ($LASTEXITCODE) { throw "Visual Studio discovery failed: $LASTEXITCODE" }
    $MSBuild = $candidates | Select-Object -First 1
}
if (-not $MSBuild -or -not (Test-Path -LiteralPath $MSBuild -PathType Leaf) -or [IO.Path]::GetFileName($MSBuild) -ine 'MSBuild.exe') {
    throw 'Full Visual Studio 2022 MSBuild.exe is required; dotnet build is not supported. Set CODEX_SETTINGS_MSBUILD or pass -MSBuild. See settings-ui/BUILDING.md.'
}

$output = [IO.Path]::GetFullPath($OutputDirectory)
$native = Join-Path $output 'settings-native'
$portable = Join-Path $native 'portable'
# Global properties override the VC project's defaults, keeping all compiler and
# publish outputs away from any user-open portable copy or release build tree.
$intermediate = (Join-Path $native 'obj') + '\'
$binaries = (Join-Path $native 'bin') + '\'
Push-Location "$PSScriptRoot/.."
try {
    & $MSBuild settings-ui/SettingsUI.vcxproj /restore '/t:Rebuild;PublishPortable' /p:Configuration=Release /p:Platform=x64 /m:1 /v:minimal "/p:BaseIntermediateOutputPath=$intermediate" "/p:IntDir=$intermediate" "/p:OutDir=$binaries" "/p:PortableOutputDir=$portable"
    if ($LASTEXITCODE) { throw "Native Settings build/publish failed: $LASTEXITCODE. Fix the native prerequisites/errors; no stale payload will be embedded. See settings-ui/BUILDING.md." }
    & "$PSScriptRoot/package-settings.ps1" -InputDirectory $portable -OutputDirectory $output
} finally {
    Pop-Location
}
