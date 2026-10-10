//! Safe uninstall planning and detached cleanup for Windows installations.
//!
//! The helper runs from a unique temporary staging directory. It opens and
//! retains the parent process handle before acknowledging readiness, then waits
//! for normal parent exit before touching any planned artifact. A completed
//! uninstall can leave its running helper in `%TEMP%`; later uninstall attempts
//! remove at most 16 matching staging directories older than 24 hours.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND,
    ERROR_PATH_NOT_FOUND, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateEventW, OpenEventW, OpenProcess, QueryFullProcessImageNameW, SetEvent,
    WaitForSingleObject, EVENT_MODIFY_STATE, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, PostQuitMessage, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
};

const PRIVATE_HELPER_ARG: &str = "--codex-usage-uninstall-helper";
const HELPER_EXE_NAME: &str = "codex-usage-uninstall-helper.exe";
const HELPER_DIR_PREFIX: &str = "CodexUsageUninstall-";
const MANIFEST_NAME: &str = "manifest.bin";
const MANIFEST_MAGIC: &[u8] = b"CodexUsageUninstallV1\n";
const UNINSTALL_EVENT: &str = "Local\\CodexUsageUninstallInProgress";
const READY_TIMEOUT_MS: u32 = 10_000;
const HELPER_CANCEL_GRACE: Duration = Duration::from_millis(250);
const HELPER_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const HELPER_POLL_INTERVAL: Duration = Duration::from_millis(25);
const STALE_STAGE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_STALE_STAGES_PER_RUN: usize = 16;
const MAX_REGISTRY_STRING_BYTES: u32 = 131_072;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

static STAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Installation method detected for the running executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UninstallKind {
    /// Installed by the managed installer under `%LOCALAPPDATA%`.
    Managed,
    /// Standalone executable outside a recognized managed or WinGet location.
    Portable,
    /// Installed under a recognized WinGet package root.
    Winget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CleanupTarget {
    File(PathBuf),
    Directory(PathBuf),
    StartupValue {
        name: &'static str,
        only_if_executable: Option<PathBuf>,
    },
    UninstallRegistryTree,
}

#[derive(Clone, Debug)]
struct CleanupPlan {
    kind: UninstallKind,
    executable: PathBuf,
    targets: Vec<CleanupTarget>,
}

#[derive(Clone, Debug, Default)]
struct CleanupPaths {
    managed_install_dir: Option<PathBuf>,
    start_menu_shortcut: Option<PathBuf>,
    desktop_shortcut: Option<PathBuf>,
    settings_dir: Option<PathBuf>,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct PrivateEventNames {
    ready: String,
    cancel: String,
    commit: String,
}

struct PrivateEvents {
    ready: OwnedHandle,
    cancel: OwnedHandle,
    commit: OwnedHandle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitState {
    Signaled,
    TimedOut,
    Failed,
    Unexpected(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelperWaitDecision {
    Cancel,
    Commit,
    ParentExited,
    Continue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StageIdentity {
    parent_pid: u32,
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Detect managed, WinGet, or portable installation using exact path evidence.
pub fn uninstall_kind() -> UninstallKind {
    let Ok(executable) = std::env::current_exe() else {
        return UninstallKind::Portable;
    };
    detect_kind_for_executable(&executable)
}

fn create_named_event(name: &str, initially_signaled: bool) -> Result<OwnedHandle, String> {
    let name = wide(name);
    unsafe { SetLastError(WIN32_ERROR(0)) };
    let handle = unsafe {
        CreateEventW(
            None,
            true,
            initially_signaled,
            PCWSTR::from_raw(name.as_ptr()),
        )
    }
    .map_err(|error| format!("Unable to create named uninstall event: {error}"))?;
    Ok(OwnedHandle(handle))
}

fn create_global_uninstall_gate() -> Result<OwnedHandle, String> {
    let gate = create_named_event(UNINSTALL_EVENT, true)?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err("Another Codex Usage uninstall is already in progress.".to_string());
    }
    Ok(gate)
}

fn private_event_names(stage: &Path) -> Result<PrivateEventNames, String> {
    let basename = stage
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("Invalid uninstall staging directory name: {stage:?}."))?;
    if parse_stage_identity(basename).is_none() {
        return Err(format!(
            "Refusing invalid uninstall staging directory name: {stage:?}."
        ));
    }
    Ok(PrivateEventNames {
        ready: format!("Local\\{basename}-Ready"),
        cancel: format!("Local\\{basename}-Cancel"),
        commit: format!("Local\\{basename}-Commit"),
    })
}

fn parse_stage_identity(basename: &str) -> Option<StageIdentity> {
    let components = basename
        .strip_prefix(HELPER_DIR_PREFIX)?
        .split('-')
        .collect::<Vec<_>>();
    if components.len() != 3
        || components[0].is_empty()
        || !components[0].bytes().all(|byte| byte.is_ascii_digit())
        || components[1].len() != 32
        || !components[1]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || components[2].len() != 16
        || !components[2]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let parent_pid = components[0].parse::<u32>().ok().filter(|pid| *pid != 0)?;
    if parent_pid.to_string() != components[0] {
        return None;
    }
    u128::from_str_radix(components[1], 16).ok()?;
    u64::from_str_radix(components[2], 16).ok()?;
    Some(StageIdentity { parent_pid })
}

fn create_private_events(names: &PrivateEventNames) -> Result<PrivateEvents, String> {
    let ready = create_named_event(&names.ready, false)
        .map_err(|error| format!("Unable to create helper readiness event: {error}"))?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err("Uninstall helper readiness event already exists.".to_string());
    }
    let cancel = create_named_event(&names.cancel, false)
        .map_err(|error| format!("Unable to create helper cancellation event: {error}"))?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err("Uninstall helper cancellation event already exists.".to_string());
    }
    let commit = create_named_event(&names.commit, false)
        .map_err(|error| format!("Unable to create helper commit event: {error}"))?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err("Uninstall helper commit event already exists.".to_string());
    }
    Ok(PrivateEvents {
        ready,
        cancel,
        commit,
    })
}

fn open_private_event(
    name: &str,
    access: windows::Win32::System::Threading::SYNCHRONIZATION_ACCESS_RIGHTS,
) -> Result<OwnedHandle, String> {
    let name = wide(name);
    unsafe { OpenEventW(access, false, PCWSTR::from_raw(name.as_ptr())) }
        .map(OwnedHandle)
        .map_err(|error| format!("Unable to open private uninstall event: {error}"))
}

fn open_private_events(names: &PrivateEventNames) -> Result<PrivateEvents, String> {
    Ok(PrivateEvents {
        ready: open_private_event(&names.ready, EVENT_MODIFY_STATE)?,
        cancel: open_private_event(&names.cancel, SYNCHRONIZATION_SYNCHRONIZE)?,
        commit: open_private_event(&names.commit, SYNCHRONIZATION_SYNCHRONIZE)?,
    })
}

fn classify_wait_result(result: windows::Win32::Foundation::WAIT_EVENT) -> WaitState {
    if result == WAIT_OBJECT_0 {
        WaitState::Signaled
    } else if result == WAIT_TIMEOUT {
        WaitState::TimedOut
    } else if result.0 == u32::MAX {
        WaitState::Failed
    } else {
        WaitState::Unexpected(result.0)
    }
}

fn is_file_not_found_hresult(code: u32) -> bool {
    code == (0x8007_0000 | ERROR_FILE_NOT_FOUND.0)
}

fn helper_wait_decision(
    cancel_signaled: bool,
    commit_signaled: bool,
    parent_exited: bool,
    committed: bool,
) -> HelperWaitDecision {
    if cancel_signaled {
        HelperWaitDecision::Cancel
    } else if !committed && commit_signaled {
        HelperWaitDecision::Commit
    } else if parent_exited {
        HelperWaitDecision::ParentExited
    } else {
        HelperWaitDecision::Continue
    }
}

/// Begin asynchronous cleanup, then leave the current message loop normally.
///
/// WinGet installations are never deleted natively. Portable copies remove
/// only their executable and matching startup values. Settings remain unless
/// `remove_settings` is true.
pub fn begin_uninstall(remove_settings: bool) -> Result<(), String> {
    if uninstall_kind() == UninstallKind::Winget {
        return Err(winget_refusal());
    }

    // Signal the startup gate before any staging work so newly launched app
    // instances cannot race the helper's readiness handshake.
    let _gate = create_global_uninstall_gate()?;

    cleanup_stale_staging()?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("Unable to locate current executable: {error}"))?;
    let plan = build_live_plan(&executable, remove_settings)?;
    if plan.kind == UninstallKind::Winget {
        return Err(winget_refusal());
    }

    let stage = create_staging_directory(&plan)?;
    let helper_path = stage.join(HELPER_EXE_NAME);
    let manifest_path = stage.join(MANIFEST_NAME);
    if let Err(error) = copy_helper_and_write_manifest(&executable, &helper_path, &manifest_path) {
        return Err(append_stage_cleanup_error(error, &stage));
    }

    let private_names = match private_event_names(&stage) {
        Ok(names) => names,
        Err(error) => return Err(append_stage_cleanup_error(error, &stage)),
    };
    let private_events = match create_private_events(&private_names) {
        Ok(events) => events,
        Err(error) => return Err(append_stage_cleanup_error(error, &stage)),
    };

    let mut child = match Command::new(&helper_path)
        .arg(PRIVATE_HELPER_ARG)
        .arg(std::process::id().to_string())
        .arg(if remove_settings { "1" } else { "0" })
        .current_dir(&stage)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Err(append_stage_cleanup_error(
                format!("Unable to launch uninstall helper {helper_path:?}: {error}"),
                &stage,
            ));
        }
    };

    match classify_wait_result(unsafe {
        WaitForSingleObject(private_events.ready.0, READY_TIMEOUT_MS)
    }) {
        WaitState::Signaled => {
            if let Err(error) = unsafe { SetEvent(private_events.commit.0) } {
                let message =
                    format!("Unable to commit uninstall after helper readiness: {error}.");
                return Err(abort_helper(
                    &mut child,
                    &private_events.cancel,
                    &stage,
                    message,
                ));
            }
            // Commit is visible to the helper before the owning UI loop exits.
            unsafe { PostQuitMessage(0) };
            Ok(())
        }
        WaitState::TimedOut => {
            let message =
                format!("Uninstall helper did not become ready within {READY_TIMEOUT_MS} ms.");
            Err(abort_helper(
                &mut child,
                &private_events.cancel,
                &stage,
                message,
            ))
        }
        WaitState::Failed => {
            let error = unsafe { GetLastError() };
            let message = format!(
                "Unable to wait for uninstall helper readiness: {}.",
                io::Error::from_raw_os_error(error.0 as i32)
            );
            Err(abort_helper(
                &mut child,
                &private_events.cancel,
                &stage,
                message,
            ))
        }
        WaitState::Unexpected(result) => {
            let message = format!(
                "Unable to wait for uninstall helper readiness (unexpected wait result {result:#x})."
            );
            Err(abort_helper(
                &mut child,
                &private_events.cancel,
                &stage,
                message,
            ))
        }
    }
}

/// Return whether the signaled startup gate currently blocks new app instances.
pub fn is_uninstall_in_progress() -> Result<bool, String> {
    let name = wide(UNINSTALL_EVENT);
    let handle = match unsafe {
        OpenEventW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            PCWSTR::from_raw(name.as_ptr()),
        )
    } {
        Ok(handle) => OwnedHandle(handle),
        Err(error) if is_file_not_found_hresult(error.code().0 as u32) => return Ok(false),
        Err(error) => return Err(format!("Unable to inspect uninstall startup gate: {error}")),
    };
    match classify_wait_result(unsafe { WaitForSingleObject(handle.0, 0) }) {
        WaitState::Signaled => Ok(true),
        WaitState::TimedOut => Ok(false),
        WaitState::Failed => {
            let error = unsafe { GetLastError() };
            Err(format!(
                "Unable to inspect uninstall startup gate: {}",
                io::Error::from_raw_os_error(error.0 as i32)
            ))
        }
        WaitState::Unexpected(result) => Err(format!(
            "Unable to inspect uninstall startup gate (unexpected wait result {result:#x})."
        )),
    }
}

/// Show a visible startup error before the application initializes its UI.
pub fn show_startup_error(message: &str) {
    show_message("Codex Usage startup error", message, MB_ICONERROR);
}

pub fn show_startup_info(message: &str) {
    show_message("Codex Usage", message, MB_ICONINFORMATION);
}

fn abort_helper(
    child: &mut Child,
    cancel_event: &OwnedHandle,
    stage: &Path,
    message: String,
) -> String {
    let mut failure = format!("{message} Staging path: {stage:?}.");
    if let Err(error) = unsafe { SetEvent(cancel_event.0) } {
        failure.push_str(&format!(" Unable to signal helper cancellation: {error}."));
    }

    let mut status = match wait_for_child_exit(child, HELPER_CANCEL_GRACE) {
        Ok(status) => status,
        Err(error) => {
            failure.push_str(&format!(
                " Unable to confirm helper exit before fallback: {error}."
            ));
            None
        }
    };
    if status.is_none() {
        if let Err(error) = child.kill() {
            failure.push_str(&format!(" Unable to terminate uninstall helper: {error}."));
        }
        status = match wait_for_child_exit(child, HELPER_REAP_TIMEOUT) {
            Ok(status) => status,
            Err(error) => {
                failure.push_str(&format!(" Unable to reap uninstall helper: {error}."));
                None
            }
        };
    }

    if let Some(status) = status {
        failure.push_str(&format!(" Helper exited with status {status}."));
        if let Err(error) = remove_generated_stage(stage) {
            failure.push_str(&format!(" {error}"));
        }
    } else {
        failure.push_str(&format!(
            " The helper could not be confirmed exited; staging files were preserved at {stage:?}."
        ));
    }
    failure
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(error) => return Err(error.to_string()),
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(HELPER_POLL_INTERVAL);
    }
}

fn wait_signaled(handle: HANDLE, description: &str) -> Result<bool, String> {
    match classify_wait_result(unsafe { WaitForSingleObject(handle, 0) }) {
        WaitState::Signaled => Ok(true),
        WaitState::TimedOut => Ok(false),
        WaitState::Failed => {
            let error = unsafe { GetLastError() };
            Err(format!(
                "Unable to wait for {description}: {}",
                io::Error::from_raw_os_error(error.0 as i32)
            ))
        }
        WaitState::Unexpected(result) => Err(format!(
            "Unable to wait for {description} (unexpected wait result {result:#x})."
        )),
    }
}

fn wait_for_helper_commit(
    cancel_event: &OwnedHandle,
    commit_event: &OwnedHandle,
    parent: &OwnedHandle,
) -> Result<(), String> {
    loop {
        let decision = helper_wait_decision(
            wait_signaled(cancel_event.0, "uninstall cancellation")?,
            wait_signaled(commit_event.0, "uninstall commit")?,
            wait_signaled(parent.0, "uninstall parent exit")?,
            false,
        );
        match decision {
            HelperWaitDecision::Cancel => {
                return Err("Uninstall was cancelled before commit.".to_string())
            }
            HelperWaitDecision::Commit => break,
            HelperWaitDecision::ParentExited => {
                return Err("Uninstall parent exited before committing cleanup.".to_string())
            }
            HelperWaitDecision::Continue => std::thread::sleep(HELPER_POLL_INTERVAL),
        }
    }

    loop {
        let decision = helper_wait_decision(
            wait_signaled(cancel_event.0, "uninstall cancellation")?,
            false,
            wait_signaled(parent.0, "uninstall parent exit")?,
            true,
        );
        match decision {
            HelperWaitDecision::Cancel => {
                return Err(
                    "Uninstall was cancelled while waiting for the parent to exit.".to_string(),
                )
            }
            HelperWaitDecision::ParentExited => return Ok(()),
            HelperWaitDecision::Commit | HelperWaitDecision::Continue => {
                std::thread::sleep(HELPER_POLL_INTERVAL)
            }
        }
    }
}

/// Handle private helper mode before normal application startup.
pub fn handle_cli_mode(args: &[OsString]) -> Option<i32> {
    if args.get(1).map(OsString::as_os_str) != Some(OsStr::new(PRIVATE_HELPER_ARG)) {
        return None;
    }

    let result = run_helper_mode(args);
    match result {
        Ok(message) => {
            show_message("Uninstall Codex Usage", &message, MB_ICONINFORMATION);
            Some(0)
        }
        Err(error) => {
            show_message("Uninstall failed", &error, MB_ICONERROR);
            Some(1)
        }
    }
}

fn detect_kind_for_executable(executable: &Path) -> UninstallKind {
    let managed_dir = env_path("LOCALAPPDATA").map(|path| path.join("Programs").join("CodexUsage"));
    let winget_roots = winget_install_roots();
    let resolved_executable = canonical_or_normalized(executable);
    let resolved_executable = resolved_executable.as_deref().unwrap_or(executable);
    let registry_evidence =
        read_registry_string(UNINSTALL_KEY, "InstallLocation").and_then(|install| {
            read_registry_string(UNINSTALL_KEY, "DisplayIcon").map(|icon| (install, icon))
        });
    let (registered_install, registered_icon) = match registry_evidence {
        Ok(evidence) => evidence,
        Err(_) => {
            if winget_roots
                .iter()
                .any(|root| is_same_or_child_path(resolved_executable, root))
            {
                return UninstallKind::Winget;
            }
            return UninstallKind::Portable;
        }
    };
    let registered_install = registered_install
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .and_then(canonical_or_normalized);
    let registered_icon = registered_icon
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .and_then(canonical_or_normalized);
    let managed_dir = managed_dir.as_deref().and_then(canonical_or_normalized);
    let kind = classify_from_evidence(
        resolved_executable,
        managed_dir.as_deref(),
        registered_install.as_deref(),
        registered_icon.as_deref(),
        &[],
    );
    if kind == UninstallKind::Managed {
        return kind;
    }
    classify_from_evidence(resolved_executable, None, None, None, &winget_roots)
}

fn build_live_plan(executable: &Path, remove_settings: bool) -> Result<CleanupPlan, String> {
    let executable = canonical_or_normalized(executable)
        .ok_or_else(|| format!("Unable to resolve current executable path: {executable:?}"))?;
    let kind = detect_kind_for_executable(&executable);
    let paths = live_cleanup_paths(kind, remove_settings)?;
    let mut targets = build_targets(kind, &executable, remove_settings, &paths)?;

    if kind == UninstallKind::Managed {
        let local_app_data = env_path("LOCALAPPDATA")
            .ok_or_else(|| "LOCALAPPDATA is unavailable or not an absolute path.".to_string())?;
        let install_dir = local_app_data.join("Programs").join("CodexUsage");
        let expected_exe = install_dir.join("codex-usage.exe");
        if !resolved_equal(&executable, &expected_exe) {
            return Err(format!(
                "Refusing managed cleanup because executable path changed: {executable:?}"
            ));
        }
        verify_managed_registry_match(&install_dir, &expected_exe)?;
    }

    for target in &targets {
        if let Some(path) = target.path() {
            validate_destructive_path(path)?;
        }
    }
    targets.shrink_to_fit();
    Ok(CleanupPlan {
        kind,
        executable,
        targets,
    })
}

fn live_cleanup_paths(kind: UninstallKind, remove_settings: bool) -> Result<CleanupPaths, String> {
    let mut paths = CleanupPaths::default();
    if kind == UninstallKind::Managed {
        let local_app_data = env_path("LOCALAPPDATA")
            .ok_or_else(|| "LOCALAPPDATA is unavailable or not an absolute path.".to_string())?;
        let app_data = env_path("APPDATA")
            .ok_or_else(|| "APPDATA is unavailable or not an absolute path.".to_string())?;
        paths.managed_install_dir = Some(local_app_data.join("Programs").join("CodexUsage"));
        paths.start_menu_shortcut = Some(
            app_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("Codex Usage.lnk"),
        );
        paths.desktop_shortcut = Some(
            dirs::desktop_dir()
                .ok_or_else(|| "Unable to resolve current user's Desktop directory.".to_string())?
                .join("Codex Usage.lnk"),
        );
        if remove_settings {
            paths.settings_dir = Some(app_data.join("CodexUsage"));
        }
    } else if kind == UninstallKind::Portable && remove_settings {
        let app_data = env_path("APPDATA")
            .ok_or_else(|| "APPDATA is unavailable or not an absolute path.".to_string())?;
        paths.settings_dir = Some(app_data.join("CodexUsage"));
    }
    Ok(paths)
}

fn build_targets(
    kind: UninstallKind,
    executable: &Path,
    remove_settings: bool,
    paths: &CleanupPaths,
) -> Result<Vec<CleanupTarget>, String> {
    let mut targets = Vec::new();
    match kind {
        UninstallKind::Managed => {
            let install_dir = paths.managed_install_dir.as_ref().ok_or_else(|| {
                "Managed install directory is missing from cleanup plan.".to_string()
            })?;
            let start_menu = paths
                .start_menu_shortcut
                .as_ref()
                .ok_or_else(|| "Start Menu shortcut is missing from cleanup plan.".to_string())?;
            let desktop = paths
                .desktop_shortcut
                .as_ref()
                .ok_or_else(|| "Desktop shortcut is missing from cleanup plan.".to_string())?;
            targets.extend([
                CleanupTarget::StartupValue {
                    name: "CodexUsage",
                    only_if_executable: None,
                },
                CleanupTarget::StartupValue {
                    name: "ClaudeCodeUsageMonitor",
                    only_if_executable: None,
                },
                CleanupTarget::File(start_menu.clone()),
            ]);
            targets.push(CleanupTarget::File(desktop.clone()));
            targets.push(CleanupTarget::UninstallRegistryTree);
            if remove_settings {
                targets.push(CleanupTarget::Directory(
                    paths
                        .settings_dir
                        .as_ref()
                        .ok_or_else(|| {
                            "Settings directory is missing from cleanup plan.".to_string()
                        })?
                        .clone(),
                ));
            }
            targets.push(CleanupTarget::Directory(install_dir.clone()));
        }
        UninstallKind::Portable => {
            targets.push(CleanupTarget::File(executable.to_path_buf()));
            targets.extend([
                CleanupTarget::StartupValue {
                    name: "CodexUsage",
                    only_if_executable: Some(executable.to_path_buf()),
                },
                CleanupTarget::StartupValue {
                    name: "ClaudeCodeUsageMonitor",
                    only_if_executable: Some(executable.to_path_buf()),
                },
            ]);
            if remove_settings {
                targets.push(CleanupTarget::Directory(
                    paths
                        .settings_dir
                        .as_ref()
                        .ok_or_else(|| {
                            "Settings directory is missing from cleanup plan.".to_string()
                        })?
                        .clone(),
                ));
            }
        }
        UninstallKind::Winget => {}
    }
    Ok(targets)
}

fn classify_from_evidence(
    executable: &Path,
    expected_managed_dir: Option<&Path>,
    registered_install: Option<&Path>,
    registered_icon: Option<&Path>,
    winget_roots: &[PathBuf],
) -> UninstallKind {
    if let (Some(expected_dir), Some(registered_install), Some(registered_icon)) =
        (expected_managed_dir, registered_install, registered_icon)
    {
        let expected_exe = expected_dir.join("codex-usage.exe");
        if paths_equal(executable, &expected_exe)
            && paths_equal(registered_install, expected_dir)
            && paths_equal(registered_icon, &expected_exe)
        {
            return UninstallKind::Managed;
        }
    }
    if winget_roots
        .iter()
        .any(|root| is_same_or_child_path(executable, root))
    {
        UninstallKind::Winget
    } else {
        UninstallKind::Portable
    }
}

impl CleanupTarget {
    fn path(&self) -> Option<&Path> {
        match self {
            Self::File(path) | Self::Directory(path) => Some(path),
            Self::StartupValue { .. } | Self::UninstallRegistryTree => None,
        }
    }
}

fn run_helper_mode(args: &[OsString]) -> Result<String, String> {
    if args.len() != 4 {
        return Err("Invalid uninstall helper arguments.".to_string());
    }
    let parent_pid = args[2]
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| "Invalid uninstall parent process ID.".to_string())?;
    let remove_settings = match args[3].to_str() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err("Invalid uninstall settings option.".to_string()),
    };

    // Retain this handle before opening the readiness event or acknowledging readiness.
    let parent = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            parent_pid,
        )
    }
    .map_err(|error| format!("Unable to open uninstall parent process {parent_pid}: {error}"))?;
    let parent = OwnedHandle(parent);

    let stage = validate_helper_staging()?;
    let stage_identity = stage
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(parse_stage_identity)
        .ok_or_else(|| "Invalid uninstall helper staging directory identity.".to_string())?;
    if stage_identity.parent_pid != parent_pid {
        return Err(
            "Uninstall helper staging directory does not match parent process ID.".to_string(),
        );
    }
    let manifest = read_manifest(&stage.join(MANIFEST_NAME))?;
    if manifest.parent_pid != parent_pid {
        return Err("Uninstall helper manifest does not match parent process ID.".to_string());
    }
    let parent_image = query_process_image(parent.0)?;
    if !resolved_equal(&parent_image, &manifest.parent_executable) {
        return Err(format!(
            "Uninstall helper parent executable did not match manifest: {parent_image:?}"
        ));
    }
    let plan = build_live_plan(&parent_image, remove_settings)?;
    if plan.kind == UninstallKind::Winget {
        return Err(winget_refusal());
    }

    let _gate = open_private_event(UNINSTALL_EVENT, SYNCHRONIZATION_SYNCHRONIZE)
        .map_err(|error| format!("Unable to retain uninstall startup gate: {error}"))?;
    let private_names = private_event_names(&stage)?;
    let private_events = open_private_events(&private_names)?;
    unsafe { SetEvent(private_events.ready.0) }
        .map_err(|error| format!("Unable to signal uninstall readiness: {error}"))?;

    wait_for_helper_commit(&private_events.cancel, &private_events.commit, &parent)?;

    let failures = execute_plan(&plan);
    if failures.is_empty() {
        Ok(format!(
            "Codex Usage uninstall completed. Temporary helper may remain at {:?}; future uninstall attempts clean at most {MAX_STALE_STAGES_PER_RUN} matching staging directories older than 24 hours.",
            stage
        ))
    } else {
        Err(format!(
            "Uninstall was incomplete. No success is claimed.\n{}",
            failures.join("\n")
        ))
    }
}

fn execute_plan(plan: &CleanupPlan) -> Vec<String> {
    let mut failures = Vec::new();
    for target in &plan.targets {
        let result = match target {
            CleanupTarget::File(path) => remove_file_if_present(path),
            CleanupTarget::Directory(path) => remove_directory_if_present(path),
            CleanupTarget::StartupValue {
                name,
                only_if_executable,
            } => remove_startup_value(name, only_if_executable.as_deref()),
            CleanupTarget::UninstallRegistryTree => {
                remove_matching_uninstall_registry_tree(&plan.executable)
            }
        };
        if let Err(error) = result {
            failures.push(error);
        }
    }
    failures
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    validate_destructive_path(path)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Unable to inspect {path:?}: {error}")),
        Ok(metadata) if is_reparse_point(&metadata) => Err(format!(
            "Refusing to remove {path:?}: unexpected reparse point."
        )),
        Ok(metadata) if !metadata.is_file() => Err(format!(
            "Refusing to remove {path:?}: expected a regular file."
        )),
        Ok(_) => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Unable to remove {path:?}: {error}")),
        },
    }
}

fn remove_directory_if_present(path: &Path) -> Result<(), String> {
    validate_destructive_path(path)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Unable to inspect {path:?}: {error}")),
        Ok(metadata) if is_reparse_point(&metadata) => Err(format!(
            "Refusing to remove {path:?}: unexpected reparse point."
        )),
        Ok(metadata) if !metadata.is_dir() => Err(format!(
            "Refusing to remove {path:?}: expected a directory."
        )),
        Ok(_) => remove_directory_tree(path),
    }
}

fn remove_directory_tree(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(directory) = pending.pop() {
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Unable to inspect {directory:?}: {error}")),
        };
        if is_reparse_point(&metadata) || !metadata.is_dir() {
            return Err(format!(
                "Refusing to inspect {directory:?}: it is not a regular directory."
            ));
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Unable to inspect {directory:?}: {error}")),
        };
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("Unable to inspect contents of {directory:?}: {error}"))?;
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("Unable to inspect {path:?}: {error}")),
            };
            if is_reparse_point(&metadata) {
                return Err(format!(
                    "Refusing to remove {root:?}: unexpected reparse point at {path:?}."
                ));
            }
            if metadata.is_dir() {
                pending.push(path.clone());
                directories.push(path);
            } else if metadata.is_file() {
                files.push(path);
            } else {
                return Err(format!(
                    "Refusing to remove {root:?}: unsupported filesystem artifact {path:?}."
                ));
            }
        }
    }

    for path in files {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if is_reparse_point(&metadata) || !metadata.is_file() => {
                return Err(format!(
                    "Refusing to remove changed filesystem artifact {path:?}."
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Unable to inspect {path:?}: {error}")),
            Ok(_) => {}
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Unable to remove file {path:?}: {error}")),
        }
    }
    for path in directories.into_iter().rev() {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if is_reparse_point(&metadata) || !metadata.is_dir() => {
                return Err(format!("Refusing to remove changed directory {path:?}."));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Unable to inspect {path:?}: {error}")),
            Ok(_) => {}
        }
        match fs::remove_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Unable to remove directory {path:?}: {error}")),
        }
    }
    Ok(())
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn remove_startup_value(name: &str, expected_executable: Option<&Path>) -> Result<(), String> {
    if let Some(expected) = expected_executable {
        let Some(value) = read_registry_string(RUN_KEY, name)? else {
            return Ok(());
        };
        if !startup_value_references(&value, expected) {
            return Ok(());
        }
    }

    let key = match open_registry_key_raw(RUN_KEY, KEY_SET_VALUE) {
        Ok(key) => RegistryKey(key),
        Err(error) if is_registry_not_found_code(error) => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Unable to open registry key HKCU\\{RUN_KEY}: {}",
                registry_error(error)
            ));
        }
    };
    let value_name = wide(name);
    let status = unsafe { RegDeleteValueW(key.0, PCWSTR::from_raw(value_name.as_ptr())) };
    if status.0 == 0 || is_registry_not_found_code(status) {
        Ok(())
    } else {
        Err(format!(
            "Unable to remove registry value HKCU\\{RUN_KEY}\\{name}: {}",
            registry_error(status)
        ))
    }
}

fn remove_matching_uninstall_registry_tree(executable: &Path) -> Result<(), String> {
    let install_dir = executable.parent().ok_or_else(|| {
        format!("Unable to determine managed install directory for {executable:?}")
    })?;
    match open_registry_key_raw(UNINSTALL_KEY, KEY_QUERY_VALUE) {
        Ok(key) => drop(RegistryKey(key)),
        Err(error) if is_registry_not_found_code(error) => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Unable to inspect registry subtree HKCU\\{UNINSTALL_KEY}: {}",
                registry_error(error)
            ));
        }
    }
    verify_managed_registry_match(install_dir, executable)?;
    let key = wide(UNINSTALL_KEY);
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR::from_raw(key.as_ptr())) };
    if status.0 == 0 || is_registry_not_found_code(status) {
        Ok(())
    } else {
        Err(format!(
            "Unable to remove registry subtree HKCU\\{UNINSTALL_KEY}: {}",
            registry_error(status)
        ))
    }
}

fn verify_managed_registry_match(install_dir: &Path, executable: &Path) -> Result<(), String> {
    let registered_install = read_registry_string(UNINSTALL_KEY, "InstallLocation")?;
    let registered_icon = read_registry_string(UNINSTALL_KEY, "DisplayIcon")?;
    if !registered_install
        .as_deref()
        .is_some_and(|value| resolved_equal(Path::new(value), install_dir))
        || !registered_icon
            .as_deref()
            .is_some_and(|value| resolved_equal(Path::new(value), executable))
    {
        return Err(format!(
            "Refusing managed cleanup: HKCU\\{UNINSTALL_KEY} no longer matches install directory {install_dir:?} and executable {executable:?}."
        ));
    }
    Ok(())
}

fn read_registry_string(key_path: &str, value_name: &str) -> Result<Option<OsString>, String> {
    let key = match open_registry_key_raw(key_path, KEY_QUERY_VALUE) {
        Ok(key) => key,
        Err(error) if is_registry_not_found_code(error) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Unable to open registry key HKCU\\{key_path}: {}",
                registry_error(error)
            ));
        }
    };
    let key = RegistryKey(key);
    let value_wide = wide(value_name);
    let mut value_type = windows::Win32::System::Registry::REG_VALUE_TYPE(0);
    let mut byte_len = 0u32;
    let first = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR::from_raw(value_wide.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut byte_len),
        )
    };
    if first.0 != 0 {
        if is_registry_not_found_code(first) {
            return Ok(None);
        }
        return Err(format!(
            "Unable to read registry value HKCU\\{key_path}\\{value_name}: {}",
            registry_error(first)
        ));
    }
    if !matches!(value_type, REG_SZ | REG_EXPAND_SZ) || byte_len < 2 || !byte_len.is_multiple_of(2)
    {
        return Ok(None);
    }
    if byte_len > MAX_REGISTRY_STRING_BYTES {
        return Err(format!(
            "Registry value HKCU\\{key_path}\\{value_name} is unexpectedly large ({byte_len} bytes)."
        ));
    }
    let mut bytes = vec![0u8; byte_len as usize];
    let second = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR::from_raw(value_wide.as_ptr()),
            None,
            Some(&mut value_type),
            Some(bytes.as_mut_ptr()),
            Some(&mut byte_len),
        )
    };
    if second.0 != 0 {
        if is_registry_not_found_code(second) {
            return Ok(None);
        }
        return Err(format!(
            "Unable to read registry value HKCU\\{key_path}\\{value_name}: {}",
            registry_error(second)
        ));
    }
    if byte_len > bytes.len() as u32 || !byte_len.is_multiple_of(2) {
        return Err(format!(
            "Registry value HKCU\\{key_path}\\{value_name} changed to an invalid size while being read."
        ));
    }
    let wide_values = bytes[..byte_len as usize]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    Ok(Some(OsString::from_wide(&wide_values)))
}

fn open_registry_key_raw(
    path: &str,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<HKEY, WIN32_ERROR> {
    let path_wide = wide(path);
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path_wide.as_ptr()),
            0,
            access,
            &mut key,
        )
    };
    if status.0 == 0 {
        Ok(key)
    } else {
        Err(status)
    }
}

fn is_registry_not_found_code(error: WIN32_ERROR) -> bool {
    error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND
}

fn registry_error(error: WIN32_ERROR) -> io::Error {
    io::Error::from_raw_os_error(error.0 as i32)
}

fn startup_value_references(value: &OsStr, executable: &Path) -> bool {
    let wide = value.encode_wide().collect::<Vec<_>>();
    let is_outer_whitespace = |unit: &u16| matches!(*unit, 0x20 | 0x09 | 0x0a | 0x0d);
    let start = wide
        .iter()
        .position(|unit| !is_outer_whitespace(unit))
        .unwrap_or(wide.len());
    let end = wide
        .iter()
        .rposition(|unit| !is_outer_whitespace(unit))
        .map_or(start, |index| index + 1);
    let mut path_wide = &wide[start..end];
    if path_wide.len() >= 2
        && path_wide.first() == Some(&(b'"' as u16))
        && path_wide.last() == Some(&(b'"' as u16))
    {
        path_wide = &path_wide[1..path_wide.len() - 1];
    }
    let path = PathBuf::from(OsString::from_wide(path_wide));
    path.is_absolute() && resolved_equal(&path, executable)
}

fn winget_refusal() -> String {
    "WinGet owns this installation. Uninstall it with: winget uninstall --id rozsazoltan.codexusage --exact".to_string()
}

fn winget_install_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(local_app_data) = env_path("LOCALAPPDATA") {
        roots.push(
            local_app_data
                .join("Microsoft")
                .join("WinGet")
                .join("Packages"),
        );
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        roots.push(PathBuf::from(program_files).join("WinGet").join("Packages"));
    } else {
        roots.push(PathBuf::from(r"C:\Program Files\WinGet\Packages"));
    }
    if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
        roots.push(
            PathBuf::from(program_files_x86)
                .join("WinGet")
                .join("Packages"),
        );
    } else {
        roots.push(PathBuf::from(r"C:\Program Files (x86)\WinGet\Packages"));
    }
    roots
}

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\CodexUsage";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn env_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    path.is_absolute().then_some(path)
}

fn resolved_equal(left: &Path, right: &Path) -> bool {
    if !left.is_absolute() || !right.is_absolute() {
        return false;
    }
    let Some(left) = canonical_or_normalized(left) else {
        return false;
    };
    let Some(right) = canonical_or_normalized(right) else {
        return false;
    };
    path_key(&left)
        .zip(path_key(&right))
        .is_some_and(|(a, b)| ordinal_case_insensitive_equal(&a, &b))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    path_key(left)
        .zip(path_key(right))
        .is_some_and(|(a, b)| ordinal_case_insensitive_equal(&a, &b))
}

fn canonical_or_normalized(path: &Path) -> Option<PathBuf> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    normalize_absolute_path(&path)
}

fn normalize_absolute_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let prefix_depth = normalized.components().count().min(2);
                if normalized.components().count() > prefix_depth {
                    normalized.pop();
                } else {
                    return None;
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Some(normalized)
}

fn path_key(path: &Path) -> Option<Vec<u16>> {
    let normalized = normalize_absolute_path(path)?;
    let mut value = normalized
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == b'/' as u16 {
                b'\\' as u16
            } else {
                unit
            }
        })
        .collect::<Vec<_>>();
    let unc_prefix = wide_no_nul(r"\\?\UNC\");
    let extended_prefix = wide_no_nul(r"\\?\");
    if value.len() >= unc_prefix.len()
        && ordinal_case_insensitive_equal(&value[..unc_prefix.len()], &unc_prefix)
    {
        value.splice(..unc_prefix.len(), [b'\\' as u16, b'\\' as u16]);
    } else if value.len() >= extended_prefix.len()
        && ordinal_case_insensitive_equal(&value[..extended_prefix.len()], &extended_prefix)
    {
        value.drain(..extended_prefix.len());
    }
    while value.len() > 3 && value.last() == Some(&(b'\\' as u16)) {
        value.pop();
    }
    Some(value)
}

fn is_same_or_child_path(candidate: &Path, root: &Path) -> bool {
    let Some(candidate) = path_key(candidate) else {
        return false;
    };
    let Some(root) = path_key(root) else {
        return false;
    };
    if ordinal_case_insensitive_equal(&candidate, &root) {
        return true;
    }
    if candidate.len() <= root.len()
        || !ordinal_case_insensitive_equal(&candidate[..root.len()], &root)
    {
        return false;
    }
    root.last() == Some(&(b'\\' as u16)) || candidate[root.len()] == b'\\' as u16
}

fn ordinal_case_insensitive_equal(left: &[u16], right: &[u16]) -> bool {
    unsafe { CompareStringOrdinal(left, right, true) == CSTR_EQUAL }
}

fn wide_no_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

fn validate_destructive_path(path: &Path) -> Result<(), String> {
    let normalized = normalize_absolute_path(path)
        .ok_or_else(|| format!("Refusing non-absolute destructive path {path:?}."))?;
    let mut current = PathBuf::new();
    for component in normalized.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_reparse_point(&metadata) => {
                return Err(format!(
                    "Refusing destructive path {path:?}: unexpected reparse point at {current:?}."
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "Unable to inspect destructive path {current:?}: {error}"
                ))
            }
        }
    }
    Ok(())
}

fn create_staging_directory(plan: &CleanupPlan) -> Result<PathBuf, String> {
    let temp = std::env::temp_dir();
    if !temp.is_absolute() {
        return Err(format!("Temporary directory is not absolute: {temp:?}"));
    }
    let temp = fs::canonicalize(&temp)
        .map_err(|error| format!("Unable to resolve temporary directory {temp:?}: {error}"))?;
    validate_destructive_path(&temp)?;
    for target in &plan.targets {
        if let Some(path) = target.path() {
            if is_same_or_child_path(&temp, path) || is_same_or_child_path(path, &temp) {
                return Err(format!(
                    "Refusing staging under temporary directory {temp:?}: it overlaps cleanup target {path:?}."
                ));
            }
        }
    }

    for _ in 0..32 {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = STAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = temp.join(format!(
            "{HELPER_DIR_PREFIX}{}-{nonce:032x}-{sequence:016x}",
            std::process::id()
        ));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Unable to create staging directory {directory:?}: {error}"
                ))
            }
        }
    }
    Err("Unable to allocate unique uninstall staging directory after 32 attempts.".to_string())
}

fn copy_helper_and_write_manifest(
    parent_executable: &Path,
    helper_path: &Path,
    manifest_path: &Path,
) -> Result<(), String> {
    let source = File::open(parent_executable)
        .map_err(|error| format!("Unable to open helper source {parent_executable:?}: {error}"))?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(helper_path)
        .map_err(|error| format!("Unable to create staged helper {helper_path:?}: {error}"))?;
    let mut source = source;
    io::copy(&mut source, &mut destination)
        .map_err(|error| format!("Unable to stage helper at {helper_path:?}: {error}"))?;
    destination
        .sync_all()
        .map_err(|error| format!("Unable to flush staged helper {helper_path:?}: {error}"))?;
    write_manifest(manifest_path, std::process::id(), parent_executable)
}

fn append_stage_cleanup_error(mut error: String, stage: &Path) -> String {
    if let Err(cleanup_error) = remove_generated_stage(stage) {
        error.push_str(&format!(" {cleanup_error}"));
    }
    error
}

fn remove_generated_stage(stage: &Path) -> Result<(), String> {
    let temp = fs::canonicalize(std::env::temp_dir())
        .map_err(|error| format!("Unable to resolve temporary directory: {error}"))?;
    if !stage
        .parent()
        .is_some_and(|parent| resolved_equal(parent, &temp))
        || !stage
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with(HELPER_DIR_PREFIX))
    {
        return Err(format!(
            "Refusing to remove unexpected staging path {stage:?}."
        ));
    }
    validate_destructive_path(stage)?;
    let entries = match fs::read_dir(stage) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Unable to inspect staging directory {stage:?}: {error}"
            ));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("Unable to inspect staging directory {stage:?}: {error}"))?;
        let path = entry.path();
        if entry.file_name() != OsStr::new(HELPER_EXE_NAME)
            && entry.file_name() != OsStr::new(MANIFEST_NAME)
        {
            return Err(format!(
                "Refusing to remove unexpected staging artifact {path:?}."
            ));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("Unable to inspect staging artifact {path:?}: {error}"))?;
        if is_reparse_point(&metadata) || !metadata.is_file() {
            return Err(format!(
                "Refusing to remove unsafe staging artifact {path:?}."
            ));
        }
    }
    remove_directory_tree(stage)
}

#[derive(Debug)]
struct HelperManifest {
    parent_pid: u32,
    parent_executable: PathBuf,
}

fn write_manifest(path: &Path, parent_pid: u32, parent_executable: &Path) -> Result<(), String> {
    let encoded = parent_executable
        .as_os_str()
        .encode_wide()
        .collect::<Vec<_>>();
    let length = u32::try_from(encoded.len())
        .map_err(|_| "Parent executable path is too long for helper manifest.".to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Unable to create helper manifest {path:?}: {error}"))?;
    file.write_all(MANIFEST_MAGIC)
        .and_then(|_| file.write_all(&parent_pid.to_le_bytes()))
        .and_then(|_| file.write_all(&length.to_le_bytes()))
        .map_err(|error| format!("Unable to write helper manifest {path:?}: {error}"))?;
    for unit in encoded {
        file.write_all(&unit.to_le_bytes())
            .map_err(|error| format!("Unable to write helper manifest {path:?}: {error}"))?;
    }
    file.sync_all()
        .map_err(|error| format!("Unable to flush helper manifest {path:?}: {error}"))
}

fn read_manifest(path: &Path) -> Result<HelperManifest, String> {
    validate_destructive_path(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Unable to inspect helper manifest {path:?}: {error}"))?;
    if is_reparse_point(&metadata) || !metadata.is_file() || metadata.len() > 131_072 {
        return Err(format!("Refusing invalid helper manifest {path:?}."));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| format!("Unable to read helper manifest {path:?}: {error}"))?;
    let header_len = MANIFEST_MAGIC.len() + 8;
    if bytes.get(..MANIFEST_MAGIC.len()) != Some(MANIFEST_MAGIC) || bytes.len() < header_len {
        return Err(format!("Invalid helper manifest header at {path:?}."));
    }
    let pid_start = MANIFEST_MAGIC.len();
    let parent_pid = u32::from_le_bytes(bytes[pid_start..pid_start + 4].try_into().unwrap());
    let length_start = pid_start + 4;
    let unit_count =
        u32::from_le_bytes(bytes[length_start..length_start + 4].try_into().unwrap()) as usize;
    let expected_len = header_len
        .checked_add(unit_count.saturating_mul(2))
        .ok_or_else(|| format!("Invalid helper manifest length at {path:?}."))?;
    if bytes.len() != expected_len {
        return Err(format!("Invalid helper manifest length at {path:?}."));
    }
    let parent_path = bytes[header_len..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect::<Vec<_>>();
    Ok(HelperManifest {
        parent_pid,
        parent_executable: PathBuf::from(OsString::from_wide(&parent_path)),
    })
}

fn validate_helper_staging() -> Result<PathBuf, String> {
    let helper = std::env::current_exe()
        .map_err(|error| format!("Unable to locate staged helper executable: {error}"))?;
    let stage = helper
        .parent()
        .ok_or_else(|| format!("Staged helper has no parent directory: {helper:?}"))?;
    if helper.file_name() != Some(OsStr::new(HELPER_EXE_NAME))
        || !stage
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| parse_stage_identity(name).is_some())
    {
        return Err(format!(
            "Refusing helper outside expected staging layout: {helper:?}"
        ));
    }
    let temp = fs::canonicalize(std::env::temp_dir()).map_err(|error| {
        format!("Unable to resolve temporary directory for helper {helper:?}: {error}")
    })?;
    if !stage
        .parent()
        .is_some_and(|parent| resolved_equal(parent, &temp))
    {
        return Err(format!(
            "Refusing helper outside temporary directory: {helper:?}"
        ));
    }
    validate_destructive_path(stage)?;
    Ok(stage.to_path_buf())
}

fn query_process_image(process: HANDLE) -> Result<PathBuf, String> {
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR::from_raw(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .map_err(|error| format!("Unable to read uninstall parent executable path: {error}"))?;
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

fn cleanup_stale_staging() -> Result<(), String> {
    let temp = fs::canonicalize(std::env::temp_dir()).map_err(|error| {
        format!("Unable to resolve temporary directory for stale helper cleanup: {error}")
    })?;
    let now = SystemTime::now();
    let entries = fs::read_dir(&temp)
        .map_err(|error| format!("Unable to inspect temporary directory {temp:?}: {error}"))?;
    let mut inspected = 0usize;
    for entry in entries {
        if inspected >= MAX_STALE_STAGES_PER_RUN {
            break;
        }
        let entry = entry
            .map_err(|error| format!("Unable to inspect temporary directory {temp:?}: {error}"))?;
        let path = entry.path();
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(HELPER_DIR_PREFIX))
        {
            continue;
        }
        inspected += 1;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("Unable to inspect stale helper staging {path:?}: {error}"))?;
        if is_reparse_point(&metadata) || !metadata.is_dir() {
            continue;
        }
        let manifest_path = path.join(MANIFEST_NAME);
        if read_manifest(&manifest_path).is_err() {
            continue;
        }
        let modified = fs::metadata(&manifest_path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                format!("Unable to read age of helper manifest {manifest_path:?}: {error}")
            })?;
        if now.duration_since(modified).unwrap_or_default() < STALE_STAGE_AGE {
            continue;
        }
        validate_stale_stage_contents(&path)?;
        remove_directory_tree(&path)?;
    }
    Ok(())
}

fn validate_stale_stage_contents(path: &Path) -> Result<(), String> {
    validate_destructive_path(path)?;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Unable to inspect stale helper staging {path:?}: {error}"
            ));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("Unable to inspect stale helper staging {path:?}: {error}"))?;
        let name = entry.file_name();
        if name != OsStr::new(HELPER_EXE_NAME) && name != OsStr::new(MANIFEST_NAME) {
            return Err(format!(
                "Refusing stale helper staging with unexpected artifact {:?}.",
                entry.path()
            ));
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            format!(
                "Unable to inspect stale helper artifact {:?}: {error}",
                entry.path()
            )
        })?;
        if is_reparse_point(&metadata) || !metadata.is_file() {
            return Err(format!(
                "Refusing stale helper reparse point or non-file {:?}.",
                entry.path()
            ));
        }
    }
    Ok(())
}

fn show_message(
    title: &str,
    message: &str,
    icon: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) {
    let title = wide(title);
    let message = wide(message);
    unsafe {
        let _ = MessageBoxW(
            windows::Win32::Foundation::HWND::default(),
            PCWSTR::from_raw(message.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            MB_OK | icon,
        );
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> CleanupPaths {
        CleanupPaths {
            managed_install_dir: Some(PathBuf::from(
                r"C:\Users\user\AppData\Local\Programs\CodexUsage",
            )),
            start_menu_shortcut: Some(PathBuf::from(
                r"C:\Users\user\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Codex Usage.lnk",
            )),
            desktop_shortcut: Some(PathBuf::from(r"C:\Users\user\Desktop\Codex Usage.lnk")),
            settings_dir: Some(PathBuf::from(r"C:\Users\user\AppData\Roaming\CodexUsage")),
        }
    }

    #[test]
    fn wait_results_and_missing_gate_errors_are_classified_precisely() {
        assert_eq!(classify_wait_result(WAIT_OBJECT_0), WaitState::Signaled);
        assert_eq!(classify_wait_result(WAIT_TIMEOUT), WaitState::TimedOut);
        assert_eq!(
            classify_wait_result(windows::Win32::Foundation::WAIT_EVENT(u32::MAX)),
            WaitState::Failed
        );
        assert_eq!(
            classify_wait_result(windows::Win32::Foundation::WAIT_EVENT(17)),
            WaitState::Unexpected(17)
        );

        assert!(is_file_not_found_hresult(
            0x8007_0000 | ERROR_FILE_NOT_FOUND.0
        ));
        assert!(!is_file_not_found_hresult(
            0x8007_0000 | ERROR_PATH_NOT_FOUND.0
        ));
        assert!(!is_file_not_found_hresult(0x8007_0005));
    }

    #[test]
    fn helper_cancellation_takes_priority_over_commit_and_parent_exit() {
        assert_eq!(
            helper_wait_decision(true, true, true, false),
            HelperWaitDecision::Cancel
        );
        assert_eq!(
            helper_wait_decision(false, true, true, false),
            HelperWaitDecision::Commit
        );
        assert_eq!(
            helper_wait_decision(false, false, true, false),
            HelperWaitDecision::ParentExited
        );
        assert_eq!(
            helper_wait_decision(false, true, false, true),
            HelperWaitDecision::Continue
        );
    }

    #[test]
    fn staged_event_identity_requires_the_exact_generated_basename_shape() {
        let basename = "CodexUsageUninstall-42-00000000000000000000000000000001-0000000000000002";
        assert_eq!(
            parse_stage_identity(basename),
            Some(StageIdentity { parent_pid: 42 })
        );
        assert!(parse_stage_identity("CodexUsageUninstall-42-short-0000000000000002").is_none());
        assert!(parse_stage_identity(
            "CodexUsageUninstall-0-00000000000000000000000000000001-0000000000000002"
        )
        .is_none());
        assert!(parse_stage_identity(
            "CodexUsageUninstall-42-00000000000000000000000000000001-0000000000000002-extra"
        )
        .is_none());
        assert!(parse_stage_identity(
            "CodexUsageUninstall-042-00000000000000000000000000000001-0000000000000002"
        )
        .is_none());
        assert!(parse_stage_identity(
            "CodexUsageUninstall-42-0000000000000000000000000000000A-0000000000000002"
        )
        .is_none());

        let names = private_event_names(Path::new(basename)).unwrap();
        assert_eq!(names.ready, format!("Local\\{basename}-Ready"));
        assert_eq!(names.cancel, format!("Local\\{basename}-Cancel"));
        assert_eq!(names.commit, format!("Local\\{basename}-Commit"));
    }

    fn registry_matches(executable: &Path, expected_dir: &Path) -> bool {
        let expected_exe = expected_dir.join("codex-usage.exe");
        classify_from_evidence(
            executable,
            Some(expected_dir),
            Some(expected_dir),
            Some(&expected_exe),
            &[],
        ) == UninstallKind::Managed
    }

    #[test]
    fn managed_classification_requires_exact_executable_and_both_registry_paths() {
        let install = Path::new(r"C:\Users\user\AppData\Local\Programs\CodexUsage");
        let exe = install.join("codex-usage.exe");
        assert!(registry_matches(&exe, install));
        assert!(!registry_matches(&install.join("other.exe"), install));
        assert_eq!(
            classify_from_evidence(
                &exe,
                Some(install),
                Some(Path::new(r"C:\Other\CodexUsage")),
                Some(&exe),
                &[],
            ),
            UninstallKind::Portable
        );
        assert_eq!(
            classify_from_evidence(&exe, Some(install), Some(install), None, &[]),
            UninstallKind::Portable
        );
    }

    #[test]
    fn winget_classification_uses_component_aware_root_match() {
        let root = Path::new(r"C:\Users\user\AppData\Local\Microsoft\WinGet\Packages");
        assert_eq!(
            classify_from_evidence(
                &root.join(r"Rozsa.CodexUsage_1.0.0\codex-usage.exe"),
                None,
                None,
                None,
                &[root.to_path_buf()],
            ),
            UninstallKind::Winget
        );
        assert_eq!(
            classify_from_evidence(
                Path::new(r"C:\Users\user\AppData\Local\Microsoft\WinGet\PackagesBackup\app.exe"),
                None,
                None,
                None,
                &[root.to_path_buf()],
            ),
            UninstallKind::Portable
        );
    }

    #[test]
    fn managed_and_portable_cleanup_plans_have_narrow_scope() {
        let executable =
            Path::new(r"C:\Users\user\AppData\Local\Programs\CodexUsage\codex-usage.exe");
        let cleanup_paths = paths();
        let managed =
            build_targets(UninstallKind::Managed, executable, false, &cleanup_paths).unwrap();
        assert!(managed.contains(&CleanupTarget::Directory(PathBuf::from(
            r"C:\Users\user\AppData\Local\Programs\CodexUsage"
        ))));
        assert!(managed.contains(&CleanupTarget::UninstallRegistryTree));
        assert!(!managed.contains(&CleanupTarget::Directory(PathBuf::from(
            r"C:\Users\user\AppData\Roaming\CodexUsage"
        ))));

        let portable_exe = Path::new(r"D:\Portable\codex-usage.exe");
        let portable =
            build_targets(UninstallKind::Portable, portable_exe, false, &cleanup_paths).unwrap();
        assert!(portable.contains(&CleanupTarget::File(portable_exe.to_path_buf())));
        assert!(!portable
            .iter()
            .any(|target| matches!(target, CleanupTarget::Directory(_))));
        assert!(!portable.contains(&CleanupTarget::UninstallRegistryTree));
        assert!(portable.contains(&CleanupTarget::StartupValue {
            name: "CodexUsage",
            only_if_executable: Some(portable_exe.to_path_buf()),
        }));
    }

    #[test]
    fn settings_target_is_opt_in_and_winget_plan_is_empty() {
        let exe = Path::new(r"D:\Portable\codex-usage.exe");
        let cleanup_paths = paths();
        let portable = build_targets(UninstallKind::Portable, exe, true, &cleanup_paths).unwrap();
        assert!(portable.contains(&CleanupTarget::Directory(PathBuf::from(
            r"C:\Users\user\AppData\Roaming\CodexUsage"
        ))));
        assert!(
            build_targets(UninstallKind::Winget, exe, true, &cleanup_paths)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn path_guards_compare_components_not_prefixes() {
        assert!(is_same_or_child_path(
            Path::new(r"C:\Root\child\file.exe"),
            Path::new(r"C:\Root")
        ));
        assert!(!is_same_or_child_path(
            Path::new(r"C:\RootBackup\file.exe"),
            Path::new(r"C:\Root")
        ));
        assert!(paths_equal(
            Path::new(r"C:\Root\CodexUsage.exe"),
            Path::new(r"c:\root\codexusage.EXE")
        ));
    }

    #[test]
    fn portable_startup_cleanup_requires_an_exact_executable_reference() {
        let executable = Path::new(r"C:\Users\user\Portable Apps\codex-usage.exe");
        assert!(startup_value_references(
            OsStr::new(r#""C:\Users\user\Portable Apps\codex-usage.exe""#),
            executable
        ));
        assert!(!startup_value_references(
            OsStr::new(r#""C:\Users\user\Portable Apps\codex-usage.exe" --background"#),
            executable
        ));
        assert!(!startup_value_references(
            OsStr::new(r"C:\Users\user\Portable Apps\codex-usage-old.exe"),
            executable
        ));
    }
}
