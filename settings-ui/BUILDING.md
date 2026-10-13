# Native settings companion: build and validation

This is an unpackaged, framework-dependent C++20/C++/WinRT WinUI 3 application.
XAML is compiled to XBF and indexed in a PRI. There is no .NET application runtime.
The Rust widget remains the state owner. The companion connects to its private
named pipe, requests authoritative snapshots, and submits typed setting/action
requests; it never reads or writes settings files itself. The General, Monitoring,
Appearance, Placement, and About pages are native WinUI controls. The window uses
Mica where supported, an opaque fallback on Windows 10/high contrast, and is
centered within the nearest monitor's work area.

## Prerequisites

- Visual Studio 2022 C++ Build Tools, v143 x64 toolchain.
- Windows SDK **10.0.26100.0**, including headers, libraries and WinRT metadata.
- Full .NET Framework MSBuild and .NET Framework 4.7.2 or later for the XAML build
  tasks. A `dotnet` SDK is not required; do not use `dotnet build`.
- PowerShell **7.2 or later**, with `pwsh` on `PATH`, for `PublishPortable` and
  the release scripts. Windows PowerShell 5.1 is not supported.
- Internet access to nuget.org for the initial restore.
- To run: the shared **Windows App Runtime 1.8, x64**, at least the servicing
  level required by the pinned Windows App SDK. This project does not install it.

The scripts use `QUOTA_SETTINGS_MSBUILD` to select a nonstandard full MSBuild
path when automatic discovery is insufficient; `CODEX_SETTINGS_MSBUILD` remains
supported as a fallback for existing environments. `QUOTA_SETTINGS_PAYLOAD`
selects a prebuilt payload for a Rust release build, and
`QUOTA_SETTINGS_SKIP_NATIVE_BUILD=1` opts out of native builds for Rust-only debug
work. The corresponding `CODEX_SETTINGS_PAYLOAD` and
`CODEX_SETTINGS_SKIP_NATIVE_BUILD` variables remain supported when their new
`QUOTA_SETTINGS_*` variables are unset.

Pinned NuGet packages:

- `Microsoft.WindowsAppSDK` **1.8.260921001** (stable 1.8 servicing release).
- `Microsoft.Windows.CppWinRT` **2.0.250303.1** (stable 2.x generator).

The native VC project uses desktop targets, so the UWP VS workload is not needed.
Two explicit targets invoke the NuGet XAML compiler before MIDL and before C++
compilation. `XamlSupport.cpp` compiles its generated implementation. No generated
source is checked in or hand-edited. Package restore and build outputs stay here
in ignored `packages/`, `obj/`, and `bin/` directories.

## Build and publish

From the repository root in PowerShell, using the verified installation path:

```powershell
$msbuild = 'D:\program\vs-buildtools\2022\MSBuild\Current\Bin\MSBuild.exe'
& $msbuild settings-ui\SettingsUI.vcxproj /restore /t:Rebuild /p:Configuration=Release /p:Platform=x64 /m /v:minimal
& $msbuild settings-ui\SettingsUI.vcxproj /t:PublishPortable /p:Configuration=Release /p:Platform=x64 /m /v:minimal
```

`PublishPortable` writes to `settings-ui\bin\portable` by default. To validate
the publish target without replacing a running portable copy, pass an alternate
directory, for example `/p:PortableOutputDir=D:\temp\settings-ui-publish`.
The publisher creates a missing destination parent only after source validation.

Publication stages all five files in a unique sibling directory, checks their
SHA-256 hashes and verifies that the source files did not change. It then renames
the old destination to a backup and the staging directory to the destination.
These two renames are **not an atomic directory exchange**: the destination can
be briefly absent. A failed replacement restores the backup where possible;
process termination or power loss cannot be rolled back automatically. A failed
rollback retains the backup and reports its recovery path. Cleanup failures leave
the committed destination intact and report any retained backup.

Existing files are never overwritten in place. If the old files or directory
are locked, close the existing Settings window and retry; the publisher does not
stop processes. Unexpected destination files, subdirectories, and reparse paths
are refused rather than removed. Identical payload content returns `Unchanged`
without replacing the directory.

`scripts\build-release.ps1` publishes to the dedicated ignored
`target\settings-portable-build` directory and passes that directory explicitly
to `scripts\package-settings.ps1 -InputDirectory`. It does not publish or package
the shared developer `settings-ui\bin\portable` directory. The payload still
contains exactly five flat files; the shared Windows App Runtime remains an
external prerequisite and is never bundled or installed by publication.

Change `$msbuild` to the full VS 2022 MSBuild path on other machines. Do not copy
the entire compiler output directory into a release: it includes symbols and
unused SDK-transitive WebView2/background-task artifacts. `PublishPortable`
publishes only the five runtime files:

| File | Observed Release x64 bytes |
| --- | ---: |
| `SettingsUI.exe` | 672,256 |
| `Microsoft.WindowsAppRuntime.Bootstrap.dll` | 399,712 |
| `SettingsUI.pri` | 4,480 |
| `App.xbf` | 690 |
| `MainWindow.xbf` | 2,918 |
| **Total** | **1,080,056** |

Keep these files together. This is a folder deployment, not a standalone EXE.
The executable statically links its C++ CRT; the shared Windows App Runtime has
its own prerequisites. Shared runtime installation size is not included above.

## Runtime smoke test

On an interactive Windows desktop, run from the repository root after publishing:

```powershell
pwsh -NoProfile -File scripts\tests\settings-ui.smoke.ps1
# Test a different coherent payload without building or republishing it:
pwsh -NoProfile -File scripts\tests\settings-ui.smoke.ps1 -CompanionDirectory target\settings-portable-build
```

The PowerShell 7.2+ smoke client hosts a private current-user named pipe in its
own process and supplies a synthetic snapshot. It checks the initial `get`, a
visible HWND, all five startup page routes, UI Automation navigation across all
five pages, and exit code 0 after a shutdown event, pipe disconnect, and ordinary
window close. The client uses Windows' .NET Framework UI Automation assemblies;
this does not add a .NET runtime requirement to the native companion.

The companion runs from an unrelated temporary working directory. Test files
and child diagnostic logs are isolated in a unique directory under
`%TEMP%\opencode` by default (`-TemporaryDirectory` overrides the parent).
The test cleans only its own files and processes. It does not use the widget,
read credentials, change real settings, install a runtime, or invoke update,
uninstall, reset, or provider actions. A failing check exits nonzero. This is a
companion smoke test, not end-to-end validation of the Rust widget.

If a development Settings launch exits before connecting, check which payload
the widget actually uses. Debug builds without embedded settings use
`settings-ui\bin\portable`; `scripts\build-release.ps1` publishes a separate
`target\settings-portable-build` copy and does not refresh that developer folder.
Rebuild and publish the complete five-file payload rather than copying individual
files. A mixed EXE/XBF/PRI can fail XAML connection-ID binding with
`E_NOINTERFACE` (`0x80004002`) and still exit with code 0, without an Application
Error/WER crash record. Startup diagnostics, when emitted by the companion, are
written to `%LOCALAPPDATA%\QuotaUsage-SettingsUI-startup.log`, with a `%TEMP%`
fallback. A settings window that opens successfully but crashes on close is a
separate lifecycle failure; the smoke test checks that path explicitly.

## Observed validation (2026-10-10)

- Full restore and clean Release x64 rebuild succeeded: **0 warnings, 0 errors**.
- MSBuild 17.14.40, MSVC 14.44.35207, Windows SDK 10.0.26100.0, installed .NET
  Framework 4.8.09221. MIDL/MIDLRT, C++/WinRT projection generation, the net472
  XAML compiler, native linker and PRI generation all ran successfully.
- The five-file portable directory launched from an unrelated working directory.
  A real `Quota Usage` settings window had a nonzero HWND. UI Automation selected
  all five pages and verified their principal controls against a synthetic
  protocol snapshot; the companion sent the expected initial `get` request.
- A safe UI Automation interaction emitted the expected `show_codex=false` set
  request. A synthetic backend rejection restored the authoritative enabled
  state and exposed the backend error. Closing the test pipe caused the
  companion to exit. No real settings were changed and no destructive action
  was invoked.
- Window bounds were `(420, 136, 1500, 896)` (1080 × 760) on the test desktop.
- Loaded `Microsoft.UI.Xaml.dll` came from the already-installed x64 runtime
  `Microsoft.WindowsAppRuntime.1.8_8000.994.2142.0`. No runtime was installed.
- Module inspection found no CLR/coreclr/hostfxr modules. `dumpbin /dependents`
  showed OS API imports and the bootstrap DLL, not a .NET runtime dependency.
- Only the smoke-test process was closed after each successful test.

Not yet validated: clean-machine deployment, Windows 10/high-contrast visual
appearance, other architectures, comprehensive accessibility review, and an
end-to-end run against the Rust widget. The UI smoke used a synthetic snapshot;
it does not replace Rust integration testing.

## Upstream references checked

- [Microsoft native WinUI sample project](https://github.com/microsoft/WindowsAppSDK-Samples/blob/main/Samples/Windowing/cpp/cpp-winui/SampleApp/SampleApp.vcxproj)
- [Windows App SDK package versions](https://api.nuget.org/v3-flatcontainer/microsoft.windowsappsdk/index.json)
- [C++/WinRT package versions](https://api.nuget.org/v3-flatcontainer/microsoft.windows.cppwinrt/index.json)

The sample establishes native `ApplicationDefinition`, `Page`, MIDL and
PackageReference patterns; this project intentionally omits its MSIX/UWP workload
dependency and uses the NuGet compiler integration described above.
