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
- Internet access to nuget.org for the initial restore.
- To run: the shared **Windows App Runtime 1.8, x64**, at least the servicing
  level required by the pinned Windows App SDK. This project does not install it.

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

Change `$msbuild` to the full VS 2022 MSBuild path on other machines. Do not copy
the entire compiler output directory into a release: it includes symbols and
unused SDK-transitive WebView2/background-task artifacts. `PublishPortable` copies
only the five runtime files to `settings-ui/bin/portable/`:

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

## Observed validation (2026-10-10)

- Full restore and clean Release x64 rebuild succeeded: **0 warnings, 0 errors**.
- MSBuild 17.14.40, MSVC 14.44.35207, Windows SDK 10.0.26100.0, installed .NET
  Framework 4.8.09221. MIDL/MIDLRT, C++/WinRT projection generation, the net472
  XAML compiler, native linker and PRI generation all ran successfully.
- The five-file portable directory launched from an unrelated working directory.
  A real `Codex Usage` settings window had a nonzero HWND. UI Automation selected
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
