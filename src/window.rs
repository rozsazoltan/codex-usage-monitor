use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::Controls::{NMHDR, TTN_SHOW};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::{ExtractIconExW, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::diagnose;
use crate::localization::{self, LanguageId, Strings};
use crate::models::AppUsageData;
use crate::native_interop::{
    self, Color, TIMER_COUNTDOWN, TIMER_FRESHNESS, TIMER_POLL, TIMER_TASKBAR_RETRY,
    TIMER_UPDATE_CHECK, WM_APP_TRAY, WM_APP_USAGE_UPDATED,
};
use crate::poller;
use crate::provider_icons::{self, Provider};
use crate::provider_poll;
use crate::quota_refresh;
use crate::quota_text;
use crate::quota_tooltip;
use crate::recovery_events;
use crate::settings_store;
use crate::settings_ui::{self, Action, Key, Operation, Page};
use crate::theme;
use crate::tray_icon;
use crate::updater::{self, InstallChannel, ReleaseDescriptor, UpdateCheckResult};

/// Wrapper to make HWND sendable across threads (safe for PostMessage usage)
#[derive(Clone, Copy)]
struct SendHwnd(isize);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }
    fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

/// Shared application state
struct AppState {
    hwnd: SendHwnd,
    taskbar_broadcast_hwnd: Option<HWND>,
    taskbar_hwnd: Option<HWND>,
    tray_notify_hwnd: Option<HWND>,
    win_event_hook: Option<HWINEVENTHOOK>,
    is_dark: bool,
    embedded: bool,
    language_override: Option<LanguageId>,
    language: LanguageId,
    appearance: Appearance,
    install_channel: InstallChannel,

    session_percent: f64,
    session_text: String,
    weekly_percent: f64,
    weekly_text: String,
    codex_session_percent: f64,
    codex_session_text: String,
    codex_weekly_percent: f64,
    codex_weekly_text: String,
    antigravity_session_percent: f64,
    antigravity_session_text: String,
    antigravity_weekly_percent: f64,
    antigravity_weekly_text: String,
    opencode_session_percent: f64,
    opencode_session_text: String,
    opencode_weekly_percent: f64,
    opencode_weekly_text: String,
    cursor_session_percent: f64,
    cursor_session_text: String,
    cursor_weekly_percent: f64,
    cursor_weekly_text: String,
    grok_session_percent: f64,
    grok_session_text: String,
    grok_weekly_percent: f64,
    grok_weekly_text: String,
    copilot_session_percent: f64,
    copilot_session_text: String,
    copilot_weekly_percent: f64,
    copilot_weekly_text: String,
    provider_availability: poller::ProviderAvailability,
    development_overrides: [DevelopmentOverride; 7],
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    show_opencode: bool,
    show_cursor: bool,
    show_grok: bool,
    show_copilot: bool,
    allow_unlimited_providers: bool,
    show_tray_icon: bool,
    show_provider_icons: bool,
    show_session_window: bool,
    show_weekly_window: bool,
    alert_threshold_percent: u8,
    notified_quota_windows: BTreeSet<String>,

    data: Option<AppUsageData>,

    poll_interval_ms: u32,
    adaptive_refresh: bool,
    monitor: provider_poll::Monitor,
    recovery_watch: Option<recovery_events::Watch>,
    recovery_debounce: recovery_events::Debounce,
    has_poll_result: bool,
    update_status: UpdateStatus,
    last_update_check_unix: Option<u64>,

    taskbar_index: usize,
    monitor_device: Option<String>,
    effective_monitor_device: Option<String>,
    pending_monitor_device: Option<Option<String>>,
    legacy_selection_pending: bool,
    tray_offset: i32,
    anchor_left: bool,
    dragging: bool,
    drag_start_mouse_x: i32,
    drag_start_client_x: i32,
    drag_start_offset: i32,

    widget_visible: bool,
    settings_pane_open: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct DevelopmentOverride {
    enabled: bool,
    force_available: bool,
    session_percent: Option<f64>,
    weekly_percent: Option<f64>,
}

#[derive(Clone, Debug)]
struct ProviderDisplay {
    session_percent: f64,
    session_text: String,
    weekly_percent: f64,
    weekly_text: String,
}

#[derive(Clone, Debug)]
enum UpdateStatus {
    Idle,
    Checking,
    Applying,
    UpToDate,
    Available(ReleaseDescriptor),
}

const POLL_1_MIN: u32 = 60_000;
const POLL_5_MIN: u32 = 300_000;
const POLL_15_MIN: u32 = 900_000;
const POLL_1_HOUR: u32 = 3_600_000;

// Menu item IDs for update frequency
const IDM_FREQ_1MIN: u16 = 10;
const IDM_FREQ_5MIN: u16 = 11;
const IDM_FREQ_15MIN: u16 = 12;
const IDM_FREQ_1HOUR: u16 = 13;
const IDM_ADAPTIVE_REFRESH: u16 = 14;
const IDM_START_WITH_WINDOWS: u16 = 20;
const IDM_RESET_POSITION: u16 = 30;
const IDM_ANCHOR_LEFT: u16 = 34;
const IDM_VERSION_ACTION: u16 = 31;
const IDM_LANG_SYSTEM: u16 = 40;
const IDM_LANG_ENGLISH: u16 = 41;
const IDM_LANG_DUTCH: u16 = 42;
const IDM_LANG_SPANISH: u16 = 43;
const IDM_LANG_FRENCH: u16 = 44;
const IDM_LANG_GERMAN: u16 = 45;
const IDM_LANG_JAPANESE: u16 = 46;
const IDM_LANG_KOREAN: u16 = 47;
const IDM_LANG_TRADITIONAL_CHINESE: u16 = 48;
const IDM_LANG_RUSSIAN: u16 = 49;
const IDM_LANG_PORTUGUESE_BRAZIL: u16 = 50;
const IDM_LANG_SIMPLIFIED_CHINESE: u16 = 51;
const IDM_MODEL_CLAUDE_CODE: u16 = 60;
const IDM_MODEL_CODEX: u16 = 61;
const IDM_MODEL_ANTIGRAVITY: u16 = 62;
const IDM_SHOW_SESSION_WINDOW: u16 = 71;
const IDM_SHOW_WEEKLY_WINDOW: u16 = 72;
const IDM_ALERT_OFF: u16 = 80;
const IDM_ALERT_10: u16 = 81;
const IDM_ALERT_20: u16 = 82;
const IDM_ALERT_30: u16 = 83;
const IDM_PALETTE_SYSTEM: u16 = 90;
const IDM_PALETTE_DARK: u16 = 91;
const IDM_PALETTE_LIGHT: u16 = 92;
const IDM_BAR_CONTINUOUS: u16 = 93;
const IDM_BAR_SEGMENTED: u16 = 94;
const IDM_BAR_STANDARD: u16 = 95;
const IDM_BAR_SLIM: u16 = 96;
const IDM_FONT_STANDARD: u16 = 97;
const IDM_FONT_LARGE: u16 = 98;
const IDM_APPEARANCE_RECOMMENDED: u16 = 99;
const IDM_APPEARANCE_RESET: u16 = 100;
const IDM_COLOR_CLAUDE: u16 = 101;
const IDM_COLOR_CODEX: u16 = 102;
const IDM_COLOR_RESET: u16 = 103;
const IDM_ABOUT_STAR: u16 = 104;
const IDM_UNINSTALL: u16 = 105;
const IDM_MONITOR_FIRST: u16 = 200;
const MAX_MONITOR_MENU_ITEMS: usize = 100;
const IDM_SETTINGS: u16 = 106;
const IDM_APPEARANCE_PAGE: u16 = 107;

const WM_DPICHANGED_MSG: u32 = 0x02E0;
const WM_APP_UPDATE_CHECK_COMPLETE: u32 = WM_APP + 2;
const WM_APP_TASKBAR_CREATED: u32 = WM_APP + 4;
const WM_APP_MONITOR_REFRESH: u32 = WM_APP + 5;
const TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS: u64 = 750;

/// How often the watchdog thread polls for an explorer.exe restart (which
/// recreates the taskbar and wipes our tray-icon registration).
const TASKBAR_WATCH_INTERVAL_SECS: u64 = 2;

static SUPPRESS_TRAY_REPOSITION_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static MONITOR_TRANSITION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static PENDING_MONITOR_REFRESH: AtomicBool = AtomicBool::new(false);
static PENDING_SETTING_CHANGE: AtomicBool = AtomicBool::new(false);
static PENDING_TASKBAR_REFRESH: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct ChooseColor {
    size: u32,
    owner: HWND,
    instance: HINSTANCE,
    result: u32,
    custom_colors: *mut u32,
    flags: u32,
    data: LPARAM,
    hook: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> u32>,
    template: PCWSTR,
}

#[link(name = "comdlg32")]
extern "system" {
    fn ChooseColorW(dialog: *mut ChooseColor) -> BOOL;
}

fn choose_accent_color(hwnd: HWND, initial: Color) -> Option<Color> {
    let mut custom_colors = [0x00FF_FFFFu32; 16];
    let mut dialog = ChooseColor {
        size: std::mem::size_of::<ChooseColor>() as u32,
        owner: hwnd,
        instance: HINSTANCE::default(),
        result: initial.to_colorref(),
        custom_colors: custom_colors.as_mut_ptr(),
        flags: 0x0000_0001 | 0x0000_0002,
        data: LPARAM(0),
        hook: None,
        template: PCWSTR::null(),
    };
    unsafe {
        ChooseColorW(&mut dialog).as_bool().then(|| {
            Color::new(
                (dialog.result & 0xFF) as u8,
                ((dialog.result >> 8) & 0xFF) as u8,
                ((dialog.result >> 16) & 0xFF) as u8,
            )
        })
    }
}

/// Current system DPI (96 = 100% scaling, 144 = 150%, 192 = 200%, etc.)
static CURRENT_DPI: AtomicU32 = AtomicU32::new(96);

/// Scale a base pixel value (designed at 96 DPI) to the current DPI.
fn sc(px: i32) -> i32 {
    let dpi = CURRENT_DPI.load(Ordering::Relaxed);
    (px as f64 * dpi as f64 / 96.0).round() as i32
}

/// Re-query the monitor DPI for our window and update the cached value.
/// Uses GetDpiForWindow which returns the live DPI (unlike GetDpiForSystem
/// which is cached at process startup and never changes).
fn refresh_dpi() {
    let hwnd = {
        let state = lock_state();
        state.as_ref().map(|s| s.hwnd.to_hwnd())
    };
    if let Some(hwnd) = hwnd {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi > 0 {
            CURRENT_DPI.store(dpi, Ordering::Relaxed);
        }
    }
}

/// Spacing below which two relaunches are treated as a storm (e.g. explorer.exe
/// crash-looping); when detected we back off instead of spawning in a tight loop.
const RELAUNCH_THROTTLE_SECS: u64 = 10;
const RELAUNCH_BACKOFF_SECS: u64 = 30;
/// Environment flag set on a relaunched child so it waits for the previous
/// instance's single-instance mutex instead of exiting immediately.
const ENV_RELAUNCH: &str = "QUOTA_USAGE_RELAUNCH";
const LEGACY_ENV_RELAUNCH: &str = "CODEX_USAGE_RELAUNCH";
/// Unix timestamp (seconds) of the relaunch that spawned this process, passed to
/// the child so it can detect a relaunch storm.
const ENV_LAST_RELAUNCH_UNIX: &str = "QUOTA_USAGE_LAST_RELAUNCH_UNIX";
const LEGACY_ENV_LAST_RELAUNCH_UNIX: &str = "CODEX_USAGE_LAST_RELAUNCH_UNIX";

/// Relaunch the widget as a fresh process after explorer.exe has restarted.
///
/// When the shell restarts it destroys our embedded child window outright (the
/// window is gone, not merely orphaned - `IsWindow` returns false) and leaves
/// the UI thread parked in `GetMessage` with no window to recreate in place.
/// Spawning a clean new process - which re-embeds into the freshly created
/// taskbar - and exiting this one is the robust recovery. The child is flagged
/// via `ENV_RELAUNCH` so it waits for this instance's single-instance mutex to
/// be released before taking over (see the guard in `run`).
fn relaunch_self() {
    settings_ui::shutdown();
    // Back off if we are relaunching very soon after the relaunch that spawned
    // us: that signals the shell is crash-looping, not a one-off restart.
    let now = now_unix_secs();
    let last = std::env::var(ENV_LAST_RELAUNCH_UNIX)
        .or_else(|_| std::env::var(LEGACY_ENV_LAST_RELAUNCH_UNIX))
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if last != 0 && now.saturating_sub(last) < RELAUNCH_THROTTLE_SECS {
        diagnose::log("relaunch storm detected; backing off before relaunching");
        std::thread::sleep(Duration::from_secs(RELAUNCH_BACKOFF_SECS));
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            diagnose::log_error("watchdog: unable to resolve current executable", error);
            return;
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    match std::process::Command::new(exe)
        .args(&args)
        .env_remove(LEGACY_ENV_RELAUNCH)
        .env_remove(LEGACY_ENV_LAST_RELAUNCH_UNIX)
        .env(ENV_RELAUNCH, "1")
        .env(ENV_LAST_RELAUNCH_UNIX, now.to_string())
        .spawn()
    {
        Ok(_) => {
            diagnose::log("watchdog: relaunched fresh instance, exiting old one");
            std::process::exit(0);
        }
        Err(error) => {
            diagnose::log_error("watchdog: unable to spawn relaunched instance", error);
        }
    }
}

/// Detect explorer.exe restarts and recover from them.
///
/// Once Explorer destroys the taskbar, the embedded child window may also be
/// destroyed and its UI message loop can stop. This independent thread repairs
/// stale live attachments in-process and relaunches only when the widget HWND is
/// gone.
fn spawn_taskbar_watchdog() {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(TASKBAR_WATCH_INTERVAL_SECS));
        if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
            continue;
        }
        let Some(snapshot) = watchdog_window_snapshot() else {
            continue;
        };
        if !snapshot.embedded {
            continue;
        }
        let taskbars = native_interop::find_taskbars();
        let widget_alive = unsafe { IsWindow(snapshot.hwnd).as_bool() };
        let attachment_matches = watchdog_attachment_matches(&snapshot, &taskbars, widget_alive);
        match taskbar_watchdog_action(
            MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire),
            snapshot.embedded,
            widget_alive,
            attachment_matches,
        ) {
            TaskbarWatchdogAction::Ignore => {}
            TaskbarWatchdogAction::Refresh => {
                diagnose::log(
                    "watchdog: live widget attachment is stale; requesting in-process refresh",
                );
                unsafe {
                    let _ =
                        PostMessageW(snapshot.hwnd, WM_APP_TASKBAR_CREATED, WPARAM(0), LPARAM(0));
                }
            }
            TaskbarWatchdogAction::Relaunch => {
                // Re-read both the transition guard and the live HWND immediately
                // before relaunching; an obsolete taskbar snapshot must not
                // restart a widget that has since migrated successfully.
                if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
                    continue;
                }
                let Some(current) = watchdog_window_snapshot() else {
                    continue;
                };
                let current_alive = unsafe { IsWindow(current.hwnd).as_bool() };
                let current_taskbars = native_interop::find_taskbars();
                let current_attachment_matches =
                    watchdog_attachment_matches(&current, &current_taskbars, current_alive);
                match taskbar_watchdog_action(
                    MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire),
                    current.embedded,
                    current_alive,
                    current_attachment_matches,
                ) {
                    TaskbarWatchdogAction::Relaunch => {
                        diagnose::log("watchdog: widget HWND was destroyed; relaunching");
                        relaunch_self();
                    }
                    TaskbarWatchdogAction::Refresh => unsafe {
                        let _ = PostMessageW(
                            current.hwnd,
                            WM_APP_TASKBAR_CREATED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    },
                    TaskbarWatchdogAction::Ignore => {}
                }
            }
        }
    });
}

#[derive(Clone)]
struct WatchdogWindowSnapshot {
    hwnd: HWND,
    embedded: bool,
    taskbar_hwnd: Option<HWND>,
    effective_monitor_device: Option<String>,
}

fn watchdog_window_snapshot() -> Option<WatchdogWindowSnapshot> {
    let state = lock_state();
    state.as_ref().map(|state| WatchdogWindowSnapshot {
        hwnd: state.hwnd.to_hwnd(),
        embedded: state.embedded,
        taskbar_hwnd: state.taskbar_hwnd,
        effective_monitor_device: state.effective_monitor_device.clone(),
    })
}

fn watchdog_attachment_matches(
    snapshot: &WatchdogWindowSnapshot,
    taskbars: &[native_interop::TaskbarWindow],
    widget_alive: bool,
) -> bool {
    if !snapshot.embedded || !widget_alive {
        return false;
    }
    let (Some(taskbar_hwnd), Some(effective_monitor)) = (
        snapshot.taskbar_hwnd,
        snapshot.effective_monitor_device.as_deref(),
    ) else {
        return false;
    };
    let taskbar_is_current = taskbars.iter().any(|taskbar| {
        taskbar.hwnd == taskbar_hwnd
            && native_interop::taskbar_monitor_device(taskbar).as_deref() == Some(effective_monitor)
    });
    taskbar_is_current
        && unsafe { GetParent(snapshot.hwnd).is_ok_and(|parent| parent == taskbar_hwnd) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskbarWatchdogAction {
    Ignore,
    Refresh,
    Relaunch,
}

fn taskbar_watchdog_action(
    transition_in_progress: bool,
    embedded: bool,
    widget_alive: bool,
    current_attachment_matches: bool,
) -> TaskbarWatchdogAction {
    if transition_in_progress || !embedded {
        TaskbarWatchdogAction::Ignore
    } else if !widget_alive {
        TaskbarWatchdogAction::Relaunch
    } else if current_attachment_matches {
        TaskbarWatchdogAction::Ignore
    } else {
        TaskbarWatchdogAction::Refresh
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragMonitorTransition {
    None,
    Succeeded(i32),
    Failed,
}

fn drag_offset_to_persist(drag_offset: i32, transition: DragMonitorTransition) -> i32 {
    match transition {
        DragMonitorTransition::Succeeded(offset) => offset,
        DragMonitorTransition::None | DragMonitorTransition::Failed => drag_offset,
    }
}

fn load_embedded_app_icons() -> (HICON, HICON) {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return (HICON::default(), HICON::default());
        }

        let mut large_icon = HICON::default();
        let mut small_icon = HICON::default();
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(exe_buf.as_ptr()),
            0,
            Some(&mut large_icon),
            Some(&mut small_icon),
            1,
        );

        if extracted == 0 {
            (HICON::default(), HICON::default())
        } else {
            (large_icon, small_icon)
        }
    }
}

unsafe impl Send for AppState {}

static STATE: Mutex<Option<AppState>> = Mutex::new(None);

/// Lock STATE safely, recovering from poisoned mutex
fn lock_state() -> MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

const SETTINGS_DIR: &str = "QuotaUsage";
const LEGACY_SETTINGS_DIRS: [&str; 2] = ["CodexUsage", "ClaudeCodeUsageMonitor"];

fn appdata_path(directory: &str) -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata).join(directory).join("settings.json")
}

fn settings_path() -> PathBuf {
    appdata_path(SETTINGS_DIR)
}

fn legacy_settings_paths() -> [PathBuf; 2] {
    LEGACY_SETTINGS_DIRS.map(appdata_path)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Palette {
    #[default]
    System,
    HighContrastDark,
    HighContrastLight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BarStyle {
    #[default]
    Continuous,
    Segmented,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BarThickness {
    Standard,
    #[default]
    Slim,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FontSize {
    #[default]
    Standard,
    Large,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProviderSpacing {
    None,
    #[default]
    Small,
    Normal,
    Large,
}

impl ProviderSpacing {
    fn scaled_gap(self) -> i32 {
        sc(match self {
            Self::None => 2,
            Self::Small => 4,
            Self::Normal => 7,
            Self::Large => 11,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProviderId {
    Claude,
    Codex,
    Antigravity,
    #[serde(rename = "opencode")]
    OpenCode,
    Cursor,
    Grok,
    Copilot,
}

impl ProviderId {
    const ALL: [Self; 7] = [
        Self::Claude,
        Self::Codex,
        Self::Antigravity,
        Self::OpenCode,
        Self::Cursor,
        Self::Grok,
        Self::Copilot,
    ];

    fn index(self) -> usize {
        match self {
            Self::Claude => 0,
            Self::Codex => 1,
            Self::Antigravity => 2,
            Self::OpenCode => 3,
            Self::Cursor => 4,
            Self::Grok => 5,
            Self::Copilot => 6,
        }
    }
}

fn default_provider_order() -> [ProviderId; 7] {
    [
        ProviderId::Claude,
        ProviderId::Codex,
        ProviderId::Antigravity,
        ProviderId::Copilot,
        ProviderId::Cursor,
        ProviderId::Grok,
        ProviderId::OpenCode,
    ]
}

// Keep sparse older settings files on their historical values; Appearance::default
// defines the current defaults for new installs and explicit appearance resets.
fn legacy_default_provider_order() -> [ProviderId; 7] {
    ProviderId::ALL
}

fn legacy_default_provider_spacing() -> ProviderSpacing {
    ProviderSpacing::Small
}

fn legacy_default_provider_monochrome() -> bool {
    false
}

fn valid_provider_order(order: &[ProviderId; 7]) -> bool {
    order.iter().copied().collect::<HashSet<_>>().len() == ProviderId::ALL.len()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProviderIconPosition {
    #[default]
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Appearance {
    palette: Palette,
    bar_style: BarStyle,
    bar_thickness: BarThickness,
    font_size: FontSize,
    #[serde(default = "legacy_default_provider_spacing")]
    provider_spacing: ProviderSpacing,
    #[serde(default = "legacy_default_provider_order")]
    provider_order: [ProviderId; 7],
    #[serde(default)]
    provider_icon_position: ProviderIconPosition,
    #[serde(default = "legacy_default_provider_monochrome")]
    provider_monochrome: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_accent: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_accent: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    antigravity_accent: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opencode_accent: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_accent: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grok_accent: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copilot_accent: Option<[u8; 3]>,
    show_provider_icon: bool,
    show_claude_icon: bool,
    show_codex_icon: bool,
    show_antigravity_icon: bool,
    show_opencode_icon: bool,
    show_cursor_icon: bool,
    show_grok_icon: bool,
    show_copilot_icon: bool,
    show_percentage: bool,
    show_bar: bool,
    show_reset_time: bool,
    usage_text_color: Option<[u8; 3]>,
    window_label_font_size: u8,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            palette: Palette::default(),
            bar_style: BarStyle::default(),
            bar_thickness: BarThickness::default(),
            font_size: FontSize::default(),
            provider_spacing: ProviderSpacing::Normal,
            provider_order: default_provider_order(),
            provider_icon_position: ProviderIconPosition::default(),
            provider_monochrome: true,
            claude_accent: None,
            codex_accent: None,
            antigravity_accent: None,
            opencode_accent: None,
            cursor_accent: None,
            grok_accent: None,
            copilot_accent: None,
            show_provider_icon: true,
            show_claude_icon: true,
            show_codex_icon: true,
            show_antigravity_icon: true,
            show_opencode_icon: true,
            show_cursor_icon: true,
            show_grok_icon: true,
            show_copilot_icon: true,
            show_percentage: true,
            show_bar: true,
            show_reset_time: true,
            usage_text_color: None,
            window_label_font_size: 9,
        }
    }
}

impl Appearance {
    fn translucent_dark_taskbar() -> Self {
        Self {
            palette: Palette::HighContrastDark,
            bar_style: BarStyle::Continuous,
            bar_thickness: BarThickness::Slim,
            font_size: FontSize::Large,
            claude_accent: None,
            codex_accent: None,
            ..Self::default()
        }
    }
    fn is_dark(self, system_is_dark: bool) -> bool {
        match self.palette {
            Palette::System => system_is_dark,
            Palette::HighContrastDark => true,
            Palette::HighContrastLight => false,
        }
    }

    fn font_px(self) -> i32 {
        match self.font_size {
            FontSize::Standard => 12,
            FontSize::Large => 13,
        }
    }

    /// Physical row height; large text needs one more pixel of line height.
    fn row_height(self) -> i32 {
        sc((match self.font_size {
            FontSize::Standard => ROW_HEIGHT,
            FontSize::Large => ROW_HEIGHT + 1,
        })
        .max(i32::from(self.window_label_font_size) + 6))
    }

    fn bar_height(self) -> i32 {
        sc(match self.bar_thickness {
            BarThickness::Standard => SEGMENT_H,
            BarThickness::Slim => 8,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    tray_offset: i32,
    #[serde(default)]
    anchor_left: bool,
    #[serde(default)]
    taskbar_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    monitor_device: Option<String>,
    #[serde(default = "default_poll_interval")]
    poll_interval_ms: u32,
    #[serde(default = "default_adaptive_refresh")]
    adaptive_refresh: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default)]
    appearance: Appearance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_update_check_unix: Option<u64>,
    #[serde(default = "default_widget_visible")]
    widget_visible: bool,
    #[serde(default = "default_settings_pane_open")]
    settings_pane_open: bool,
    #[serde(default = "default_show_tray_icon")]
    show_tray_icon: bool,
    #[serde(default = "default_show_provider_icons")]
    show_provider_icons: bool,
    #[serde(default = "default_show_claude_code")]
    show_claude_code: bool,
    #[serde(default = "default_show_codex")]
    show_codex: bool,
    #[serde(default = "default_show_antigravity")]
    show_antigravity: bool,
    #[serde(default = "default_show_new_provider")]
    show_opencode: bool,
    #[serde(default = "default_show_new_provider")]
    show_cursor: bool,
    #[serde(default = "default_show_new_provider")]
    show_grok: bool,
    #[serde(default = "default_show_new_provider")]
    show_copilot: bool,
    #[serde(default)]
    allow_unlimited_providers: bool,
    #[serde(default = "default_show_usage_window")]
    show_session_window: bool,
    #[serde(default = "default_show_usage_window")]
    show_weekly_window: bool,
    #[serde(default)]
    alert_threshold_percent: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notified_quota_windows: Vec<String>,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            tray_offset: 0,
            anchor_left: false,
            taskbar_index: 0,
            monitor_device: None,
            poll_interval_ms: default_poll_interval(),
            adaptive_refresh: true,
            language: None,
            appearance: Appearance::default(),
            last_update_check_unix: None,
            widget_visible: true,
            settings_pane_open: default_settings_pane_open(),
            show_tray_icon: default_show_tray_icon(),
            show_provider_icons: default_show_provider_icons(),
            show_claude_code: false,
            show_codex: true,
            show_antigravity: false,
            show_opencode: false,
            show_cursor: false,
            show_grok: false,
            show_copilot: false,
            allow_unlimited_providers: false,
            show_session_window: true,
            show_weekly_window: true,
            alert_threshold_percent: 0,
            notified_quota_windows: Vec::new(),
        }
    }
}

fn default_adaptive_refresh() -> bool {
    true
}

fn default_poll_interval() -> u32 {
    POLL_15_MIN
}

fn default_widget_visible() -> bool {
    true
}

fn default_settings_pane_open() -> bool {
    true
}

fn default_show_tray_icon() -> bool {
    true
}

fn default_show_provider_icons() -> bool {
    true
}

fn default_show_claude_code() -> bool {
    false
}

fn default_show_codex() -> bool {
    true
}

fn default_show_antigravity() -> bool {
    false
}

fn default_show_new_provider() -> bool {
    false
}

fn default_show_usage_window() -> bool {
    true
}

fn load_settings(claude_code_available: bool) -> SettingsFile {
    let current_path = settings_path();
    let legacy_paths = legacy_settings_paths();
    let (settings, migrated) = load_settings_from_paths(
        &current_path,
        &legacy_paths[0],
        &legacy_paths[1],
    )
        .unwrap_or_else(|| (SettingsFile::default(), false));
    let settings = normalize_settings(settings);
    let (settings, claude_auto_disabled) =
        apply_claude_code_availability(settings, claude_code_available);
    if migrated || claude_auto_disabled {
        save_settings(&settings);
        if migrated {
            diagnose::log(format!("migrated legacy settings to {}", current_path.display()));
        }
        if claude_auto_disabled {
            diagnose::log(
                "disabled Claude Code monitoring because no CLI credentials are available",
            );
        }
    }
    settings
}

fn apply_claude_code_availability(
    mut settings: SettingsFile,
    claude_code_available: bool,
) -> (SettingsFile, bool) {
    let disabled = settings.show_claude_code && !claude_code_available;
    if disabled {
        settings.show_claude_code = false;
        settings = normalize_settings(settings);
    }
    (settings, disabled)
}

fn load_settings_from_paths(
    current_path: &std::path::Path,
    codex_legacy_path: &std::path::Path,
    claude_legacy_path: &std::path::Path,
) -> Option<(SettingsFile, bool)> {
    if let Ok(content) = std::fs::read_to_string(current_path) {
        if let Ok(settings) = serde_json::from_str(&content) {
            return Some((settings, false));
        }
    }

    [codex_legacy_path, claude_legacy_path]
        .into_iter()
        .find_map(|path| {
            let content = std::fs::read_to_string(path).ok()?;
            serde_json::from_str(&content)
                .ok()
                .map(|settings| (settings, true))
        })
}

fn normalize_settings(mut settings: SettingsFile) -> SettingsFile {
    // Preserve the pre-existing widget-icon preference on legacy settings.
    if !settings.show_provider_icons {
        settings.appearance.show_provider_icon = false;
    }
    settings.show_provider_icons = settings.appearance.show_provider_icon;
    if !valid_provider_order(&settings.appearance.provider_order) {
        settings.appearance.provider_order = default_provider_order();
    }
    if !settings.appearance.show_percentage
        && !settings.appearance.show_bar
        && !settings.appearance.show_reset_time
    {
        settings.appearance.show_percentage = true;
    }
    if !(8..=14).contains(&settings.appearance.window_label_font_size) {
        settings.appearance.window_label_font_size = 9;
    }
    if !(POLL_1_MIN..=POLL_1_HOUR).contains(&settings.poll_interval_ms) {
        settings.poll_interval_ms = POLL_15_MIN;
    }
    if !settings.show_claude_code
        && !settings.show_codex
        && !settings.show_antigravity
        && !settings.show_opencode
        && !settings.show_cursor
        && !settings.show_grok
        && !settings.show_copilot
    {
        settings.show_codex = true;
    }
    if !settings.show_session_window && !settings.show_weekly_window {
        settings.show_session_window = true;
    }
    if !matches!(settings.alert_threshold_percent, 0 | 10 | 20 | 30) {
        settings.alert_threshold_percent = 0;
    }
    settings.notified_quota_windows.sort();
    settings.notified_quota_windows.dedup();
    settings
}

fn save_settings(settings: &SettingsFile) {
    if let Err(error) = settings_store::save(&settings_path(), || Some(settings.clone())) {
        diagnose::log_error("unable to save settings", error);
    }
}

fn save_state_settings() {
    if let Err(error) = persist_state_settings() {
        SETTINGS_SAVE_ERROR.with(|slot| *slot.borrow_mut() = Some(error.clone()));
    }
    if let Some(s) = lock_state().as_ref() {
        unsafe {
            let _ = PostMessageW(
                s.hwnd.to_hwnd(),
                settings_ui::WM_SETTINGS,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

fn persist_state_settings() -> Result<(), String> {
    settings_store::save(&settings_path(), || {
        let state = lock_state();
        state.as_ref().map(|s| SettingsFile {
            tray_offset: s.tray_offset,
            anchor_left: s.anchor_left,
            taskbar_index: if s.monitor_device.is_some() || s.legacy_selection_pending {
                s.taskbar_index
            } else {
                0
            },
            monitor_device: s.monitor_device.clone(),
            poll_interval_ms: s.poll_interval_ms,
            adaptive_refresh: s.adaptive_refresh,
            language: s
                .language_override
                .map(|language| language.code().to_string()),
            appearance: s.appearance,
            last_update_check_unix: s.last_update_check_unix,
            widget_visible: s.widget_visible,
            settings_pane_open: s.settings_pane_open,
            show_tray_icon: s.show_tray_icon,
            show_provider_icons: s.show_provider_icons,
            show_claude_code: s.show_claude_code,
            show_codex: s.show_codex,
            show_antigravity: s.show_antigravity,
            show_opencode: s.show_opencode,
            show_cursor: s.show_cursor,
            show_grok: s.show_grok,
            show_copilot: s.show_copilot,
            allow_unlimited_providers: s.allow_unlimited_providers,
            show_session_window: s.show_session_window,
            show_weekly_window: s.show_weekly_window,
            alert_threshold_percent: s.alert_threshold_percent,
            notified_quota_windows: s.notified_quota_windows.iter().cloned().collect(),
        })
    })
    .map_err(|error| {
        diagnose::log_error("unable to save settings", &error);
        format!("The setting was applied but could not be saved: {error}")
    })
}

thread_local! {
    static SETTINGS_SAVE_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn format_precise_reset_time(resets_at: Option<SystemTime>) -> Option<String> {
    let local = native_interop::system_time_to_local(resets_at?)?;
    Some(format_local_system_time(local))
}

fn format_local_system_time(local: SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
    )
}

fn service_tooltip(
    service: &str,
    session_text: &str,
    weekly_text: &str,
    show_session_window: bool,
    show_weekly_window: bool,
) -> String {
    let mut parts = Vec::new();
    if show_session_window {
        parts.push(format!("5h {session_text}"));
    }
    if show_weekly_window {
        parts.push(format!("7d {weekly_text}"));
    }
    format!("{service}: {}", parts.join(" | "))
}

struct QuotaAlert {
    kind: tray_icon::TrayIconKind,
    title: String,
    message: String,
}

fn collect_low_quota_alerts(state: &mut AppState, data: &AppUsageData) -> Vec<QuotaAlert> {
    let threshold = state.alert_threshold_percent;
    if threshold == 0 {
        return Vec::new();
    }

    let strings = state.language.strings();
    let mut alerts = Vec::new();
    if state.show_claude_code {
        if let Some(usage) = data.claude_code.as_ref() {
            append_provider_alerts(
                &mut alerts,
                &mut state.notified_quota_windows,
                threshold,
                state.language,
                tray_icon::TrayIconKind::Claude,
                "claude",
                strings.claude_code_model,
                usage,
                strings,
            );
        }
    }
    if state.show_codex {
        if let Some(usage) = data.codex.as_ref() {
            append_provider_alerts(
                &mut alerts,
                &mut state.notified_quota_windows,
                threshold,
                state.language,
                tray_icon::TrayIconKind::Codex,
                "codex",
                strings.codex_model,
                usage,
                strings,
            );
        }
    }
    if state.show_antigravity {
        if let Some(usage) = data.antigravity.as_ref() {
            append_provider_alerts(
                &mut alerts,
                &mut state.notified_quota_windows,
                threshold,
                state.language,
                tray_icon::TrayIconKind::Antigravity,
                "antigravity",
                strings.antigravity_model,
                usage,
                strings,
            );
        }
    }
    if state.show_opencode {
        if let Some(usage) = data.opencode.as_ref() {
            append_provider_alerts(
                &mut alerts,
                &mut state.notified_quota_windows,
                threshold,
                state.language,
                tray_icon::TrayIconKind::OpenCode,
                "opencode",
                strings.opencode_model,
                usage,
                strings,
            );
        }
    }
    if state.show_cursor {
        if let Some(usage) = data.cursor.as_ref() {
            append_provider_alerts(
                &mut alerts,
                &mut state.notified_quota_windows,
                threshold,
                state.language,
                tray_icon::TrayIconKind::Cursor,
                "cursor",
                strings.cursor_model,
                usage,
                strings,
            );
        }
    }
    if state.show_grok {
        if let Some(usage) = data.grok.as_ref() {
            append_provider_alerts(
                &mut alerts,
                &mut state.notified_quota_windows,
                threshold,
                state.language,
                tray_icon::TrayIconKind::Grok,
                "grok",
                strings.grok_model,
                usage,
                strings,
            );
        }
    }
    if state.show_copilot {
        if let Some(usage) = data.copilot.as_ref() {
            append_provider_alerts(
                &mut alerts,
                &mut state.notified_quota_windows,
                threshold,
                state.language,
                tray_icon::TrayIconKind::Copilot,
                "copilot",
                strings.copilot_model,
                usage,
                strings,
            );
        }
    }
    alerts
}

#[allow(clippy::too_many_arguments)]
fn append_provider_alerts(
    alerts: &mut Vec<QuotaAlert>,
    notified: &mut BTreeSet<String>,
    threshold: u8,
    language: LanguageId,
    kind: tray_icon::TrayIconKind,
    provider_key: &str,
    provider_label: &str,
    usage: &crate::models::UsageData,
    strings: Strings,
) {
    append_quota_alert(
        alerts,
        notified,
        threshold,
        language,
        kind,
        provider_key,
        provider_label,
        "session",
        strings.session_window,
        &usage.session,
    );
    append_quota_alert(
        alerts,
        notified,
        threshold,
        language,
        kind,
        provider_key,
        provider_label,
        "weekly",
        strings.weekly_window,
        &usage.weekly,
    );
}

#[allow(clippy::too_many_arguments)]
fn append_quota_alert(
    alerts: &mut Vec<QuotaAlert>,
    notified: &mut BTreeSet<String>,
    threshold: u8,
    language: LanguageId,
    kind: tray_icon::TrayIconKind,
    provider_key: &str,
    provider_label: &str,
    window_key: &str,
    window_label: &str,
    section: &crate::models::UsageSection,
) {
    let prefix = format!("{provider_key}:{window_key}:");
    let reset_key = section
        .resets_at
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let key = format!("{prefix}{reset_key}");
    notified.retain(|existing| !existing.starts_with(&prefix) || existing == &key);

    let remaining = poller::remaining_percentage(section.percentage).round() as u8;
    if remaining > threshold || !notified.insert(key) {
        return;
    }

    let reset = format_precise_reset_time(section.resets_at);
    let (title, message) = if language == LanguageId::SimplifiedChinese {
        (
            format!("{provider_label} 额度提醒"),
            format!(
                "{window_label}额度仅剩 {remaining}%，重置时间：{}",
                reset.unwrap_or_else(|| "未知".to_string())
            ),
        )
    } else {
        (
            format!("{provider_label} quota alert"),
            format!(
                "{window_label} quota has {remaining}% remaining. Reset: {}",
                reset.unwrap_or_else(|| "unknown".to_string())
            ),
        )
    };
    alerts.push(QuotaAlert {
        kind,
        title,
        message,
    });
}

fn tray_icon_data_from_state() -> Option<tray_icon::TrayIconData> {
    let state = lock_state();
    match state.as_ref() {
        Some(s) if s.has_poll_result => {
            let mut services = Vec::new();
            let strings = s.language.strings();
            let providers = [
                (
                    s.show_claude_code,
                    strings.claude_code_model,
                    &s.session_text,
                    &s.weekly_text,
                ),
                (
                    s.show_codex,
                    strings.codex_model,
                    &s.codex_session_text,
                    &s.codex_weekly_text,
                ),
                (
                    s.show_antigravity,
                    strings.antigravity_model,
                    &s.antigravity_session_text,
                    &s.antigravity_weekly_text,
                ),
                (
                    s.show_opencode,
                    strings.opencode_model,
                    &s.opencode_session_text,
                    &s.opencode_weekly_text,
                ),
                (
                    s.show_cursor,
                    strings.cursor_model,
                    &s.cursor_session_text,
                    &s.cursor_weekly_text,
                ),
                (
                    s.show_grok,
                    strings.grok_model,
                    &s.grok_session_text,
                    &s.grok_weekly_text,
                ),
                (
                    s.show_copilot,
                    strings.copilot_model,
                    &s.copilot_session_text,
                    &s.copilot_weekly_text,
                ),
            ];
            for provider_id in s.appearance.provider_order {
                let (enabled, name, session, weekly) = providers[provider_id.index()];
                if enabled {
                    services.push(service_tooltip(
                        name,
                        session,
                        weekly,
                        s.show_session_window,
                        s.show_weekly_window,
                    ));
                }
            }
            Some(tray_icon::TrayIconData {
                tooltip: if services.is_empty() {
                    strings.window_title.to_string()
                } else {
                    services.join("\n")
                },
            })
        }
        Some(s) => {
            let strings = s.language.strings();
            let tooltip = match (
                s.show_claude_code,
                s.show_codex,
                s.show_antigravity,
                s.show_opencode,
                s.show_cursor,
                s.show_grok,
                s.show_copilot,
            ) {
                (false, true, false, false, false, false, false) => strings.codex_window_title,
                (false, false, true, false, false, false, false) => {
                    strings.antigravity_window_title
                }
                (false, false, false, true, false, false, false) => strings.opencode_window_title,
                (false, false, false, false, true, false, false) => strings.cursor_window_title,
                (false, false, false, false, false, true, false) => strings.grok_window_title,
                (false, false, false, false, false, false, true) => strings.copilot_window_title,
                _ => strings.window_title,
            };
            Some(tray_icon::TrayIconData {
                tooltip: tooltip.to_string(),
            })
        }
        None => None,
    }
}

fn sync_tray_icons(hwnd: HWND) {
    let enabled = lock_state()
        .as_ref()
        .map(|state| state.show_tray_icon)
        .unwrap_or(false);
    let icon = enabled.then(tray_icon_data_from_state).flatten();
    tray_icon::sync(hwnd, icon.as_ref());
}

fn taskbar_created_message() -> u32 {
    static TASKBAR_CREATED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *TASKBAR_CREATED.get_or_init(|| {
        let name = native_interop::wide_str("TaskbarCreated");
        unsafe { RegisterWindowMessageW(PCWSTR::from_raw(name.as_ptr())) }
    })
}

fn toggle_widget_visibility(hwnd: HWND) {
    let new_visible = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            s.widget_visible = !s.widget_visible;
            s.widget_visible
        } else {
            return;
        }
    };
    save_state_settings();
    if new_visible {
        recover_widget_visibility(hwnd);
        render_layered();
    } else {
        set_widget_visibility(hwnd, false);
    }
}

fn recover_widget_visibility(hwnd: HWND) {
    // An explicit Show request must resolve the live attachment, not just move
    // using a cached Explorer HWND. It must also restore a minimized popup:
    // WS_VISIBLE remains set while minimized, so the routine visibility guard
    // intentionally does not suffice here.
    let placement = refresh_monitor_selection(hwnd);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    // Showing can restore the old normal rectangle; apply the resolved placement
    // afterwards without changing z-order or activating the widget.
    if placement.reposition {
        position_at_taskbar_without_save();
    }
}

fn set_widget_visibility(hwnd: HWND, visible: bool) {
    unsafe {
        // Placement retries must not repeatedly show/raise an already visible
        // widget. Inspect its own visibility bit: Explorer may hide the parent
        // for auto-hide or full-screen UI without changing our desired state.
        let shown = GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE.0 != 0;
        if shown != visible {
            let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }
}

fn apply_configured_widget_visibility(hwnd: HWND) {
    let visible = lock_state().as_ref().map(|state| state.widget_visible);
    if let Some(visible) = visible {
        set_widget_visibility(hwnd, visible);
    }
}

#[derive(Clone, Debug)]
struct ResolvedMonitorTarget {
    monitor: native_interop::DisplayMonitor,
    taskbar: Option<(usize, native_interop::TaskbarWindow)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StartupPlacementDecision {
    placement_succeeded: bool,
    show_widget: bool,
    retry: bool,
}

fn startup_placement_decision(
    target_has_taskbar: bool,
    transition_succeeded: bool,
    widget_visible: bool,
    selection_pending: bool,
) -> StartupPlacementDecision {
    StartupPlacementDecision {
        placement_succeeded: transition_succeeded,
        show_widget: transition_succeeded && widget_visible,
        retry: selection_pending || !transition_succeeded || !target_has_taskbar,
    }
}

fn select_monitor<'a>(
    monitors: &'a [native_interop::DisplayMonitor],
    requested_device: Option<&str>,
    legacy_device: Option<&str>,
) -> Option<&'a native_interop::DisplayMonitor> {
    requested_device
        .and_then(|device| monitors.iter().find(|monitor| monitor.device == device))
        .or_else(|| {
            requested_device
                .is_none()
                .then_some(legacy_device)
                .flatten()
                .and_then(|device| monitors.iter().find(|monitor| monitor.device == device))
        })
        .or_else(|| monitors.iter().find(|monitor| monitor.is_primary))
        .or_else(|| monitors.first())
}

fn resolve_monitor_target(
    monitors: &[native_interop::DisplayMonitor],
    taskbars: &[native_interop::TaskbarWindow],
    requested_device: Option<&str>,
    legacy_device: Option<&str>,
) -> Option<ResolvedMonitorTarget> {
    resolve_monitor_target_with(
        monitors,
        taskbars,
        requested_device,
        legacy_device,
        native_interop::taskbar_monitor_device,
    )
}

fn resolve_monitor_target_with<F>(
    monitors: &[native_interop::DisplayMonitor],
    taskbars: &[native_interop::TaskbarWindow],
    requested_device: Option<&str>,
    legacy_device: Option<&str>,
    mut taskbar_device: F,
) -> Option<ResolvedMonitorTarget>
where
    F: FnMut(&native_interop::TaskbarWindow) -> Option<String>,
{
    let monitor = select_monitor(monitors, requested_device, legacy_device)?.clone();
    let taskbar = taskbars.iter().enumerate().find_map(|(index, taskbar)| {
        (taskbar_device(taskbar).as_deref() == Some(&monitor.device)).then_some((index, *taskbar))
    });
    Some(ResolvedMonitorTarget { monitor, taskbar })
}

fn legacy_monitor_device(
    taskbars: &[native_interop::TaskbarWindow],
    index: usize,
) -> Option<String> {
    (index > 0)
        .then(|| taskbars.get(index))
        .flatten()
        .and_then(native_interop::taskbar_monitor_device)
}

fn migrate_legacy_taskbar_selection(
    settings: &mut SettingsFile,
    monitors: &[native_interop::DisplayMonitor],
    legacy_taskbar_devices: &[Option<String>],
) -> bool {
    if settings.monitor_device.is_some() || settings.taskbar_index == 0 {
        return false;
    }
    let Some(device) = legacy_taskbar_devices
        .get(settings.taskbar_index)
        .cloned()
        .flatten()
    else {
        return false;
    };
    if monitors.iter().any(|monitor| monitor.device == device) {
        settings.monitor_device = Some(device);
        true
    } else {
        false
    }
}

struct MonitorTransitionGuard(HWND);

impl MonitorTransitionGuard {
    fn new(hwnd: HWND) -> Option<Self> {
        MONITOR_TRANSITION_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(hwnd))
    }
}

impl Drop for MonitorTransitionGuard {
    fn drop(&mut self) {
        MONITOR_TRANSITION_IN_PROGRESS.store(false, Ordering::Release);
        let refresh_monitor = PENDING_MONITOR_REFRESH.swap(false, Ordering::AcqRel);
        let refresh_taskbar = PENDING_TASKBAR_REFRESH.swap(false, Ordering::AcqRel);
        let setting_change = PENDING_SETTING_CHANGE.swap(false, Ordering::AcqRel);
        unsafe {
            if refresh_monitor {
                let _ = PostMessageW(
                    self.0,
                    WM_APP_MONITOR_REFRESH,
                    WPARAM(usize::from(setting_change)),
                    LPARAM(0),
                );
            }
            if refresh_taskbar {
                let _ = PostMessageW(self.0, WM_APP_TASKBAR_CREATED, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn transition_to_monitor(hwnd: HWND, target: &ResolvedMonitorTarget) -> bool {
    let (tray_offset, anchor_left, dragging) = {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return false;
        };
        (s.tray_offset, s.anchor_left, s.dragging)
    };
    if dragging {
        return false;
    }

    refresh_dpi();
    let width = total_widget_width();
    let height = sc(WIDGET_HEIGHT);
    let floating_position =
        floating_window_position(target.monitor.rect, width, height, anchor_left, tray_offset);
    let Some(_transition) = MonitorTransitionGuard::new(hwnd) else {
        return false;
    };

    let (embedded, position, taskbar) = if let Some((_, taskbar)) = target.taskbar {
        let rect = native_interop::get_taskbar_rect(taskbar.hwnd).unwrap_or(taskbar.rect);
        let tray_left = tray_left_for_taskbar(taskbar.hwnd, rect);
        let embedded_position = taskbar_window_position(
            taskbar.hwnd,
            rect,
            width,
            height,
            tray_left,
            tray_offset,
            anchor_left,
        )
        .map(|(position, _)| position);
        let Some(position) = embedded_position else {
            diagnose::log(format!(
                "taskbar attachment unavailable on {}; retaining current placement for retry",
                target.monitor.device
            ));
            return false;
        };
        if !native_interop::set_window_mode(hwnd, Some(taskbar.hwnd), position) {
            diagnose::log(format!(
                "taskbar attachment failed on {}; retaining current placement for retry",
                target.monitor.device
            ));
            return false;
        }
        (true, position, Some(taskbar))
    } else if native_interop::set_window_mode(hwnd, None, floating_position) {
        (false, floating_position, None)
    } else {
        return false;
    };

    refresh_dpi();
    let width_after_move = total_widget_width();
    let height_after_move = sc(WIDGET_HEIGHT);
    let adjusted_position = if let Some(taskbar) = taskbar {
        let rect = native_interop::get_taskbar_rect(taskbar.hwnd).unwrap_or(taskbar.rect);
        let tray_left = tray_left_for_taskbar(taskbar.hwnd, rect);
        taskbar_window_position(
            taskbar.hwnd,
            rect,
            width_after_move,
            height_after_move,
            tray_left,
            tray_offset,
            anchor_left,
        )
        .map(|(position, _)| position)
        .unwrap_or(position)
    } else {
        floating_window_position(
            target.monitor.rect,
            width_after_move,
            height_after_move,
            anchor_left,
            tray_offset,
        )
    };
    if adjusted_position != position {
        let parent = taskbar.map(|taskbar| taskbar.hwnd);
        if !native_interop::position_window(hwnd, parent, adjusted_position) {
            diagnose::log(
                "monitor transition DPI-adjusted position failed; keeping first placement",
            );
        }
    }
    let (taskbar_index, taskbar_hwnd) = match taskbar {
        Some(taskbar) => (target.taskbar.map(|(index, _)| index), Some(taskbar.hwnd)),
        None => (None, None),
    };
    let tray_notify = taskbar_hwnd
        .and_then(|taskbar| native_interop::find_child_window(taskbar, "TrayNotifyWnd"));
    let hook = tray_notify.and_then(|tray_hwnd| {
        native_interop::set_tray_event_hook(
            native_interop::get_window_thread_id(tray_hwnd),
            on_tray_location_changed,
        )
    });

    let old_hook = {
        let mut state = lock_state();
        let Some(s) = state.as_mut() else {
            drop(state);
            if let Some(hook) = hook {
                native_interop::unhook_win_event(hook);
            }
            return false;
        };
        let old_hook = s.win_event_hook.take();
        s.taskbar_hwnd = taskbar_hwnd;
        s.tray_notify_hwnd = tray_notify;
        s.win_event_hook = hook;
        if let Some(taskbar_index) = taskbar_index {
            s.taskbar_index = taskbar_index;
        } else if !s.legacy_selection_pending {
            s.taskbar_index = 0;
        }
        s.effective_monitor_device = Some(target.monitor.device.clone());
        s.embedded = embedded;
        old_hook
    };
    if let Some(hook) = old_hook {
        native_interop::unhook_win_event(hook);
    }
    drop(_transition);
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MonitorRefreshResult {
    retry: bool,
    reposition: bool,
}

fn refresh_monitor_selection(hwnd: HWND) -> MonitorRefreshResult {
    if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
        return MonitorRefreshResult {
            retry: true,
            reposition: false,
        };
    }
    let monitors = native_interop::find_monitors();
    let taskbars = native_interop::find_taskbars();
    let (
        requested,
        pending_selection,
        legacy_index,
        legacy_pending,
        effective,
        embedded,
        current_taskbar,
    ) = {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return MonitorRefreshResult {
                retry: false,
                reposition: false,
            };
        };
        (
            s.monitor_device.clone(),
            s.pending_monitor_device.clone(),
            s.taskbar_index,
            s.legacy_selection_pending,
            s.effective_monitor_device.clone(),
            s.embedded,
            s.taskbar_hwnd,
        )
    };
    let requested_for_resolution = pending_selection
        .clone()
        .unwrap_or_else(|| requested.clone());
    let legacy_device = (pending_selection.is_none() && legacy_pending)
        .then(|| legacy_monitor_device(&taskbars, legacy_index))
        .flatten();
    let Some(target) = resolve_monitor_target(
        &monitors,
        &taskbars,
        requested_for_resolution.as_deref(),
        legacy_device.as_deref(),
    ) else {
        let _ = unsafe { SetTimer(hwnd, TIMER_TASKBAR_RETRY, 2_000, None) };
        return MonitorRefreshResult {
            retry: true,
            reposition: false,
        };
    };
    let target_taskbar = target.taskbar.map(|(_, taskbar)| taskbar.hwnd);
    let native_parent_matches = unsafe {
        IsWindow(hwnd).as_bool()
            && match target_taskbar {
                Some(taskbar) => GetParent(hwnd).is_ok_and(|parent| parent == taskbar),
                None => GetParent(hwnd).is_err(),
            }
    };
    let attachment_matches = if let Some(taskbar) = target_taskbar {
        embedded && current_taskbar == Some(taskbar) && native_parent_matches
    } else {
        !embedded && current_taskbar.is_none() && native_parent_matches
    };
    let runtime_matches = effective.as_deref() == Some(target.monitor.device.as_str());
    let transitioned = if attachment_matches && runtime_matches {
        true
    } else {
        transition_to_monitor(hwnd, &target)
    };
    if !transitioned {
        let _ = unsafe { SetTimer(hwnd, TIMER_TASKBAR_RETRY, 2_000, None) };
        return MonitorRefreshResult {
            retry: true,
            reposition: false,
        };
    }

    let pending_selection_resolved =
        pending_selection
            .as_ref()
            .is_some_and(|selection| match selection {
                Some(device) => device == &target.monitor.device,
                None => select_monitor(&monitors, None, None)
                    .is_some_and(|primary| primary.device == target.monitor.device),
            });
    let mut pending_selection_committed = false;
    if pending_selection_resolved {
        let mut state = lock_state();
        if let Some(state) = state.as_mut() {
            if state.pending_monitor_device == pending_selection {
                let selected_device = pending_selection.clone().unwrap_or_default();
                if state.monitor_device != selected_device {
                    state.tray_offset = 0;
                }
                state.monitor_device = selected_device;
                state.pending_monitor_device = None;
                state.legacy_selection_pending = false;
                pending_selection_committed = true;
            }
        }
        if pending_selection_committed {
            drop(state);
            save_state_settings();
            publish_settings();
        }
    }
    let migrated_legacy = pending_selection.is_none()
        && legacy_pending
        && legacy_device.as_deref() == Some(target.monitor.device.as_str());
    if migrated_legacy {
        if let Some(state) = lock_state().as_mut() {
            state.monitor_device = Some(target.monitor.device.clone());
            state.legacy_selection_pending = false;
        }
        save_state_settings();
    }

    let needs_retry = legacy_pending && !migrated_legacy
        || pending_selection.is_some() && !pending_selection_resolved
        || requested_for_resolution
            .as_deref()
            .is_some_and(|device| device != target.monitor.device)
        || target.taskbar.is_none()
        || !lock_state().as_ref().is_some_and(|state| state.embedded);
    unsafe {
        if needs_retry {
            SetTimer(hwnd, TIMER_TASKBAR_RETRY, 2_000, None);
        } else {
            let _ = KillTimer(hwnd, TIMER_TASKBAR_RETRY);
        }
    }
    apply_configured_widget_visibility(hwnd);
    MonitorRefreshResult {
        retry: needs_retry,
        reposition: true,
    }
}

fn taskbar_at_point(pt: POINT) -> Option<(usize, native_interop::TaskbarWindow)> {
    native_interop::find_taskbars()
        .into_iter()
        .enumerate()
        .find(|(_, taskbar)| {
            pt.x >= taskbar.rect.left
                && pt.x < taskbar.rect.right
                && pt.y >= taskbar.rect.top
                && pt.y < taskbar.rect.bottom
        })
}

fn tray_left_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT) -> i32 {
    let mut tray_left = taskbar_rect.right;
    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }
    tray_left
}

fn clamp_offset_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT, offset: i32) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let max_offset = (tray_left - taskbar_rect.left - total_widget_width()).max(0);
    offset.clamp(0, max_offset)
}

fn offset_for_drop_point(
    taskbar_hwnd: HWND,
    taskbar_rect: RECT,
    pt: POINT,
    drag_start_client_x: i32,
) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let desired_left = pt.x - taskbar_rect.left - drag_start_client_x;
    let offset = tray_left - taskbar_rect.left - total_widget_width() - desired_left;
    clamp_offset_for_taskbar(taskbar_hwnd, taskbar_rect, offset)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn update_check_interval() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}

fn auto_update_check_due(last_update_check_unix: Option<u64>) -> bool {
    auto_update_check_due_at(last_update_check_unix, now_unix_secs())
}

fn auto_update_check_due_at(last_update_check_unix: Option<u64>, now_unix: u64) -> bool {
    let Some(last_update_check_unix) = last_update_check_unix else {
        return true;
    };

    now_unix.saturating_sub(last_update_check_unix) >= update_check_interval().as_secs()
}

fn schedule_auto_update_check(hwnd: HWND) {
    let delay_ms = {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return;
        };

        if auto_update_check_due(s.last_update_check_unix) {
            None
        } else {
            let elapsed = now_unix_secs().saturating_sub(s.last_update_check_unix.unwrap_or(0));
            let remaining_secs = update_check_interval().as_secs().saturating_sub(elapsed);
            Some((remaining_secs.saturating_mul(1000)).min(u32::MAX as u64) as u32)
        }
    };

    unsafe {
        let _ = KillTimer(hwnd, TIMER_UPDATE_CHECK);
        if let Some(delay_ms) = delay_ms {
            SetTimer(hwnd, TIMER_UPDATE_CHECK, delay_ms.max(1), None);
        }
    }
}

fn effective_poll_interval(state: &AppState) -> u32 {
    state
        .monitor
        .delay_ms(Instant::now(), state.poll_interval_ms)
}

fn refresh_usage_texts(state: &mut AppState) {
    let strings = state.language.strings();
    let show_remaining = state.language == LanguageId::SimplifiedChinese;
    let Some(data) = state.data.as_ref() else {
        return;
    };

    if let Some(claude_code) = data.claude_code.as_ref() {
        state.session_text = poller::format_line(
            &claude_code.session,
            strings,
            show_remaining,
            poller::UsageWindowKind::Session,
        );
        state.weekly_text = poller::format_line(
            &claude_code.weekly,
            strings,
            show_remaining,
            poller::UsageWindowKind::Weekly,
        );
    } else if state.show_claude_code {
        let label = state.monitor.services[0]
            .error
            .map(|e| poll_error_display_label(e, state.language))
            .unwrap_or("...");
        state.session_text = label.to_string();
        state.weekly_text = label.to_string();
    }

    if let Some(codex) = data.codex.as_ref() {
        state.codex_session_text = poller::format_codex_line(
            &codex.session,
            strings,
            show_remaining,
            poller::UsageWindowKind::Session,
        );
        state.codex_weekly_text = poller::format_codex_line(
            &codex.weekly,
            strings,
            show_remaining,
            poller::UsageWindowKind::Weekly,
        );
    } else if state.show_codex {
        let label = state.monitor.services[1]
            .error
            .map(|e| poll_error_display_label(e, state.language))
            .unwrap_or("...");
        state.codex_session_text = label.to_string();
        state.codex_weekly_text = label.to_string();
    }

    if let Some(antigravity) = data.antigravity.as_ref() {
        state.antigravity_session_text = poller::format_line(
            &antigravity.session,
            strings,
            show_remaining,
            poller::UsageWindowKind::Session,
        );
        state.antigravity_weekly_text =
            if antigravity.weekly.resets_at.is_none() && antigravity.weekly.percentage == 0.0 {
                "--".to_string()
            } else {
                poller::format_line(
                    &antigravity.weekly,
                    strings,
                    show_remaining,
                    poller::UsageWindowKind::Weekly,
                )
            };
    } else if state.show_antigravity {
        let label = state.monitor.services[2]
            .error
            .map(|e| poll_error_display_label(e, state.language))
            .unwrap_or("...");
        state.antigravity_session_text = label.to_string();
        state.antigravity_weekly_text = label.to_string();
    }

    if let Some(opencode) = data.opencode.as_ref() {
        state.opencode_session_text = poller::format_line(
            &opencode.session,
            strings,
            show_remaining,
            poller::UsageWindowKind::Session,
        );
        state.opencode_weekly_text = poller::format_line(
            &opencode.weekly,
            strings,
            show_remaining,
            poller::UsageWindowKind::Weekly,
        );
    } else if state.show_opencode {
        let label = state.monitor.services[3]
            .error
            .map(|e| poll_error_display_label(e, state.language))
            .unwrap_or("...");
        state.opencode_session_text = label.to_string();
        state.opencode_weekly_text = label.to_string();
    }

    if let Some(cursor) = data.cursor.as_ref() {
        state.cursor_session_text = poller::format_line(
            &cursor.session,
            strings,
            show_remaining,
            poller::UsageWindowKind::Session,
        );
        state.cursor_weekly_text = poller::format_line(
            &cursor.weekly,
            strings,
            show_remaining,
            poller::UsageWindowKind::Weekly,
        );
    } else if state.show_cursor {
        let label = state.monitor.services[4]
            .error
            .map(|e| poll_error_display_label(e, state.language))
            .unwrap_or("...");
        state.cursor_session_text = label.to_string();
        state.cursor_weekly_text = label.to_string();
    }

    if let Some(grok) = data.grok.as_ref() {
        state.grok_session_text = poller::format_line(
            &grok.session,
            strings,
            show_remaining,
            poller::UsageWindowKind::Session,
        );
        state.grok_weekly_text = poller::format_line(
            &grok.weekly,
            strings,
            show_remaining,
            poller::UsageWindowKind::Weekly,
        );
    } else if state.show_grok {
        let label = state.monitor.services[5]
            .error
            .map(|e| poll_error_display_label(e, state.language))
            .unwrap_or("...");
        state.grok_session_text = label.to_string();
        state.grok_weekly_text = label.to_string();
    }

    if let Some(copilot) = data.copilot.as_ref() {
        state.copilot_session_text = poller::format_line(
            &copilot.session,
            strings,
            show_remaining,
            poller::UsageWindowKind::Session,
        );
        state.copilot_weekly_text = poller::format_line(
            &copilot.weekly,
            strings,
            show_remaining,
            poller::UsageWindowKind::Weekly,
        );
    } else if state.show_copilot {
        let label = state.monitor.services[6]
            .error
            .map(|e| poll_error_display_label(e, state.language))
            .unwrap_or("...");
        state.copilot_session_text = label.to_string();
        state.copilot_weekly_text = label.to_string();
    }
    for (id, session, weekly) in [
        (0, &mut state.session_text, &mut state.weekly_text),
        (
            1,
            &mut state.codex_session_text,
            &mut state.codex_weekly_text,
        ),
        (
            2,
            &mut state.antigravity_session_text,
            &mut state.antigravity_weekly_text,
        ),
        (
            3,
            &mut state.opencode_session_text,
            &mut state.opencode_weekly_text,
        ),
        (
            4,
            &mut state.cursor_session_text,
            &mut state.cursor_weekly_text,
        ),
        (5, &mut state.grok_session_text, &mut state.grok_weekly_text),
        (
            6,
            &mut state.copilot_session_text,
            &mut state.copilot_weekly_text,
        ),
    ] {
        let stale = state.monitor.services[id].stale(SystemTime::now());
        quota_refresh::mark_stale(session, stale);
        quota_refresh::mark_stale(weekly, stale);
    }
}

fn set_window_title(hwnd: HWND, strings: Strings) {
    unsafe {
        let title = native_interop::wide_str(strings.window_title);
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(title.as_ptr()));
    }
}

fn show_info_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn show_error_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn confirm_and_begin_uninstall(hwnd: HWND) {
    let language = lock_state()
        .as_ref()
        .map(|state| state.language)
        .unwrap_or(LanguageId::English);
    let chinese = language == LanguageId::SimplifiedChinese;
    let kind = crate::uninstaller::uninstall_kind();
    if kind == crate::uninstaller::UninstallKind::Winget {
        show_info_message(
            hwnd,
            if chinese {
                "卸载 Quota Usage"
            } else {
                "Uninstall Quota Usage"
            },
            if chinese {
                "请使用以下命令卸载 Quota Usage：\n\nwinget uninstall --id rozsazoltan.codexusage --exact"
            } else {
                "To uninstall Quota Usage, run this command:\n\nwinget uninstall --id rozsazoltan.codexusage --exact"
            },
        );
        return;
    }
    let kind_name = match kind {
        crate::uninstaller::UninstallKind::Managed => {
            if chinese {
                "托管安装"
            } else {
                "managed installation"
            }
        }
        crate::uninstaller::UninstallKind::Portable => {
            if chinese {
                "便携版"
            } else {
                "portable copy"
            }
        }
        crate::uninstaller::UninstallKind::Winget => {
            if chinese {
                "winget 安装"
            } else {
                "winget installation"
            }
        }
    };
    let title = if chinese {
        "卸载 Quota Usage"
    } else {
        "Uninstall Quota Usage"
    };
    let prompt = if chinese {
        format!("确定卸载 Quota Usage？\n\n检测到：{kind_name}。此操作无法撤销。")
    } else {
        format!("Uninstall Quota Usage?\n\nDetected: {kind_name}. This action cannot be undone.")
    };
    let (title_wide, prompt_wide) = (
        native_interop::wide_str(title),
        native_interop::wide_str(&prompt),
    );
    let accepted = unsafe {
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(prompt_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    };
    if !accepted {
        return;
    }

    let settings_prompt = if chinese {
        "同时删除保存的设置？\n\n默认保留设置。选择“是”删除设置，“否”保留设置，“取消”中止卸载。"
    } else {
        "Also remove saved settings?\n\nSettings are preserved by default. Choose Yes to remove settings, No to keep them, or Cancel to abort uninstall."
    };
    let (settings_title, settings_prompt_wide) = (
        native_interop::wide_str(title),
        native_interop::wide_str(settings_prompt),
    );
    let choice = unsafe {
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(settings_prompt_wide.as_ptr()),
            PCWSTR::from_raw(settings_title.as_ptr()),
            MB_YESNOCANCEL | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if choice == IDCANCEL {
        return;
    }

    settings_ui::shutdown();
    if let Err(error) = crate::uninstaller::begin_uninstall(choice == IDYES) {
        show_error_message(
            hwnd,
            if chinese {
                "卸载失败"
            } else {
                "Uninstall failed"
            },
            &error,
        );
    }
}

fn show_update_prompt(hwnd: HWND, strings: Strings, release: &ReleaseDescriptor) -> bool {
    let message = strings
        .update_prompt_now
        .replace("{version}", &release.latest_version);

    unsafe {
        let title_wide = native_interop::wide_str(strings.update_available);
        let message_wide = native_interop::wide_str(&message);
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

fn apply_language_to_state(state: &mut AppState, language_override: Option<LanguageId>) {
    state.language_override = language_override;
    state.language = localization::resolve_language(language_override);
    refresh_usage_texts(state);
}

fn update_language_change() -> bool {
    let (hwnd, strings) = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return false;
        };

        if app_state.language_override.is_some() {
            return false;
        }

        let new_language = localization::detect_system_language();
        if new_language == app_state.language {
            return false;
        }

        apply_language_to_state(app_state, None);
        (app_state.hwnd.to_hwnd(), app_state.language.strings())
    };
    // SetWindowTextW synchronously re-enters wnd_proc, which also locks STATE.
    set_window_title(hwnd, strings);
    true
}

fn version_action_label(
    strings: Strings,
    language: LanguageId,
    install_channel: InstallChannel,
    status: &UpdateStatus,
) -> String {
    let current = env!("CARGO_PKG_VERSION");
    match status {
        UpdateStatus::Idle => format!("v{current} - {}", strings.check_for_updates),
        UpdateStatus::Checking => format!("v{current} - {}", strings.checking_for_updates),
        UpdateStatus::Applying => format!("v{current} - {}", strings.applying_update),
        UpdateStatus::UpToDate => format!("v{current} - {}", strings.up_to_date_short),
        UpdateStatus::Available(release) => match install_channel {
            InstallChannel::Portable => {
                format!(
                    "v{current} - {} v{}",
                    strings.update_to, release.latest_version
                )
            }
            InstallChannel::Winget => format!(
                "v{current} - {} v{}",
                localization::update_via_winget(language),
                release.latest_version
            ),
        },
    }
}

fn update_available(status: &UpdateStatus) -> bool {
    matches!(status, UpdateStatus::Available(_))
}

fn begin_update_check(hwnd: HWND, interactive: bool) {
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let (strings, install_channel) = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            if interactive {
                show_info_message(
                    hwnd,
                    app_state.language.strings().updates,
                    app_state.language.strings().update_in_progress,
                );
            }
            return;
        }

        app_state.update_status = UpdateStatus::Checking;
        (app_state.language.strings(), app_state.install_channel)
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        let checked_at = now_unix_secs();
        match updater::check_for_updates() {
            Ok(UpdateCheckResult::UpToDate) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::UpToDate;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    show_info_message(hwnd, strings.updates, strings.up_to_date);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Ok(UpdateCheckResult::Available(release)) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release.clone());
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive && show_update_prompt(hwnd, strings, &release) {
                    match install_channel {
                        InstallChannel::Portable => begin_update_apply(hwnd, release),
                        InstallChannel::Winget => begin_winget_update(hwnd),
                    }
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Err(error) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Idle;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    let message = format!("{}.\n\n{}", strings.update_failed, error);
                    show_error_message(hwnd, strings.updates, &message);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn begin_update_apply(hwnd: HWND, release: ReleaseDescriptor) {
    settings_ui::shutdown();
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let strings = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            show_info_message(
                hwnd,
                app_state.language.strings().updates,
                app_state.language.strings().update_in_progress,
            );
            return;
        }

        app_state.update_status = UpdateStatus::Applying;
        app_state.language.strings()
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        match updater::begin_self_update(&release) {
            Ok(()) => unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Err(error) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release);
                    }
                }
                let message = format!("{}.\n\n{}", strings.update_failed, error);
                show_error_message(hwnd, strings.updates, &message);
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn begin_winget_update(hwnd: HWND) {
    settings_ui::shutdown();
    let strings = {
        let state = lock_state();
        state.as_ref().map(|s| s.language.strings())
    }
    .unwrap_or(LanguageId::English.strings());

    match updater::begin_winget_update() {
        Ok(()) => unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Err(error) => {
            let message = format!("{}.\n\n{}", strings.update_failed, error);
            show_error_message(hwnd, strings.updates, &message);
        }
    }
}

const STARTUP_REGISTRY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_REGISTRY_KEY: &str = "QuotaUsage";
const LEGACY_STARTUP_REGISTRY_KEYS: [&str; 2] = ["CodexUsage", "ClaudeCodeUsageMonitor"];

/// Returns true only if the startup registry value points to this executable.
fn is_startup_enabled() -> bool {
    let Some(current_exe) = current_exe_path_string() else {
        return false;
    };
    std::iter::once(STARTUP_REGISTRY_KEY)
        .chain(LEGACY_STARTUP_REGISTRY_KEYS)
        .filter_map(read_startup_value)
        .any(|value| startup_command_matches(&value, &current_exe))
}

fn startup_command_matches(value: &str, exe: &str) -> bool {
    value.trim().trim_matches('"').eq_ignore_ascii_case(exe)
}

fn current_exe_path_string() -> Option<String> {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        (len > 0).then(|| String::from_utf16_lossy(&exe_buf[..len]))
    }
}

fn read_startup_value(key: &str) -> Option<String> {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
        let key_name = native_interop::wide_str(key);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        );
        if result.is_err() {
            return None;
        }

        // Query the size of the value
        let mut data_size: u32 = 0;
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut data_size),
        );
        if result.is_err() || data_size == 0 {
            let _ = RegCloseKey(hkey);
            return None;
        }

        // Read the value
        let mut buf = vec![0u8; data_size as usize];
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut data_size),
        );
        let _ = RegCloseKey(hkey);
        if result.is_err() {
            return None;
        }

        // Convert the registry value (UTF-16) to a string
        let wide_slice =
            std::slice::from_raw_parts(buf.as_ptr() as *const u16, data_size as usize / 2);
        Some(
            String::from_utf16_lossy(wide_slice)
                .trim_end_matches('\0')
                .to_string(),
        )
    }
}

fn delete_startup_value(key: &str) {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
        let key_name = native_interop::wide_str(key);
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
        .is_ok()
        {
            let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(key_name.as_ptr()));
            let _ = RegCloseKey(hkey);
        }
    }
}

fn migrate_legacy_startup_entry() {
    let Some(current_exe) = current_exe_path_string() else {
        return;
    };
    let current_value = read_startup_value(STARTUP_REGISTRY_KEY);
    let current_matches = current_value
        .as_deref()
        .is_some_and(|value| startup_command_matches(value, &current_exe));
    let matching_legacy_keys = LEGACY_STARTUP_REGISTRY_KEYS
        .iter()
        .copied()
        .filter(|key| {
            read_startup_value(key)
                .as_deref()
                .is_some_and(|value| startup_command_matches(value, &current_exe))
        })
        .collect::<Vec<_>>();
    if matching_legacy_keys.is_empty() {
        return;
    }

    // Never overwrite another command registered under the new product name.
    if current_value.is_none() && !current_matches {
        set_startup_enabled(true);
    }

    if read_startup_value(STARTUP_REGISTRY_KEY)
        .as_deref()
        .is_some_and(|value| startup_command_matches(value, &current_exe))
    {
        for key in matching_legacy_keys {
            delete_startup_value(&key);
        }
        diagnose::log("migrated legacy startup registry entry to QuotaUsage");
    }
}

fn set_startup_enabled(enable: bool) {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if result.is_err() {
            return;
        }

        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        if enable {
            let mut exe_buf = [0u16; 260];
            let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
            if len > 0 {
                // Quoting also supports installation paths containing spaces.
                let command = native_interop::wide_str(&format!(
                    "\"{}\"",
                    String::from_utf16_lossy(&exe_buf[..len])
                ));
                let byte_len = (command.len() * 2) as u32;
                let _ = RegSetValueExW(
                    hkey,
                    PCWSTR::from_raw(key_name.as_ptr()),
                    0,
                    REG_SZ,
                    Some(std::slice::from_raw_parts(
                        command.as_ptr() as *const u8,
                        byte_len as usize,
                    )),
                );
            }
        } else {
            if let Some(executable) = current_exe_path_string() {
                if read_startup_value(STARTUP_REGISTRY_KEY)
                    .as_deref()
                    .is_some_and(|value| startup_command_matches(value, &executable))
                {
                    let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(key_name.as_ptr()));
                }
                for legacy_key in LEGACY_STARTUP_REGISTRY_KEYS {
                    if read_startup_value(legacy_key)
                        .as_deref()
                        .is_some_and(|value| startup_command_matches(value, &executable))
                    {
                        let legacy_key_name = native_interop::wide_str(legacy_key);
                        let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(legacy_key_name.as_ptr()));
                    }
                }
            }
        }

        let _ = RegCloseKey(hkey);
    }
}

fn restore_startup_registration(enabled: bool) -> bool {
    set_startup_enabled(enabled);
    let restored = is_startup_enabled() == enabled;
    if !restored {
        diagnose::log("failed to restore the previous Windows startup registration");
    }
    restored
}

// Dimensions matching the C# version
const SEGMENT_W: i32 = 10;
const SEGMENT_H: i32 = 13;
const ROW_HEIGHT: i32 = 18;
const SEGMENT_GAP: i32 = 1;
const SEGMENT_COUNT: i32 = 10;

const LEFT_DIVIDER_W: i32 = 3;
const DIVIDER_RIGHT_MARGIN: i32 = 10;
const LABEL_WIDTH: i32 = 18;
const LABEL_RIGHT_MARGIN: i32 = 10;
const BAR_RIGHT_MARGIN: i32 = 4;
const TEXT_WIDTH: i32 = 110;
const SIMPLIFIED_CHINESE_LABEL_WIDTH: i32 = 20;
const SIMPLIFIED_CHINESE_TEXT_WIDTH: i32 = 138;
const RIGHT_MARGIN: i32 = 1;
const WIDGET_HEIGHT: i32 = 46;

fn is_drag_handle_point(client_x: i32, client_y: i32) -> bool {
    let divider_h = sc(25);
    let divider_top = (sc(WIDGET_HEIGHT) - divider_h) / 2;
    client_x >= 0
        && client_x < sc(LEFT_DIVIDER_W)
        && client_y >= divider_top
        && client_y < divider_top + divider_h
}

fn cursor_is_on_drag_handle(hwnd: HWND) -> bool {
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() || !ScreenToClient(hwnd, &mut pt).as_bool() {
            return false;
        }
        is_drag_handle_point(pt.x, pt.y)
    }
}

fn active_model_count(
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    show_opencode: bool,
    show_cursor: bool,
    show_grok: bool,
    show_copilot: bool,
) -> i32 {
    (show_claude_code as i32
        + show_codex as i32
        + show_antigravity as i32
        + show_opencode as i32
        + show_cursor as i32
        + show_grok as i32
        + show_copilot as i32)
        .max(1)
}

fn row_bar_segment_count(active_models: i32) -> i32 {
    match active_models {
        1 => SEGMENT_COUNT,
        2 => 5,
        _ => 4,
    }
}

/// Labels and reset texts that can occupy the widest space in each column.
fn usage_layout_samples(language: LanguageId) -> (Vec<&'static str>, Vec<String>) {
    let strings = language.strings();
    let labels = vec![strings.session_window, strings.weekly_window];
    let resets = if language == LanguageId::SimplifiedChinese {
        vec!["23:59重置 *".to_string(), "12/31重置 *".to_string()]
    } else {
        [
            strings.day_suffix,
            strings.hour_suffix,
            strings.minute_suffix,
            strings.second_suffix,
        ]
        .into_iter()
        .map(|suffix| format!("99{suffix} *"))
        .chain(std::iter::once(format!("{} *", strings.now)))
        .collect()
    };
    (labels, resets)
}

unsafe fn create_widget_font(appearance: Appearance) -> HFONT {
    let font_name = native_interop::wide_str("Microsoft YaHei UI");
    CreateFontW(
        -sc(appearance.font_px()),
        0,
        0,
        0,
        if appearance.palette == Palette::System {
            FW_SEMIBOLD.0 as i32
        } else {
            FW_BOLD.0 as i32
        },
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_TT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        PCWSTR::from_raw(font_name.as_ptr()),
    )
}

/// Label and quota-text column widths in physical pixels, measured with the
/// fonts used for drawing so larger text and longer translations still fit.
fn usage_layout_widths(language: LanguageId, appearance: Appearance) -> (i32, i32) {
    let reset_x = sc(quota_text::reset_offset(appearance.font_px()));
    let fallback = if language == LanguageId::SimplifiedChinese {
        (
            sc(SIMPLIFIED_CHINESE_LABEL_WIDTH),
            sc(SIMPLIFIED_CHINESE_TEXT_WIDTH),
        )
    } else {
        (sc(LABEL_WIDTH), sc(TEXT_WIDTH))
    };
    unsafe {
        let hdc = GetDC(HWND::default());
        if hdc.is_invalid() {
            return fallback;
        }
        let measure = |font: HFONT, values: &mut dyn Iterator<Item = &str>| {
            let old_font = SelectObject(hdc, font);
            let widest = values
                .map(|value| {
                    let wide: Vec<u16> = value.encode_utf16().collect();
                    let mut size = SIZE::default();
                    if GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool() {
                        size.cx
                    } else {
                        0
                    }
                })
                .max()
                .unwrap_or(0);
            SelectObject(hdc, old_font);
            let _ = DeleteObject(font);
            widest
        };
        let (labels, resets) = usage_layout_samples(language);
        let label_font =
            quota_text::create_reset_font(sc(i32::from(appearance.window_label_font_size)));
        let reset_font = quota_text::create_reset_font(sc(appearance.font_px()));
        let widths = if label_font.is_invalid() || reset_font.is_invalid() {
            let _ = DeleteObject(label_font);
            let _ = DeleteObject(reset_font);
            fallback
        } else {
            (
                measure(label_font, &mut labels.into_iter()) + sc(2),
                (reset_x + measure(reset_font, &mut resets.iter().map(String::as_str)) + sc(3))
                    .max(fallback.1),
            )
        };
        ReleaseDC(HWND::default(), hdc);
        let text_width = match (appearance.show_percentage, appearance.show_reset_time) {
            (true, true) => widths.1,
            (true, false) => sc(quota_text::percent_width(appearance.font_px())),
            (false, true) => widths.1 - reset_x,
            (false, false) => 0,
        };
        (widths.0, text_width)
    }
}

fn usage_percent_for_display(language: LanguageId, used_percentage: f64) -> f64 {
    if language == LanguageId::SimplifiedChinese {
        poller::remaining_percentage(used_percentage)
    } else {
        used_percentage.clamp(0.0, 100.0)
    }
}

fn total_widget_width_for(active_models: i32, language: LanguageId, appearance: Appearance) -> i32 {
    let bar_segments = row_bar_segment_count(active_models);
    let (label_width, text_width) = usage_layout_widths(language, appearance);
    let model_width = model_usage_width(bar_segments, text_width, appearance);

    sc(LEFT_DIVIDER_W)
        + sc(DIVIDER_RIGHT_MARGIN)
        + label_width
        + sc(LABEL_RIGHT_MARGIN)
        + model_width * active_models
        + appearance.provider_spacing.scaled_gap() * (active_models - 1)
        + sc(RIGHT_MARGIN)
}

fn total_widget_width() -> i32 {
    let (active_models, language, appearance) = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| {
                (
                    active_model_count(
                        s.show_claude_code,
                        s.show_codex,
                        s.show_antigravity,
                        s.show_opencode,
                        s.show_cursor,
                        s.show_grok,
                        s.show_copilot,
                    ),
                    s.language,
                    s.appearance,
                )
            })
            .unwrap_or((1, LanguageId::English, Appearance::default()))
    };
    total_widget_width_for(active_models, language, appearance)
}

fn claude_accent_color() -> Color {
    Color::from_hex("#D97757")
}

fn custom_color(color: Option<[u8; 3]>, fallback: Color) -> Color {
    color
        .map(|[r, g, b]| Color::new(r, g, b))
        .unwrap_or(fallback)
}

fn appearance_colors(appearance: Appearance, system_is_dark: bool) -> (Color, Color, Color) {
    match appearance.palette {
        Palette::System if system_is_dark => (
            Color::from_hex("#1C1C1C"),
            Color::from_hex("#888888"),
            Color::from_hex("#444444"),
        ),
        Palette::System => (
            Color::from_hex("#F3F3F3"),
            Color::from_hex("#404040"),
            Color::from_hex("#AAAAAA"),
        ),
        Palette::HighContrastDark => (
            Color::from_hex("#1C1C1C"),
            Color::from_hex("#FFFFFF"),
            Color::from_hex("#30343B"),
        ),
        Palette::HighContrastLight => (
            Color::from_hex("#F3F3F3"),
            Color::from_hex("#101820"),
            Color::from_hex("#A0A8B2"),
        ),
    }
}

/// Reset times stay quieter than percentages; high-contrast palettes keep them legible.
fn reset_text_color(appearance: Appearance, is_dark: bool) -> Color {
    Color::from_hex(match appearance.palette {
        Palette::System if is_dark => "#B5B5B5",
        Palette::System => "#666666",
        Palette::HighContrastDark => "#D8D8D8",
        Palette::HighContrastLight => "#38424C",
    })
}

fn antigravity_accent_color() -> Color {
    Color::from_hex("#4285F4")
}

fn opencode_accent_color() -> Color {
    Color::from_hex("#5B8DEF")
}

fn cursor_accent_color() -> Color {
    Color::from_hex("#6E6E6E")
}

fn grok_accent_color() -> Color {
    Color::from_hex("#A8A8A8")
}

fn copilot_accent_color() -> Color {
    Color::from_hex("#A371F7")
}

pub fn run() {
    // Enable Per-Monitor DPI Awareness V2 for crisp rendering at any scale factor
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CURRENT_DPI.store(GetDpiForSystem(), Ordering::Relaxed);
    }
    diagnose::log("window::run started");

    // Single-instance guard: silently exit if another instance is running.
    // Exception: when relaunched after an explorer restart (ENV_RELAUNCH set),
    // wait for the previous instance to release the mutex, then take over.
    let is_relaunch = std::env::var(ENV_RELAUNCH).is_ok()
        || std::env::var(LEGACY_ENV_RELAUNCH).is_ok();
    // Hold both the current and historical mutex names so pre-rebrand and
    // current processes continue to exclude each other during upgrades.
    let mut mutexes = Vec::new();
    for name in [
        "Global\\QuotaUsage",
        "Global\\CodexUsage",
        "Global\\ClaudeCodeUsageMonitor",
    ] {
        let mutex_name = native_interop::wide_str(name);
        let handle = unsafe { CreateMutexW(None, true, PCWSTR::from_raw(mutex_name.as_ptr())) };
        match handle {
            Ok(handle) => {
                if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                    if is_relaunch {
                        diagnose::log(format!("relaunch: waiting for previous instance on {name}"));
                        let wait_result = unsafe { WaitForSingleObject(handle, 10_000) };
                        if wait_result != WAIT_OBJECT_0 && wait_result != WAIT_ABANDONED {
                            diagnose::log(format!(
                                "startup aborted: previous instance did not release {name} ({wait_result:?})"
                            ));
                            return;
                        }
                    } else {
                        diagnose::log(format!(
                            "startup aborted: another instance owns {name}"
                        ));
                        return;
                    }
                }
                mutexes.push(handle);
            }
            Err(error) => {
                diagnose::log_error(
                    "startup aborted: unable to create single-instance mutex",
                    error,
                );
                return;
            }
        }
    }
    let _mutexes = mutexes;

    migrate_legacy_startup_entry();

    let class_name = native_interop::wide_str("QuotaUsage");

    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap();
        let (large_icon, small_icon) = load_embedded_app_icons();

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hIcon: large_icon,
            hIconSm: small_icon,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            diagnose::log("RegisterClassExW returned 0");
        }

        let provider_availability = poller::provider_availability();
        let mut settings = load_settings(provider_availability.claude_code_available);
        let startup_monitors = native_interop::find_monitors();
        let startup_taskbars = native_interop::find_taskbars();
        let startup_legacy_taskbar_devices: Vec<_> = startup_taskbars
            .iter()
            .map(native_interop::taskbar_monitor_device)
            .collect();
        if migrate_legacy_taskbar_selection(
            &mut settings,
            &startup_monitors,
            &startup_legacy_taskbar_devices,
        ) {
            save_settings(&settings);
            diagnose::log("migrated legacy taskbar index to a monitor device selection");
        }
        let legacy_selection_pending =
            settings.monitor_device.is_none() && settings.taskbar_index > 0;
        let legacy_device = legacy_selection_pending
            .then(|| legacy_monitor_device(&startup_taskbars, settings.taskbar_index))
            .flatten();
        let startup_target = resolve_monitor_target(
            &startup_monitors,
            &startup_taskbars,
            settings.monitor_device.as_deref(),
            legacy_device.as_deref(),
        );
        let language_override = settings.language.as_deref().and_then(LanguageId::from_code);
        let language = localization::resolve_language(language_override);
        let install_channel = updater::current_install_channel();

        // Create as layered popup (will be reparented into taskbar)
        let title = native_interop::wide_str(language.strings().window_title);
        let initial_model_count = active_model_count(
            settings.show_claude_code,
            settings.show_codex,
            settings.show_antigravity,
            settings.show_opencode,
            settings.show_cursor,
            settings.show_grok,
            settings.show_copilot,
        );
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            total_widget_width_for(initial_model_count, language, settings.appearance),
            sc(WIDGET_HEIGHT),
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .unwrap();

        if !large_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(large_icon.0 as isize),
            );
        }
        if !small_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(small_icon.0 as isize),
            );
        }

        diagnose::log(format!("main window created hwnd={:?}", hwnd));

        let is_dark = theme::is_dark_mode();
        {
            let mut state = lock_state();
            *state = Some(AppState {
                hwnd: SendHwnd::from_hwnd(hwnd),
                taskbar_broadcast_hwnd: None,
                taskbar_hwnd: None,
                tray_notify_hwnd: None,
                win_event_hook: None,
                is_dark,
                embedded: false,
                language_override,
                language,
                appearance: settings.appearance,
                install_channel,
                session_percent: 0.0,
                session_text: "--".to_string(),
                weekly_percent: 0.0,
                weekly_text: "--".to_string(),
                codex_session_percent: 0.0,
                codex_session_text: "--".to_string(),
                codex_weekly_percent: 0.0,
                codex_weekly_text: "--".to_string(),
                antigravity_session_percent: 0.0,
                antigravity_session_text: "--".to_string(),
                antigravity_weekly_percent: 0.0,
                antigravity_weekly_text: "--".to_string(),
                opencode_session_percent: 0.0,
                opencode_session_text: "--".to_string(),
                opencode_weekly_percent: 0.0,
                opencode_weekly_text: "--".to_string(),
                cursor_session_percent: 0.0,
                cursor_session_text: "--".to_string(),
                cursor_weekly_percent: 0.0,
                cursor_weekly_text: "--".to_string(),
                grok_session_percent: 0.0,
                grok_session_text: "--".to_string(),
                grok_weekly_percent: 0.0,
                grok_weekly_text: "--".to_string(),
                copilot_session_percent: 0.0,
                copilot_session_text: "--".to_string(),
                copilot_weekly_percent: 0.0,
                copilot_weekly_text: "--".to_string(),
                provider_availability,
                development_overrides: [DevelopmentOverride::default(); 7],
                show_claude_code: settings.show_claude_code,
                show_codex: settings.show_codex,
                show_antigravity: settings.show_antigravity,
                show_opencode: settings.show_opencode,
                show_cursor: settings.show_cursor,
                show_grok: settings.show_grok,
                show_copilot: settings.show_copilot,
                allow_unlimited_providers: settings.allow_unlimited_providers,
                show_tray_icon: settings.show_tray_icon,
                show_provider_icons: settings.show_provider_icons,
                show_session_window: settings.show_session_window,
                show_weekly_window: settings.show_weekly_window,
                alert_threshold_percent: settings.alert_threshold_percent,
                notified_quota_windows: settings.notified_quota_windows.into_iter().collect(),
                data: None,
                poll_interval_ms: settings.poll_interval_ms,
                adaptive_refresh: settings.adaptive_refresh,
                monitor: provider_poll::Monitor::new(
                    [
                        settings.show_claude_code,
                        settings.show_codex,
                        settings.show_antigravity,
                        settings.show_opencode,
                        settings.show_cursor,
                        settings.show_grok,
                        settings.show_copilot,
                    ],
                    Instant::now(),
                ),
                recovery_watch: Some(recovery_events::Watch::register(hwnd)),
                recovery_debounce: recovery_events::Debounce::default(),
                has_poll_result: false,
                update_status: UpdateStatus::Idle,
                last_update_check_unix: settings.last_update_check_unix,
                taskbar_index: settings.taskbar_index,
                monitor_device: settings.monitor_device.clone(),
                // This records a successfully applied native placement, not
                // merely the monitor we intend to use at startup.
                effective_monitor_device: None,
                pending_monitor_device: None,
                legacy_selection_pending,
                tray_offset: settings.tray_offset,
                anchor_left: settings.anchor_left,
                dragging: false,
                drag_start_mouse_x: 0,
                drag_start_client_x: 0,
                drag_start_offset: 0,
                widget_visible: settings.widget_visible,
                settings_pane_open: settings.settings_pane_open,
            });
        }

        // The taskbar widget becomes a WS_CHILD window after attachment and
        // therefore no longer receives Explorer's top-level broadcasts. Keep
        // a hidden top-level window alive to relay TaskbarCreated promptly.
        let broadcast_title = native_interop::wide_str("Quota Usage taskbar monitor");
        let broadcast_hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(broadcast_title.as_ptr()),
            WS_POPUP,
            0,
            0,
            1,
            1,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .expect("failed to create the taskbar broadcast receiver window");
        if let Some(state) = lock_state().as_mut() {
            state.taskbar_broadcast_hwnd = Some(broadcast_hwnd);
        }

        let transitioned = startup_target
            .as_ref()
            .is_some_and(|target| transition_to_monitor(hwnd, target));
        if !transitioned {
            if startup_target
                .as_ref()
                .is_some_and(|target| target.taskbar.is_some())
            {
                diagnose::log(
                    "startup taskbar embedding failed; keeping widget hidden and retrying",
                );
            } else {
                diagnose::log(
                    "startup monitor placement failed; keeping widget hidden and retrying",
                );
            }
        }
        let selection_pending = lock_state().as_ref().is_some_and(|state| {
            state.legacy_selection_pending
                || state
                    .monitor_device
                    .as_deref()
                    .is_some_and(|device| state.effective_monitor_device.as_deref() != Some(device))
        });
        let startup_placement = startup_placement_decision(
            startup_target
                .as_ref()
                .is_some_and(|target| target.taskbar.is_some()),
            transitioned,
            settings.widget_visible,
            selection_pending,
        );
        if startup_placement.retry {
            SetTimer(hwnd, TIMER_TASKBAR_RETRY, 2_000, None);
        }

        // Register system tray icon(s)
        sync_tray_icons(hwnd);

        // Position and show (only if widget_visible preference is true)
        position_at_taskbar();
        if startup_placement.placement_succeeded {
            set_widget_visibility(hwnd, startup_placement.show_widget);
        }
        diagnose::log("window shown");

        // Initial render via UpdateLayeredWindow (for embedded) or InvalidateRect (fallback)
        render_layered();

        // Poll timer: 15 minutes
        let initial_poll_ms = {
            let state = lock_state();
            state
                .as_ref()
                .map(|s| s.poll_interval_ms)
                .unwrap_or(POLL_15_MIN)
        };
        SetTimer(hwnd, TIMER_POLL, initial_poll_ms, None);
        SetTimer(hwnd, TIMER_FRESHNESS, 30_000, None);

        // Watch for explorer.exe restarts so we can re-embed and re-add the tray
        // icon (the shell discards tray registrations when it restarts). This
        // runs on a dedicated thread, NOT a window timer: once explorer destroys
        // the taskbar, our embedded child window stops receiving all messages
        // (WM_TIMER included), so a timer would never fire again.
        spawn_taskbar_watchdog();

        // Initial poll
        let send_hwnd = SendHwnd::from_hwnd(hwnd);
        request_poll(send_hwnd, false);

        schedule_auto_update_check(hwnd);
        begin_update_check(hwnd, false);

        // Initial theme check
        check_theme_change();

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        settings_ui::shutdown();
    }
}

/// Keep missing-window explanations aligned with the Codex quota cells.
fn update_quota_tooltips() {
    let (hwnd, regions) = {
        let state = lock_state();
        let Some(s) = state.as_ref() else { return };
        let mut regions = Vec::new();
        if !s.dragging {
            let chinese = s.language == LanguageId::SimplifiedChinese;
            let count = active_model_count(
                s.show_claude_code,
                s.show_codex,
                s.show_antigravity,
                s.show_opencode,
                s.show_cursor,
                s.show_grok,
                s.show_copilot,
            );
            let (label_width, _) = usage_layout_widths(s.language, s.appearance);
            let mut left = sc(LEFT_DIVIDER_W)
                + sc(DIVIDER_RIGHT_MARGIN)
                + label_width
                + sc(LABEL_RIGHT_MARGIN);
            let display: [ProviderDisplay; 7] =
                std::array::from_fn(|index| provider_display(s, ProviderId::ALL[index]));
            let tooltip_providers = [
                (
                    0,
                    s.show_claude_code,
                    "Claude Code",
                    &display[0].session_text,
                    &display[0].weekly_text,
                ),
                (
                    1,
                    s.show_codex,
                    "Codex",
                    &display[1].session_text,
                    &display[1].weekly_text,
                ),
                (
                    2,
                    s.show_antigravity,
                    "Antigravity",
                    &display[2].session_text,
                    &display[2].weekly_text,
                ),
                (
                    3,
                    s.show_opencode,
                    "OpenCode",
                    &display[3].session_text,
                    &display[3].weekly_text,
                ),
                (
                    4,
                    s.show_cursor,
                    "Cursor",
                    &display[4].session_text,
                    &display[4].weekly_text,
                ),
                (
                    5,
                    s.show_grok,
                    "Grok",
                    &display[5].session_text,
                    &display[5].weekly_text,
                ),
                (
                    6,
                    s.show_copilot,
                    "Copilot",
                    &display[6].session_text,
                    &display[6].weekly_text,
                ),
            ];
            for provider_id in s.appearance.provider_order {
                let (id, visible, name, session, weekly) = tooltip_providers[provider_id.index()];
                if !visible {
                    continue;
                }
                // Match the compact provider columns used by paint_content,
                // rather than the worst-case width reserved by the window.
                let dc = unsafe { GetDC(s.hwnd.to_hwnd()) };
                let text_width = (if s.show_session_window {
                    usage_value_width(dc, session, s.appearance)
                } else {
                    0
                })
                .max(if s.show_weekly_window {
                    usage_value_width(dc, weekly, s.appearance)
                } else {
                    0
                });
                unsafe {
                    ReleaseDC(s.hwnd.to_hwnd(), dc);
                }
                let width =
                    model_usage_width(row_bar_segment_count(count), text_width, s.appearance);
                let description =
                    s.monitor.services[id].description(SystemTime::now(), Instant::now(), chinese);
                let mut text = format!("{name}\n{description}");
                if s.monitor.services[id].error.is_some() && session == "!" {
                    text.push_str(if chinese {
                        "\n本服务本次未返回额度。"
                    } else {
                        "\nThis provider did not return quota in the latest update."
                    });
                }
                if name == "Codex" && s.monitor.services[id].data.is_some() {
                    for (missing, is_weekly) in [(session == "--", false), (weekly == "--", true)] {
                        if missing {
                            text.push('\n');
                            text.push_str(quota_tooltip::message(chinese, is_weekly));
                        }
                    }
                }
                regions.push((
                    RECT {
                        left,
                        top: 0,
                        right: left + width,
                        bottom: sc(WIDGET_HEIGHT),
                    },
                    text,
                ));
                left += width + s.appearance.provider_spacing.scaled_gap();
            }
        }
        (s.hwnd.to_hwnd(), regions)
    };
    quota_tooltip::sync(hwnd, regions);
}

/// Render opaque taskbar content with ClearType via UpdateLayeredWindow.
fn render_layered() {
    refresh_dpi();
    update_quota_tooltips();
    let (
        hwnd_val,
        is_dark,
        embedded,
        language,
        appearance,
        strings,
        session_pct,
        session_text,
        weekly_pct,
        weekly_text,
        codex_session_pct,
        codex_session_text,
        codex_weekly_pct,
        codex_weekly_text,
        antigravity_session_pct,
        antigravity_session_text,
        antigravity_weekly_pct,
        antigravity_weekly_text,
        opencode_session_pct,
        opencode_session_text,
        opencode_weekly_pct,
        opencode_weekly_text,
        cursor_session_pct,
        cursor_session_text,
        cursor_weekly_pct,
        cursor_weekly_text,
        grok_session_pct,
        grok_session_text,
        grok_weekly_pct,
        grok_weekly_text,
        copilot_session_pct,
        copilot_session_text,
        copilot_weekly_pct,
        copilot_weekly_text,
        show_claude_code,
        show_codex,
        show_antigravity,
        show_opencode,
        show_cursor,
        show_grok,
        show_copilot,
        show_provider_icons,
        show_session_window,
        show_weekly_window,
    ) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => {
                let display: [ProviderDisplay; 7] =
                    std::array::from_fn(|index| provider_display(s, ProviderId::ALL[index]));
                (
                    s.hwnd,
                    s.is_dark,
                    s.embedded,
                    s.language,
                    s.appearance,
                    s.language.strings(),
                    display[0].session_percent,
                    display[0].session_text.clone(),
                    display[0].weekly_percent,
                    display[0].weekly_text.clone(),
                    display[1].session_percent,
                    display[1].session_text.clone(),
                    display[1].weekly_percent,
                    display[1].weekly_text.clone(),
                    display[2].session_percent,
                    display[2].session_text.clone(),
                    display[2].weekly_percent,
                    display[2].weekly_text.clone(),
                    display[3].session_percent,
                    display[3].session_text.clone(),
                    display[3].weekly_percent,
                    display[3].weekly_text.clone(),
                    display[4].session_percent,
                    display[4].session_text.clone(),
                    display[4].weekly_percent,
                    display[4].weekly_text.clone(),
                    display[5].session_percent,
                    display[5].session_text.clone(),
                    display[5].weekly_percent,
                    display[5].weekly_text.clone(),
                    display[6].session_percent,
                    display[6].session_text.clone(),
                    display[6].weekly_percent,
                    display[6].weekly_text.clone(),
                    s.show_claude_code,
                    s.show_codex,
                    s.show_antigravity,
                    s.show_opencode,
                    s.show_cursor,
                    s.show_grok,
                    s.show_copilot,
                    s.show_provider_icons,
                    s.show_session_window,
                    s.show_weekly_window,
                )
            }
            None => return,
        }
    };

    let hwnd = hwnd_val.to_hwnd();

    // For non-embedded fallback, just invalidate and let WM_PAINT handle it
    if !embedded {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    let mut client_rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client_rect);
    }
    let width = client_rect.right - client_rect.left;
    let height = client_rect.bottom - client_rect.top;
    if width <= 0 || height <= 0 {
        return;
    }

    let accent = custom_color(appearance.claude_accent, claude_accent_color());
    let (bg_color, text_color, track) = appearance_colors(appearance, is_dark);
    let codex_accent = custom_color(
        appearance.codex_accent,
        provider_icons::codex_color(appearance.is_dark(is_dark)),
    );
    let antigravity_accent = antigravity_accent_color();
    let opencode_accent = custom_color(appearance.opencode_accent, opencode_accent_color());
    let cursor_accent = custom_color(appearance.cursor_accent, cursor_accent_color());
    let grok_accent = custom_color(appearance.grok_accent, grok_accent_color());
    let copilot_accent = custom_color(appearance.copilot_accent, copilot_accent_color());

    unsafe {
        let screen_dc = GetDC(hwnd);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mem_dc = CreateCompatibleDC(screen_dc);
        let dib =
            CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();

        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(hwnd, screen_dc);
            return;
        }

        let old_bmp = SelectObject(mem_dc, dib);
        let pixel_count = (width * height) as usize;

        // Render once with the actual taskbar background colour.
        // Using an opaque background lets us use CLEARTYPE_QUALITY for
        // sub-pixel font rendering that matches the rest of the OS.
        paint_content(
            mem_dc,
            width,
            height,
            is_dark,
            appearance,
            &bg_color,
            &text_color,
            &accent,
            &track,
            language,
            strings,
            session_pct,
            &session_text,
            weekly_pct,
            &weekly_text,
            codex_session_pct,
            &codex_session_text,
            codex_weekly_pct,
            &codex_weekly_text,
            antigravity_session_pct,
            &antigravity_session_text,
            antigravity_weekly_pct,
            &antigravity_weekly_text,
            opencode_session_pct,
            &opencode_session_text,
            opencode_weekly_pct,
            &opencode_weekly_text,
            cursor_session_pct,
            &cursor_session_text,
            cursor_weekly_pct,
            &cursor_weekly_text,
            grok_session_pct,
            &grok_session_text,
            grok_weekly_pct,
            &grok_weekly_text,
            copilot_session_pct,
            &copilot_session_text,
            copilot_weekly_pct,
            &copilot_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            show_opencode,
            show_cursor,
            show_grok,
            show_copilot,
            show_provider_icons,
            show_session_window,
            show_weekly_window,
            &codex_accent,
            &antigravity_accent,
            &opencode_accent,
            &cursor_accent,
            &grok_accent,
            &copilot_accent,
        );

        // System mode blends with the taskbar. High contrast modes use a solid
        // backdrop, so text stays legible even over translucent wallpapers.
        let bg_bgr = bg_color.to_colorref();
        let bg_alpha = if appearance.palette == Palette::System {
            1u32
        } else {
            255u32
        };
        let pixel_data = std::slice::from_raw_parts_mut(bits as *mut u32, pixel_count);
        for px in pixel_data.iter_mut() {
            let rgb = *px & 0x00FFFFFF;
            if rgb == bg_bgr {
                *px = (bg_alpha << 24) | if bg_alpha == 1 { 0 } else { rgb };
            } else {
                *px = rgb | 0xFF000000;
            }
        }

        // Push to window via UpdateLayeredWindow
        let pt_src = POINT { x: 0, y: 0 };
        let sz = SIZE {
            cx: width,
            cy: height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: 0, // AC_SRC_OVER
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1, // AC_SRC_ALPHA
        };

        let _ = UpdateLayeredWindow(
            hwnd,
            screen_dc,
            None,
            Some(&sz),
            mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        // Cleanup
        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, screen_dc);
    }
}

/// Paint all widget content onto a DC with a given background color.
#[derive(Clone, Copy)]
struct WidgetProvider<'a> {
    enabled: bool,
    show_icon: bool,
    icon: Provider,
    session_percent: f64,
    session_text: &'a str,
    weekly_percent: f64,
    weekly_text: &'a str,
    accent: &'a Color,
}

fn paint_content(
    hdc: HDC,
    width: i32,
    height: i32,
    is_dark: bool,
    appearance: Appearance,
    bg: &Color,
    text_color: &Color,
    accent: &Color,
    track: &Color,
    language: LanguageId,
    strings: Strings,
    session_pct: f64,
    session_text: &str,
    weekly_pct: f64,
    weekly_text: &str,
    codex_session_pct: f64,
    codex_session_text: &str,
    codex_weekly_pct: f64,
    codex_weekly_text: &str,
    antigravity_session_pct: f64,
    antigravity_session_text: &str,
    antigravity_weekly_pct: f64,
    antigravity_weekly_text: &str,
    opencode_session_pct: f64,
    opencode_session_text: &str,
    opencode_weekly_pct: f64,
    opencode_weekly_text: &str,
    cursor_session_pct: f64,
    cursor_session_text: &str,
    cursor_weekly_pct: f64,
    cursor_weekly_text: &str,
    grok_session_pct: f64,
    grok_session_text: &str,
    grok_weekly_pct: f64,
    grok_weekly_text: &str,
    copilot_session_pct: f64,
    copilot_session_text: &str,
    copilot_weekly_pct: f64,
    copilot_weekly_text: &str,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    show_opencode: bool,
    show_cursor: bool,
    show_grok: bool,
    show_copilot: bool,
    _show_provider_icons: bool,
    show_session_window: bool,
    show_weekly_window: bool,
    codex_accent: &Color,
    antigravity_accent: &Color,
    opencode_accent: &Color,
    cursor_accent: &Color,
    grok_accent: &Color,
    copilot_accent: &Color,
) {
    unsafe {
        let is_dark = appearance.is_dark(is_dark);
        let session_pct = usage_percent_for_display(language, session_pct);
        let weekly_pct = usage_percent_for_display(language, weekly_pct);
        let codex_session_pct = usage_percent_for_display(language, codex_session_pct);
        let codex_weekly_pct = usage_percent_for_display(language, codex_weekly_pct);
        let antigravity_session_pct = usage_percent_for_display(language, antigravity_session_pct);
        let antigravity_weekly_pct = usage_percent_for_display(language, antigravity_weekly_pct);
        let opencode_session_pct = usage_percent_for_display(language, opencode_session_pct);
        let opencode_weekly_pct = usage_percent_for_display(language, opencode_weekly_pct);
        let cursor_session_pct = usage_percent_for_display(language, cursor_session_pct);
        let cursor_weekly_pct = usage_percent_for_display(language, cursor_weekly_pct);
        let grok_session_pct = usage_percent_for_display(language, grok_session_pct);
        let grok_weekly_pct = usage_percent_for_display(language, grok_weekly_pct);
        let copilot_session_pct = usage_percent_for_display(language, copilot_session_pct);
        let copilot_weekly_pct = usage_percent_for_display(language, copilot_weekly_pct);
        let (label_width, _) = usage_layout_widths(language, appearance);

        let client_rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };

        let bg_brush = CreateSolidBrush(COLORREF(bg.to_colorref()));
        FillRect(hdc, &client_rect, bg_brush);
        let _ = DeleteObject(bg_brush);

        // Left divider
        let divider_h = sc(25);
        let divider_top = (height - divider_h) / 2;
        let divider_bottom = divider_top + divider_h;

        let (div_left, div_right) = if is_dark {
            ((80, 80, 80), (40, 40, 40))
        } else {
            ((160, 160, 160), (230, 230, 230))
        };

        let left_brush = CreateSolidBrush(COLORREF(native_interop::colorref(
            div_left.0, div_left.1, div_left.2,
        )));
        let left_rect = RECT {
            left: 0,
            top: divider_top,
            right: sc(2),
            bottom: divider_bottom,
        };
        FillRect(hdc, &left_rect, left_brush);
        let _ = DeleteObject(left_brush);

        let right_brush = CreateSolidBrush(COLORREF(native_interop::colorref(
            div_right.0,
            div_right.1,
            div_right.2,
        )));
        let right_rect = RECT {
            left: sc(2),
            top: divider_top,
            right: sc(3),
            bottom: divider_bottom,
        };
        FillRect(hdc, &right_rect, right_brush);
        let _ = DeleteObject(right_brush);

        let content_x = sc(LEFT_DIVIDER_W) + sc(DIVIDER_RIGHT_MARGIN);
        let row_gap = if height < sc(WIDGET_HEIGHT) {
            sc(2)
        } else {
            sc(8)
        }
        .min((height - 2 * appearance.row_height()).max(0));
        let row1_y = (height - 2 * appearance.row_height() - row_gap).max(0) / 2;
        let row2_y = row1_y + appearance.row_height() + row_gap;
        let single_row_y = (height - appearance.row_height()) / 2;

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));

        let font = create_widget_font(appearance);
        let old_font = SelectObject(hdc, font);

        // Keep the default aligned with the first row while allowing the user to
        // center marks or align them with the lower row.
        let icon_size = sc(provider_icons::SIZE);
        let icon_y = match appearance.provider_icon_position {
            ProviderIconPosition::Top if show_session_window && show_weekly_window => {
                row1_y + (appearance.row_height() - icon_size) / 2
            }
            ProviderIconPosition::Bottom if show_session_window && show_weekly_window => {
                row2_y + (appearance.row_height() - icon_size) / 2
            }
            ProviderIconPosition::Top | ProviderIconPosition::Bottom => {
                single_row_y + (appearance.row_height() - icon_size) / 2
            }
            ProviderIconPosition::Center => (height - icon_size) / 2,
        };
        let active_models = active_model_count(
            show_claude_code,
            show_codex,
            show_antigravity,
            show_opencode,
            show_cursor,
            show_grok,
            show_copilot,
        );
        let providers = [
            WidgetProvider {
                enabled: show_claude_code,
                show_icon: appearance.show_claude_icon,
                icon: Provider::Claude,
                session_percent: session_pct,
                session_text,
                weekly_percent: weekly_pct,
                weekly_text,
                accent,
            },
            WidgetProvider {
                enabled: show_codex,
                show_icon: appearance.show_codex_icon,
                icon: Provider::Codex,
                session_percent: codex_session_pct,
                session_text: codex_session_text,
                weekly_percent: codex_weekly_pct,
                weekly_text: codex_weekly_text,
                accent: codex_accent,
            },
            WidgetProvider {
                enabled: show_antigravity,
                show_icon: appearance.show_antigravity_icon,
                icon: Provider::Antigravity,
                session_percent: antigravity_session_pct,
                session_text: antigravity_session_text,
                weekly_percent: antigravity_weekly_pct,
                weekly_text: antigravity_weekly_text,
                accent: antigravity_accent,
            },
            WidgetProvider {
                enabled: show_opencode,
                show_icon: appearance.show_opencode_icon,
                icon: Provider::OpenCode,
                session_percent: opencode_session_pct,
                session_text: opencode_session_text,
                weekly_percent: opencode_weekly_pct,
                weekly_text: opencode_weekly_text,
                accent: opencode_accent,
            },
            WidgetProvider {
                enabled: show_cursor,
                show_icon: appearance.show_cursor_icon,
                icon: Provider::Cursor,
                session_percent: cursor_session_pct,
                session_text: cursor_session_text,
                weekly_percent: cursor_weekly_pct,
                weekly_text: cursor_weekly_text,
                accent: cursor_accent,
            },
            WidgetProvider {
                enabled: show_grok,
                show_icon: appearance.show_grok_icon,
                icon: Provider::Grok,
                session_percent: grok_session_pct,
                session_text: grok_session_text,
                weekly_percent: grok_weekly_pct,
                weekly_text: grok_weekly_text,
                accent: grok_accent,
            },
            WidgetProvider {
                enabled: show_copilot,
                show_icon: appearance.show_copilot_icon,
                icon: Provider::Copilot,
                session_percent: copilot_session_pct,
                session_text: copilot_session_text,
                weekly_percent: copilot_weekly_pct,
                weekly_text: copilot_weekly_text,
                accent: copilot_accent,
            },
        ];
        let provider_text_widths = providers.map(|provider| {
            (if show_session_window {
                usage_value_width(hdc, provider.session_text, appearance)
            } else {
                0
            })
            .max(if show_weekly_window {
                usage_value_width(hdc, provider.weekly_text, appearance)
            } else {
                0
            })
        });
        let icon_tint = appearance.usage_text_color.or_else(|| {
            appearance
                .provider_monochrome
                .then_some([text_color.r, text_color.g, text_color.b])
        });
        let mut icon_x = content_x + label_width + sc(LABEL_RIGHT_MARGIN);
        for provider_id in appearance.provider_order {
            let index = provider_id.index();
            let provider = providers[index];
            if provider.enabled {
                if appearance.show_provider_icon && provider.show_icon {
                    provider_icons::draw(
                        hdc,
                        icon_x,
                        icon_y,
                        icon_size,
                        provider.icon,
                        icon_tint,
                    );
                }
                icon_x += model_usage_width(
                    row_bar_segment_count(active_models),
                    provider_text_widths[index],
                    appearance,
                ) + appearance.provider_spacing.scaled_gap();
            }
        }

        if show_session_window {
            draw_row(
                hdc,
                content_x,
                if show_weekly_window {
                    row1_y
                } else {
                    single_row_y
                },
                is_dark,
                appearance,
                text_color,
                strings.session_window,
                false,
                &providers,
                appearance.provider_order,
                track,
                label_width,
                provider_text_widths,
            );
        }
        if show_weekly_window {
            draw_row(
                hdc,
                content_x,
                if show_session_window {
                    row2_y
                } else {
                    single_row_y
                },
                is_dark,
                appearance,
                text_color,
                strings.weekly_window,
                true,
                &providers,
                appearance.provider_order,
                track,
                label_width,
                provider_text_widths,
            );
        }

        SelectObject(hdc, old_font);
        let _ = DeleteObject(font);
    }
}

fn poll_error_display_label(error: poller::PollError, language: LanguageId) -> &'static str {
    match error {
        poller::PollError::AuthRequired
        | poller::PollError::NoCredentials
        | poller::PollError::TokenExpired => "!",
        poller::PollError::NetworkUnavailable => {
            if language == LanguageId::SimplifiedChinese {
                "网络"
            } else {
                "NET"
            }
        }
        poller::PollError::RateLimited(_) => {
            if language == LanguageId::SimplifiedChinese {
                "限流"
            } else {
                "429"
            }
        }
        poller::PollError::ServerError => {
            if language == LanguageId::SimplifiedChinese {
                "服务"
            } else {
                "5XX"
            }
        }
        poller::PollError::RequestFailed => {
            if language == LanguageId::SimplifiedChinese {
                "错误"
            } else {
                "ERR"
            }
        }
    }
}

fn request_poll(hwnd: SendHwnd, force: bool) {
    {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            let enabled = [
                s.show_claude_code,
                s.show_codex,
                s.show_antigravity,
                s.show_opencode,
                s.show_cursor,
                s.show_grok,
                s.show_copilot,
            ];
            s.monitor.configure(enabled, Instant::now());
            if force {
                s.monitor.force(Instant::now(), true);
            }
        }
    }
    start_poll(hwnd, force);
}

fn start_poll(hwnd: SendHwnd, queue: bool) {
    let Some(mut guard) = quota_refresh::POLLS.request(queue) else {
        return;
    };
    if let Err(error) = std::thread::Builder::new()
        .name("quota-poll".into())
        .spawn(move || loop {
            do_poll(hwnd);
            if !guard.next() {
                break;
            }
        })
    {
        diagnose::log_error("unable to start quota poll", error);
    }
}

fn do_poll(send_hwnd: SendHwnd) {
    let jobs = {
        let mut state = lock_state();
        let Some(s) = state.as_mut() else {
            return;
        };
        s.monitor.plan(Instant::now())
    };
    if !jobs.is_empty() {
        unsafe {
            let _ = PostMessageW(
                send_hwnd.to_hwnd(),
                WM_APP_USAGE_UPDATED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    provider_poll::launch(
        jobs,
        |job| {
            provider_poll::execute(
                job,
                || poller::poll_provider(job.id),
                poller::credential_watch_snapshot,
            )
        },
        move |job, attempt| complete_provider_poll(send_hwnd, job, attempt),
    );
}

fn complete_provider_poll(
    send_hwnd: SendHwnd,
    job: &provider_poll::Job,
    attempt: provider_poll::Attempt,
) {
    let hwnd = send_hwnd.to_hwnd();
    let (alerts, auth_notice) = {
        let mut state = lock_state();
        let Some(s) = state.as_mut() else {
            return;
        };
        let completion = s.monitor.finish(
            &job,
            attempt,
            Instant::now(),
            SystemTime::now(),
            s.poll_interval_ms,
            s.adaptive_refresh,
        );
        if !completion.accepted {
            // A selection change can leave a new request waiting for the old
            // worker to finish. Release it without publishing the stale result.
            unsafe {
                SetTimer(hwnd, TIMER_POLL, effective_poll_interval(s), None);
            }
            return;
        }
        let mut fresh = AppUsageData::default();
        if completion.successful {
            let data = s.monitor.services[job.id].data.clone();
            match job.id {
                0 => fresh.claude_code = data,
                1 => fresh.codex = data,
                2 => fresh.antigravity = data,
                3 => fresh.opencode = data,
                4 => fresh.cursor = data,
                5 => fresh.grok = data,
                6 => fresh.copilot = data,
                _ => {}
            }
        }
        let alerts = collect_low_quota_alerts(s, &fresh);
        let cached = s.monitor.cached();
        for (usage, session, weekly) in [
            (
                cached.claude_code.as_ref(),
                &mut s.session_percent,
                &mut s.weekly_percent,
            ),
            (
                cached.codex.as_ref(),
                &mut s.codex_session_percent,
                &mut s.codex_weekly_percent,
            ),
            (
                cached.antigravity.as_ref(),
                &mut s.antigravity_session_percent,
                &mut s.antigravity_weekly_percent,
            ),
            (
                cached.opencode.as_ref(),
                &mut s.opencode_session_percent,
                &mut s.opencode_weekly_percent,
            ),
            (
                cached.cursor.as_ref(),
                &mut s.cursor_session_percent,
                &mut s.cursor_weekly_percent,
            ),
            (
                cached.grok.as_ref(),
                &mut s.grok_session_percent,
                &mut s.grok_weekly_percent,
            ),
            (
                cached.copilot.as_ref(),
                &mut s.copilot_session_percent,
                &mut s.copilot_weekly_percent,
            ),
        ] {
            *session = usage.map_or(0.0, |u| u.session.percentage);
            *weekly = usage.map_or(0.0, |u| u.weekly.percentage);
        }
        s.data = Some(cached);
        s.has_poll_result = true;
        refresh_usage_texts(s);
        unsafe {
            SetTimer(hwnd, TIMER_POLL, effective_poll_interval(s), None);
        }
        let service = &s.monitor.services[job.id];
        diagnose::log(format!(
            "provider poll completed id={} error={:?} interval_ms={}",
            job.id, service.error, service.interval_ms
        ));
        let notice = completion.notify_auth.then(|| {
            let strings = s.language.strings();
            match job.id {
                0 => (
                    tray_icon::TrayIconKind::Claude,
                    strings.token_expired_title,
                    strings.token_expired_body,
                ),
                1 => (
                    tray_icon::TrayIconKind::Codex,
                    strings.codex_token_expired_title,
                    strings.codex_token_expired_body,
                ),
                2 => (
                    tray_icon::TrayIconKind::Antigravity,
                    strings.antigravity_token_expired_title,
                    strings.antigravity_token_expired_body,
                ),
                3 => (
                    tray_icon::TrayIconKind::OpenCode,
                    strings.opencode_token_expired_title,
                    strings.opencode_token_expired_body,
                ),
                4 => (
                    tray_icon::TrayIconKind::Cursor,
                    strings.cursor_token_expired_title,
                    strings.cursor_token_expired_body,
                ),
                5 => (
                    tray_icon::TrayIconKind::Grok,
                    strings.grok_token_expired_title,
                    strings.grok_token_expired_body,
                ),
                6 => (
                    tray_icon::TrayIconKind::Copilot,
                    strings.copilot_token_expired_title,
                    strings.copilot_token_expired_body,
                ),
                _ => (tray_icon::TrayIconKind::Copilot, "", ""),
            }
        });
        (alerts, notice)
    };
    for alert in &alerts {
        tray_icon::notify_balloon(hwnd, alert.kind, &alert.title, &alert.message);
    }
    if !alerts.is_empty() {
        save_state_settings();
    }
    if let Some((kind, title, body)) = auth_notice {
        tray_icon::notify_balloon(hwnd, kind, title, body);
    }
    unsafe {
        let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
    }
}

fn schedule_countdown_timer() {
    let mut state = lock_state();
    let s = match state.as_mut() {
        Some(s) => s,
        None => return,
    };

    let hwnd = s.hwnd.to_hwnd();
    let Some(data) = s.data.as_ref() else {
        return;
    };
    let delays = [
        data.claude_code
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.claude_code
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
        data.codex
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.codex
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
        data.antigravity
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.antigravity
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
    ];
    let min_delay = delays.into_iter().flatten().min();

    let ms = min_delay
        .unwrap_or(Duration::from_secs(60))
        .as_millis()
        .max(1000) as u32;

    unsafe {
        SetTimer(hwnd, TIMER_COUNTDOWN, ms, None);
    }
}

fn check_theme_change() {
    let new_dark = theme::is_dark_mode();
    let changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.is_dark != new_dark {
                s.is_dark = new_dark;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if changed {
        render_layered();
    }
}

fn check_language_change() {
    if update_language_change() {
        render_layered();
    }
}

fn update_display() {
    let mut state = lock_state();
    let s = match state.as_mut() {
        Some(s) => s,
        None => return,
    };

    refresh_usage_texts(s);
}

fn suppress_tray_reposition_for(duration: Duration) {
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *until = Some(Instant::now() + duration);
}

fn tray_reposition_is_suppressed() -> bool {
    let now = Instant::now();
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    match *until {
        Some(deadline) if now < deadline => true,
        Some(_) => {
            *until = None;
            false
        }
        None => false,
    }
}

fn position_at_taskbar() {
    position_at_taskbar_with_save(true);
}

fn position_at_taskbar_without_save() {
    position_at_taskbar_with_save(false);
}

fn floating_window_position(
    monitor_rect: RECT,
    width: i32,
    height: i32,
    anchor_left: bool,
    offset: i32,
) -> native_interop::WindowPosition {
    let width = width.max(1);
    let height = height.max(1);
    let max_offset = monitor_rect
        .right
        .saturating_sub(monitor_rect.left)
        .saturating_sub(width)
        .max(0);
    let offset = offset.clamp(0, max_offset);
    let requested_x = if anchor_left {
        monitor_rect.left
    } else {
        monitor_rect
            .right
            .saturating_sub(width)
            .saturating_sub(offset)
    };
    let max_x = monitor_rect
        .right
        .saturating_sub(width)
        .max(monitor_rect.left);
    let max_y = monitor_rect
        .bottom
        .saturating_sub(height)
        .max(monitor_rect.top);
    native_interop::WindowPosition {
        x: requested_x.clamp(monitor_rect.left, max_x),
        y: monitor_rect
            .bottom
            .saturating_sub(height)
            .clamp(monitor_rect.top, max_y),
        width,
        height,
    }
}

fn taskbar_window_position(
    taskbar_hwnd: HWND,
    taskbar_rect: RECT,
    widget_width: i32,
    widget_height: i32,
    tray_left: i32,
    offset: i32,
    anchor_left: bool,
) -> Option<(native_interop::WindowPosition, i32)> {
    let width = widget_width.max(1);
    let taskbar_height = (taskbar_rect.bottom - taskbar_rect.top).max(1);
    let height = widget_height.max(1).min(taskbar_height);
    let max_offset = (tray_left - taskbar_rect.left - width).max(0);
    let offset = offset.clamp(0, max_offset);
    let screen_x = compute_anchor_x(taskbar_rect.left, tray_left, width, offset, anchor_left);
    let screen_y = compute_anchor_y(taskbar_rect.top, taskbar_height, height);
    let point = native_interop::screen_to_client(taskbar_hwnd, screen_x, screen_y)?;
    Some((
        native_interop::WindowPosition {
            x: point.x,
            y: point.y,
            width,
            height,
        },
        offset,
    ))
}

fn placement_matches(rect: RECT, position: native_interop::WindowPosition) -> bool {
    rect.left == position.x
        && rect.top == position.y
        && rect.right - rect.left == position.width
        && rect.bottom - rect.top == position.height
}

fn position_at_taskbar_with_save(save_offset: bool) {
    refresh_dpi();
    // Drop the app-state lock before any Win32 call that may synchronously
    // re-enter our window procedure.
    let (hwnd, embedded, tray_offset, anchor_left, taskbar_hwnd, monitor_device) = {
        let state = lock_state();
        let s = match state.as_ref() {
            Some(s) => s,
            None => return,
        };

        // Don't fight the user's drag
        if s.dragging {
            return;
        }

        (
            s.hwnd.to_hwnd(),
            s.embedded,
            s.tray_offset,
            s.anchor_left,
            s.taskbar_hwnd,
            s.effective_monitor_device.clone(),
        )
    };
    let widget_width = total_widget_width();
    let widget_height = sc(WIDGET_HEIGHT);
    let (parent, position, clamped_offset) = if embedded {
        let Some(taskbar_hwnd) = taskbar_hwnd else {
            diagnose::log("embedded widget has no taskbar handle");
            return;
        };
        let Some(taskbar_rect) = native_interop::get_taskbar_rect(taskbar_hwnd)
            .or_else(|| native_interop::get_window_rect_safe(taskbar_hwnd))
        else {
            diagnose::log("position_at_taskbar skipped: unable to query taskbar rect");
            return;
        };
        let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
        let Some((position, offset)) = taskbar_window_position(
            taskbar_hwnd,
            taskbar_rect,
            widget_width,
            widget_height,
            tray_left,
            tray_offset,
            anchor_left,
        ) else {
            diagnose::log("position_at_taskbar skipped: taskbar client coordinates unavailable");
            return;
        };
        (Some(taskbar_hwnd), position, offset)
    } else {
        let monitors = native_interop::find_monitors();
        let taskbars = native_interop::find_taskbars();
        let Some(target) =
            resolve_monitor_target(&monitors, &taskbars, monitor_device.as_deref(), None)
        else {
            diagnose::log("position_at_taskbar skipped: no active display");
            return;
        };
        if target.taskbar.is_some() {
            diagnose::log(
                "floating reposition skipped: selected display has a taskbar; waiting to embed",
            );
            return;
        }
        let position = floating_window_position(
            target.monitor.rect,
            widget_width,
            widget_height,
            anchor_left,
            tray_offset,
        );
        let max_offset = target
            .monitor
            .rect
            .right
            .saturating_sub(target.monitor.rect.left)
            .saturating_sub(widget_width.max(1))
            .max(0);
        (None, position, tray_offset.clamp(0, max_offset))
    };

    let offset_changed = if anchor_left {
        false
    } else {
        let mut state = lock_state();
        state.as_mut().is_some_and(|s| {
            if s.tray_offset != clamped_offset {
                s.tray_offset = clamped_offset;
                true
            } else {
                false
            }
        })
    };
    if offset_changed && save_offset {
        save_state_settings();
    }

    // Explorer can emit tray location events even when its geometry is unchanged.
    // Avoid making an unchanged layered child participate in another native
    // positioning/redraw cycle. Query the live rectangle rather than caching it,
    // since Explorer can move the child independently of our own placement calls.
    let already_positioned = parent.is_some_and(|parent| {
        native_interop::get_window_rect_safe(hwnd).is_some_and(|rect| {
            native_interop::screen_to_client(parent, rect.left, rect.top).is_some_and(|point| {
                placement_matches(
                    RECT {
                        left: point.x,
                        top: point.y,
                        right: point.x + (rect.right - rect.left),
                        bottom: point.y + (rect.bottom - rect.top),
                    },
                    position,
                )
            })
        })
    });
    if !already_positioned && !native_interop::position_window(hwnd, parent, position) {
        diagnose::log("position_at_taskbar failed to reposition widget");
        return;
    }
    let dpi_before_refresh = CURRENT_DPI.load(Ordering::Relaxed);
    refresh_dpi();
    if CURRENT_DPI.load(Ordering::Relaxed) != dpi_before_refresh {
        // A move across displays can change DPI only after the first native
        // placement. Recompute once using the new scale and clamp again.
        position_at_taskbar_without_save();
    }
}

fn compute_anchor_x(
    taskbar_left: i32,
    tray_left: i32,
    width: i32,
    offset: i32,
    anchor_left: bool,
) -> i32 {
    if anchor_left {
        taskbar_left
    } else {
        (tray_left - width - offset).max(taskbar_left)
    }
}

fn compute_anchor_y(anchor_top: i32, anchor_height: i32, widget_height: i32) -> i32 {
    let anchor_bottom = anchor_top + anchor_height;
    (anchor_bottom - widget_height).max(anchor_top)
}

/// WinEvent callback for tray icon location changes
unsafe extern "system" fn on_tray_location_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    static LAST_REPOSITION: Mutex<Option<std::time::Instant>> = Mutex::new(None);

    if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
        return;
    }

    let is_tray = {
        let state = lock_state();
        state
            .as_ref()
            .and_then(|s| s.tray_notify_hwnd)
            .map(|h| h == hwnd)
            .unwrap_or(false)
    };

    if is_tray {
        if tray_reposition_is_suppressed() {
            return;
        }

        let should_reposition = {
            let mut last = LAST_REPOSITION.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::Instant::now();
            if last
                .map(|t| now.duration_since(t).as_millis() > 500)
                .unwrap_or(true)
            {
                *last = Some(now);
                true
            } else {
                false
            }
        };
        if should_reposition {
            position_at_taskbar();
            render_layered();
        }
    }
}

fn recover_polling(hwnd: HWND, network: bool) {
    if !MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
        let placement = refresh_monitor_selection(hwnd);
        if placement.reposition {
            position_at_taskbar();
        }
        render_layered();
    }
    let accepted = {
        let mut state = lock_state();
        state.as_mut().is_some_and(|s| {
            let now = Instant::now();
            if !s.recovery_debounce.accept(network, now) {
                return false;
            }
            s.monitor.force(now, false);
            true
        })
    };
    if accepted {
        diagnose::log(if network {
            "network restored: refreshing providers"
        } else {
            "system resumed: refreshing providers"
        });
        start_poll(SendHwnd::from_hwnd(hwnd), true);
    }
}

/// Main window procedure
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_created = taskbar_created_message();
    if taskbar_created != 0 && msg == taskbar_created {
        let (main_hwnd, broadcast_hwnd) = {
            let state = lock_state();
            state.as_ref().map_or((None, None), |app_state| {
                (
                    Some(app_state.hwnd.to_hwnd()),
                    app_state.taskbar_broadcast_hwnd,
                )
            })
        };
        if let Some(main_hwnd) = main_hwnd {
            if broadcast_hwnd == Some(hwnd) {
                let _ = PostMessageW(main_hwnd, WM_APP_TASKBAR_CREATED, WPARAM(0), LPARAM(0));
            } else if main_hwnd == hwnd {
                if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
                    PENDING_TASKBAR_REFRESH.store(true, Ordering::Release);
                } else {
                    sync_tray_icons(hwnd);
                    let placement = refresh_monitor_selection(hwnd);
                    if placement.reposition {
                        position_at_taskbar();
                    }
                    render_layered();
                }
            }
        }
        return LRESULT(0);
    }
    let (is_broadcast_receiver, broadcast_main) = {
        let state = lock_state();
        let state = state.as_ref();
        (
            state.is_some_and(|state| state.taskbar_broadcast_hwnd == Some(hwnd)),
            state.map(|state| state.hwnd.to_hwnd()),
        )
    };
    if is_broadcast_receiver {
        if matches!(msg, WM_DISPLAYCHANGE | WM_SETTINGCHANGE) {
            if let Some(main) = broadcast_main {
                let _ = SendMessageW(main, msg, wparam, lparam);
            }
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    match msg {
        WM_NOTIFY if lparam.0 != 0 => {
            let notification = &*(lparam.0 as *const NMHDR);
            if notification.code == TTN_SHOW {
                // A tooltip owned by an embedded child must be above the
                // taskbar too. Raise only the transient tip when it appears,
                // never the widget or its Explorer parent on refresh.
                let _ = SetWindowPos(
                    notification.hwndFrom,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER,
                );
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => {
            // For non-embedded fallback, paint normally
            let embedded = {
                let state = lock_state();
                state.as_ref().map(|s| s.embedded).unwrap_or(false)
            };
            if embedded {
                // Layered windows don't use WM_PAINT; just validate the region
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            } else {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hdc, hwnd);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_POWERBROADCAST
            if wparam.0 == PBT_APMRESUMEAUTOMATIC as usize
                || wparam.0 == PBT_APMRESUMESUSPEND as usize =>
        {
            recover_polling(hwnd, false);
            LRESULT(1)
        }
        _ if msg == recovery_events::MESSAGE => {
            recover_polling(hwnd, true);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED_MSG | WM_SETTINGCHANGE => {
            if msg == WM_DPICHANGED_MSG {
                let new_dpi = (wparam.0 & 0xFFFF) as u32;
                CURRENT_DPI.store(new_dpi, Ordering::Relaxed);
            }
            if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
                let setting_change = msg == WM_SETTINGCHANGE;
                PENDING_MONITOR_REFRESH.store(true, Ordering::Release);
                if setting_change {
                    PENDING_SETTING_CHANGE.store(true, Ordering::Release);
                }
                refresh_dpi();
                return LRESULT(0);
            }
            if msg == WM_SETTINGCHANGE {
                check_theme_change();
                check_language_change();
            }
            refresh_dpi();
            let placement = if matches!(msg, WM_DISPLAYCHANGE | WM_SETTINGCHANGE) {
                refresh_monitor_selection(hwnd)
            } else {
                MonitorRefreshResult {
                    retry: false,
                    reposition: true,
                }
            };
            if placement.reposition {
                position_at_taskbar();
            }
            render_layered();
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            match timer_id {
                TIMER_POLL => {
                    request_poll(SendHwnd::from_hwnd(hwnd), false);
                }
                TIMER_COUNTDOWN => {
                    update_display();
                    render_layered();
                    schedule_countdown_timer();
                }
                TIMER_TASKBAR_RETRY => {
                    let placement = refresh_monitor_selection(hwnd);
                    if placement.reposition {
                        position_at_taskbar();
                    }
                    render_layered();
                    if !placement.retry {
                        diagnose::log("monitor target resolved; placement retry timer stopped");
                    }
                }
                TIMER_FRESHNESS => {
                    update_display();
                    render_layered();
                }
                TIMER_UPDATE_CHECK => {
                    begin_update_check(hwnd, false);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_USAGE_UPDATED => {
            check_theme_change();
            check_language_change();
            render_layered();
            schedule_countdown_timer();
            suppress_tray_reposition_for(Duration::from_millis(
                TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS,
            ));
            sync_tray_icons(hwnd);
            LRESULT(0)
        }
        WM_APP_UPDATE_CHECK_COMPLETE => {
            schedule_auto_update_check(hwnd);
            publish_settings();
            LRESULT(0)
        }
        WM_APP_TASKBAR_CREATED => {
            if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
                PENDING_TASKBAR_REFRESH.store(true, Ordering::Release);
            } else {
                sync_tray_icons(hwnd);
                let placement = refresh_monitor_selection(hwnd);
                if placement.reposition {
                    position_at_taskbar();
                }
                render_layered();
            }
            LRESULT(0)
        }
        WM_APP_MONITOR_REFRESH => {
            if MONITOR_TRANSITION_IN_PROGRESS.load(Ordering::Acquire) {
                PENDING_MONITOR_REFRESH.store(true, Ordering::Release);
                if wparam.0 != 0 {
                    PENDING_SETTING_CHANGE.store(true, Ordering::Release);
                }
            } else {
                if wparam.0 != 0 {
                    check_theme_change();
                    check_language_change();
                }
                refresh_dpi();
                let placement = refresh_monitor_selection(hwnd);
                if placement.reposition {
                    position_at_taskbar();
                }
                render_layered();
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            if cursor_is_on_drag_handle(hwnd) {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
            let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if !is_drag_handle_point(client_x, client_y) {
                return LRESULT(0);
            }

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut state = lock_state();
            if let Some(s) = state.as_mut() {
                if s.anchor_left {
                    return LRESULT(0);
                }
                s.dragging = true;
                s.drag_start_mouse_x = pt.x;
                s.drag_start_client_x = client_x;
                s.drag_start_offset = s.tray_offset;
            }
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let drag_state = {
                let state = lock_state();
                state.as_ref().and_then(|s| {
                    s.dragging.then_some((
                        s.hwnd.to_hwnd(),
                        s.drag_start_mouse_x,
                        s.drag_start_offset,
                        s.embedded,
                        s.taskbar_hwnd,
                        s.effective_monitor_device.clone(),
                    ))
                })
            };
            if let Some((widget_hwnd, start_x, start_offset, embedded, taskbar_hwnd, device)) =
                drag_state
            {
                let proposed_offset = start_offset
                    .saturating_add(start_x.saturating_sub(pt.x))
                    .max(0);
                let width = total_widget_width();
                let height = sc(WIDGET_HEIGHT);
                let placement = if embedded {
                    taskbar_hwnd.and_then(|taskbar_hwnd| {
                        native_interop::get_taskbar_rect(taskbar_hwnd)
                            .or_else(|| native_interop::get_window_rect_safe(taskbar_hwnd))
                            .and_then(|rect| {
                                let tray_left = tray_left_for_taskbar(taskbar_hwnd, rect);
                                taskbar_window_position(
                                    taskbar_hwnd,
                                    rect,
                                    width,
                                    height,
                                    tray_left,
                                    proposed_offset,
                                    false,
                                )
                            })
                    })
                } else {
                    let monitors = native_interop::find_monitors();
                    let taskbars = native_interop::find_taskbars();
                    resolve_monitor_target(&monitors, &taskbars, device.as_deref(), None)
                        .filter(|target| target.taskbar.is_none())
                        .map(|target| {
                            let max_offset = target
                                .monitor
                                .rect
                                .right
                                .saturating_sub(target.monitor.rect.left)
                                .saturating_sub(width.max(1))
                                .max(0);
                            let offset = proposed_offset.min(max_offset);
                            (
                                floating_window_position(
                                    target.monitor.rect,
                                    width,
                                    height,
                                    false,
                                    offset,
                                ),
                                offset,
                            )
                        })
                };
                if let Some((position, offset)) = placement {
                    if let Some(state) = lock_state().as_mut() {
                        if state.dragging {
                            state.tray_offset = offset;
                        }
                    }
                    let parent = embedded.then_some(taskbar_hwnd).flatten();
                    let _ = native_interop::position_window(widget_hwnd, parent, position);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let drag_result = {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    if s.dragging {
                        s.dragging = false;
                        Some((
                            s.effective_monitor_device.clone(),
                            s.drag_start_client_x,
                            s.tray_offset,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some((current_device, drag_start_client_x, drag_offset)) = drag_result {
                let _ = ReleaseCapture();
                let mut monitor_transition = DragMonitorTransition::None;
                if let Some((_target_index, target_taskbar)) = taskbar_at_point(pt) {
                    let target_device = native_interop::taskbar_monitor_device(&target_taskbar);
                    if target_device.as_deref() != current_device.as_deref() {
                        let new_offset = offset_for_drop_point(
                            target_taskbar.hwnd,
                            target_taskbar.rect,
                            pt,
                            drag_start_client_x,
                        );
                        let monitors = native_interop::find_monitors();
                        let taskbars = native_interop::find_taskbars();
                        let target = target_device.as_deref().and_then(|device| {
                            resolve_monitor_target(&monitors, &taskbars, Some(device), None)
                        });
                        if let Some(target) = target {
                            monitor_transition = DragMonitorTransition::Failed;
                            if let Some(state) = lock_state().as_mut() {
                                state.tray_offset = new_offset;
                            }
                            if transition_to_monitor(hwnd, &target) {
                                monitor_transition = DragMonitorTransition::Succeeded(new_offset);
                                if let Some(state) = lock_state().as_mut() {
                                    state.monitor_device = Some(target.monitor.device.clone());
                                    state.pending_monitor_device = None;
                                    state.legacy_selection_pending = false;
                                    state.tray_offset =
                                        drag_offset_to_persist(drag_offset, monitor_transition);
                                }
                                position_at_taskbar_without_save();
                                render_layered();
                            } else {
                                {
                                    let mut state = lock_state();
                                    if let Some(state) = state.as_mut() {
                                        state.tray_offset =
                                            drag_offset_to_persist(drag_offset, monitor_transition);
                                        state.pending_monitor_device =
                                            Some(Some(target.monitor.device.clone()));
                                    }
                                }
                                diagnose::log("taskbar drop could not be embedded; retaining old placement and retrying");
                                unsafe {
                                    let _ = SetTimer(hwnd, TIMER_TASKBAR_RETRY, 2_000, None);
                                }
                            }
                        }
                    }
                }
                if !matches!(monitor_transition, DragMonitorTransition::Succeeded(_)) {
                    if let Some(state) = lock_state().as_mut() {
                        state.tray_offset = drag_offset_to_persist(drag_offset, monitor_transition);
                    }
                }
                // Persist once, after any failed cross-taskbar move has restored
                // the old placement offset and successful moves have been clamped.
                save_state_settings();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_context_menu(hwnd);
            LRESULT(0)
        }
        settings_ui::WM_SETTINGS => {
            handle_settings_requests(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_command(hwnd, wparam.0 as u16);
            publish_settings();
            LRESULT(0)
        }
        _ if msg == WM_APP_TRAY => {
            match tray_icon::handle_message(lparam) {
                tray_icon::TrayAction::ToggleWidget => {
                    toggle_widget_visibility(hwnd);
                }
                tray_icon::TrayAction::ShowContextMenu => {
                    show_context_menu(hwnd);
                }
                tray_icon::TrayAction::None => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let (is_main_window, watch, hook, broadcast_hwnd) = {
                let mut state = lock_state();
                match state.as_mut() {
                    Some(app_state) if app_state.hwnd.to_hwnd() == hwnd => (
                        true,
                        app_state.recovery_watch.take(),
                        app_state.win_event_hook,
                        app_state.taskbar_broadcast_hwnd.take(),
                    ),
                    _ => (false, None, None, None),
                }
            };
            if !is_main_window {
                return LRESULT(0);
            }
            drop(watch);
            quota_tooltip::clear();
            if let Some(h) = hook {
                native_interop::unhook_win_event(h);
            }
            if let Some(broadcast_hwnd) = broadcast_hwnd {
                let _ = DestroyWindow(broadcast_hwnd);
            }
            tray_icon::remove_all(hwnd);
            settings_ui::shutdown();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn show_context_menu(hwnd: HWND) {
    show_settings_menu(hwnd);
}

fn open_settings(hwnd: HWND, page: Page) {
    refresh_settings_availability();
    match settings_ui::open(hwnd.0 as isize, page) {
        Ok(()) => publish_settings(),
        Err(error) => show_error_message(hwnd, "Settings unavailable", &error),
    }
}

fn show_settings_menu(hwnd: HWND) {
    let (strings, visible) = {
        let state = lock_state();
        let Some(s) = state.as_ref() else { return };
        (s.language.strings(), s.widget_visible)
    };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (id, label) in [
            (1, strings.refresh),
            (
                tray_icon::IDM_TOGGLE_WIDGET,
                if visible {
                    "Hide widget"
                } else {
                    "Show widget"
                },
            ),
            (IDM_SETTINGS, strings.settings),
            (IDM_APPEARANCE_PAGE, "Appearance"),
            (IDM_ABOUT_STAR, "About"),
            (2, strings.exit),
        ] {
            let text = native_interop::wide_str(label);
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                usize::from(id),
                PCWSTR::from_raw(text.as_ptr()),
            );
        }
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

fn effective_provider_availability(state: &AppState) -> poller::ProviderAvailability {
    let mut availability = state.provider_availability;
    #[cfg(debug_assertions)]
    for provider in ProviderId::ALL {
        let development = state.development_overrides[provider.index()];
        if !development.enabled || !development.force_available {
            continue;
        }
        match provider {
            ProviderId::Claude => availability.claude_code_available = true,
            ProviderId::Codex => availability.codex_available = true,
            ProviderId::Antigravity => availability.antigravity_available = true,
            ProviderId::OpenCode => availability.opencode_available = true,
            ProviderId::Cursor => availability.cursor_available = true,
            ProviderId::Grok => availability.grok_available = true,
            ProviderId::Copilot => availability.copilot_available = true,
        }
    }
    availability
}

fn development_override_json(override_state: DevelopmentOverride) -> serde_json::Value {
    use serde_json::json;
    if cfg!(debug_assertions) {
        json!({
            "enabled": override_state.enabled,
            "force_available": override_state.force_available,
            "session_percent": override_state.session_percent,
            "weekly_percent": override_state.weekly_percent,
        })
    } else {
        json!({
            "enabled": false,
            "force_available": false,
            "session_percent": null,
            "weekly_percent": null,
        })
    }
}

fn development_overrides_snapshot(state: &AppState) -> serde_json::Value {
    use serde_json::json;
    let overrides = state.development_overrides;
    json!({
        "claude": development_override_json(overrides[ProviderId::Claude.index()]),
        "codex": development_override_json(overrides[ProviderId::Codex.index()]),
        "antigravity": development_override_json(overrides[ProviderId::Antigravity.index()]),
        "opencode": development_override_json(overrides[ProviderId::OpenCode.index()]),
        "cursor": development_override_json(overrides[ProviderId::Cursor.index()]),
        "grok": development_override_json(overrides[ProviderId::Grok.index()]),
        "copilot": development_override_json(overrides[ProviderId::Copilot.index()]),
    })
}

fn provider_availability_json(availability: poller::ProviderAvailability) -> serde_json::Value {
    serde_json::json!({
        "claude_code_available": availability.claude_code_available,
        "codex_available": availability.codex_available,
        "antigravity_available": availability.antigravity_available,
        "opencode_available": availability.opencode_available,
        "cursor_available": availability.cursor_available,
        "grok_available": availability.grok_available,
        "copilot_available": availability.copilot_available,
    })
}

fn provider_display(state: &AppState, provider: ProviderId) -> ProviderDisplay {
    let (session_percent, session_text, weekly_percent, weekly_text) = match provider {
        ProviderId::Claude => (
            state.session_percent,
            state.session_text.as_str(),
            state.weekly_percent,
            state.weekly_text.as_str(),
        ),
        ProviderId::Codex => (
            state.codex_session_percent,
            state.codex_session_text.as_str(),
            state.codex_weekly_percent,
            state.codex_weekly_text.as_str(),
        ),
        ProviderId::Antigravity => (
            state.antigravity_session_percent,
            state.antigravity_session_text.as_str(),
            state.antigravity_weekly_percent,
            state.antigravity_weekly_text.as_str(),
        ),
        ProviderId::OpenCode => (
            state.opencode_session_percent,
            state.opencode_session_text.as_str(),
            state.opencode_weekly_percent,
            state.opencode_weekly_text.as_str(),
        ),
        ProviderId::Cursor => (
            state.cursor_session_percent,
            state.cursor_session_text.as_str(),
            state.cursor_weekly_percent,
            state.cursor_weekly_text.as_str(),
        ),
        ProviderId::Grok => (
            state.grok_session_percent,
            state.grok_session_text.as_str(),
            state.grok_weekly_percent,
            state.grok_weekly_text.as_str(),
        ),
        ProviderId::Copilot => (
            state.copilot_session_percent,
            state.copilot_session_text.as_str(),
            state.copilot_weekly_percent,
            state.copilot_weekly_text.as_str(),
        ),
    };
    let mut display = ProviderDisplay {
        session_percent,
        session_text: session_text.to_string(),
        weekly_percent,
        weekly_text: weekly_text.to_string(),
    };
    #[cfg(not(debug_assertions))]
    let _ = provider;
    #[cfg(debug_assertions)]
    {
        let development = state.development_overrides[provider.index()];
        if development.enabled {
            if let Some(percent) = development.session_percent {
                display.session_percent = percent;
                display.session_text = development_percentage_text(percent, state.language);
            }
            if let Some(percent) = development.weekly_percent {
                display.weekly_percent = percent;
                display.weekly_text = development_percentage_text(percent, state.language);
            }
        }
    }
    display
}

#[cfg(debug_assertions)]
fn development_percentage_text(percent: f64, language: LanguageId) -> String {
    let shown = if language == LanguageId::SimplifiedChinese {
        poller::remaining_percentage(percent)
    } else {
        percent
    };
    format!("{shown:.0}%")
}

fn settings_snapshot() -> serde_json::Value {
    use serde_json::json;
    let startup = is_startup_enabled();
    let monitors: Vec<_> = native_interop::find_monitors()
        .iter()
        .map(|monitor| json!({"device":monitor.device,"primary":monitor.is_primary}))
        .collect();
    let state = lock_state();
    let Some(s) = state.as_ref() else {
        return json!(null);
    };
    let dark = s.appearance.is_dark(s.is_dark);
    let claude = custom_color(s.appearance.claude_accent, claude_accent_color());
    let codex = custom_color(s.appearance.codex_accent, provider_icons::codex_color(dark));
    let antigravity = custom_color(s.appearance.antigravity_accent, antigravity_accent_color());
    let opencode = custom_color(s.appearance.opencode_accent, opencode_accent_color());
    let cursor = custom_color(s.appearance.cursor_accent, cursor_accent_color());
    let grok = custom_color(s.appearance.grok_accent, grok_accent_color());
    let copilot = custom_color(s.appearance.copilot_accent, copilot_accent_color());
    let candidate = SettingsCandidate::from_app_state(s, startup);
    let preview = widget_preview_image_with_development(
        &candidate,
        s.language,
        s.is_dark,
        &s.development_overrides,
    );
    let availability = effective_provider_availability(s);
    json!({
        "widget_preview_image":preview.as_ref().map(|image| image.0.as_str()),
        "widget_preview_width":preview.as_ref().map(|image| image.1),
        "widget_preview_height":preview.as_ref().map(|image| image.2),
        "widget_preview_dpi":CURRENT_DPI.load(Ordering::Relaxed),
        "settings": {
            "language":s.language_override.map_or("system", LanguageId::code),
            "startup":startup, "widget_visible":s.widget_visible,
            "settings_pane_open":s.settings_pane_open,
            "show_tray_icon":s.show_tray_icon,"show_provider_icons":s.show_provider_icons,
            "show_claude_code":s.show_claude_code,"show_codex":s.show_codex,
            "show_antigravity":s.show_antigravity,"show_opencode":s.show_opencode,
            "show_cursor":s.show_cursor,"show_grok":s.show_grok,
            "show_copilot":s.show_copilot,"allow_unlimited_providers":s.allow_unlimited_providers,"show_session_window":s.show_session_window,
            "show_weekly_window":s.show_weekly_window,"poll_interval_ms":s.poll_interval_ms,
            "adaptive_refresh":s.adaptive_refresh,"alert_threshold_percent":s.alert_threshold_percent,
            "palette":s.appearance.palette,"bar_style":s.appearance.bar_style,
            "bar_thickness":s.appearance.bar_thickness,"font_size":s.appearance.font_size,
            "provider_spacing":s.appearance.provider_spacing,
            "provider_order":s.appearance.provider_order,
            "provider_icon_position":s.appearance.provider_icon_position,
            "provider_monochrome":s.appearance.provider_monochrome,
            "show_provider_icon":s.appearance.show_provider_icon,
            "show_claude_icon":s.appearance.show_claude_icon,
            "show_codex_icon":s.appearance.show_codex_icon,
            "show_antigravity_icon":s.appearance.show_antigravity_icon,
            "show_opencode_icon":s.appearance.show_opencode_icon,
            "show_cursor_icon":s.appearance.show_cursor_icon,
            "show_grok_icon":s.appearance.show_grok_icon,
            "show_copilot_icon":s.appearance.show_copilot_icon,
            "show_percentage":s.appearance.show_percentage,"show_bar":s.appearance.show_bar,
            "show_reset_time":s.appearance.show_reset_time,
            "usage_text_color":s.appearance.usage_text_color,
            "window_label_font_size":s.appearance.window_label_font_size,
            "claude_accent":s.appearance.claude_accent,"codex_accent":s.appearance.codex_accent,
            "antigravity_accent":s.appearance.antigravity_accent,"opencode_accent":s.appearance.opencode_accent,
            "cursor_accent":s.appearance.cursor_accent,"grok_accent":s.appearance.grok_accent,
            "copilot_accent":s.appearance.copilot_accent,
            "monitor_device":s.monitor_device,"anchor_left":s.anchor_left
        },
        "effective_language":s.language.code(),
        "claude_code_available":availability.claude_code_available,
        "codex_available":availability.codex_available,
        "antigravity_available":availability.antigravity_available,
        "opencode_available":availability.opencode_available,
        "cursor_available":availability.cursor_available,
        "grok_available":availability.grok_available,
        "copilot_available":availability.copilot_available,
        "credential_availability":provider_availability_json(s.provider_availability),
        "development_enabled":cfg!(debug_assertions),
        "development_overrides":development_overrides_snapshot(s),
        "effective_appearance":{"dark":dark,"claude_accent":[claude.r,claude.g,claude.b],"codex_accent":[codex.r,codex.g,codex.b],"antigravity_accent":[antigravity.r,antigravity.g,antigravity.b],"opencode_accent":[opencode.r,opencode.g,opencode.b],"cursor_accent":[cursor.r,cursor.g,cursor.b],"grok_accent":[grok.r,grok.g,grok.b],"copilot_accent":[copilot.r,copilot.g,copilot.b]},
        "update_busy":matches!(s.update_status, UpdateStatus::Checking | UpdateStatus::Applying),
        "update_available":update_available(&s.update_status),
        "monitors":monitors,"version":env!("CARGO_PKG_VERSION"),
        "languages":LanguageId::ALL.iter().map(|l| json!({"code":l.code(),"name":l.native_name()})).collect::<Vec<_>>(),
        "update_status":version_action_label(s.language.strings(),s.language,s.install_channel,&s.update_status),
        "install_channel":match s.install_channel { InstallChannel::Portable => "portable", InstallChannel::Winget => "winget" }
    })
}

/// Return canonical settings with a draft bitmap, without changing or publishing state.
fn preview_settings_snapshot(
    changes: &[settings_ui::Change],
) -> Result<serde_json::Value, SettingsApplyError> {
    let snapshot = settings_snapshot();
    let (current, availability, language, system_dark, development_overrides) = {
        let state = lock_state();
        let s = state
            .as_ref()
            .ok_or_else(|| SettingsApplyError::new("unavailable", "Widget is shutting down"))?;
        (
            SettingsCandidate::from_app_state(
                s,
                snapshot["settings"]["startup"].as_bool().unwrap_or(false),
            ),
            effective_provider_availability(s),
            s.language,
            s.is_dark,
            s.development_overrides,
        )
    };
    let monitors = snapshot["monitors"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value["device"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    draft_settings_snapshot_with_development(
        snapshot,
        &current,
        changes,
        availability,
        &monitors,
        language,
        system_dark,
        &development_overrides,
    )
}

#[cfg(test)]
fn draft_settings_snapshot(
    snapshot: serde_json::Value,
    current: &SettingsCandidate,
    changes: &[settings_ui::Change],
    availability: poller::ProviderAvailability,
    monitors: &[String],
    language: LanguageId,
    system_dark: bool,
) -> Result<serde_json::Value, SettingsApplyError> {
    draft_settings_snapshot_with_development(
        snapshot,
        current,
        changes,
        availability,
        monitors,
        language,
        system_dark,
        &[DevelopmentOverride::default(); 7],
    )
}

fn draft_settings_snapshot_with_development(
    mut snapshot: serde_json::Value,
    current: &SettingsCandidate,
    changes: &[settings_ui::Change],
    availability: poller::ProviderAvailability,
    monitors: &[String],
    language: LanguageId,
    system_dark: bool,
    development_overrides: &[DevelopmentOverride; 7],
) -> Result<serde_json::Value, SettingsApplyError> {
    let candidate =
        validate_settings_changes_with_availability(current, changes, availability, monitors)?;
    let language = candidate.language_override.unwrap_or_else(|| {
        if changes.iter().any(|change| change.key == Key::Language) {
            localization::detect_system_language()
        } else {
            language
        }
    });
    let preview = widget_preview_image_with_development(
        &candidate,
        language,
        system_dark,
        development_overrides,
    );
    snapshot["widget_preview_image"] =
        serde_json::json!(preview.as_ref().map(|image| image.0.as_str()));
    snapshot["widget_preview_width"] = serde_json::json!(preview.as_ref().map(|image| image.1));
    snapshot["widget_preview_height"] = serde_json::json!(preview.as_ref().map(|image| image.2));
    snapshot["widget_preview_dpi"] = CURRENT_DPI.load(Ordering::Relaxed).into();
    Ok(snapshot)
}

/// Paint deterministic sample quotas through the real widget renderer into an
/// isolated top-down DIB. No window handles or live quota state are involved.
fn preview_provider_display(
    override_state: DevelopmentOverride,
    language: LanguageId,
    session_text: &str,
    weekly_text: &str,
) -> ProviderDisplay {
    let mut display = ProviderDisplay {
        session_percent: 18.0,
        session_text: session_text.to_string(),
        weekly_percent: 42.0,
        weekly_text: weekly_text.to_string(),
    };
    #[cfg(debug_assertions)]
    if override_state.enabled {
        if let Some(percent) = override_state.session_percent {
            display.session_percent = percent;
            display.session_text = development_percentage_text(percent, language);
        }
        if let Some(percent) = override_state.weekly_percent {
            display.weekly_percent = percent;
            display.weekly_text = development_percentage_text(percent, language);
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = (override_state, language);
    display
}

#[cfg(test)]
fn widget_preview_image(
    candidate: &SettingsCandidate,
    language: LanguageId,
    system_dark: bool,
) -> Option<(String, i32, i32)> {
    widget_preview_image_with_development(
        candidate,
        language,
        system_dark,
        &[DevelopmentOverride::default(); 7],
    )
}

fn widget_preview_image_with_development(
    candidate: &SettingsCandidate,
    language: LanguageId,
    system_dark: bool,
    development_overrides: &[DevelopmentOverride; 7],
) -> Option<(String, i32, i32)> {
    let appearance = candidate.appearance;
    let width = total_widget_width_for(
        active_model_count(
            candidate.show_claude_code,
            candidate.show_codex,
            candidate.show_antigravity,
            candidate.show_opencode,
            candidate.show_cursor,
            candidate.show_grok,
            candidate.show_copilot,
        ),
        language,
        appearance,
    );
    let height = sc(WIDGET_HEIGHT);
    let byte_count = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if byte_count == 0 || byte_count > 1024 * 1024 {
        return None;
    }
    let (bg, text, track) = appearance_colors(appearance, system_dark);
    let claude = custom_color(appearance.claude_accent, claude_accent_color());
    let codex = custom_color(
        appearance.codex_accent,
        provider_icons::codex_color(appearance.is_dark(system_dark)),
    );
    let antigravity = antigravity_accent_color();
    let opencode = custom_color(appearance.opencode_accent, opencode_accent_color());
    let cursor = custom_color(appearance.cursor_accent, cursor_accent_color());
    let grok = custom_color(appearance.grok_accent, grok_accent_color());
    let copilot = custom_color(appearance.copilot_accent, copilot_accent_color());
    unsafe {
        let dc = CreateCompatibleDC(HDC::default());
        if dc.is_invalid() {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        if bitmap.is_invalid() || bits.is_null() {
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(dc);
            return None;
        }
        // Initialize the entire allocation even if a later GDI paint fails.
        std::ptr::write_bytes(bits.cast::<u8>(), 0, byte_count);
        let previous = SelectObject(dc, bitmap);
        let strings = language.strings();
        let (session, weekly) = if language == LanguageId::SimplifiedChinese {
            ("82%  23:59重置".to_string(), "58%  12/31重置".to_string())
        } else {
            (
                format!("18% · 2{}", strings.hour_suffix),
                format!("42% · 3{}", strings.day_suffix),
            )
        };
        let preview_values: [ProviderDisplay; 7] = std::array::from_fn(|index| {
            preview_provider_display(
                development_overrides[index],
                language,
                &session,
                &weekly,
            )
        });
        paint_content(
            dc,
            width,
            height,
            system_dark,
            appearance,
            &bg,
            &text,
            &claude,
            &track,
            language,
            strings,
            preview_values[0].session_percent,
            &preview_values[0].session_text,
            preview_values[0].weekly_percent,
            &preview_values[0].weekly_text,
            preview_values[1].session_percent,
            &preview_values[1].session_text,
            preview_values[1].weekly_percent,
            &preview_values[1].weekly_text,
            preview_values[2].session_percent,
            &preview_values[2].session_text,
            preview_values[2].weekly_percent,
            &preview_values[2].weekly_text,
            preview_values[3].session_percent,
            &preview_values[3].session_text,
            preview_values[3].weekly_percent,
            &preview_values[3].weekly_text,
            preview_values[4].session_percent,
            &preview_values[4].session_text,
            preview_values[4].weekly_percent,
            &preview_values[4].weekly_text,
            preview_values[5].session_percent,
            &preview_values[5].session_text,
            preview_values[5].weekly_percent,
            &preview_values[5].weekly_text,
            preview_values[6].session_percent,
            &preview_values[6].session_text,
            preview_values[6].weekly_percent,
            &preview_values[6].weekly_text,
            candidate.show_claude_code,
            candidate.show_codex,
            candidate.show_antigravity,
            candidate.show_opencode,
            candidate.show_cursor,
            candidate.show_grok,
            candidate.show_copilot,
            candidate.show_provider_icons,
            candidate.show_session_window,
            candidate.show_weekly_window,
            &codex,
            &antigravity,
            &opencode,
            &cursor,
            &grok,
            &copilot,
        );
        let _ = GdiFlush();
        // SAFETY: the selected DIB provides byte_count initialized bytes until
        // deletion below. Copy before releasing the bitmap and its DC.
        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), byte_count);
        let image = encode_preview_bmp(width, height, pixels);
        SelectObject(dc, previous);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(dc);
        Some((
            format!("data:image/bmp;base64,{}", base64_encode(&image)),
            width,
            height,
        ))
    }
}

fn encode_preview_bmp(width: i32, height: i32, pixels: &[u8]) -> Vec<u8> {
    let mut bmp = Vec::with_capacity(54 + pixels.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&(-height).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 16]);
    // BI_RGB is opaque BGRX: explicitly clear GDI's unspecified alpha byte.
    bmp.extend_from_slice(pixels);
    for alpha_index in (57..bmp.len()).step_by(4) {
        bmp[alpha_index] = 0;
    }
    bmp
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(char::from(ALPHABET[(bits >> 18) as usize]));
        encoded.push(char::from(ALPHABET[((bits >> 12) & 63) as usize]));
        encoded.push(if chunk.len() > 1 {
            char::from(ALPHABET[((bits >> 6) & 63) as usize])
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            char::from(ALPHABET[(bits & 63) as usize])
        } else {
            '='
        });
    }
    encoded
}

fn publish_settings() {
    settings_ui::publish(settings_snapshot());
}

fn refresh_settings_availability() {
    let availability = poller::provider_availability();
    if let Some(s) = lock_state().as_mut() {
        s.provider_availability = availability;
    }
}

fn handle_settings_requests(hwnd: HWND) {
    if let Some(error) = settings_ui::take_error() {
        show_error_message(hwnd, "Settings unavailable", &error);
    }
    let mut handled_request = false;
    for pending in settings_ui::drain() {
        if pending.cancelled.load(Ordering::Acquire) {
            continue;
        }
        handled_request = true;
        if let Operation::Preview { changes } = &pending.request.operation {
            // Drafts are read-only: no credential polling, publication, revision
            // increment, persistence, HWND operation or backend reconfiguration.
            let preview = preview_settings_snapshot(changes);
            let (state, result) = match preview {
                Ok(state) => (state, Ok(())),
                Err(error) => (settings_snapshot(), Err(error)),
            };
            let reply = settings_reply(pending.request.id, settings_ui::revision(), state, result);
            let _ = pending.reply.try_send(reply);
            continue;
        }
        let development_result = match &pending.request.operation {
            Operation::DevelopmentOverride {
                provider,
                enabled,
                force_available,
                session_percent,
                weekly_percent,
            } => Some(apply_development_override(
                *provider,
                *enabled,
                *force_available,
                *session_percent,
                *weekly_percent,
            )),
            Operation::DevelopmentReset => Some(reset_development_overrides()),
            _ => None,
        };
        if let Some(result) = development_result {
            let state = settings_snapshot();
            settings_ui::publish(state.clone());
            let reply = settings_reply(pending.request.id, settings_ui::revision(), state, result);
            let _ = pending.reply.try_send(reply);
            continue;
        }
        refresh_settings_availability();
        SETTINGS_SAVE_ERROR.with(|slot| slot.borrow_mut().take());
        let result = match pending.request.operation {
            Operation::Get => Ok(()),
            Operation::Set {
                key: Key::SettingsPaneOpen,
                value,
            } => setting_value(value).and_then(apply_settings_pane_open),
            Operation::Set { key, value } => {
                apply_settings_batch(hwnd, &[settings_ui::Change { key, value }])
            }
            Operation::SetBatch { changes }
                if changes.len() == 1 && changes[0].key == Key::SettingsPaneOpen =>
            {
                setting_value(changes[0].value.clone()).and_then(apply_settings_pane_open)
            }
            Operation::SetBatch { changes } => apply_settings_batch(hwnd, &changes),
            Operation::Preview { .. } => unreachable!("preview handled without side effects"),
            Operation::Persist => persist_settings_request(),
            Operation::Action { action } => apply_settings_action(hwnd, action)
                .map_err(|message| SettingsApplyError::new("action_failed", message))
                .and_then(|()| {
                    SETTINGS_SAVE_ERROR
                        .with(|slot| slot.borrow_mut().take())
                        .map_or(Ok(()), |message| {
                            Err(SettingsApplyError::new("save_failed", message))
                        })
                }),
            Operation::DevelopmentOverride { .. } | Operation::DevelopmentReset => {
                unreachable!("development operations are handled before settings updates")
            }
        };
        let state = settings_snapshot();
        settings_ui::publish(state.clone());
        let reply = settings_reply(pending.request.id, settings_ui::revision(), state, result);
        let _ = pending.reply.try_send(reply);
    }
    if !handled_request {
        refresh_settings_availability();
        publish_settings();
    }
}

#[derive(Debug)]
struct SettingsApplyError {
    code: &'static str,
    message: String,
}

impl SettingsApplyError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn settings_reply(
    id: u32,
    revision: u64,
    state: serde_json::Value,
    result: Result<(), SettingsApplyError>,
) -> serde_json::Value {
    let mut reply = serde_json::json!({
        "v": 1,
        "id": id,
        "ok": result.is_ok(),
        "revision": revision,
        "state": state
    });
    if let Err(error) = result {
        reply["error"] = error.message.into();
        reply["error_code"] = error.code.into();
    }
    reply
}

fn invalid_setting(message: impl Into<String>) -> SettingsApplyError {
    SettingsApplyError::new("invalid_setting", message)
}

fn setting_value<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, SettingsApplyError> {
    serde_json::from_value(value)
        .map_err(|error| invalid_setting(format!("Invalid setting value: {error}")))
}

fn apply_development_override(
    provider: settings_ui::DevelopmentProvider,
    enabled: bool,
    force_available: bool,
    session_percent: Option<f64>,
    weekly_percent: Option<f64>,
) -> Result<(), SettingsApplyError> {
    #[cfg(debug_assertions)]
    {
        for percent in [session_percent, weekly_percent].into_iter().flatten() {
            if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
                return Err(invalid_setting(
                    "Development quota percentages must be between 0 and 100.",
                ));
            }
        }
        let mut state = lock_state();
        let app_state = state
            .as_mut()
            .ok_or_else(|| SettingsApplyError::new("unavailable", "Widget is shutting down"))?;
        app_state.development_overrides[provider.index()] = DevelopmentOverride {
            enabled,
            force_available,
            session_percent,
            weekly_percent,
        };
        drop(state);
        render_layered();
        Ok(())
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (
            provider,
            enabled,
            force_available,
            session_percent,
            weekly_percent,
        );
        Err(SettingsApplyError::new(
            "development_disabled",
            "Development overrides are available only in debug builds.",
        ))
    }
}

fn reset_development_overrides() -> Result<(), SettingsApplyError> {
    #[cfg(debug_assertions)]
    {
        let mut state = lock_state();
        let app_state = state
            .as_mut()
            .ok_or_else(|| SettingsApplyError::new("unavailable", "Widget is shutting down"))?;
        app_state.development_overrides = [DevelopmentOverride::default(); 7];
        drop(state);
        render_layered();
        Ok(())
    }
    #[cfg(not(debug_assertions))]
    {
        Err(SettingsApplyError::new(
            "development_disabled",
            "Development overrides are available only in debug builds.",
        ))
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SettingsCandidate {
    language_override: Option<LanguageId>,
    startup: bool,
    widget_visible: bool,
    settings_pane_open: bool,
    show_tray_icon: bool,
    show_provider_icons: bool,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    show_opencode: bool,
    show_cursor: bool,
    show_grok: bool,
    show_copilot: bool,
    allow_unlimited_providers: bool,
    show_session_window: bool,
    show_weekly_window: bool,
    poll_interval_ms: u32,
    adaptive_refresh: bool,
    alert_threshold_percent: u8,
    appearance: Appearance,
    monitor_device: Option<String>,
    anchor_left: bool,
}

impl SettingsCandidate {
    #[cfg(test)]
    fn from_settings(settings: &SettingsFile, startup: bool) -> Self {
        Self {
            language_override: settings.language.as_deref().and_then(LanguageId::from_code),
            startup,
            widget_visible: settings.widget_visible,
            settings_pane_open: settings.settings_pane_open,
            show_tray_icon: settings.show_tray_icon,
            show_provider_icons: settings.show_provider_icons,
            show_claude_code: settings.show_claude_code,
            show_codex: settings.show_codex,
            show_antigravity: settings.show_antigravity,
            show_opencode: settings.show_opencode,
            show_cursor: settings.show_cursor,
            show_grok: settings.show_grok,
            show_copilot: settings.show_copilot,
            allow_unlimited_providers: settings.allow_unlimited_providers,
            show_session_window: settings.show_session_window,
            show_weekly_window: settings.show_weekly_window,
            poll_interval_ms: settings.poll_interval_ms,
            adaptive_refresh: settings.adaptive_refresh,
            alert_threshold_percent: settings.alert_threshold_percent,
            appearance: settings.appearance,
            monitor_device: settings.monitor_device.clone(),
            anchor_left: settings.anchor_left,
        }
    }

    fn from_app_state(state: &AppState, startup: bool) -> Self {
        Self {
            language_override: state.language_override,
            startup,
            widget_visible: state.widget_visible,
            settings_pane_open: state.settings_pane_open,
            show_tray_icon: state.show_tray_icon,
            show_provider_icons: state.show_provider_icons,
            show_claude_code: state.show_claude_code,
            show_codex: state.show_codex,
            show_antigravity: state.show_antigravity,
            show_opencode: state.show_opencode,
            show_cursor: state.show_cursor,
            show_grok: state.show_grok,
            show_copilot: state.show_copilot,
            allow_unlimited_providers: state.allow_unlimited_providers,
            show_session_window: state.show_session_window,
            show_weekly_window: state.show_weekly_window,
            poll_interval_ms: state.poll_interval_ms,
            adaptive_refresh: state.adaptive_refresh,
            alert_threshold_percent: state.alert_threshold_percent,
            appearance: state.appearance,
            monitor_device: state.monitor_device.clone(),
            anchor_left: state.anchor_left,
        }
    }
}

/// Validate all edits against one final candidate. No application state or OS
/// side effect is touched until this succeeds.
fn validate_settings_changes_with_availability(
    current: &SettingsCandidate,
    changes: &[settings_ui::Change],
    availability: poller::ProviderAvailability,
    monitor_devices: &[String],
) -> Result<SettingsCandidate, SettingsApplyError> {
    if changes.len() > settings_ui::MAX_BATCH_CHANGES {
        return Err(invalid_setting(format!(
            "A settings batch may contain at most {} changes.",
            settings_ui::MAX_BATCH_CHANGES
        )));
    }

    let mut candidate = current.clone();
    let mut seen = HashSet::with_capacity(changes.len());
    for change in changes {
        let canonical_key = if change.key == Key::ShowProviderIcons {
            Key::ShowProviderIcon
        } else {
            change.key
        };
        if !seen.insert(canonical_key) {
            return Err(invalid_setting(format!(
                "The setting {:?} occurs more than once in the batch.",
                change.key
            )));
        }
        match change.key {
            Key::Language => {
                let code: String = setting_value(change.value.clone())?;
                candidate.language_override = if code == "system" {
                    None
                } else {
                    Some(
                        LanguageId::from_code(&code)
                            .filter(|language| language.code() == code)
                            .ok_or_else(|| invalid_setting("Unsupported language"))?,
                    )
                };
            }
            Key::Startup => candidate.startup = setting_value(change.value.clone())?,
            Key::WidgetVisible => candidate.widget_visible = setting_value(change.value.clone())?,
            Key::SettingsPaneOpen => {
                candidate.settings_pane_open = setting_value(change.value.clone())?
            }
            Key::ShowTrayIcon => candidate.show_tray_icon = setting_value(change.value.clone())?,
            Key::ShowProviderIcons => {
                candidate.show_provider_icons = setting_value(change.value.clone())?;
                candidate.appearance.show_provider_icon = candidate.show_provider_icons;
            }
            Key::ShowProviderIcon => {
                candidate.appearance.show_provider_icon = setting_value(change.value.clone())?;
                candidate.show_provider_icons = candidate.appearance.show_provider_icon;
            }
            Key::ShowPercentage => {
                candidate.appearance.show_percentage = setting_value(change.value.clone())?
            }
            Key::ShowBar => candidate.appearance.show_bar = setting_value(change.value.clone())?,
            Key::ShowResetTime => {
                candidate.appearance.show_reset_time = setting_value(change.value.clone())?
            }
            Key::UsageTextColor => {
                candidate.appearance.usage_text_color = setting_value(change.value.clone())?
            }
            Key::WindowLabelFontSize => {
                let size: u8 = setting_value(change.value.clone())?;
                if !(8..=14).contains(&size) {
                    return Err(invalid_setting(
                        "Window label font size must be an integer between 8 and 14.",
                    ));
                }
                candidate.appearance.window_label_font_size = size;
            }
            Key::ShowClaudeCode => {
                candidate.show_claude_code = setting_value(change.value.clone())?
            }
            Key::ShowCodex => candidate.show_codex = setting_value(change.value.clone())?,
            Key::ShowAntigravity => {
                candidate.show_antigravity = setting_value(change.value.clone())?
            }
            Key::ShowOpenCode => candidate.show_opencode = setting_value(change.value.clone())?,
            Key::ShowCursor => candidate.show_cursor = setting_value(change.value.clone())?,
            Key::ShowGrok => candidate.show_grok = setting_value(change.value.clone())?,
            Key::ShowCopilot => candidate.show_copilot = setting_value(change.value.clone())?,
            Key::AllowUnlimitedProviders => {
                candidate.allow_unlimited_providers = setting_value(change.value.clone())?
            }
            Key::ShowSessionWindow => {
                candidate.show_session_window = setting_value(change.value.clone())?
            }
            Key::ShowWeeklyWindow => {
                candidate.show_weekly_window = setting_value(change.value.clone())?
            }
            Key::AdaptiveRefresh => {
                candidate.adaptive_refresh = setting_value(change.value.clone())?
            }
            Key::AnchorLeft => candidate.anchor_left = setting_value(change.value.clone())?,
            Key::PollIntervalMs => {
                let interval: u32 = setting_value(change.value.clone())?;
                if !(POLL_1_MIN..=POLL_1_HOUR).contains(&interval) {
                    return Err(invalid_setting(
                        "Polling interval must be between 60000 and 3600000 milliseconds.",
                    ));
                }
                candidate.poll_interval_ms = interval;
            }
            Key::AlertThresholdPercent => {
                let threshold: u8 = setting_value(change.value.clone())?;
                if !matches!(threshold, 0 | 10 | 20 | 30) {
                    return Err(invalid_setting("Alert threshold must be 0, 10, 20, or 30."));
                }
                candidate.alert_threshold_percent = threshold;
            }
            Key::Palette => candidate.appearance.palette = setting_value(change.value.clone())?,
            Key::BarStyle => candidate.appearance.bar_style = setting_value(change.value.clone())?,
            Key::ProviderSpacing => {
                candidate.appearance.provider_spacing = setting_value(change.value.clone())?
            }
            Key::ProviderOrder => {
                let order: Vec<ProviderId> = setting_value(change.value.clone())?;
                let order: [ProviderId; 7] = order.try_into().map_err(|_| {
                    invalid_setting("Provider order must contain all seven providers exactly once.")
                })?;
                if !valid_provider_order(&order) {
                    return Err(invalid_setting(
                        "Provider order must contain all seven providers exactly once.",
                    ));
                }
                candidate.appearance.provider_order = order;
            }
            Key::ProviderIconPosition => {
                candidate.appearance.provider_icon_position =
                    setting_value(change.value.clone())?
            }
            Key::ProviderMonochrome => {
                candidate.appearance.provider_monochrome = setting_value(change.value.clone())?
            }
            Key::BarThickness => {
                candidate.appearance.bar_thickness = setting_value(change.value.clone())?
            }
            Key::FontSize => candidate.appearance.font_size = setting_value(change.value.clone())?,
            Key::ClaudeAccent => {
                candidate.appearance.claude_accent = setting_value(change.value.clone())?
            }
            Key::CodexAccent => {
                candidate.appearance.codex_accent = setting_value(change.value.clone())?
            }
            Key::AntigravityAccent => {
                candidate.appearance.antigravity_accent = setting_value(change.value.clone())?
            }
            Key::OpencodeAccent => {
                candidate.appearance.opencode_accent = setting_value(change.value.clone())?
            }
            Key::CursorAccent => {
                candidate.appearance.cursor_accent = setting_value(change.value.clone())?
            }
            Key::GrokAccent => {
                candidate.appearance.grok_accent = setting_value(change.value.clone())?
            }
            Key::CopilotAccent => {
                candidate.appearance.copilot_accent = setting_value(change.value.clone())?
            }
            Key::ShowClaudeIcon => {
                candidate.appearance.show_claude_icon = setting_value(change.value.clone())?
            }
            Key::ShowCodexIcon => {
                candidate.appearance.show_codex_icon = setting_value(change.value.clone())?
            }
            Key::ShowAntigravityIcon => {
                candidate.appearance.show_antigravity_icon = setting_value(change.value.clone())?
            }
            Key::ShowOpencodeIcon => {
                candidate.appearance.show_opencode_icon = setting_value(change.value.clone())?
            }
            Key::ShowCursorIcon => {
                candidate.appearance.show_cursor_icon = setting_value(change.value.clone())?
            }
            Key::ShowGrokIcon => {
                candidate.appearance.show_grok_icon = setting_value(change.value.clone())?
            }
            Key::ShowCopilotIcon => {
                candidate.appearance.show_copilot_icon = setting_value(change.value.clone())?
            }
            Key::MonitorDevice => {
                let device: Option<String> = setting_value(change.value.clone())?;
                if let Some(device) = device {
                    if !monitor_devices.iter().any(|available| available == &device) {
                        return Err(invalid_setting(
                            "The selected monitor is no longer available.",
                        ));
                    }
                    candidate.monitor_device = Some(device);
                } else {
                    candidate.monitor_device = None;
                }
            }
        }
    }

    if !candidate.appearance.show_percentage
        && !candidate.appearance.show_bar
        && !candidate.appearance.show_reset_time
    {
        return Err(invalid_setting(
            "At least one usage element must remain visible.",
        ));
    }
    if !candidate.show_claude_code
        && !candidate.show_codex
        && !candidate.show_antigravity
        && !candidate.show_opencode
        && !candidate.show_cursor
        && !candidate.show_grok
        && !candidate.show_copilot
    {
        return Err(invalid_setting(
            "At least one provider must remain enabled.",
        ));
    }
    let enabled_providers = candidate.show_claude_code as u8
        + candidate.show_codex as u8
        + candidate.show_antigravity as u8
        + candidate.show_opencode as u8
        + candidate.show_cursor as u8
        + candidate.show_grok as u8
        + candidate.show_copilot as u8;
    if enabled_providers > 3 && !candidate.allow_unlimited_providers {
        return Err(invalid_setting(
            "At most 3 providers can stay enabled at once. Enable more on the Experimental page.",
        ));
    }
    if !candidate.show_session_window && !candidate.show_weekly_window {
        return Err(invalid_setting(
            "At least one usage window must remain enabled.",
        ));
    }
    for (provider, was_enabled, is_enabled, available) in [
        (
            "Claude Code",
            current.show_claude_code,
            candidate.show_claude_code,
            availability.claude_code_available,
        ),
        (
            "Codex",
            current.show_codex,
            candidate.show_codex,
            availability.codex_available,
        ),
        (
            "Antigravity",
            current.show_antigravity,
            candidate.show_antigravity,
            availability.antigravity_available,
        ),
        (
            "OpenCode",
            current.show_opencode,
            candidate.show_opencode,
            availability.opencode_available,
        ),
        (
            "Cursor",
            current.show_cursor,
            candidate.show_cursor,
            availability.cursor_available,
        ),
        (
            "Grok",
            current.show_grok,
            candidate.show_grok,
            availability.grok_available,
        ),
        (
            "GitHub Copilot",
            current.show_copilot,
            candidate.show_copilot,
            availability.copilot_available,
        ),
    ] {
        if is_enabled && !was_enabled && !available {
            let message = if provider == "Claude Code" {
                "Sign in to the Claude CLI before enabling Claude Code monitoring.".to_string()
            } else {
                format!(
                    "Local credentials for {provider} are unavailable. Sign in before enabling monitoring."
                )
            };
            return Err(invalid_setting(message));
        }
    }
    Ok(candidate)
}

#[cfg(test)]
fn provider_availability_for_tests() -> poller::ProviderAvailability {
    // Deterministic synthetic credentials; tests never consult the host.
    poller::ProviderAvailability {
        claude_code_available: true,
        codex_available: true,
        antigravity_available: true,
        opencode_available: true,
        cursor_available: true,
        grok_available: true,
        copilot_available: true,
    }
}

#[cfg(test)]
/// Use fixed synthetic non-Claude credentials in legacy bool-based tests.
fn validate_settings_changes(
    current: &SettingsCandidate,
    changes: &[settings_ui::Change],
    claude_code_available: bool,
    monitor_devices: &[String],
) -> Result<SettingsCandidate, SettingsApplyError> {
    validate_settings_changes_with_availability(
        current,
        changes,
        poller::ProviderAvailability {
            claude_code_available,
            ..provider_availability_for_tests()
        },
        monitor_devices,
    )
}

fn settings_batch_needs_widget_redraw(changes: &[settings_ui::Change]) -> bool {
    changes
        .iter()
        .any(|change| change.key != Key::SettingsPaneOpen)
}

fn apply_settings_batch(
    hwnd: HWND,
    changes: &[settings_ui::Change],
) -> Result<(), SettingsApplyError> {
    let redraw_widget = settings_batch_needs_widget_redraw(changes);
    let startup = is_startup_enabled();
    let (
        current,
        provider_availability,
        effective_monitor_device,
        pending_monitor_device,
        current_embedded,
        current_taskbar,
        legacy_selection_pending,
    ) = {
        let state = lock_state();
        let app_state = state
            .as_ref()
            .ok_or_else(|| SettingsApplyError::new("unavailable", "Widget is shutting down"))?;
        (
            SettingsCandidate::from_app_state(app_state, startup),
            effective_provider_availability(app_state),
            app_state.effective_monitor_device.clone(),
            app_state.pending_monitor_device.clone(),
            app_state.embedded,
            app_state.taskbar_hwnd,
            app_state.legacy_selection_pending,
        )
    };
    let monitor_requested = changes
        .iter()
        .any(|change| matches!(change.key, Key::MonitorDevice));
    let monitors = if monitor_requested {
        native_interop::find_monitors()
    } else {
        Vec::new()
    };
    let taskbars = if monitor_requested {
        native_interop::find_taskbars()
    } else {
        Vec::new()
    };
    let monitor_devices: Vec<String> = monitors
        .iter()
        .map(|monitor| monitor.device.clone())
        .collect();
    let candidate = validate_settings_changes_with_availability(
        &current,
        changes,
        provider_availability,
        &monitor_devices,
    )?;

    let startup_changed = candidate.startup != current.startup;
    let monitor_target = if monitor_requested {
        resolve_monitor_target(
            &monitors,
            &taskbars,
            candidate.monitor_device.as_deref(),
            None,
        )
    } else {
        None
    };
    if monitor_requested && monitor_target.is_none() {
        return Err(SettingsApplyError::new(
            "monitor_unavailable",
            "No active display is available for the requested placement.",
        ));
    }
    let monitor_changed = monitor_target.as_ref().is_some_and(|target| {
        candidate.monitor_device != current.monitor_device
            || pending_monitor_device
                .as_ref()
                .is_some_and(|pending| pending != &candidate.monitor_device)
            || legacy_selection_pending
            || effective_monitor_device.as_deref() != Some(target.monitor.device.as_str())
            || match target.taskbar {
                Some((_, taskbar)) => !current_embedded || current_taskbar != Some(taskbar.hwnd),
                None => current_embedded || current_taskbar.is_some(),
            }
    });
    let provider_changed = [
        candidate.show_claude_code != current.show_claude_code,
        candidate.show_codex != current.show_codex,
        candidate.show_antigravity != current.show_antigravity,
        candidate.show_opencode != current.show_opencode,
        candidate.show_cursor != current.show_cursor,
        candidate.show_grok != current.show_grok,
        candidate.show_copilot != current.show_copilot,
    ]
    .into_iter()
    .any(|changed| changed);
    let windows_changed = candidate.show_session_window != current.show_session_window
        || candidate.show_weekly_window != current.show_weekly_window;
    let language_changed = candidate.language_override != current.language_override;
    let appearance_changed = candidate.appearance != current.appearance;
    let position_changed = monitor_changed
        || candidate.anchor_left != current.anchor_left
        || provider_changed
        || language_changed
        || appearance_changed;
    let tooltip_changed = candidate.show_tray_icon != current.show_tray_icon
        || provider_changed
        || windows_changed
        || language_changed;

    let previous_monitor_target = if monitor_changed {
        effective_monitor_device
            .as_deref()
            .and_then(|device| resolve_monitor_target(&monitors, &taskbars, Some(device), None))
    } else {
        None
    };

    // Registry registration and taskbar attachment are the only OS-side
    // effects here. If a later step fails, restore any effect already applied.
    if startup_changed {
        set_startup_enabled(candidate.startup);
        if is_startup_enabled() != candidate.startup {
            let restored = restore_startup_registration(current.startup);
            let rollback_note = if restored {
                ""
            } else {
                " The previous startup registration could not be restored."
            };
            return Err(SettingsApplyError::new(
                "startup_failed",
                format!(
                    "Windows startup registration could not be changed. Check registry permissions.{rollback_note}"
                ),
            ));
        }
    }
    if monitor_changed {
        let target = monitor_target
            .as_ref()
            .expect("monitor target was resolved before native changes");
        if !transition_to_monitor(hwnd, target) {
            if monitor_requested {
                let retry_standalone_monitor_change =
                    changes.len() == 1 && matches!(changes[0].key, Key::MonitorDevice);
                if let Some(app_state) = lock_state().as_mut() {
                    app_state.pending_monitor_device =
                        retry_standalone_monitor_change.then(|| candidate.monitor_device.clone());
                }
                if retry_standalone_monitor_change {
                    unsafe {
                        let _ = SetTimer(hwnd, TIMER_TASKBAR_RETRY, 2_000, None);
                    }
                }
            }
            let startup_restored =
                !startup_changed || restore_startup_registration(current.startup);
            let rollback_note = if startup_restored {
                ""
            } else {
                " The previous startup registration could not be restored."
            };
            return Err(SettingsApplyError::new(
                "monitor_transition_failed",
                format!("Could not place the widget on the selected display.{rollback_note}"),
            ));
        }
    }

    let (reconfigure_poll, show_widget, language_title) = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            drop(state);
            let startup_restored =
                !startup_changed || restore_startup_registration(current.startup);
            let monitor_restored = !monitor_changed
                || previous_monitor_target
                    .clone()
                    .is_some_and(|target| transition_to_monitor(hwnd, &target));
            if !startup_restored || !monitor_restored {
                diagnose::log(format!(
                    "settings batch compensation incomplete (startup_restored={startup_restored}, monitor_restored={monitor_restored})"
                ));
            }
            let rollback_note = if startup_restored && monitor_restored {
                ""
            } else {
                "; one or more OS settings could not be restored"
            };
            return Err(SettingsApplyError::new(
                "unavailable",
                format!("Widget is shutting down{rollback_note}"),
            ));
        };
        app_state.widget_visible = candidate.widget_visible;
        app_state.settings_pane_open = candidate.settings_pane_open;
        app_state.show_tray_icon = candidate.show_tray_icon;
        app_state.show_provider_icons = candidate.show_provider_icons;
        app_state.show_claude_code = candidate.show_claude_code;
        app_state.show_codex = candidate.show_codex;
        app_state.show_antigravity = candidate.show_antigravity;
        app_state.show_opencode = candidate.show_opencode;
        app_state.show_cursor = candidate.show_cursor;
        app_state.show_grok = candidate.show_grok;
        app_state.show_copilot = candidate.show_copilot;
        app_state.allow_unlimited_providers = candidate.allow_unlimited_providers;
        app_state.show_session_window = candidate.show_session_window;
        app_state.show_weekly_window = candidate.show_weekly_window;
        app_state.poll_interval_ms = candidate.poll_interval_ms;
        app_state.adaptive_refresh = candidate.adaptive_refresh;
        app_state.alert_threshold_percent = candidate.alert_threshold_percent;
        app_state.appearance = candidate.appearance;
        app_state.monitor_device = candidate.monitor_device.clone();
        if monitor_requested {
            app_state.pending_monitor_device = None;
            app_state.legacy_selection_pending = false;
            if candidate.monitor_device.is_none() {
                app_state.taskbar_index = 0;
            }
        }
        app_state.anchor_left = candidate.anchor_left;
        if monitor_changed {
            app_state.tray_offset = 0;
        }
        if provider_changed {
            app_state.session_text = "...".to_string();
            app_state.weekly_text = "...".to_string();
            app_state.codex_session_text = "...".to_string();
            app_state.codex_weekly_text = "...".to_string();
            app_state.antigravity_session_text = "...".to_string();
            app_state.antigravity_weekly_text = "...".to_string();
            app_state.opencode_session_text = "...".to_string();
            app_state.opencode_weekly_text = "...".to_string();
            app_state.cursor_session_text = "...".to_string();
            app_state.cursor_weekly_text = "...".to_string();
            app_state.grok_session_text = "...".to_string();
            app_state.grok_weekly_text = "...".to_string();
            app_state.copilot_session_text = "...".to_string();
            app_state.copilot_weekly_text = "...".to_string();
        }
        if language_changed {
            apply_language_to_state(app_state, candidate.language_override);
        }
        let reconfigure_poll = (candidate.poll_interval_ms != current.poll_interval_ms
            || candidate.adaptive_refresh != current.adaptive_refresh)
            && app_state.has_poll_result;
        if reconfigure_poll {
            app_state.monitor.reconfigure(
                candidate.poll_interval_ms,
                candidate.adaptive_refresh,
                Instant::now(),
                SystemTime::now(),
            );
        }
        (
            reconfigure_poll,
            candidate.widget_visible,
            language_changed.then(|| app_state.language.strings()),
        )
    };

    if let Some(strings) = language_title {
        set_window_title(hwnd, strings);
    }
    if reconfigure_poll {
        let interval = lock_state()
            .as_ref()
            .map(effective_poll_interval)
            .unwrap_or(candidate.poll_interval_ms);
        unsafe {
            SetTimer(hwnd, TIMER_POLL, interval, None);
        }
    }
    if position_changed {
        position_at_taskbar_without_save();
    }
    if current.widget_visible != candidate.widget_visible {
        if show_widget {
            recover_widget_visibility(hwnd);
        } else {
            set_widget_visibility(hwnd, false);
        }
    } else if monitor_changed && show_widget {
        // Reparenting can leave a child window hidden on some shell/taskbar
        // transitions even though the user's visibility preference is on.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    if redraw_widget {
        render_layered();
    }
    if tooltip_changed {
        sync_tray_icons(hwnd);
    }
    if provider_changed {
        request_poll(SendHwnd::from_hwnd(hwnd), true);
    }

    // Persistence happens after the live batch is committed. A failed save is
    // reported to the caller, but must not roll back any applied setting or OS
    // effect; a later `persist` request snapshots this same effective state.
    persist_settings_request()
}

fn persist_settings_request() -> Result<(), SettingsApplyError> {
    persist_state_settings().map_err(|message| SettingsApplyError::new("save_failed", message))
}

/// Apply and persist only the navigation-pane preference. Its caller uses a
/// standalone setting request so draft edits in the native settings window are
/// neither applied nor accidentally included in this write.
fn apply_settings_pane_open(open: bool) -> Result<(), SettingsApplyError> {
    {
        let mut state = lock_state();
        let app_state = state
            .as_mut()
            .ok_or_else(|| SettingsApplyError::new("unavailable", "Widget is shutting down"))?;
        app_state.settings_pane_open = open;
    }

    let path = settings_path();
    let snapshot_error = std::cell::RefCell::new(None::<std::io::Error>);
    let saved = settings_store::save(&path, || {
        let mut settings = match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str::<SettingsFile>(&contents).unwrap_or_default(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SettingsFile::default()
            }
            Err(error) => {
                *snapshot_error.borrow_mut() = Some(error);
                return None;
            }
        };
        settings.settings_pane_open = open;
        Some(settings)
    });
    if let Some(error) = snapshot_error.into_inner() {
        return Err(SettingsApplyError::new(
            "save_failed",
            format!("The navigation pane preference could not be saved: {error}"),
        ));
    }
    saved.map_err(|error| {
        SettingsApplyError::new(
            "save_failed",
            format!("The navigation pane preference could not be saved: {error}"),
        )
    })
}

fn apply_settings_action(hwnd: HWND, action: Action) -> Result<(), String> {
    let command = match action {
        Action::Refresh => 1,
        Action::Update => IDM_VERSION_ACTION,
        Action::Uninstall => IDM_UNINSTALL,
        Action::Recommended => IDM_APPEARANCE_RECOMMENDED,
        Action::ResetAppearance => IDM_APPEARANCE_RESET,
        Action::ResetColors => IDM_COLOR_RESET,
        Action::ResetPosition => IDM_RESET_POSITION,
        Action::Exit => {
            unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return Ok(());
        }
        Action::Repository => {
            let repository = native_interop::wide_str(env!("CARGO_PKG_REPOSITORY"));
            let result = unsafe {
                ShellExecuteW(
                    hwnd,
                    PCWSTR::null(),
                    PCWSTR::from_raw(repository.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                )
            };
            return if result.0 as isize > 32 {
                Ok(())
            } else {
                Err("Could not open the project repository in the default browser.".into())
            };
        }
    };
    unsafe {
        handle_command(hwnd, command);
    }
    Ok(())
}

fn paint(hdc: HDC, hwnd: HWND) {
    let (
        is_dark,
        appearance,
        language,
        strings,
        session_pct,
        session_text,
        weekly_pct,
        weekly_text,
        codex_session_pct,
        codex_session_text,
        codex_weekly_pct,
        codex_weekly_text,
        antigravity_session_pct,
        antigravity_session_text,
        antigravity_weekly_pct,
        antigravity_weekly_text,
        opencode_session_pct,
        opencode_session_text,
        opencode_weekly_pct,
        opencode_weekly_text,
        cursor_session_pct,
        cursor_session_text,
        cursor_weekly_pct,
        cursor_weekly_text,
        grok_session_pct,
        grok_session_text,
        grok_weekly_pct,
        grok_weekly_text,
        copilot_session_pct,
        copilot_session_text,
        copilot_weekly_pct,
        copilot_weekly_text,
        show_claude_code,
        show_codex,
        show_antigravity,
        show_opencode,
        show_cursor,
        show_grok,
        show_copilot,
        show_provider_icons,
        show_session_window,
        show_weekly_window,
    ) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => {
                let display: [ProviderDisplay; 7] =
                    std::array::from_fn(|index| provider_display(s, ProviderId::ALL[index]));
                (
                    s.is_dark,
                    s.appearance,
                    s.language,
                    s.language.strings(),
                    display[0].session_percent,
                    display[0].session_text.clone(),
                    display[0].weekly_percent,
                    display[0].weekly_text.clone(),
                    display[1].session_percent,
                    display[1].session_text.clone(),
                    display[1].weekly_percent,
                    display[1].weekly_text.clone(),
                    display[2].session_percent,
                    display[2].session_text.clone(),
                    display[2].weekly_percent,
                    display[2].weekly_text.clone(),
                    display[3].session_percent,
                    display[3].session_text.clone(),
                    display[3].weekly_percent,
                    display[3].weekly_text.clone(),
                    display[4].session_percent,
                    display[4].session_text.clone(),
                    display[4].weekly_percent,
                    display[4].weekly_text.clone(),
                    display[5].session_percent,
                    display[5].session_text.clone(),
                    display[5].weekly_percent,
                    display[5].weekly_text.clone(),
                    display[6].session_percent,
                    display[6].session_text.clone(),
                    display[6].weekly_percent,
                    display[6].weekly_text.clone(),
                    s.show_claude_code,
                    s.show_codex,
                    s.show_antigravity,
                    s.show_opencode,
                    s.show_cursor,
                    s.show_grok,
                    s.show_copilot,
                    s.show_provider_icons,
                    s.show_session_window,
                    s.show_weekly_window,
                )
            }
            None => return,
        }
    };

    let accent = custom_color(appearance.claude_accent, claude_accent_color());
    let (bg_color, text_color, track) = appearance_colors(appearance, is_dark);
    let codex_accent = custom_color(
        appearance.codex_accent,
        provider_icons::codex_color(appearance.is_dark(is_dark)),
    );
    let antigravity_accent = antigravity_accent_color();
    let opencode_accent = custom_color(appearance.opencode_accent, opencode_accent_color());
    let cursor_accent = custom_color(appearance.cursor_accent, cursor_accent_color());
    let grok_accent = custom_color(appearance.grok_accent, grok_accent_color());
    let copilot_accent = custom_color(appearance.copilot_accent, copilot_accent_color());

    unsafe {
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;

        if width <= 0 || height <= 0 {
            return;
        }

        let mem_dc = CreateCompatibleDC(hdc);
        let mem_bmp = CreateCompatibleBitmap(hdc, width, height);
        let old_bmp = SelectObject(mem_dc, mem_bmp);

        paint_content(
            mem_dc,
            width,
            height,
            is_dark,
            appearance,
            &bg_color,
            &text_color,
            &accent,
            &track,
            language,
            strings,
            session_pct,
            &session_text,
            weekly_pct,
            &weekly_text,
            codex_session_pct,
            &codex_session_text,
            codex_weekly_pct,
            &codex_weekly_text,
            antigravity_session_pct,
            &antigravity_session_text,
            antigravity_weekly_pct,
            &antigravity_weekly_text,
            opencode_session_pct,
            &opencode_session_text,
            opencode_weekly_pct,
            &opencode_weekly_text,
            cursor_session_pct,
            &cursor_session_text,
            cursor_weekly_pct,
            &cursor_weekly_text,
            grok_session_pct,
            &grok_session_text,
            grok_weekly_pct,
            &grok_weekly_text,
            copilot_session_pct,
            &copilot_session_text,
            copilot_weekly_pct,
            &copilot_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            show_opencode,
            show_cursor,
            show_grok,
            show_copilot,
            show_provider_icons,
            show_session_window,
            show_weekly_window,
            &codex_accent,
            &antigravity_accent,
            &opencode_accent,
            &cursor_accent,
            &grok_accent,
            &copilot_accent,
        );

        let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(mem_bmp);
        let _ = DeleteDC(mem_dc);
    }
}

fn draw_row(
    hdc: HDC,
    x: i32,
    y: i32,
    is_dark: bool,
    appearance: Appearance,
    text_color: &Color,
    label: &str,
    weekly: bool,
    providers: &[WidgetProvider<'_>; 7],
    provider_order: [ProviderId; 7],
    track: &Color,
    label_width: i32,
    provider_text_widths: [i32; 7],
) {
    let active_models = providers.iter().filter(|provider| provider.enabled).count() as i32;
    let segment_count = row_bar_segment_count(active_models);
    let value_color = custom_color(appearance.usage_text_color, *text_color);

    unsafe {
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));
        let label_font =
            quota_text::create_reset_font(sc(i32::from(appearance.window_label_font_size)));
        let previous_font = SelectObject(hdc, label_font);
        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut label_rect = RECT {
            left: x,
            top: y,
            right: x + label_width,
            bottom: y + appearance.row_height(),
        };
        let _ = DrawTextW(
            hdc,
            &mut label_wide,
            &mut label_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(hdc, previous_font);
        let _ = DeleteObject(label_font);

        let mut model_x = x + label_width + sc(LABEL_RIGHT_MARGIN);
        for provider_id in provider_order {
            let index = provider_id.index();
            let provider = providers[index];
            if !provider.enabled {
                continue;
            }
            let (percent, text) = if weekly {
                (provider.weekly_percent, provider.weekly_text)
            } else {
                (provider.session_percent, provider.session_text)
            };
            draw_usage_bar(
                hdc,
                model_x + provider_icon_width(appearance),
                y,
                segment_count,
                percent,
                text,
                provider.accent,
                track,
                &value_color,
                provider_text_widths[index],
                is_dark,
                appearance,
            );
            model_x += model_usage_width(segment_count, provider_text_widths[index], appearance)
                + appearance.provider_spacing.scaled_gap();
        }
    }
}

fn provider_icon_width(appearance: Appearance) -> i32 {
    if appearance.show_provider_icon {
        sc(provider_icons::SIZE + provider_icons::RIGHT_MARGIN)
    } else {
        0
    }
}

fn model_usage_width(segment_count: i32, text_width: i32, appearance: Appearance) -> i32 {
    provider_icon_width(appearance)
        + if appearance.show_bar {
            (sc(SEGMENT_W) + sc(SEGMENT_GAP)) * segment_count - sc(SEGMENT_GAP)
                + if text_width > 0 {
                    sc(BAR_RIGHT_MARGIN)
                } else {
                    0
                }
        } else {
            0
        }
        + text_width
}

/// Keep the next provider close to the text that is actually visible, while
/// measuring reset strings with the same font used by the quota renderer.
/// The widget still reserves the translated worst-case width in its overall
/// size, so a later long reset value can never be clipped by the window edge.
fn usage_value_width(hdc: HDC, value: &str, appearance: Appearance) -> i32 {
    let percent_width = if appearance.show_percentage {
        sc(quota_text::percent_width(appearance.font_px()))
    } else {
        0
    };
    if !appearance.show_reset_time {
        return percent_width;
    }
    let (_, reset) = quota_text::parts(value);
    if reset.is_empty() {
        return percent_width;
    }

    unsafe {
        let font = quota_text::create_reset_font(sc(appearance.font_px()));
        if font.is_invalid() {
            return percent_width + sc(3);
        }
        let old_font = SelectObject(hdc, font);
        let wide: Vec<u16> = reset.encode_utf16().collect();
        let mut size = SIZE::default();
        let measured = if GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool() {
            size.cx
        } else {
            0
        };
        SelectObject(hdc, old_font);
        let _ = DeleteObject(font);
        (if appearance.show_percentage {
            sc(quota_text::reset_offset(appearance.font_px()))
        } else {
            0
        }) + measured
            + sc(3)
    }
}

fn usage_bar_fill_percentage(percent: f64, text: &str) -> f64 {
    if text == "--" {
        0.0
    } else {
        percent.clamp(0.0, 100.0)
    }
}

fn draw_usage_bar(
    hdc: HDC,
    bar_x: i32,
    y: i32,
    segment_count: i32,
    percent: f64,
    text: &str,
    accent: &Color,
    track: &Color,
    text_color: &Color,
    text_width: i32,
    is_dark: bool,
    appearance: Appearance,
) {
    let seg_w = sc(SEGMENT_W);
    let seg_h = appearance.bar_height();
    let seg_gap = sc(SEGMENT_GAP);
    let bar_width = segment_count * (seg_w + seg_gap) - seg_gap;
    let corner_r = seg_h / 2;
    let bar_y = y + (appearance.row_height() - seg_h) / 2;

    let percent_clamped = usage_bar_fill_percentage(percent, text);
    if !appearance.show_bar {
        // Hidden bars reserve no width and perform no GDI drawing.
    } else if appearance.bar_style == BarStyle::Segmented {
        for i in 0..segment_count {
            let rect = RECT {
                left: bar_x + i * (seg_w + seg_gap),
                top: bar_y,
                right: bar_x + i * (seg_w + seg_gap) + seg_w,
                bottom: bar_y + seg_h,
            };
            draw_rounded_rect(hdc, &rect, track, corner_r);
            let fraction =
                (percent_clamped * segment_count as f64 / 100.0 - i as f64).clamp(0.0, 1.0);
            fill_rounded_bar(hdc, &rect, fraction, accent, corner_r);
        }
    } else {
        let rect = RECT {
            left: bar_x,
            top: bar_y,
            right: bar_x + bar_width,
            bottom: bar_y + seg_h,
        };
        draw_rounded_rect(hdc, &rect, track, corner_r);
        fill_rounded_bar(hdc, &rect, percent_clamped / 100.0, accent, corner_r);
    }

    let text_x = bar_x
        + if appearance.show_bar {
            bar_width + sc(BAR_RIGHT_MARGIN)
        } else {
            0
        };
    quota_text::draw(
        hdc,
        text_x,
        y,
        appearance.row_height(),
        text_width,
        text,
        text_color,
        &reset_text_color(appearance, is_dark),
        appearance.font_px(),
        appearance.show_percentage,
        appearance.show_reset_time,
        sc,
    );
}

fn fill_rounded_bar(hdc: HDC, rect: &RECT, fraction: f64, color: &Color, radius: i32) {
    draw_rounded_shape(hdc, rect, color, radius, fraction);
}

fn draw_rounded_rect(hdc: HDC, rect: &RECT, color: &Color, radius: i32) {
    draw_rounded_shape(hdc, rect, color, radius, 1.0);
}

/// Supersample small bar shapes so their rounded ends stay smooth at taskbar scale.
fn draw_rounded_shape(hdc: HDC, rect: &RECT, color: &Color, radius: i32, fraction: f64) {
    const SCALE: i32 = 4;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return;
    }
    let scaled_width = width * SCALE;
    let scaled_height = height * SCALE;
    let scaled_fill_width = (scaled_width as f64 * fraction.clamp(0.0, 1.0)).round() as i32;
    if scaled_fill_width <= 0 {
        return;
    }
    unsafe {
        let memory_dc = CreateCompatibleDC(hdc);
        if memory_dc.is_invalid() {
            return;
        }
        let bitmap = CreateCompatibleBitmap(hdc, width * SCALE, height * SCALE);
        if bitmap.is_invalid() {
            let _ = DeleteDC(memory_dc);
            return;
        }
        let old_bitmap = SelectObject(memory_dc, bitmap);
        let _ = SetStretchBltMode(memory_dc, HALFTONE);
        let _ = StretchBlt(
            memory_dc,
            0,
            0,
            scaled_width,
            scaled_height,
            hdc,
            rect.left,
            rect.top,
            width,
            height,
            SRCCOPY,
        );
        let cap_diameter = (radius * 2 * SCALE)
            .min(scaled_fill_width)
            .min(scaled_height);
        let rgn = CreateRoundRectRgn(
            0,
            0,
            scaled_fill_width + 1,
            scaled_height + 1,
            cap_diameter,
            cap_diameter,
        );
        let brush = CreateSolidBrush(COLORREF(color.to_colorref()));
        let _ = FillRgn(memory_dc, rgn, brush);
        let _ = SetStretchBltMode(hdc, HALFTONE);
        let _ = StretchBlt(
            hdc,
            rect.left,
            rect.top,
            width,
            height,
            memory_dc,
            0,
            0,
            scaled_width,
            scaled_height,
            SRCCOPY,
        );
        let _ = DeleteObject(rgn);
        let _ = DeleteObject(brush);
        SelectObject(memory_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(memory_dc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_show_raises_only_the_tooltip_not_its_widget_owner() {
        use windows::core::{w, PWSTR};
        use windows::Win32::UI::Controls::*;
        static SAW_SHOW: AtomicBool = AtomicBool::new(false);
        static POSITION_CHANGES: AtomicU32 = AtomicU32::new(0);
        unsafe extern "system" fn test_proc(
            hwnd: HWND,
            msg: u32,
            wp: WPARAM,
            lp: LPARAM,
        ) -> LRESULT {
            if msg == WM_WINDOWPOSCHANGING {
                POSITION_CHANGES.fetch_add(1, Ordering::Relaxed);
            }
            if msg == WM_NOTIFY && lp.0 != 0 && (*(lp.0 as *const NMHDR)).code == TTN_SHOW {
                SAW_SHOW.store(true, Ordering::Relaxed);
                return wnd_proc(hwnd, msg, wp, lp);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        unsafe {
            let controls = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_WIN95_CLASSES,
            };
            InitCommonControlsEx(&controls).ok().unwrap();
            let instance = GetModuleHandleW(None).unwrap().into();
            let class = w!("CodexTooltipShowTest");
            assert_ne!(
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(test_proc),
                    hInstance: instance,
                    lpszClassName: class,
                    ..Default::default()
                }),
                0
            );
            let host = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("tooltip-host-test"),
                WS_POPUP,
                0,
                0,
                100,
                30,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let owner = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class,
                w!("tooltip-owner-test"),
                WS_CHILD,
                0,
                0,
                100,
                30,
                host,
                None,
                instance,
                None,
            )
            .unwrap();
            // A hidden parent must not make every retry show its child again.
            set_widget_visibility(owner, true);
            assert!(!IsWindowVisible(owner).as_bool());
            assert_ne!(GetWindowLongW(owner, GWL_STYLE) as u32 & WS_VISIBLE.0, 0);
            let changes = POSITION_CHANGES.load(Ordering::Relaxed);
            for _ in 0..5 {
                set_widget_visibility(owner, true);
            }
            assert_eq!(POSITION_CHANGES.load(Ordering::Relaxed), changes);
            set_widget_visibility(owner, false);
            let tooltip = CreateWindowExW(
                WS_EX_NOACTIVATE,
                w!("tooltips_class32"),
                w!(""),
                WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                0,
                0,
                100,
                30,
                owner,
                None,
                None,
                None,
            )
            .unwrap();
            let mut text: Vec<u16> = "quota test".encode_utf16().chain(Some(0)).collect();
            let info = TTTOOLINFOW {
                cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd: owner,
                uId: 1,
                lpszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            assert_ne!(
                SendMessageW(
                    tooltip,
                    TTM_ADDTOOLW,
                    WPARAM(0),
                    LPARAM(&info as *const _ as isize)
                )
                .0,
                0
            );
            SendMessageW(
                tooltip,
                TTM_TRACKACTIVATE,
                WPARAM(1),
                LPARAM(&info as *const _ as isize),
            );
            assert!(
                SAW_SHOW.load(Ordering::Relaxed),
                "the child tool must receive TTN_SHOW"
            );
            assert_ne!(
                GetWindowLongW(tooltip, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                0
            );
            assert_eq!(
                GetWindowLongW(owner, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                0
            );
            assert_eq!(
                GetWindowLongW(host, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                0
            );
            let _ = DestroyWindow(tooltip);
            let _ = DestroyWindow(owner);
            let _ = DestroyWindow(host);
            UnregisterClassW(class, instance).unwrap();
        }
    }

    #[test]
    fn unchanged_child_placement_requires_matching_position_and_size() {
        let rect = RECT {
            left: -40,
            top: 8,
            right: 160,
            bottom: 38,
        };
        let position = native_interop::WindowPosition {
            x: -40,
            y: 8,
            width: 200,
            height: 30,
        };
        assert!(placement_matches(rect, position));
        for changed in [
            native_interop::WindowPosition { x: -39, ..position },
            native_interop::WindowPosition { y: 9, ..position },
            native_interop::WindowPosition {
                width: 201,
                ..position
            },
            native_interop::WindowPosition {
                height: 31,
                ..position
            },
        ] {
            assert!(!placement_matches(rect, changed));
        }
    }

    #[test]
    fn widget_elements_default_and_legacy_settings_round_trip() {
        let legacy: SettingsFile = serde_json::from_str("{}").unwrap();
        let a = legacy.appearance;
        assert!(a.show_provider_icon && a.show_percentage && a.show_bar && a.show_reset_time);
        assert_eq!(a.usage_text_color, None);
        assert_eq!(a.window_label_font_size, 9);
        assert_eq!(a.provider_spacing, ProviderSpacing::Normal);
        assert_eq!(a.provider_order, default_provider_order());
        assert!(a.provider_monochrome);
        let mut previous_settings = serde_json::to_value(SettingsFile::default()).unwrap();
        let previous_appearance = previous_settings["appearance"].as_object_mut().unwrap();
        previous_appearance.remove("provider_spacing");
        previous_appearance.remove("provider_order");
        previous_appearance.remove("provider_monochrome");
        let previous: SettingsFile = serde_json::from_value(previous_settings).unwrap();
        assert_eq!(previous.appearance.provider_spacing, ProviderSpacing::Small);
        assert_eq!(previous.appearance.provider_order, legacy_default_provider_order());
        assert!(!previous.appearance.provider_monochrome);
        let settings = SettingsFile {
            appearance: Appearance {
                show_provider_icon: false,
                show_percentage: false,
                show_bar: false,
                usage_text_color: Some([12, 34, 56]),
                window_label_font_size: 14,
                ..Appearance::default()
            },
            ..SettingsFile::default()
        };
        let loaded: SettingsFile =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(loaded.appearance, settings.appearance);
        assert_eq!(
            serde_json::to_value(settings).unwrap()["appearance"]["provider_spacing"],
            "normal"
        );
        let old_icons =
            normalize_settings(serde_json::from_str(r#"{"show_provider_icons":false}"#).unwrap());
        assert!(!old_icons.appearance.show_provider_icon);
    }

    #[test]
    fn widget_elements_normalize_invalid_saved_settings() {
        let normalized = normalize_settings(SettingsFile {
            appearance: Appearance {
                show_percentage: false,
                show_bar: false,
                show_reset_time: false,
                window_label_font_size: 255,
                ..Appearance::default()
            },
            ..SettingsFile::default()
        });
        assert!(normalized.appearance.show_percentage);
        assert!(!normalized.appearance.show_bar && !normalized.appearance.show_reset_time);
        assert_eq!(normalized.appearance.window_label_font_size, 9);
    }

    #[test]
    fn normalize_keeps_one_provider_of_seven() {
        let all_off = SettingsFile {
            show_claude_code: false,
            show_codex: false,
            show_antigravity: false,
            show_opencode: false,
            show_cursor: false,
            show_grok: false,
            show_copilot: false,
            ..SettingsFile::default()
        };
        let normalized = normalize_settings(all_off);
        assert!(normalized.show_codex);
        assert!(
            !normalized.show_claude_code
                && !normalized.show_antigravity
                && !normalized.show_opencode
                && !normalized.show_cursor
                && !normalized.show_grok
                && !normalized.show_copilot
        );
        let one_on = SettingsFile {
            show_claude_code: false,
            show_codex: false,
            show_antigravity: false,
            show_opencode: false,
            show_cursor: false,
            show_grok: true,
            show_copilot: false,
            ..SettingsFile::default()
        };
        let kept = normalize_settings(one_on);
        assert!(kept.show_grok && !kept.show_codex);
    }

    #[test]
    fn provider_cap_rejects_fourth_provider_unless_lifted() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        assert!(current.show_codex);
        let enable_more = [Key::ShowOpenCode, Key::ShowCursor, Key::ShowGrok].map(|key| {
            settings_ui::Change {
                key,
                value: json!(true),
            }
        });
        let availability = provider_availability_for_tests();
        assert!(validate_settings_changes_with_availability(
            &current,
            &enable_more,
            availability,
            &[]
        )
        .is_err());
        let lifted = validate_settings_changes_with_availability(
            &current,
            &[settings_ui::Change {
                key: Key::AllowUnlimitedProviders,
                value: json!(true),
            }],
            availability,
            &[],
        )
        .unwrap();
        assert!(lifted.allow_unlimited_providers);
        let candidate = validate_settings_changes_with_availability(
            &lifted,
            &enable_more,
            availability,
            &[],
        )
        .unwrap();
        assert!(
            candidate.show_codex
                && candidate.show_opencode
                && candidate.show_cursor
                && candidate.show_grok
        );
    }

    #[test]
    fn widget_element_validation_uses_final_candidate_and_rejects_last_element() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(
            &SettingsFile {
                appearance: Appearance {
                    show_bar: false,
                    show_reset_time: false,
                    ..Appearance::default()
                },
                ..SettingsFile::default()
            },
            false,
        );
        let off = settings_ui::Change {
            key: Key::ShowPercentage,
            value: json!(false),
        };
        let on = settings_ui::Change {
            key: Key::ShowBar,
            value: json!(true),
        };
        assert!(
            validate_settings_changes(&current, std::slice::from_ref(&off), false, &[]).is_err()
        );
        for changes in [[off.clone(), on.clone()], [on.clone(), off.clone()]] {
            let candidate = validate_settings_changes(&current, &changes, false, &[]).unwrap();
            assert!(!candidate.appearance.show_percentage && candidate.appearance.show_bar);
        }
        let default = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let changes = [Key::ShowPercentage, Key::ShowBar, Key::ShowResetTime].map(|key| {
            settings_ui::Change {
                key,
                value: json!(false),
            }
        });
        assert!(validate_settings_changes(&default, &changes, false, &[]).is_err());
        assert!(current.appearance.show_percentage && !current.appearance.show_bar);
    }

    #[test]
    fn widget_colors_font_and_switches_are_strictly_typed() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        for (key, value) in [
            (Key::UsageTextColor, json!([256, 0, 0])),
            (Key::UsageTextColor, json!([-1, 0, 0])),
            (Key::UsageTextColor, json!([0, 0])),
            (Key::UsageTextColor, json!("#ffffff")),
            (Key::WindowLabelFontSize, json!(7)),
            (Key::WindowLabelFontSize, json!(15)),
            (Key::WindowLabelFontSize, json!(9.5)),
            (Key::WindowLabelFontSize, json!("9")),
            (Key::ShowProviderIcon, json!(0)),
            (Key::ShowPercentage, json!("false")),
            (Key::ShowBar, json!(null)),
            (Key::ShowResetTime, json!(1)),
        ] {
            assert!(validate_settings_changes(
                &current,
                &[settings_ui::Change { key, value }],
                false,
                &[]
            )
            .is_err());
        }
        for size in 8..=14 {
            let candidate = validate_settings_changes(
                &current,
                &[settings_ui::Change {
                    key: Key::WindowLabelFontSize,
                    value: json!(size),
                }],
                false,
                &[],
            )
            .unwrap();
            assert_eq!(candidate.appearance.window_label_font_size, size);
        }
        let candidate = validate_settings_changes(
            &current,
            &[settings_ui::Change {
                key: Key::UsageTextColor,
                value: json!([0, 128, 255]),
            }],
            false,
            &[],
        )
        .unwrap();
        assert_eq!(candidate.appearance.usage_text_color, Some([0, 128, 255]));
        assert_eq!(
            candidate.appearance.codex_accent,
            current.appearance.codex_accent
        );
        let cleared = validate_settings_changes(
            &candidate,
            &[settings_ui::Change {
                key: Key::UsageTextColor,
                value: json!(null),
            }],
            false,
            &[],
        )
        .unwrap();
        assert_eq!(cleared.appearance.usage_text_color, None);
    }

    #[test]
    fn provider_spacing_presets_are_strict_and_adjust_model_geometry() {
        use serde_json::json;

        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let presets = [
            (ProviderSpacing::None, "none", 2),
            (ProviderSpacing::Small, "small", 4),
            (ProviderSpacing::Normal, "normal", 7),
            (ProviderSpacing::Large, "large", 11),
        ];
        for (preset, wire_value, pixels) in presets {
            let candidate = validate_settings_changes(
                &current,
                &[settings_ui::Change {
                    key: Key::ProviderSpacing,
                    value: json!(wire_value),
                }],
                false,
                &[],
            )
            .unwrap();
            assert_eq!(candidate.appearance.provider_spacing, preset);
            assert_eq!(
                serde_json::to_value(preset).unwrap(),
                json!(wire_value)
            );
            assert_eq!(preset.scaled_gap(), sc(pixels));
        }
        for value in [json!("extra_small"), json!(null), json!(1)] {
            assert!(validate_settings_changes(
                &current,
                &[settings_ui::Change {
                    key: Key::ProviderSpacing,
                    value,
                }],
                false,
                &[],
            )
            .is_err());
        }

        let small = Appearance::default();
        let none = Appearance {
            provider_spacing: ProviderSpacing::None,
            ..small
        };
        let large = Appearance {
            provider_spacing: ProviderSpacing::Large,
            ..small
        };
        let small_width = total_widget_width_for(2, LanguageId::English, small);
        assert_eq!(
            total_widget_width_for(2, LanguageId::English, none),
            small_width - sc(2)
        );
        assert_eq!(
            total_widget_width_for(2, LanguageId::English, large),
            small_width + sc(7)
        );
    }

    #[test]
    fn provider_spacing_draft_preview_changes_geometry_without_changing_settings() {
        use serde_json::json;

        let current = SettingsCandidate::from_settings(
            &SettingsFile {
                show_opencode: true,
                ..SettingsFile::default()
            },
            false,
        );
        let original = current.clone();
        let snapshot = json!({
            "settings": {"provider_spacing": "small"},
            "monitors": [],
            "effective_language": "en",
            "effective_appearance": {"dark": true}
        });
        let draft = draft_settings_snapshot(
            snapshot.clone(),
            &current,
            &[settings_ui::Change {
                key: Key::ProviderSpacing,
                value: json!("large"),
            }],
            provider_availability_for_tests(),
            &[],
            LanguageId::English,
            true,
        )
        .unwrap();
        let large = Appearance {
            provider_spacing: ProviderSpacing::Large,
            ..current.appearance
        };
        assert_eq!(draft["settings"]["provider_spacing"], "small");
        assert_eq!(
            draft["widget_preview_width"],
            total_widget_width_for(2, LanguageId::English, large)
        );
        assert_eq!(current, original);
        assert_eq!(snapshot["settings"]["provider_spacing"], "small");
    }

    #[test]
    fn hidden_widget_columns_release_their_width() {
        let a = Appearance::default();
        let width = total_widget_width_for(1, LanguageId::English, a);
        for hidden in [
            Appearance {
                show_provider_icon: false,
                ..a
            },
            Appearance {
                show_bar: false,
                ..a
            },
            Appearance {
                show_percentage: false,
                ..a
            },
            Appearance {
                show_reset_time: false,
                ..a
            },
        ] {
            assert!(total_widget_width_for(1, LanguageId::English, hidden) < width);
        }
        let bar_only = Appearance {
            show_provider_icon: false,
            show_percentage: false,
            show_reset_time: false,
            ..a
        };
        assert_eq!(usage_layout_widths(LanguageId::English, bar_only).1, 0);
        assert_eq!(
            model_usage_width(10, 0, bar_only),
            (sc(SEGMENT_W) + sc(SEGMENT_GAP)) * 10 - sc(SEGMENT_GAP)
        );
    }

    #[test]
    fn recommended_and_reset_appearance_restore_widget_defaults() {
        for a in [
            Appearance::default(),
            Appearance::translucent_dark_taskbar(),
        ] {
            assert!(a.show_provider_icon && a.show_percentage && a.show_bar && a.show_reset_time);
            assert_eq!(a.window_label_font_size, 9);
            assert_eq!(a.usage_text_color, None);
            assert_eq!(a.provider_spacing, ProviderSpacing::Normal);
            assert_eq!(a.provider_order, default_provider_order());
            assert!(a.provider_monochrome);
        }
    }

    #[test]
    fn draft_preview_renders_without_mutating_canonical_snapshot_or_candidate() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let original = current.clone();
        let canonical = json!({"settings":{"show_bar":true},"monitors":[],"effective_language":"en","effective_appearance":{"dark":true}});
        let original_snapshot = canonical.clone();
        let draft = draft_settings_snapshot(
            canonical.clone(),
            &current,
            &[settings_ui::Change {
                key: Key::ShowBar,
                value: json!(false),
            }],
            provider_availability_for_tests(),
            &[],
            LanguageId::English,
            true,
        )
        .unwrap();
        assert_eq!(draft["settings"], canonical["settings"]);
        assert_eq!(draft["effective_language"], canonical["effective_language"]);
        assert_eq!(
            draft["effective_appearance"],
            canonical["effective_appearance"]
        );
        assert_eq!(
            draft["widget_preview_dpi"],
            CURRENT_DPI.load(Ordering::Relaxed)
        );
        assert!(draft["widget_preview_image"]
            .as_str()
            .unwrap()
            .starts_with("data:image/bmp;base64,Qk"));
        assert_eq!(draft["widget_preview_height"], sc(WIDGET_HEIGHT));
        assert!(
            draft["widget_preview_width"].as_i64().unwrap()
                < i64::from(total_widget_width_for(
                    active_model_count(
                        current.show_claude_code,
                        current.show_codex,
                        current.show_antigravity,
                        current.show_opencode,
                        current.show_cursor,
                        current.show_grok,
                        current.show_copilot,
                    ),
                    LanguageId::English,
                    current.appearance
                ))
        );
        assert_eq!(current, original);
        assert_eq!(canonical, original_snapshot);
        let invalid = [Key::ShowBar, Key::ShowPercentage, Key::ShowResetTime].map(|key| {
            settings_ui::Change {
                key,
                value: json!(false),
            }
        });
        assert!(draft_settings_snapshot(
            canonical.clone(),
            &current,
            &invalid,
            provider_availability_for_tests(),
            &[],
            LanguageId::English,
            true
        )
        .is_err());
        assert_eq!(canonical, original_snapshot);
    }

    #[test]
    fn preview_bmp_is_top_down_bgrx_and_base64_padding_is_standard() {
        for (bytes, expected) in [
            (b"".as_slice(), ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
        ] {
            assert_eq!(base64_encode(bytes), expected);
        }
        let bmp = encode_preview_bmp(1, 1, &[1, 2, 3, 255]);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 58);
        assert_eq!(i32::from_le_bytes(bmp[22..26].try_into().unwrap()), -1);
        assert_eq!(&bmp[54..], &[1, 2, 3, 0]);
    }

    #[test]
    fn real_widget_preview_round_trips_through_the_windows_bitmap_loader() {
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let (image, width, height) =
            widget_preview_image(&current, LanguageId::English, true).unwrap();
        let encoded = image.strip_prefix("data:image/bmp;base64,").unwrap();
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bytes = Vec::new();
        for group in encoded.as_bytes().chunks_exact(4) {
            let mut bits = 0u32;
            for glyph in group {
                let digit = if *glyph == b'=' {
                    0
                } else {
                    alphabet.iter().position(|value| value == glyph).unwrap() as u32
                };
                bits = (bits << 6) | digit;
            }
            bytes.push((bits >> 16) as u8);
            if group[2] != b'=' {
                bytes.push((bits >> 8) as u8);
            }
            if group[3] != b'=' {
                bytes.push(bits as u8);
            }
        }
        assert_eq!(bytes.len(), 54 + (width * height * 4) as usize);
        assert_eq!(height, sc(WIDGET_HEIGHT));
        let path = std::env::temp_dir().join(format!(
            "codex-widget-preview-{}-{}.bmp",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, &bytes).unwrap();
        let wide_path = native_interop::wide_str(path.to_str().unwrap());
        let loaded = unsafe {
            LoadImageW(
                None,
                PCWSTR(wide_path.as_ptr()),
                IMAGE_BITMAP,
                0,
                0,
                LR_LOADFROMFILE | LR_CREATEDIBSECTION,
            )
        };
        std::fs::remove_file(&path).unwrap();
        let loaded =
            loaded.expect("the Windows bitmap decoder must accept the actual renderer output");
        let mut bitmap = BITMAP::default();
        let size = unsafe {
            GetObjectW(
                HGDIOBJ(loaded.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some((&mut bitmap as *mut BITMAP).cast()),
            )
        };
        unsafe {
            let _ = DeleteObject(HGDIOBJ(loaded.0));
        }
        assert_eq!(size, std::mem::size_of::<BITMAP>() as i32);
        assert_eq!(bitmap.bmWidth, width);
        assert_eq!(bitmap.bmHeight, height);
        assert_eq!(bitmap.bmBitsPixel, 32);
    }

    #[test]
    fn automatic_update_check_remains_due_every_24_hours() {
        let interval = update_check_interval().as_secs();
        let last_check = 1_000;

        assert!(auto_update_check_due_at(None, last_check));
        assert!(!auto_update_check_due_at(
            Some(last_check),
            last_check + interval - 1
        ));
        assert!(auto_update_check_due_at(
            Some(last_check),
            last_check + interval
        ));
        assert!(!auto_update_check_due_at(Some(last_check + 1), last_check));
    }

    fn display_monitor(device: &str, primary: bool, rect: RECT) -> native_interop::DisplayMonitor {
        native_interop::DisplayMonitor {
            device: device.to_string(),
            rect,
            is_primary: primary,
        }
    }

    #[test]
    fn settings_protocol_rejects_invalid_values_before_mutating_windows() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        for (key, value) in [
            (Key::PollIntervalMs, json!(0)),
            (Key::PollIntervalMs, json!(3_600_001)),
            (Key::PollIntervalMs, json!(60_000.5)),
            (Key::AdaptiveRefresh, json!("true")),
            (Key::ShowTrayIcon, json!("true")),
            (Key::SettingsPaneOpen, json!("false")),
            (Key::AlertThresholdPercent, json!(15)),
            (Key::Language, json!("../../arbitrary")),
            (Key::Palette, json!("custom")),
            (Key::BarStyle, json!("unknown")),
            (Key::BarThickness, json!("unknown")),
            (Key::FontSize, json!("unknown")),
            (Key::ClaudeAccent, json!([256, 0, 0])),
            (Key::CodexAccent, json!([0, 0])),
            (Key::AntigravityAccent, json!([256, 0, 0])),
            (Key::OpencodeAccent, json!([0, 0])),
            (Key::CursorAccent, json!("red")),
            (Key::GrokAccent, json!([1, 2, 3, 4])),
            (Key::CopilotAccent, json!([-1, 0, 0])),
            (Key::ShowClaudeIcon, json!("true")),
            (Key::ShowCopilotIcon, json!("false")),
            (Key::MonitorDevice, json!(r"\\.\DISPLAY99")),
        ] {
            let changes = [settings_ui::Change { key, value }];
            let mut save_calls = 0;
            if let Ok(candidate) = validate_settings_changes(&current, &changes, false, &[]) {
                assert_ne!(
                    candidate, current,
                    "invalid {key:?} unexpectedly changed state"
                );
                save_calls += 1;
            }
            assert_eq!(save_calls, 0, "invalid {key:?} must not save");
        }
    }

    #[test]
    fn settings_pane_open_is_a_strict_ui_only_preference() {
        use serde_json::json;

        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        assert!(current.settings_pane_open);
        let change = settings_ui::Change {
            key: Key::SettingsPaneOpen,
            value: json!(false),
        };
        let candidate = validate_settings_changes(&current, &[change.clone()], false, &[])
            .expect("boolean settings-pane value should be accepted");
        let mut expected = current.clone();
        expected.settings_pane_open = false;

        assert_eq!(candidate, expected);
        assert!(!settings_batch_needs_widget_redraw(&[change]));

        let invalid = settings_ui::Change {
            key: Key::SettingsPaneOpen,
            value: json!("false"),
        };
        assert!(validate_settings_changes(&current, &[invalid], false, &[]).is_err());
    }

    #[test]
    fn monitor_null_restores_automatic_selection_and_uses_primary_taskbar() {
        use serde_json::json;

        let current = SettingsCandidate::from_settings(
            &SettingsFile {
                taskbar_index: 0,
                monitor_device: Some(r"\\.\DISPLAY2".to_string()),
                ..SettingsFile::default()
            },
            false,
        );
        let candidate = validate_settings_changes(
            &current,
            &[settings_ui::Change {
                key: Key::MonitorDevice,
                value: json!(null),
            }],
            false,
            &[],
        )
        .expect("null selects automatic monitor mode");
        assert_eq!(candidate.monitor_device, None);

        let monitors = [
            display_monitor("DISPLAY2", false, RECT::default()),
            display_monitor("DISPLAY1", true, RECT::default()),
        ];
        assert_eq!(
            select_monitor(&monitors, None, None).unwrap().device,
            "DISPLAY1"
        );
        assert_eq!(
            serde_json::json!({"monitor_device": candidate.monitor_device})["monitor_device"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn explicit_monitor_resolution_and_removed_monitor_fallback_are_device_based() {
        let monitors = [
            display_monitor("DISPLAY2", false, RECT::default()),
            display_monitor("DISPLAY1", true, RECT::default()),
        ];
        assert_eq!(
            select_monitor(&monitors, Some("DISPLAY2"), None)
                .unwrap()
                .device,
            "DISPLAY2"
        );

        let saved_selection = Some("DISPLAY99".to_string());
        assert_eq!(
            select_monitor(&monitors, saved_selection.as_deref(), None)
                .unwrap()
                .device,
            "DISPLAY1"
        );
        assert_eq!(saved_selection.as_deref(), Some("DISPLAY99"));
    }

    #[test]
    fn active_monitor_without_a_taskbar_resolves_to_a_floating_target() {
        use serde_json::json;

        let monitor_rect = RECT {
            left: -1600,
            top: -100,
            right: 0,
            bottom: 800,
        };
        let work_rect = RECT {
            bottom: 760,
            ..monitor_rect
        };
        let monitors = [display_monitor("DISPLAY2", false, monitor_rect)];
        let target = resolve_monitor_target(&monitors, &[], Some("DISPLAY2"), None).unwrap();
        assert_eq!(target.monitor.device, "DISPLAY2");
        assert!(target.taskbar.is_none());

        let right = floating_window_position(target.monitor.rect, 300, 120, false, 50);
        assert_eq!((right.x, right.y), (-350, 680));
        assert_eq!(right.y + right.height, monitor_rect.bottom);
        assert!(right.y + right.height > work_rect.bottom);
        let left = floating_window_position(target.monitor.rect, 300, 120, true, 50);
        assert_eq!((left.x, left.y), (monitor_rect.left, 680));

        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let candidate = validate_settings_changes(
            &current,
            &[settings_ui::Change {
                key: Key::MonitorDevice,
                value: json!("DISPLAY2"),
            }],
            false,
            &["DISPLAY2".to_string()],
        )
        .expect("an active monitor is selectable without a Shell taskbar");
        assert_eq!(candidate.monitor_device.as_deref(), Some("DISPLAY2"));
    }

    #[test]
    fn active_monitor_with_a_taskbar_resolves_to_an_embedded_target() {
        let monitors = [display_monitor("DISPLAY2", false, RECT::default())];
        let taskbars = [native_interop::TaskbarWindow {
            hwnd: HWND::default(),
            rect: RECT {
                left: 42,
                ..RECT::default()
            },
        }];
        let target =
            resolve_monitor_target_with(&monitors, &taskbars, Some("DISPLAY2"), None, |taskbar| {
                (taskbar.rect.left == 42).then(|| "DISPLAY2".to_string())
            })
            .unwrap();

        assert_eq!(target.monitor.device, "DISPLAY2");
        assert_eq!(
            target.taskbar.map(|(index, taskbar)| (index, taskbar.hwnd)),
            Some((0, HWND::default()))
        );
    }

    #[test]
    fn startup_embedding_failure_stays_hidden_until_retry_and_honors_visibility() {
        let failed = startup_placement_decision(true, false, true, false);
        assert_eq!(
            failed,
            StartupPlacementDecision {
                placement_succeeded: false,
                show_widget: false,
                retry: true,
            }
        );

        let visible_retry = startup_placement_decision(true, true, true, false);
        assert!(visible_retry.placement_succeeded);
        assert!(visible_retry.show_widget);
        assert!(!visible_retry.retry);

        let hidden_retry = startup_placement_decision(true, true, false, false);
        assert!(hidden_retry.placement_succeeded);
        assert!(!hidden_retry.show_widget);
        assert!(!hidden_retry.retry);
    }

    #[test]
    fn drag_end_persists_same_monitor_and_floating_offsets_and_rolls_back_failed_migration() {
        assert_eq!(
            drag_offset_to_persist(37, DragMonitorTransition::None),
            37,
            "same-monitor drag retains its final horizontal offset"
        );
        assert_eq!(
            drag_offset_to_persist(52, DragMonitorTransition::None),
            52,
            "floating drag retains its final horizontal offset"
        );
        assert_eq!(
            drag_offset_to_persist(41, DragMonitorTransition::Failed),
            41,
            "failed monitor migration restores the old placement offset before persistence"
        );
        assert_eq!(
            drag_offset_to_persist(41, DragMonitorTransition::Succeeded(9)),
            9,
            "successful monitor migration persists its destination offset"
        );
    }

    #[test]
    fn taskbar_watchdog_ignores_stale_snapshots_during_migration_and_for_live_attachment() {
        assert_eq!(
            taskbar_watchdog_action(true, true, true, false),
            TaskbarWatchdogAction::Ignore,
            "an in-progress monitor transition must not trigger recovery"
        );
        assert_eq!(
            taskbar_watchdog_action(false, true, true, true),
            TaskbarWatchdogAction::Ignore,
            "a live widget attached to its current taskbar must not relaunch for a stale old handle"
        );
        assert_eq!(
            taskbar_watchdog_action(false, true, true, false),
            TaskbarWatchdogAction::Refresh,
            "a live but stale attachment is recoverable through the UI message loop"
        );
        assert_eq!(
            taskbar_watchdog_action(false, true, false, false),
            TaskbarWatchdogAction::Relaunch,
            "a destroyed widget HWND requires process recovery"
        );
    }

    #[test]
    fn legacy_taskbar_index_migrates_to_active_monitor_device() {
        let monitors = [display_monitor("DISPLAY2", false, RECT::default())];
        let taskbar_devices = vec![Some("DISPLAY1".to_string()), Some("DISPLAY2".to_string())];
        let mut settings = SettingsFile {
            taskbar_index: 1,
            ..SettingsFile::default()
        };

        assert!(migrate_legacy_taskbar_selection(
            &mut settings,
            &monitors,
            &taskbar_devices
        ));
        assert_eq!(settings.monitor_device.as_deref(), Some("DISPLAY2"));

        let mut unavailable = SettingsFile {
            taskbar_index: 9,
            ..SettingsFile::default()
        };
        assert!(!migrate_legacy_taskbar_selection(
            &mut unavailable,
            &monitors,
            &taskbar_devices
        ));
        assert_eq!(unavailable.taskbar_index, 9);
        assert_eq!(unavailable.monitor_device, None);
    }

    #[test]
    fn floating_widget_clamps_to_negative_and_oversized_monitor_bounds() {
        let work = RECT {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        };
        let right = floating_window_position(work, 300, 120, false, 50);
        assert_eq!((right.x, right.y), (-350, 760));
        assert_eq!((right.width, right.height), (300, 120));

        // The next placement after a DPI-driven resize keeps the same offset
        // from the work-area edge while clamping the larger widget in bounds.
        let resized = floating_window_position(work, 450, 180, false, 50);
        assert_eq!((resized.x, resized.y), (-500, 700));

        let left = floating_window_position(work, 450, 180, true, 50);
        assert_eq!((left.x, left.y), (-1920, 700));
        assert_eq!((left.width, left.height), (450, 180));

        let oversized = floating_window_position(work, 2400, 1200, false, 100);
        assert_eq!((oversized.x, oversized.y), (-1920, -200));
    }

    #[test]
    fn invalid_or_removed_monitor_devices_are_rejected() {
        use serde_json::json;

        let current = SettingsCandidate::from_settings(
            &SettingsFile {
                monitor_device: Some(r"\\.\DISPLAY2".to_string()),
                ..SettingsFile::default()
            },
            false,
        );
        for value in [json!(42), json!(r"\\.\DISPLAY2"), json!(r"\\.\DISPLAY99")] {
            assert!(validate_settings_changes(
                &current,
                &[settings_ui::Change {
                    key: Key::MonitorDevice,
                    value,
                }],
                false,
                &[],
            )
            .is_err());
        }
        assert_eq!(current.monitor_device.as_deref(), Some(r"\\.\DISPLAY2"));
    }

    #[test]
    fn save_failure_reply_contains_the_effective_state_for_persist_retry() {
        use serde_json::json;

        let effective_state = json!({
            "settings": {
                "startup": true,
                "monitor_device": null,
                "show_tray_icon": false,
                "show_provider_icons": false
            }
        });
        let reply = settings_reply(
            17,
            23,
            effective_state.clone(),
            Err(SettingsApplyError::new(
                "save_failed",
                "The setting was applied but could not be saved.",
            )),
        );

        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error_code"], "save_failed");
        assert_eq!(reply["state"], effective_state);
    }

    #[test]
    fn missing_credentials_only_block_new_provider_enables() {
        use serde_json::json;
        let provider_keys = [
            Key::ShowClaudeCode,
            Key::ShowCodex,
            Key::ShowAntigravity,
            Key::ShowOpenCode,
            Key::ShowCursor,
            Key::ShowGrok,
            Key::ShowCopilot,
        ];
        let set_enabled = |settings: &mut SettingsFile, key: Key, value| match key {
            Key::ShowClaudeCode => settings.show_claude_code = value,
            Key::ShowCodex => settings.show_codex = value,
            Key::ShowAntigravity => settings.show_antigravity = value,
            Key::ShowOpenCode => settings.show_opencode = value,
            Key::ShowCursor => settings.show_cursor = value,
            Key::ShowGrok => settings.show_grok = value,
            Key::ShowCopilot => settings.show_copilot = value,
            _ => unreachable!("only provider keys are used in this test"),
        };
        let unavailable = poller::ProviderAvailability::default();

        for key in provider_keys {
            let mut disabled_settings = SettingsFile::default();
            set_enabled(&mut disabled_settings, key, false);
            if key == Key::ShowCodex {
                // Keep a different provider enabled to preserve the min-one rule.
                disabled_settings.show_opencode = true;
            }
            let disabled = SettingsCandidate::from_settings(&disabled_settings, false);
            let enable = [settings_ui::Change {
                key,
                value: json!(true),
            }];
            assert!(validate_settings_changes_with_availability(
                &disabled,
                &enable,
                unavailable,
                &[],
            )
            .is_err(), "unavailable {key:?} must not be enabled");

            let mut enabled_settings = disabled_settings;
            set_enabled(&mut enabled_settings, key, true);
            let enabled = SettingsCandidate::from_settings(&enabled_settings, false);
            let unrelated = [settings_ui::Change {
                key: Key::WidgetVisible,
                value: json!(false),
            }];
            assert!(validate_settings_changes_with_availability(
                &enabled,
                &unrelated,
                unavailable,
                &[],
            )
            .is_ok(), "unavailable {key:?} must not block unrelated edits");

            let keep_enabled = [settings_ui::Change {
                key,
                value: json!(true),
            }];
            assert!(validate_settings_changes_with_availability(
                &enabled,
                &keep_enabled,
                unavailable,
                &[],
            )
            .is_ok(), "already-enabled unavailable {key:?} may remain enabled");

            let disable = [settings_ui::Change {
                key,
                value: json!(false),
            }];
            assert!(validate_settings_changes_with_availability(
                &enabled,
                &disable,
                unavailable,
                &[],
            )
            .is_ok(), "unavailable {key:?} must be disableable");
        }
    }

    #[test]
    fn settings_batch_provider_changes_validate_against_final_candidate_in_either_order() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let codex_off = settings_ui::Change {
            key: Key::ShowCodex,
            value: json!(false),
        };
        let claude_on = settings_ui::Change {
            key: Key::ShowClaudeCode,
            value: json!(true),
        };

        for changes in [
            vec![codex_off.clone(), claude_on.clone()],
            vec![claude_on.clone(), codex_off.clone()],
        ] {
            let candidate = validate_settings_changes(&current, &changes, true, &[]).unwrap();
            assert!(!candidate.show_codex);
            assert!(candidate.show_claude_code);
        }
    }

    #[test]
    fn settings_batch_window_changes_validate_against_final_candidate_in_either_order() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(
            &SettingsFile {
                show_session_window: true,
                show_weekly_window: false,
                ..SettingsFile::default()
            },
            false,
        );
        let session_off = settings_ui::Change {
            key: Key::ShowSessionWindow,
            value: json!(false),
        };
        let weekly_on = settings_ui::Change {
            key: Key::ShowWeeklyWindow,
            value: json!(true),
        };

        for changes in [
            vec![session_off.clone(), weekly_on.clone()],
            vec![weekly_on.clone(), session_off.clone()],
        ] {
            let candidate = validate_settings_changes(&current, &changes, false, &[]).unwrap();
            assert!(!candidate.show_session_window);
            assert!(candidate.show_weekly_window);
        }
    }

    #[test]
    fn duplicate_and_over_limit_batches_are_rejected_without_changing_candidate() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let duplicate = [
            settings_ui::Change {
                key: Key::WidgetVisible,
                value: json!(false),
            },
            settings_ui::Change {
                key: Key::WidgetVisible,
                value: json!(true),
            },
        ];
        assert!(validate_settings_changes(&current, &duplicate, true, &[]).is_err());

        let too_many: Vec<_> = (0..=settings_ui::MAX_BATCH_CHANGES)
            .map(|_| settings_ui::Change {
                key: Key::WidgetVisible,
                value: json!(false),
            })
            .collect();
        assert!(validate_settings_changes(&current, &too_many, true, &[]).is_err());
        assert_eq!(
            current,
            SettingsCandidate::from_settings(&SettingsFile::default(), false)
        );
    }

    #[test]
    fn null_accents_are_valid_and_clear_custom_colors() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(
            &SettingsFile {
                appearance: Appearance {
                    claude_accent: Some([1, 2, 3]),
                    ..Appearance::default()
                },
                ..SettingsFile::default()
            },
            false,
        );
        let candidate = validate_settings_changes(
            &current,
            &[settings_ui::Change {
                key: Key::ClaudeAccent,
                value: json!(null),
            }],
            false,
            &[],
        )
        .unwrap();
        assert_eq!(candidate.appearance.claude_accent, None);
    }

    #[test]
    fn per_provider_marks_and_bar_colors_accept_valid_values() {
        use serde_json::json;
        let current = SettingsCandidate::from_settings(&SettingsFile::default(), false);
        let candidate = validate_settings_changes(
            &current,
            &[
                settings_ui::Change {
                    key: Key::ShowAntigravityIcon,
                    value: json!(false),
                },
                settings_ui::Change {
                    key: Key::ShowOpencodeIcon,
                    value: json!(false),
                },
                settings_ui::Change {
                    key: Key::ShowCursorIcon,
                    value: json!(false),
                },
                settings_ui::Change {
                    key: Key::ShowGrokIcon,
                    value: json!(false),
                },
                settings_ui::Change {
                    key: Key::ShowCopilotIcon,
                    value: json!(false),
                },
                settings_ui::Change {
                    key: Key::AntigravityAccent,
                    value: json!([0x42, 0x85, 0xF4]),
                },
                settings_ui::Change {
                    key: Key::OpencodeAccent,
                    value: json!([0x5B, 0x8D, 0xEF]),
                },
                settings_ui::Change {
                    key: Key::CursorAccent,
                    value: json!([0x6E, 0x6E, 0x6E]),
                },
                settings_ui::Change {
                    key: Key::GrokAccent,
                    value: json!([0xA8, 0xA8, 0xA8]),
                },
                settings_ui::Change {
                    key: Key::CopilotAccent,
                    value: json!([0xA3, 0x71, 0xF7]),
                },
            ],
            false,
            &[],
        )
        .unwrap();
        assert!(!candidate.appearance.show_antigravity_icon);
        assert!(!candidate.appearance.show_opencode_icon);
        assert!(!candidate.appearance.show_cursor_icon);
        assert!(!candidate.appearance.show_grok_icon);
        assert!(!candidate.appearance.show_copilot_icon);
        assert_eq!(
            candidate.appearance.antigravity_accent,
            Some([0x42, 0x85, 0xF4])
        );
        assert_eq!(
            candidate.appearance.opencode_accent,
            Some([0x5B, 0x8D, 0xEF])
        );
        assert_eq!(candidate.appearance.cursor_accent, Some([0x6E, 0x6E, 0x6E]));
        assert_eq!(candidate.appearance.grok_accent, Some([0xA8, 0xA8, 0xA8]));
        assert_eq!(
            candidate.appearance.copilot_accent,
            Some([0xA3, 0x71, 0xF7])
        );

        let cleared = validate_settings_changes(
            &candidate,
            &[settings_ui::Change {
                key: Key::GrokAccent,
                value: json!(null),
            }],
            false,
            &[],
        )
        .unwrap();
        assert_eq!(cleared.appearance.grok_accent, None);
    }

    #[test]
    fn unavailable_quota_has_no_bar_fill() {
        assert_eq!(usage_bar_fill_percentage(100.0, "--"), 0.0);
        assert_eq!(usage_bar_fill_percentage(61.0, "剩余61%"), 61.0);
        assert_eq!(usage_bar_fill_percentage(100.0, "剩余100%"), 100.0);
    }

    #[test]
    fn localized_widget_text_fits_measured_columns_and_rows() {
        unsafe {
            let hdc = GetDC(HWND::default());
            assert!(!hdc.is_invalid());
            let width_of = |font: HFONT, text: &str| {
                let old_font = SelectObject(hdc, font);
                let mut wide: Vec<u16> = text.encode_utf16().collect();
                let mut rect = RECT::default();
                DrawTextW(hdc, &mut wide, &mut rect, DT_CALCRECT | DT_SINGLELINE);
                SelectObject(hdc, old_font);
                (rect.right, rect.bottom)
            };
            for language in LanguageId::ALL {
                for (palette, font_size, window_label_font_size) in [
                    (Palette::System, FontSize::Standard),
                    (Palette::System, FontSize::Large),
                    (Palette::HighContrastDark, FontSize::Standard),
                    (Palette::HighContrastDark, FontSize::Large),
                ]
                .into_iter()
                .flat_map(|(palette, font_size)| {
                    (8..=14).map(move |size| (palette, font_size, size))
                }) {
                    let appearance = Appearance {
                        palette,
                        font_size,
                        window_label_font_size,
                        ..Appearance::default()
                    };
                    let (label_width, text_width) = usage_layout_widths(language, appearance);
                    let reset_width =
                        text_width - sc(quota_text::reset_offset(appearance.font_px()));
                    let label_font = quota_text::create_reset_font(sc(i32::from(
                        appearance.window_label_font_size,
                    )));
                    let reset_font = quota_text::create_reset_font(sc(appearance.font_px()));
                    assert!(!label_font.is_invalid() && !reset_font.is_invalid());
                    let (labels, resets) = usage_layout_samples(language);
                    for label in labels {
                        let (width, height) = width_of(label_font, label);
                        assert!(width <= label_width, "{language:?}: {label:?}");
                        assert!(height <= appearance.row_height(), "{language:?}: {label:?}, label size {window_label_font_size}, height {height}, row {}", appearance.row_height());
                    }
                    for reset in &resets {
                        let (width, height) = width_of(reset_font, reset);
                        assert!(width <= reset_width, "{language:?}: {reset:?}");
                        assert!(height <= appearance.row_height(), "{language:?}: {reset:?}");
                    }
                    let _ = DeleteObject(label_font);
                    let _ = DeleteObject(reset_font);
                }
            }
            ReleaseDC(HWND::default(), hdc);
        }
    }

    #[test]
    fn service_tooltip_combines_visible_quota_rows() {
        assert_eq!(
            service_tooltip(
                "Codex",
                "剩余13% 19:04重置",
                "剩余86% 07/18重置",
                true,
                true
            ),
            "Codex: 5h 剩余13% 19:04重置 | 7d 剩余86% 07/18重置"
        );
        assert_eq!(
            service_tooltip("Claude Code", "13%", "86%", false, true),
            "Claude Code: 7d 86%"
        );
    }

    fn test_settings_json(language: &str) -> String {
        format!(
            r#"{{
  "tray_offset": 321,
  "taskbar_index": 1,
  "poll_interval_ms": 60000,
  "language": "{language}",
  "widget_visible": true,
  "show_claude_code": false,
  "show_codex": true,
  "show_antigravity": false
}}"#
        )
    }

    #[test]
    fn appearance_settings_preserve_legacy_defaults_and_round_trip() {
        let old: SettingsFile = serde_json::from_str(&test_settings_json("zh-CN")).unwrap();
        assert_eq!(old.appearance, Appearance::default());
        assert_eq!(old.appearance.bar_thickness, BarThickness::Slim);
        assert_eq!(old.monitor_device, None);
        assert!(old.settings_pane_open);
        assert!(old.show_tray_icon);
        assert!(old.show_provider_icons);

        let customized = SettingsFile {
            appearance: Appearance::translucent_dark_taskbar(),
            ..old
        };
        assert_eq!(customized.appearance.bar_style, BarStyle::Continuous);
        let json = serde_json::to_string(&customized).unwrap();
        let restored: SettingsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.appearance, Appearance::translucent_dark_taskbar());
    }

    #[test]
    fn per_provider_marks_default_to_visible_with_no_custom_bar_colors() {
        let defaults = Appearance::default();
        assert!(defaults.show_claude_icon);
        assert!(defaults.show_codex_icon);
        assert!(defaults.show_antigravity_icon);
        assert!(defaults.show_opencode_icon);
        assert!(defaults.show_cursor_icon);
        assert!(defaults.show_grok_icon);
        assert!(defaults.show_copilot_icon);
        assert_eq!(defaults.antigravity_accent, None);
        assert_eq!(defaults.opencode_accent, None);
        assert_eq!(defaults.cursor_accent, None);
        assert_eq!(defaults.grok_accent, None);
        assert_eq!(defaults.copilot_accent, None);

        let legacy: SettingsFile = serde_json::from_str("{}").unwrap();
        assert!(legacy.appearance.show_opencode_icon);
        assert!(legacy.appearance.show_cursor_icon);
        assert!(legacy.appearance.show_grok_icon);
        assert!(legacy.appearance.show_copilot_icon);
        assert_eq!(legacy.appearance.antigravity_accent, None);
        assert_eq!(legacy.appearance.copilot_accent, None);
    }

    #[test]
    fn custom_provider_colors_survive_settings_round_trip() {
        let settings = SettingsFile {
            appearance: Appearance {
                claude_accent: Some([0xD9, 0x77, 0x57]),
                codex_accent: Some([0x34, 0xD3, 0x99]),
                antigravity_accent: Some([0x42, 0x85, 0xF4]),
                opencode_accent: Some([0x5B, 0x8D, 0xEF]),
                cursor_accent: Some([0x6E, 0x6E, 0x6E]),
                grok_accent: Some([0xA8, 0xA8, 0xA8]),
                copilot_accent: Some([0xA3, 0x71, 0xF7]),
                ..Appearance::default()
            },
            ..SettingsFile::default()
        };
        let serialized = serde_json::to_string(&settings).unwrap();
        let restored: SettingsFile = serde_json::from_str(&serialized).unwrap();

        assert_eq!(restored.appearance.claude_accent, Some([0xD9, 0x77, 0x57]));
        assert_eq!(restored.appearance.codex_accent, Some([0x34, 0xD3, 0x99]));
        assert_eq!(
            restored.appearance.antigravity_accent,
            Some([0x42, 0x85, 0xF4])
        );
        assert_eq!(
            restored.appearance.opencode_accent,
            Some([0x5B, 0x8D, 0xEF])
        );
        assert_eq!(restored.appearance.cursor_accent, Some([0x6E, 0x6E, 0x6E]));
        assert_eq!(restored.appearance.grok_accent, Some([0xA8, 0xA8, 0xA8]));
        assert_eq!(restored.appearance.copilot_accent, Some([0xA3, 0x71, 0xF7]));
    }

    #[test]
    fn tray_and_widget_provider_icon_preferences_round_trip_independently() {
        let settings = SettingsFile {
            show_tray_icon: false,
            show_provider_icons: false,
            ..SettingsFile::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: SettingsFile = serde_json::from_str(&json).unwrap();

        assert!(!restored.show_tray_icon);
        assert!(!restored.show_provider_icons);
        assert!(SettingsFile::default().show_tray_icon);
        assert!(SettingsFile::default().show_provider_icons);
    }

    #[test]
    fn settings_pane_open_defaults_true_and_round_trips_as_a_boolean() {
        let settings = SettingsFile {
            settings_pane_open: false,
            ..SettingsFile::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: SettingsFile = serde_json::from_str(&json).unwrap();
        let legacy: SettingsFile = serde_json::from_str(&test_settings_json("en")).unwrap();

        assert!(!restored.settings_pane_open);
        assert!(legacy.settings_pane_open);
        assert!(serde_json::from_str::<SettingsFile>(r#"{"settings_pane_open":"false"}"#).is_err());
    }

    #[test]
    fn chosen_monitor_device_survives_settings_round_trip() {
        let settings = SettingsFile {
            monitor_device: Some(r"\\.\DISPLAY2".to_string()),
            ..SettingsFile::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: SettingsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.monitor_device, settings.monitor_device);
    }

    #[test]
    fn loads_legacy_settings_when_new_path_is_missing() {
        let base = std::env::temp_dir().join(format!(
            "quota-usage-settings-test-{}-{}",
            std::process::id(),
            now_unix_secs()
        ));
        let current = base.join("QuotaUsage").join("settings.json");
        let codex_legacy = base.join("CodexUsage").join("settings.json");
        let claude_legacy = base.join("ClaudeCodeUsageMonitor").join("settings.json");
        std::fs::create_dir_all(claude_legacy.parent().unwrap()).unwrap();
        std::fs::write(&claude_legacy, test_settings_json("zh-CN")).unwrap();

        let (settings, migrated) =
            load_settings_from_paths(&current, &codex_legacy, &claude_legacy).unwrap();

        assert!(migrated);
        assert_eq!(settings.tray_offset, 321);
        assert_eq!(settings.poll_interval_ms, 60_000);
        assert_eq!(settings.language.as_deref(), Some("zh-CN"));
        assert!(settings.show_codex);
        assert!(!settings.show_claude_code);
        assert!(settings.show_session_window);
        assert!(settings.show_weekly_window);
        assert_eq!(settings.alert_threshold_percent, 0);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn new_settings_take_precedence_over_legacy_settings() {
        let base = std::env::temp_dir().join(format!(
            "quota-usage-settings-precedence-test-{}-{}",
            std::process::id(),
            now_unix_secs()
        ));
        let current = base.join("QuotaUsage").join("settings.json");
        let codex_legacy = base.join("CodexUsage").join("settings.json");
        let claude_legacy = base.join("ClaudeCodeUsageMonitor").join("settings.json");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        std::fs::create_dir_all(codex_legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(claude_legacy.parent().unwrap()).unwrap();
        std::fs::write(&current, test_settings_json("en")).unwrap();
        std::fs::write(&codex_legacy, test_settings_json("fr")).unwrap();
        std::fs::write(&claude_legacy, test_settings_json("zh-CN")).unwrap();

        let (settings, migrated) =
            load_settings_from_paths(&current, &codex_legacy, &claude_legacy).unwrap();

        assert!(!migrated);
        assert_eq!(settings.language.as_deref(), Some("en"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn codex_settings_take_precedence_over_the_older_claude_directory() {
        let base = std::env::temp_dir().join(format!(
            "quota-usage-settings-legacy-precedence-test-{}-{}",
            std::process::id(),
            now_unix_secs()
        ));
        let current = base.join("QuotaUsage").join("settings.json");
        let codex_legacy = base.join("CodexUsage").join("settings.json");
        let claude_legacy = base.join("ClaudeCodeUsageMonitor").join("settings.json");
        std::fs::create_dir_all(codex_legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(claude_legacy.parent().unwrap()).unwrap();
        std::fs::write(&codex_legacy, test_settings_json("fr")).unwrap();
        std::fs::write(&claude_legacy, test_settings_json("zh-CN")).unwrap();

        let (settings, migrated) =
            load_settings_from_paths(&current, &codex_legacy, &claude_legacy).unwrap();

        assert!(migrated);
        assert_eq!(settings.language.as_deref(), Some("fr"));
        assert!(codex_legacy.exists());
        assert!(claude_legacy.exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn startup_commands_match_only_exact_paths_for_migration() {
        let executable = r"C:\Users\user\AppData\Local\Programs\QuotaUsage\quota-usage.exe";
        assert!(startup_command_matches(&format!("\"{executable}\""), executable));
        assert!(!startup_command_matches(
            r"C:\Old\quota-usage.exe",
            executable
        ));
    }

    #[test]
    fn displays_distinct_transient_error_categories() {
        assert_eq!(
            poll_error_display_label(
                poller::PollError::NetworkUnavailable,
                LanguageId::SimplifiedChinese,
            ),
            "网络"
        );
        assert_eq!(
            poll_error_display_label(
                poller::PollError::RateLimited(None),
                LanguageId::SimplifiedChinese,
            ),
            "限流"
        );
        assert_eq!(
            poll_error_display_label(poller::PollError::ServerError, LanguageId::English),
            "5XX"
        );
        assert_eq!(
            poll_error_display_label(poller::PollError::RequestFailed, LanguageId::English),
            "ERR"
        );
    }

    #[test]
    fn left_anchor_is_stable_across_tray_width_widget_size_and_monitor_changes() {
        for left in [0, -2560, 2560] {
            for tray_width in [1600, 1800, 2000] {
                for widget_width in [480, 606, 970] {
                    assert_eq!(
                        compute_anchor_x(left, left + tray_width, widget_width, 321, true),
                        left
                    );
                }
            }
        }
        assert_eq!(compute_anchor_x(0, 300, 606, 321, true), 0);
        assert_eq!(compute_anchor_x(0, 2000, 606, 321, false), 1073);
    }

    #[test]
    fn left_anchor_setting_round_trips_without_changing_manual_position() {
        let mut settings = SettingsFile {
            tray_offset: 321,
            anchor_left: true,
            ..SettingsFile::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        settings = serde_json::from_str(&json).unwrap();
        assert!(settings.anchor_left);
        assert_eq!(settings.tray_offset, 321);
        let legacy: SettingsFile = serde_json::from_str("{}").unwrap();
        assert!(!legacy.anchor_left);
    }

    #[test]
    fn startup_recognizes_quoted_commands_and_legacy_unquoted_paths() {
        let path = r"C:\Program Files\Quota Usage\quota-usage.exe";
        assert!(startup_command_matches(&format!("\"{path}\""), path));
        assert!(startup_command_matches(path, path));
        assert!(!startup_command_matches(r"C:\Other\quota-usage.exe", path));
    }

    #[test]
    fn invalid_poll_intervals_cannot_create_a_busy_timer() {
        for interval in [0, 1, u32::MAX] {
            let settings = normalize_settings(SettingsFile {
                poll_interval_ms: interval,
                ..SettingsFile::default()
            });
            assert_eq!(settings.poll_interval_ms, POLL_15_MIN);
        }
    }

    #[test]
    fn normalizes_usage_display_and_alert_settings() {
        let settings = normalize_settings(SettingsFile {
            show_session_window: false,
            show_weekly_window: false,
            alert_threshold_percent: 17,
            notified_quota_windows: vec!["codex:weekly:1".into(), "codex:weekly:1".into()],
            ..SettingsFile::default()
        });

        assert!(settings.show_session_window);
        assert!(!settings.show_weekly_window);
        assert_eq!(settings.alert_threshold_percent, 0);
        assert_eq!(settings.notified_quota_windows.len(), 1);
    }

    #[test]
    fn unavailable_claude_cli_is_disabled_without_disabling_codex() {
        let settings = SettingsFile {
            show_claude_code: true,
            show_codex: false,
            show_antigravity: false,
            show_opencode: false,
            show_cursor: false,
            show_grok: false,
            show_copilot: false,
            ..SettingsFile::default()
        };

        let (settings, changed) = apply_claude_code_availability(settings, false);

        assert!(changed);
        assert!(!settings.show_claude_code);
        assert!(settings.show_codex);
    }

    #[test]
    fn formats_precise_local_reset_time() {
        let local = SYSTEMTIME {
            wYear: 2026,
            wMonth: 7,
            wDay: 17,
            wHour: 18,
            wMinute: 30,
            ..Default::default()
        };
        assert_eq!(format_local_system_time(local), "2026-07-17 18:30");
        assert_eq!(format_precise_reset_time(None), None);
    }

    #[test]
    fn low_quota_alert_is_deduplicated_until_reset_window_changes() {
        let mut alerts = Vec::new();
        let mut notified = BTreeSet::new();
        let first_reset = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let first = crate::models::UsageSection {
            percentage: 85.0,
            resets_at: Some(first_reset),
        };

        append_quota_alert(
            &mut alerts,
            &mut notified,
            20,
            LanguageId::SimplifiedChinese,
            tray_icon::TrayIconKind::Codex,
            "codex",
            "Codex",
            "session",
            "5小时",
            &first,
        );
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].message.contains("仅剩 15%"));

        let next = crate::models::UsageSection {
            percentage: 90.0,
            resets_at: Some(first_reset + Duration::from_secs(18_000)),
        };
        append_quota_alert(
            &mut alerts,
            &mut notified,
            20,
            LanguageId::SimplifiedChinese,
            tray_icon::TrayIconKind::Codex,
            "codex",
            "Codex",
            "session",
            "5小时",
            &next,
        );
        assert_eq!(alerts.len(), 2);
        assert_eq!(notified.len(), 1);
    }
}

unsafe fn handle_command(hwnd: HWND, id: u16) {
    match id {
        1 => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.session_text = "...".to_string();
                    s.weekly_text = "...".to_string();
                    s.codex_session_text = "...".to_string();
                    s.codex_weekly_text = "...".to_string();
                }
            }
            render_layered();
            let sh = SendHwnd::from_hwnd(hwnd);
            request_poll(sh, true);
        }
        IDM_VERSION_ACTION => {
            let (install_channel, release) = {
                let state = lock_state();
                match state.as_ref() {
                    Some(s) => (
                        s.install_channel,
                        match &s.update_status {
                            UpdateStatus::Available(release) => Some(release.clone()),
                            _ => None,
                        },
                    ),
                    None => (InstallChannel::Portable, None),
                }
            };

            match install_channel {
                InstallChannel::Winget => {
                    if release.is_some() {
                        begin_winget_update(hwnd);
                    } else {
                        begin_update_check(hwnd, true);
                    }
                }
                InstallChannel::Portable => {
                    if let Some(release) = release {
                        begin_update_apply(hwnd, release);
                    } else {
                        begin_update_check(hwnd, true);
                    }
                }
            }
        }
        IDM_UNINSTALL => confirm_and_begin_uninstall(hwnd),
        2 => {
            let hook = {
                let state = lock_state();
                state.as_ref().and_then(|s| s.win_event_hook)
            };
            if let Some(h) = hook {
                native_interop::unhook_win_event(h);
            }
            PostQuitMessage(0);
        }
        IDM_RESET_POSITION => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.tray_offset = 0;
                    s.anchor_left = false;
                }
            }
            save_state_settings();
            position_at_taskbar();
        }
        IDM_ANCHOR_LEFT => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.anchor_left = !s.anchor_left;
                }
            }
            save_state_settings();
            position_at_taskbar();
        }
        id if (IDM_MONITOR_FIRST..IDM_MONITOR_FIRST + MAX_MONITOR_MENU_ITEMS as u16)
            .contains(&id) =>
        {
            let index = (id - IDM_MONITOR_FIRST) as usize;
            let monitors = native_interop::find_monitors();
            let taskbars = native_interop::find_taskbars();
            if let Some(monitor) = monitors.get(index) {
                let target =
                    resolve_monitor_target(&monitors, &taskbars, Some(&monitor.device), None);
                if target
                    .as_ref()
                    .is_some_and(|target| transition_to_monitor(hwnd, target))
                {
                    if let Some(state) = lock_state().as_mut() {
                        state.monitor_device = Some(monitor.device.clone());
                        state.legacy_selection_pending = false;
                        state.tray_offset = 0;
                    }
                    save_state_settings();
                    position_at_taskbar_without_save();
                    render_layered();
                }
            }
        }
        IDM_START_WITH_WINDOWS => {
            set_startup_enabled(!is_startup_enabled());
        }
        IDM_FREQ_1MIN | IDM_FREQ_5MIN | IDM_FREQ_15MIN | IDM_FREQ_1HOUR => {
            let new_interval = match id {
                IDM_FREQ_1MIN => POLL_1_MIN,
                IDM_FREQ_5MIN => POLL_5_MIN,
                IDM_FREQ_15MIN => POLL_15_MIN,
                IDM_FREQ_1HOUR => POLL_1_HOUR,
                _ => POLL_15_MIN,
            };
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.poll_interval_ms = new_interval;
                    if s.has_poll_result {
                        s.monitor.reconfigure(
                            s.poll_interval_ms,
                            s.adaptive_refresh,
                            Instant::now(),
                            SystemTime::now(),
                        );
                        SetTimer(hwnd, TIMER_POLL, effective_poll_interval(s), None);
                    }
                }
            }
            save_state_settings();
        }
        IDM_ADAPTIVE_REFRESH => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.adaptive_refresh = !s.adaptive_refresh;
                    if s.has_poll_result {
                        s.monitor.reconfigure(
                            s.poll_interval_ms,
                            s.adaptive_refresh,
                            Instant::now(),
                            SystemTime::now(),
                        );
                        SetTimer(hwnd, TIMER_POLL, effective_poll_interval(s), None);
                    }
                }
            }
            save_state_settings();
            render_layered();
        }
        IDM_SHOW_SESSION_WINDOW | IDM_SHOW_WEEKLY_WINDOW => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    match id {
                        IDM_SHOW_SESSION_WINDOW
                            if s.show_weekly_window || !s.show_session_window =>
                        {
                            s.show_session_window = !s.show_session_window;
                        }
                        IDM_SHOW_WEEKLY_WINDOW
                            if s.show_session_window || !s.show_weekly_window =>
                        {
                            s.show_weekly_window = !s.show_weekly_window;
                        }
                        _ => {}
                    }
                }
            }
            save_state_settings();
            render_layered();
            sync_tray_icons(hwnd);
        }
        IDM_COLOR_CLAUDE | IDM_COLOR_CODEX | IDM_COLOR_RESET => {
            if id == IDM_COLOR_RESET {
                if let Some(s) = lock_state().as_mut() {
                    s.appearance.claude_accent = None;
                    s.appearance.codex_accent = None;
                    s.appearance.antigravity_accent = None;
                    s.appearance.opencode_accent = None;
                    s.appearance.cursor_accent = None;
                    s.appearance.grok_accent = None;
                    s.appearance.copilot_accent = None;
                    s.appearance.usage_text_color = None;
                    s.appearance.provider_monochrome = false;
                }
            } else {
                let initial = {
                    let state = lock_state();
                    let Some(s) = state.as_ref() else {
                        return;
                    };
                    if id == IDM_COLOR_CLAUDE {
                        custom_color(s.appearance.claude_accent, claude_accent_color())
                    } else {
                        custom_color(
                            s.appearance.codex_accent,
                            provider_icons::codex_color(s.appearance.is_dark(s.is_dark)),
                        )
                    }
                };
                if let Some(color) = choose_accent_color(hwnd, initial) {
                    if let Some(s) = lock_state().as_mut() {
                        let value = Some([color.r, color.g, color.b]);
                        if id == IDM_COLOR_CLAUDE {
                            s.appearance.claude_accent = value;
                        } else {
                            s.appearance.codex_accent = value;
                        }
                    }
                }
            }
            save_state_settings();
            render_layered();
        }
        IDM_ABOUT_STAR => {
            open_settings(hwnd, Page::About);
        }
        IDM_SETTINGS => open_settings(hwnd, Page::General),
        IDM_APPEARANCE_PAGE => open_settings(hwnd, Page::Appearance),
        IDM_APPEARANCE_RECOMMENDED
        | IDM_APPEARANCE_RESET
        | IDM_PALETTE_SYSTEM
        | IDM_PALETTE_DARK
        | IDM_PALETTE_LIGHT
        | IDM_BAR_CONTINUOUS
        | IDM_BAR_SEGMENTED
        | IDM_BAR_STANDARD
        | IDM_BAR_SLIM
        | IDM_FONT_STANDARD
        | IDM_FONT_LARGE => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    match id {
                        IDM_APPEARANCE_RECOMMENDED => {
                            s.appearance = Appearance::translucent_dark_taskbar()
                        }
                        IDM_APPEARANCE_RESET => s.appearance = Appearance::default(),
                        IDM_PALETTE_SYSTEM => s.appearance.palette = Palette::System,
                        IDM_PALETTE_DARK => s.appearance.palette = Palette::HighContrastDark,
                        IDM_PALETTE_LIGHT => s.appearance.palette = Palette::HighContrastLight,
                        IDM_BAR_CONTINUOUS => s.appearance.bar_style = BarStyle::Continuous,
                        IDM_BAR_SEGMENTED => s.appearance.bar_style = BarStyle::Segmented,
                        IDM_BAR_STANDARD => s.appearance.bar_thickness = BarThickness::Standard,
                        IDM_BAR_SLIM => s.appearance.bar_thickness = BarThickness::Slim,
                        IDM_FONT_STANDARD => s.appearance.font_size = FontSize::Standard,
                        IDM_FONT_LARGE => s.appearance.font_size = FontSize::Large,
                        _ => {}
                    }
                    if matches!(id, IDM_APPEARANCE_RECOMMENDED | IDM_APPEARANCE_RESET) {
                        s.show_provider_icons = s.appearance.show_provider_icon;
                    }
                }
            }
            save_state_settings();
            // Font size and weight change the measured widget width.
            position_at_taskbar();
            render_layered();
        }
        IDM_ALERT_OFF | IDM_ALERT_10 | IDM_ALERT_20 | IDM_ALERT_30 => {
            let threshold = match id {
                IDM_ALERT_10 => 10,
                IDM_ALERT_20 => 20,
                IDM_ALERT_30 => 30,
                _ => 0,
            };
            let alerts = {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.alert_threshold_percent = threshold;
                    if threshold == 0 {
                        s.notified_quota_windows.clear();
                        Vec::new()
                    } else if let Some(data) = s.data.clone() {
                        collect_low_quota_alerts(s, &data)
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            };
            for alert in &alerts {
                tray_icon::notify_balloon(hwnd, alert.kind, &alert.title, &alert.message);
            }
            save_state_settings();
        }
        IDM_MODEL_CLAUDE_CODE | IDM_MODEL_CODEX | IDM_MODEL_ANTIGRAVITY => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    match id {
                        IDM_MODEL_CLAUDE_CODE => {
                            if effective_provider_availability(s).claude_code_available
                                && (s.show_codex || s.show_antigravity || !s.show_claude_code)
                            {
                                s.show_claude_code = !s.show_claude_code;
                            }
                        }
                        IDM_MODEL_CODEX => {
                            if s.show_claude_code || s.show_antigravity || !s.show_codex {
                                s.show_codex = !s.show_codex;
                            }
                        }
                        IDM_MODEL_ANTIGRAVITY => {
                            if s.show_claude_code || s.show_codex || !s.show_antigravity {
                                s.show_antigravity = !s.show_antigravity;
                            }
                        }
                        _ => {}
                    }
                    s.session_text = "...".to_string();
                    s.weekly_text = "...".to_string();
                    s.codex_session_text = "...".to_string();
                    s.codex_weekly_text = "...".to_string();
                    s.antigravity_session_text = "...".to_string();
                    s.antigravity_weekly_text = "...".to_string();
                }
            }
            save_state_settings();
            position_at_taskbar();
            render_layered();
            sync_tray_icons(hwnd);
            let sh = SendHwnd::from_hwnd(hwnd);
            request_poll(sh, true);
        }
        IDM_LANG_SYSTEM
        | IDM_LANG_ENGLISH
        | IDM_LANG_DUTCH
        | IDM_LANG_SPANISH
        | IDM_LANG_FRENCH
        | IDM_LANG_GERMAN
        | IDM_LANG_JAPANESE
        | IDM_LANG_KOREAN
        | IDM_LANG_SIMPLIFIED_CHINESE
        | IDM_LANG_TRADITIONAL_CHINESE
        | IDM_LANG_RUSSIAN
        | IDM_LANG_PORTUGUESE_BRAZIL => {
            let language_override = match id {
                IDM_LANG_SYSTEM => None,
                IDM_LANG_ENGLISH => Some(LanguageId::English),
                IDM_LANG_DUTCH => Some(LanguageId::Dutch),
                IDM_LANG_SPANISH => Some(LanguageId::Spanish),
                IDM_LANG_FRENCH => Some(LanguageId::French),
                IDM_LANG_GERMAN => Some(LanguageId::German),
                IDM_LANG_JAPANESE => Some(LanguageId::Japanese),
                IDM_LANG_KOREAN => Some(LanguageId::Korean),
                IDM_LANG_SIMPLIFIED_CHINESE => Some(LanguageId::SimplifiedChinese),
                IDM_LANG_TRADITIONAL_CHINESE => Some(LanguageId::TraditionalChinese),
                IDM_LANG_RUSSIAN => Some(LanguageId::Russian),
                IDM_LANG_PORTUGUESE_BRAZIL => Some(LanguageId::PortugueseBrazil),
                _ => None,
            };
            let language_title = {
                let mut state = lock_state();
                state.as_mut().map(|s| {
                    apply_language_to_state(s, language_override);
                    (s.hwnd.to_hwnd(), s.language.strings())
                })
            };
            if let Some((hwnd, strings)) = language_title {
                set_window_title(hwnd, strings);
            }
            save_state_settings();
            render_layered();
        }
        id if id == tray_icon::IDM_TOGGLE_WIDGET => {
            toggle_widget_visibility(hwnd);
        }
        _ => {}
    }
}
