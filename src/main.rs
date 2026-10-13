#![windows_subsystem = "windows"]
#![recursion_limit = "256"]

use std::io::Write;

mod diagnose;
mod localization;
mod models;
mod native_interop;
mod poller;
mod provider_icons;
mod provider_poll;
mod quota_refresh;
mod quota_text;
mod quota_tooltip;
mod recovery_events;
mod retry_after;
mod settings_payload;
mod settings_store;
mod settings_ui;
mod theme;
mod tray_icon;
mod uninstaller;
mod updater;
mod window;

fn main() {
    let os_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(exit_code) = uninstaller::handle_cli_mode(&os_args) {
        std::process::exit(exit_code);
    }
    match uninstaller::is_uninstall_in_progress() {
        Ok(true) => {
            uninstaller::show_startup_info(
                "An uninstall is already in progress. Please wait for it to finish before starting Quota Usage again.",
            );
            return;
        }
        Ok(false) => {}
        Err(error) => {
            uninstaller::show_startup_error(&error);
            return;
        }
    }

    let args: Vec<String> = std::env::args().collect();
    if version_requested(&args) {
        print_version();
        return;
    }

    let diagnose_enabled = args.iter().any(|arg| arg == "--diagnose");
    if diagnose_enabled {
        match diagnose::init() {
            Ok(path) => {
                diagnose::log(format!("startup args={args:?} log_path={}", path.display()));
                diagnose::log(format!(
                    "version={} install_channel={:?} executable={}",
                    env!("CARGO_PKG_VERSION"),
                    updater::current_install_channel(),
                    std::env::current_exe()
                        .map(|value| value.display().to_string())
                        .unwrap_or_else(|error| format!("unavailable:{error}"))
                ));
            }
            Err(error) => {
                // Logging may not be available yet, but keep startup behavior unchanged.
                let _ = error;
            }
        }
    }

    if let Some(exit_code) = updater::handle_cli_mode(&args) {
        if diagnose_enabled {
            diagnose::log(format!("cli mode exited with code {exit_code}"));
        }
        std::process::exit(exit_code);
    }

    if diagnose_enabled {
        diagnose::log("entering window::run");
    }
    #[cfg(windows)]
    set_app_user_model_id();
    window::run();
}

#[cfg(windows)]
fn set_app_user_model_id() {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    let app_id: Vec<u16> = "com.datarose.quotausage\0".encode_utf16().collect();
    let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(app_id.as_ptr())) };
}

fn version_requested(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "-v" | "--version"))
}

fn version_output() -> String {
    format!("{} {}\n", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

fn print_version() {
    #[cfg(windows)]
    attach_parent_console_if_needed();

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let _ = stdout.write_all(version_output().as_bytes());
    let _ = stdout.flush();
}

#[cfg(windows)]
fn attach_parent_console_if_needed() {
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };

    // Keep inherited handles intact so shell redirection continues to work.
    if unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_err() {
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}

#[cfg(test)]
mod tests {
    use super::{version_output, version_requested};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn recognizes_version_flags_among_other_arguments() {
        assert!(version_requested(&args(&["quota-usage.exe", "-v"])));
        assert!(version_requested(&args(&[
            "quota-usage.exe",
            "--diagnose",
            "--version"
        ])));
    }

    #[test]
    fn does_not_match_partial_or_executable_name_flags() {
        assert!(!version_requested(&args(&[
            "quota-usage.exe",
            "--verbose",
            "-version"
        ])));
        assert!(!version_requested(&args(&["--version"])));
    }

    #[test]
    fn formats_package_name_and_version_on_one_line() {
        assert_eq!(
            version_output(),
            format!("{} {}\n", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
        );
    }
}
