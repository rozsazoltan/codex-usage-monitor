# Codex Usage

Codex Usage is a native Windows taskbar widget for viewing Codex and Claude Code quota usage and reset times.

## Requirements

- Windows 10 or Windows 11.
- For Codex quotas, install Codex CLI and sign in to your account with `codex`. The monitor reads the CLI's local credentials.
- For Claude Code quotas, install Claude Code CLI and sign in with `claude` and `/login`. Claude Desktop sign-in does not provide Claude Code credentials.

## Install

### Portable download

Download `codex-usage.exe` from the [latest release](https://github.com/rozsazoltan/codex-usage-monitor/releases/latest) and run it from the folder where you saved it. The executable runs portably; there is no installer or first-run installation.

### Scoop and `bin`

Install [`marcosnils/bin`](https://github.com/marcosnils/bin) from Scoop's extras bucket, then install Codex Usage:

```powershell
scoop bucket add extras
scoop install extras/bin
bin install github.com/rozsazoltan/codex-usage-monitor
```

## Use

The widget appears on the taskbar, with one icon in the notification area. Right-click the widget or tray icon to open its menu. Use **Models** to enable Codex, Claude Code, or both; each provider displays its own quota data. Codex is enabled by default. Claude Code is unavailable until Claude Code CLI credentials are present.

The widget shows available 5-hour and weekly quota windows. Use **Settings → Appearance** to select **High contrast dark** or **High contrast light** and customize provider colors. Left-click the tray icon to hide or show the widget. Enable **Start with Windows** in the menu for automatic startup.

To print the app version in a terminal, run `.\codex-usage.exe --version` or `.\codex-usage.exe -v` from the folder containing the executable.

## Update and remove

Right-click the widget or tray icon and choose **Settings → Check for Updates**. Confirm the prompt to apply an available update.

Choose **Uninstall…** from the context menu to remove the app. Settings are kept by default; choose to remove them in the second confirmation prompt. A portable copy removes only its executable and matching startup entries, not its containing folder. A managed installation also removes its shortcuts, startup entries, and uninstall registration.

For a WinGet installation, use:

```powershell
winget uninstall --id rozsazoltan.codexusage --exact
```

For an installation managed by `bin`, use `bin remove <binary>` (replace `<binary>` with the installed binary name) to remove its package entry as well.

## Troubleshooting

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

## License and acknowledgment

Codex Usage is licensed under [AGPL-3.0-or-later](LICENSE).

Thanks to [upstream-ray/codex-usage-monitor](https://github.com/upstream-ray/codex-usage-monitor) for the initial structure and idea.
