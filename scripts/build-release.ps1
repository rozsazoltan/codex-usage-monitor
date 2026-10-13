#requires -Version 7.2
param([string]$MSBuild = $(if ($env:QUOTA_SETTINGS_MSBUILD) { $env:QUOTA_SETTINGS_MSBUILD } else { $env:CODEX_SETTINGS_MSBUILD }))
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $MSBuild) {
    $vswhere = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
    if (-not (Test-Path $vswhere)) { throw 'Install Visual Studio 2022 C++ Build Tools, or pass -MSBuild with the full MSBuild.exe path. See settings-ui/BUILDING.md.' }
    $MSBuild = & $vswhere -latest -version '[17.0,18.0)' -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'MSBuild\Current\Bin\MSBuild.exe' | Select-Object -First 1
}
if (-not $MSBuild -or -not (Test-Path $MSBuild)) { throw 'Full Visual Studio 2022 MSBuild is required; dotnet build is not supported.' }
Push-Location "$PSScriptRoot/.."
$previousQuotaPayload = $env:QUOTA_SETTINGS_PAYLOAD
$portableBuildDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../target/settings-portable-build'))
try {
    & $MSBuild settings-ui/SettingsUI.vcxproj /restore /t:Rebuild /p:Configuration=Release /p:Platform=x64 /m /v:minimal
    if ($LASTEXITCODE) { throw "Native build failed: $LASTEXITCODE" }
    & $MSBuild settings-ui/SettingsUI.vcxproj /t:PublishPortable /p:Configuration=Release /p:Platform=x64 "/p:PortableOutputDir=$portableBuildDirectory" /m /v:minimal
    if ($LASTEXITCODE) { throw "Native publish failed: $LASTEXITCODE" }
    & "$PSScriptRoot/package-settings.ps1" -InputDirectory $portableBuildDirectory
    $env:QUOTA_SETTINGS_PAYLOAD = (Resolve-Path target/settings-payload).Path
    cargo test --locked settings_payload::tests
    if ($LASTEXITCODE) { throw "Payload validation failed: $LASTEXITCODE" }
    cargo build --locked --release
    if ($LASTEXITCODE) { throw "Rust release build failed: $LASTEXITCODE" }
} finally {
    $env:QUOTA_SETTINGS_PAYLOAD = $previousQuotaPayload
    Pop-Location
}
