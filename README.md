# Codex Usage

Codex Usage is a native Windows taskbar widget for viewing Codex and Claude Code quota usage and reset times.

Developed by **Zoltán Rózsa** · [View project](https://github.com/rozsazoltan/codex-usage-monitor) · Licensed under [AGPL-3.0-or-later](LICENSE).

## How it works

Codex Usage is a portable Windows app that runs locally. For Codex and Claude Code, it reuses credentials from your existing CLI sign-ins; there is no separate Codex Usage account, app login, or API-key setup. It sends authenticated HTTPS requests directly to provider services to read quota usage and reset information; quota requests do not pass through a Codex Usage server. If Anthropic's usage endpoint does not provide reset information, Claude Code polling may fall back to a minimal `.` request to the Anthropic Messages API to read rate-limit headers. Update checks separately contact GitHub Releases.

## Requirements

- Windows 10 or Windows 11.
- Native settings require the shared **Windows App Runtime 1.8, x64**, at the servicing level required by the pinned Windows App SDK (currently 1.8.260921001). Get the x64 installer from [Microsoft's Windows App SDK downloads](https://learn.microsoft.com/windows/apps/windows-app-sdk/downloads). Codex Usage neither bundles nor installs the shared runtime. No .NET runtime is required by the companion.
- For Codex quotas, install Codex CLI and sign in to your account with `codex`. The monitor reads the CLI's local credentials.
- For Claude Code quotas, install Claude Code CLI and sign in with `claude` and `/login`. Claude Desktop sign-in does not provide Claude Code credentials.

## Install

### Portable download

Download `codex-usage.exe` from the [latest release](https://github.com/rozsazoltan/codex-usage-monitor/releases/latest) and run it from the folder where you saved it. The executable runs portably; there is no installer or first-run installation.

The single download embeds the compressed native settings companion, not the shared Windows App Runtime. On the first Settings open, it verifies or extracts its own files to `%LOCALAPPDATA%\CodexUsage\SettingsPayloads\<version>-<content-hash>`. The downloaded executable is not relocated. Version and updater/uninstaller CLI modes do not extract the companion. Cache files are SHA-256 verified on reuse and held read-only while the companion runs; extraction rejects unexpected files and reparse points. This cache is separate from your saved preferences.

### Scoop and `bin`

Install [`marcosnils/bin`](https://github.com/marcosnils/bin) from Scoop's extras bucket, then install Codex Usage:

```powershell
scoop bucket add extras
scoop install extras/bin
bin install github.com/rozsazoltan/codex-usage-monitor
```

## Use

The widget appears on the taskbar, with one icon in the notification area. Right-click the widget or tray icon to open its menu. Use **Settings** to choose which providers to monitor; Codex is enabled by default. Claude Code is available when Claude Code CLI credentials are present.

The widget shows available 5-hour and weekly quota windows.

Use **Appearance** to choose **System**, **Dark**, or **Light** and customize provider colors. Left-click the tray icon to hide or show the widget. Enable **Start with Windows** in Settings for automatic startup.

To print the app version in a terminal, run `.\codex-usage.exe --version` or `.\codex-usage.exe -v` from the folder containing the executable.

## Update and remove

To check for updates, right-click the widget or tray icon, choose **About**, then select **Check for updates**. Confirm the prompt to apply an available update.

To uninstall the app, choose **About** from the context menu and select **Uninstall Codex Usage**. Settings are kept by default; choose whether to remove them in the second confirmation prompt. A portable copy removes only its executable and matching startup entries, not its containing folder. A managed installation also removes its shortcuts, startup entries, and uninstall registration.

For a WinGet installation, use:

```powershell
winget uninstall --id rozsazoltan.codexusage --exact
```

For an installation managed by `bin`, use `bin remove <binary>` (replace `<binary>` with the installed binary name) to remove its package entry as well.

The app's portable/managed uninstaller also removes its private `SettingsPayloads` cache. It never uninstalls the shared Windows App Runtime, and preferences remain unless you explicitly choose to remove them. External package-manager removal may leave this cache behind.

## Troubleshooting

- If native settings cannot start, install or repair the **x64 Windows App Runtime 1.8** using Microsoft's download above; the app reports the prerequisite but does not install it silently.
- If settings payload verification fails, close every Codex Usage instance and delete only the affected version folder under `%LOCALAPPDATA%\CodexUsage\SettingsPayloads`, then restart. Do not delete your preferences or the Windows runtime. Junction/reparse-point cache directories are deliberately unsupported.

- If Codex has no data or authentication fails, run `codex` in a terminal and complete sign-in. Then choose **Refresh** from the app menu.
- If Claude Code has no data or authentication fails, run `claude`, enter `/login`, and complete sign-in. Then choose **Refresh**. Claude Desktop authentication does not count.
- If the app does not start or the widget is not visible, run:

  ```powershell
  .\codex-usage.exe --diagnose
  ```

  Run the command from the folder containing the executable. The diagnostic log is `%TEMP%\codex-usage.log`.

## Contributing

Fork this repository and clone your fork. Install the Rust toolchain, then run the app on Windows from the repository root:

```powershell
cargo run
```

Ordinary `cargo test` and debug builds need no checked-in companion binaries and perform no native restore. To use settings during development, build/publish the companion as described in [settings-ui/BUILDING.md](settings-ui/BUILDING.md); debug builds look in `settings-ui/bin/portable` when no embedded payload was supplied.

### Build the portable release

Install PowerShell 7.2+, Rust's x64 MSVC toolchain, Visual Studio 2022 C++ Build Tools (v143), Windows SDK **10.0.26100.0**, and full .NET Framework MSBuild. Initial native package restore needs nuget.org access. From PowerShell:

```powershell
./scripts/build-release.ps1
# For a nonstandard VS installation:
./scripts/build-release.ps1 -MSBuild 'D:\program\vs-buildtools\2022\MSBuild\Current\Bin\MSBuild.exe'
```

The script restores/rebuilds native XAML with full VS MSBuild (not `dotnet build`), runs `PublishPortable`, packages only the EXE, bootstrap DLL, PRI, and explicitly declared flat XBF resources, validates extraction through Rust tests, then builds `target/release/codex-usage.exe`. Unexpected artifacts or missing files fail the build. Packaging uses Windows XPRESS-Huffman compression and a deterministic, sorted SHA-256 manifest; it does not include runtime packages, PDBs, WebView2, or the build toolchain. Determinism is for identical native inputs, not a guarantee of byte-identical native compilation across toolchain versions.

`build.rs` only embeds pre-generated files and never restores packages or accesses the network. A plain `cargo build --release` deliberately fails without `CODEX_SETTINGS_PAYLOAD` pointing to generated `manifest.json` and `payload.bin`; use the release script rather than publishing a build without settings. After any native change, rerun the full release script. GitHub releases still contain only the portable EXE and its SHA-256 checksum.

## Credits and licenses

This repository is licensed under [AGPL-3.0-or-later](LICENSE).

- [upstream-ray/codex-usage-monitor](https://github.com/upstream-ray/codex-usage-monitor) is the original idea and source basis. This repository branched from v1.10 and is now independently developed. The upstream project is licensed under MIT.
- [lobehub/lobe-icons](https://github.com/lobehub/lobe-icons) provides the provider icon sources and is licensed under MIT.
- [CodeZeno/Claude-Code-Usage-Monitor](https://github.com/CodeZeno/Claude-Code-Usage-Monitor) is licensed under MIT.

Codex Usage is a maintained derivative of CodeZeno/Claude-Code-Usage-Monitor. Thanks to Craig Constable and the upstream contributors for the original project. Changes in this repository are not affiliated with or endorsed by the upstream maintainers or OpenAI.
