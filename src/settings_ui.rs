//! Settings companion protocol v1 (Rust owns all state).
//!
//! Launch: `SettingsUI.exe --pipe <full-name> --parent-pid <decimal> --page
//! <general|monitoring|appearance|placement|about>`. No settings-file access.
//! Duplex byte pipe: u32 little-endian byte length followed by UTF-8 JSON,
//! requests 1..=65536 bytes, responses 1..=2097152 bytes. The server restricts access to the current token user,
//! rejects remote clients, and checks the client PID against the spawned child.
//! Requests: {"v":1,"id":42,"op":"get"}, op="set" with "key" and
//! "value", op="set_batch" with a sparse "changes" array (at most 20
//! distinct settings), op="persist" to retry saving already-applied settings,
//! or op="action" with "action". Unknown fields/keys are rejected.
//! Draft preview: {"v":1,"id":42,"op":"preview","changes":{"show_bar":false}}.
//! Uses the same validation as set_batch without persistence or publication.
//! Preview responses retain canonical settings and effective metadata; only the
//! preview image reflects the draft. They do not advance the revision.
//! State includes optional widget_preview_image (data:image/bmp;base64,...),
//! widget_preview_width and widget_preview_height in physical pixels, plus
//! widget_preview_dpi (integer; convert pixels to DIP with pixels * 96 / DPI). Sample
//! quotas are deterministic, not live usage. Outgoing frames allow up to 2 MiB;
//! incoming requests retain their 64 KiB bound.
//! IDs are unsigned 32-bit integers. Send one request at a time.
//! Replies: {"v":1,"id":42,"ok":true,"revision":N,"state":{...}} or
//! {"v":1,"id":42,"ok":false,"revision":N,"state":{...},"error":"...",
//! "error_code":"save_failed"}.
//! Errors include effective state: a save failure does not undo a live change
//! and is reported with error_code="save_failed". Retry persistence with op="persist".
//! Events: {"v":1,"event":"state","revision":N,"state":{...}},
//! {"v":1,"event":"activate","page":"about"}, {"v":1,"event":"shutdown"}.
//! Read continuously, including before sending get. EOF/shutdown/parent death
//! closes the companion; closing the companion never exits the widget.
//! Repeated activations coalesce to the latest page while starting/connected.
//! Connection deadline: 15 seconds. Shutdown grace: 500ms, then terminate child.
//!
//! Set keys (snake_case): language ("system" or supported language code),
//! startup, widget_visible, show_tray_icon, show_provider_icons,
//! show_claude_code, show_codex, show_antigravity,
//! show_session_window, show_weekly_window, adaptive_refresh, anchor_left,
//! settings_pane_open (booleans); poll_interval_ms (60000..=3600000 integer),
//! show_provider_icon (boolean, legacy show_provider_icons alias), show_percentage,
//! show_bar, show_reset_time (booleans; at least one of the latter three required),
//! usage_text_color (null for palette text defaults, or [u8,u8,u8]; tints marks
//! and percentages independently of provider bars), window_label_font_size
//! (8..=14 integer, default 9; regular-weight usage-window labels),
//! alert_threshold_percent (0|10|20|30); palette
//! (system|high_contrast_dark|high_contrast_light), bar_style
//! (continuous|segmented), bar_thickness (standard|slim), font_size
//! (standard|large), claude_accent/codex_accent (null or [u8,u8,u8]),
//! monitor_device (null for automatic/primary selection, or a device string
//! present in state.monitors).
//! Actions: refresh, update, uninstall, repository, exit, recommended,
//! reset_appearance, reset_colors, reset_position. URLs/paths/commands cannot
//! be supplied. Update/uninstall retain the widget's confirmation flows.
//!
//! State schema: {"settings": {all set keys above with their effective values},
//! "effective_language":string,"claude_code_available":bool,
//! "monitors":[{"device":string,"primary":bool}],"version":string,
//! "update_status":string,"install_channel":"portable"|"winget"}.
//! `settings.monitor_device` may be null before explicit selection.
//! Supported languages are supplied as `languages:[{code:string,name:string}]`.
//! Additional state: `effective_appearance:{dark:bool,claude_accent:[u8,u8,u8],
//! codex_accent:[u8,u8,u8]}`, `update_busy:bool`, and `update_available:bool`.
//! Malformed JSON, invalid framing/version/operation/key or unexpected fields
//! close the connection; valid requests with invalid values return ok=false.
//! The child is assigned to a kill-on-close job, including abrupt parent exits.
//! Revision is process-local and advances for published state changes; it is
//! not an optimistic concurrency token. Always replace local controls from state.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub const WM_SETTINGS: u32 = 0x8000 + 20;
const MAX_FRAME: usize = 65_536;
const MAX_RESPONSE_FRAME: usize = 2 * 1024 * 1024;
pub const MAX_BATCH_CHANGES: usize = 20;

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    General,
    Monitoring,
    Appearance,
    Placement,
    About,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Get,
    Set { key: Key, value: Value },
    SetBatch { changes: Vec<Change> },
    Preview { changes: Vec<Change> },
    Persist,
    Action { action: Action },
}

#[derive(Debug, Deserialize)]
pub struct Request {
    pub v: u32,
    pub id: u32,
    #[serde(flatten)]
    pub operation: Operation,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Language,
    Startup,
    WidgetVisible,
    ShowTrayIcon,
    ShowProviderIcons,
    ShowProviderIcon,
    ShowPercentage,
    ShowBar,
    ShowResetTime,
    UsageTextColor,
    WindowLabelFontSize,
    ShowClaudeCode,
    ShowCodex,
    ShowAntigravity,
    ShowSessionWindow,
    ShowWeeklyWindow,
    PollIntervalMs,
    AdaptiveRefresh,
    AlertThresholdPercent,
    Palette,
    BarStyle,
    BarThickness,
    FontSize,
    ClaudeAccent,
    CodexAccent,
    MonitorDevice,
    AnchorLeft,
    SettingsPaneOpen,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub key: Key,
    pub value: Value,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Refresh,
    Update,
    Uninstall,
    Repository,
    Exit,
    Recommended,
    ResetAppearance,
    ResetColors,
    ResetPosition,
}

pub struct Pending {
    pub request: Request,
    pub reply: mpsc::SyncSender<Value>,
    pub cancelled: Arc<AtomicBool>,
}

static QUEUE: Mutex<VecDeque<Pending>> = Mutex::new(VecDeque::new());
static ERRORS: Mutex<Option<String>> = Mutex::new(None);
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static REVISION: AtomicU64 = AtomicU64::new(0);

struct Session {
    pid: u32,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    page: Arc<Mutex<Option<Page>>>,
    state: Arc<Mutex<Option<Value>>>,
}

struct LaunchPayload {
    directory: PathBuf,
    _prepared: Option<crate::settings_payload::PreparedPayload>,
}

// Fields drop in declaration order, including on unwinding: reap the child and
// close its job before releasing the verified payload's file/directory locks.
struct PayloadChild {
    child: ChildGuard,
    _job: OwnedHandle,
    _payload: LaunchPayload,
}

fn prepare_launch_payload() -> Result<LaunchPayload, String> {
    #[cfg(debug_assertions)]
    let development =
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("settings-ui/bin/portable"));
    #[cfg(not(debug_assertions))]
    let development = None;
    resolve_launch_payload(crate::settings_payload::prepare, development)
}

fn resolve_launch_payload(
    prepare: impl FnOnce() -> Result<Option<crate::settings_payload::PreparedPayload>, String>,
    development: Option<PathBuf>,
) -> Result<LaunchPayload, String> {
    let prepared = prepare()?;
    let directory = match prepared.as_ref() {
        Some(payload) => payload.directory.clone(),
        None => development.ok_or("Native settings payload is missing. Reinstall the complete application to restore Settings. The usage widget can continue running.")?,
    };
    Ok(LaunchPayload {
        directory,
        _prepared: prepared,
    })
}

pub fn drain() -> Vec<Pending> {
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect()
}

pub fn take_error() -> Option<String> {
    ERRORS.lock().unwrap_or_else(|e| e.into_inner()).take()
}

pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

pub fn publish(state: Value) {
    let revision = REVISION.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some(session) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        *session.state.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(json!({"v":1,"event":"state","revision":revision,"state":state}));
    }
}

pub fn open(hwnd: isize, page: Page) -> Result<(), String> {
    open_with_payload(hwnd, page, prepare_launch_payload)
}

fn open_with_payload(
    hwnd: isize,
    page: Page,
    prepare: impl FnOnce() -> Result<LaunchPayload, String>,
) -> Result<(), String> {
    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    open_in_session(&mut session, hwnd, page, prepare)
}

fn open_in_session(
    session: &mut Option<Session>,
    hwnd: isize,
    page: Page,
    prepare: impl FnOnce() -> Result<LaunchPayload, String>,
) -> Result<(), String> {
    if let Some(existing) = session
        .as_ref()
        .filter(|s| s.running.load(Ordering::Acquire))
    {
        allow_foreground(existing.pid);
        *existing.page.lock().unwrap_or_else(|e| e.into_inner()) = Some(page);
        return Ok(());
    }
    // Activation of an existing child must neither extract again nor release its locks.
    // Errors leave the session empty/stopped so the next Settings click can retry.
    let payload = prepare()?;
    let directory = &payload.directory;
    let executable = directory.join("SettingsUI.exe");
    if !executable.is_file() {
        return Err(format!("SettingsUI.exe is missing from {}. Build and publish the native companion using settings-ui/BUILDING.md, or reinstall the complete application payload. Windows App Runtime 1.8 x64 is required; it is not installed automatically.", directory.display()));
    }
    let name = format!(
        r"\\.\pipe\CodexUsage.Settings.{}.{}",
        std::process::id(),
        REVISION.fetch_add(1, Ordering::Relaxed)
    );
    let pipe =
        create_pipe(&name).map_err(|e| format!("Cannot create private settings pipe: {e}"))?;
    let mut command = Command::new(executable);
    command
        .current_dir(&directory)
        .arg("--pipe")
        .arg(&name)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--page")
        .arg(serde_json::to_value(page).unwrap().as_str().unwrap());
    let launch_started = Instant::now();
    let child = command.spawn().map_err(|e| {
        format!("Cannot start settings: {e}. Check the companion executable and its permissions.")
    })?;
    let child = ChildGuard(child);
    let pid = child.0.id();
    allow_foreground(pid);
    let job = child_job(&child.0)
        .map_err(|e| format!("Cannot contain settings process lifetime: {e}"))?;
    let mut child = PayloadChild {
        child,
        _job: job,
        _payload: payload,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let activation = Arc::new(Mutex::new(Some(page)));
    let state = Arc::new(Mutex::new(None));
    *session = Some(Session {
        pid,
        stop: stop.clone(),
        running: running.clone(),
        page: activation.clone(),
        state: state.clone(),
    });
    std::thread::spawn(move || {
        let result = serve(
            &pipe,
            &mut child.child.0,
            pid,
            launch_started,
            hwnd,
            &stop,
            &activation,
            &state,
        );
        if let Err(error) = result {
            if !stop.load(Ordering::Acquire) {
                *ERRORS.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("Settings connection failed: {error}. The widget remains running; it is safe to close this message. See settings-ui/BUILDING.md."));
                notify(hwnd);
            }
        }
        drop(child);
        stop.store(true, Ordering::Release);
        running.store(false, Ordering::Release);
    });
    Ok(())
}

pub fn shutdown() {
    let guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(session) = guard.as_ref() {
        session.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while session.running.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone, Copy)]
enum ChildExitPhase {
    BeforePipeConnected,
    DuringActiveConnection { connected_after_ms: u128 },
}

fn format_windows_exit_code(code: Option<i32>) -> String {
    match code {
        Some(code) => {
            let code = code as u32;
            format!("{code} (0x{code:08X})")
        }
        None => "unavailable".into(),
    }
}

fn format_child_exit_error(
    pid: u32,
    code: Option<i32>,
    launch_elapsed_ms: u128,
    phase: ChildExitPhase,
) -> String {
    let phase_description = match phase {
        ChildExitPhase::BeforePipeConnected => {
            format!("before connecting to the widget pipe after {launch_elapsed_ms} ms")
        }
        ChildExitPhase::DuringActiveConnection { connected_after_ms } => format!(
            "during the active pipe connection after {launch_elapsed_ms} ms (connected after {connected_after_ms} ms)"
        ),
    };
    format!(
        "Settings companion process PID {pid} exited {phase_description} with Windows exit code {}. Check its Application Error/WER record. If it points to resource loading, confirm the five companion payload files come from one build; if it points to a bootstrap/dependency failure, verify Windows App Runtime 1.8 x64.",
        format_windows_exit_code(code)
    )
}

fn log_child_exit(pid: u32, code: Option<i32>, launch_elapsed_ms: u128, phase: ChildExitPhase) {
    let (phase_name, connected_after_ms) = match phase {
        ChildExitPhase::BeforePipeConnected => ("before_pipe_connected", None),
        ChildExitPhase::DuringActiveConnection { connected_after_ms } => {
            ("during_active_connection", Some(connected_after_ms))
        }
    };
    let connected_after_ms = connected_after_ms
        .map(|elapsed| elapsed.to_string())
        .unwrap_or_else(|| "not_connected".into());
    crate::diagnose::log(format!(
        "settings companion exited phase={phase_name} pid={pid} launch_elapsed_ms={launch_elapsed_ms} connected_after_ms={connected_after_ms} windows_exit_code={}",
        format_windows_exit_code(code)
    ));
}

fn allow_foreground(pid: u32) {
    // SAFETY: grant foreground activation only to our spawned settings process.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
    }
}

fn notify(hwnd: isize) {
    // SAFETY: PostMessage does not dereference the window handle or carry pointers.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            windows::Win32::Foundation::HWND(hwnd as *mut _),
            WM_SETTINGS,
            windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(0),
        );
    }
}

fn parse_request(bytes: &[u8]) -> Result<Request, String> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err("Invalid frame size".into());
    }
    #[derive(Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
    enum WireRequest {
        Get {
            v: u32,
            id: u32,
        },
        Set {
            v: u32,
            id: u32,
            key: Key,
            value: Value,
        },
        SetBatch {
            v: u32,
            id: u32,
            changes: Vec<Change>,
        },
        Preview {
            v: u32,
            id: u32,
            changes: std::collections::BTreeMap<String, Value>,
        },
        Persist {
            v: u32,
            id: u32,
        },
        Action {
            v: u32,
            id: u32,
            action: Action,
        },
    }
    let wire: WireRequest = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let request = match wire {
        WireRequest::Get { v, id } => Request {
            v,
            id,
            operation: Operation::Get,
        },
        WireRequest::Set { v, id, key, value } => Request {
            v,
            id,
            operation: Operation::Set { key, value },
        },
        WireRequest::SetBatch { v, id, changes } => {
            if changes.len() > MAX_BATCH_CHANGES {
                return Err(format!(
                    "A settings batch may contain at most {MAX_BATCH_CHANGES} changes"
                ));
            }
            Request {
                v,
                id,
                operation: Operation::SetBatch { changes },
            }
        }
        WireRequest::Preview { v, id, changes } => {
            if changes.len() > MAX_BATCH_CHANGES {
                return Err(format!(
                    "A settings preview may contain at most {MAX_BATCH_CHANGES} changes"
                ));
            }
            let changes = changes
                .into_iter()
                .map(|(key, value)| {
                    let key =
                        serde_json::from_value(Value::String(key)).map_err(|e| e.to_string())?;
                    Ok(Change { key, value })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Request {
                v,
                id,
                operation: Operation::Preview { changes },
            }
        }
        WireRequest::Persist { v, id } => Request {
            v,
            id,
            operation: Operation::Persist,
        },
        WireRequest::Action { v, id, action } => Request {
            v,
            id,
            operation: Operation::Action { action },
        },
    };
    if request.v != 1 {
        return Err("Unsupported protocol version".into());
    }
    Ok(request)
}

fn frame(value: &Value) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_FRAME {
        return Err(io::Error::other("Frame too large"));
    }
    let mut result = Vec::with_capacity(bytes.len() + 4);
    result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    result.extend(bytes);
    Ok(result)
}

fn payload_length(bytes: &[u8]) -> io::Result<Option<usize>> {
    if bytes.len() < 4 {
        return Ok(None);
    }
    let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::Error::other("Invalid frame length"));
    }
    Ok(Some(length))
}

fn serve(
    pipe: &OwnedHandle,
    child: &mut Child,
    pid: u32,
    launch_started: Instant,
    hwnd: isize,
    stop: &Arc<AtomicBool>,
    page: &Mutex<Option<Page>>,
    state: &Mutex<Option<Value>>,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let connected_after_ms = loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            let elapsed_ms = launch_started.elapsed().as_millis();
            let phase = ChildExitPhase::BeforePipeConnected;
            let code = status.code();
            log_child_exit(pid, code, elapsed_ms, phase);
            return Err(io::Error::other(format_child_exit_error(
                pid, code, elapsed_ms, phase,
            )));
        }
        // SAFETY: pipe is a live, non-inheritable named-pipe handle, in NOWAIT mode.
        let connected =
            unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0;
        let error = io::Error::last_os_error().raw_os_error();
        if connected || error == Some(535) {
            let mut client_pid = 0;
            // SAFETY: pid is writable, handle remains owned for the call.
            if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut client_pid) } == 0
                || client_pid != child.id()
            {
                unsafe {
                    DisconnectNamedPipe(pipe.as_raw_handle());
                }
            } else {
                break launch_started.elapsed().as_millis();
            }
        } else if error != Some(536) && error != Some(232) {
            return Err(io::Error::last_os_error());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(
                "Companion did not connect within 15 seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut incoming = Vec::new();
    let mut outgoing = Vec::<u8>::new();
    let mut written = 0;
    let mut pending: Option<mpsc::Receiver<Value>> = None;
    let mut closing = None;
    loop {
        if let Some(status) = child.try_wait()? {
            let elapsed_ms = launch_started.elapsed().as_millis();
            let phase = ChildExitPhase::DuringActiveConnection { connected_after_ms };
            let code = status.code();
            log_child_exit(pid, code, elapsed_ms, phase);
            if status.success() {
                return Ok(());
            }
            return Err(io::Error::other(format_child_exit_error(
                pid, code, elapsed_ms, phase,
            )));
        }
        if stop.load(Ordering::Acquire) && closing.is_none() {
            closing = Some(Instant::now() + Duration::from_millis(500));
            outgoing.extend(frame(&json!({"v":1,"event":"shutdown"}))?);
        }
        if closing.is_some_and(|end| Instant::now() >= end) {
            return Ok(());
        }
        if written == outgoing.len() && closing.is_none() {
            outgoing.clear();
            written = 0;
            if let Some(receiver) = pending.as_ref() {
                if let Ok(response) = receiver.try_recv() {
                    outgoing = frame(&response)?;
                    pending = None;
                }
            }
            if outgoing.is_empty() {
                if let Some(page) = page.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    outgoing = frame(&json!({"v":1,"event":"activate","page":page}))?;
                } else if let Some(event) = state.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    outgoing = frame(&event)?;
                }
            }
        }
        if written < outgoing.len() {
            let mut count = 0;
            // SAFETY: source and count are valid; NOWAIT bounds the call duration.
            let ok = unsafe {
                WriteFile(
                    pipe.as_raw_handle(),
                    outgoing[written..].as_ptr(),
                    (outgoing.len() - written) as u32,
                    &mut count,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 && io::Error::last_os_error().raw_os_error() != Some(232) {
                return Ok(());
            }
            written += count as usize;
        }
        if pending.is_none() && closing.is_none() {
            let mut bytes = [0u8; 4096];
            let mut count = 0;
            // Never read beyond this frame: a client sending many small frames
            // cannot grow an unbounded backlog while the UI handles one request.
            let target = payload_length(&incoming)?.map_or(4, |length| length + 4);
            let needed = target.saturating_sub(incoming.len()).min(bytes.len());
            // SAFETY: buffer and count are writable; the handle remains live.
            let ok = unsafe {
                ReadFile(
                    pipe.as_raw_handle(),
                    bytes.as_mut_ptr(),
                    needed as u32,
                    &mut count,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 && io::Error::last_os_error().raw_os_error() != Some(232) {
                return Ok(());
            }
            incoming.extend_from_slice(&bytes[..count as usize]);
            if incoming.len() >= 4 {
                let length = payload_length(&incoming)?.unwrap();
                if incoming.len() >= length + 4 {
                    let request =
                        parse_request(&incoming[4..length + 4]).map_err(io::Error::other)?;
                    incoming.drain(..length + 4);
                    let (reply, receiver) = mpsc::sync_channel(1);
                    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                    if queue.len() >= 16 {
                        return Err(io::Error::other("Settings queue is full"));
                    }
                    queue.push_back(Pending {
                        request,
                        reply,
                        cancelled: stop.clone(),
                    });
                    pending = Some(receiver);
                    drop(queue);
                    notify(hwnd);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

#[repr(C)]
#[derive(Default)]
struct BasicJobLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_processes: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}

#[repr(C)]
#[derive(Default)]
struct ExtendedJobLimits {
    basic: BasicJobLimits,
    io_counters: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

fn child_job(child: &Child) -> io::Result<OwnedHandle> {
    // SAFETY: the unnamed job is non-inheritable. Structures follow the SDK ABI;
    // the owned job remains open in the worker until the child has been reaped.
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(handle);
        let mut limits = ExtendedJobLimits::default();
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if SetInformationJobObject(
            handle,
            9,
            (&limits as *const ExtendedJobLimits).cast(),
            std::mem::size_of::<ExtendedJobLimits>() as u32,
        ) == 0
            || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

/// Raw FFI is confined here because the application's windows crate does not
/// enable the Pipes/Authorization features. OwnedHandle owns every kernel handle;
/// LocalFree releases only descriptors allocated by the security conversion API.
fn create_pipe(name: &str) -> io::Result<OwnedHandle> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: all buffers have the required alignment and lifetime; the token and
    // LocalAlloc allocations are released before returning, including failures.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut size = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        let result = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        );
        let _ = CloseHandle(token);
        result?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid.0, &mut sid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        while *sid.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
        LocalFree(sid.cast());
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{text})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor,
            inherit: 0,
        };
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // DUPLEX | FIRST_PIPE_INSTANCE, BYTE | NOWAIT | REJECT_REMOTE_CLIENTS.
        let handle = CreateNamedPipeW(
            name.as_ptr(),
            3 | 0x0008_0000,
            1 | 8,
            1,
            65540,
            65540,
            0,
            &attributes,
        );
        let error = io::Error::last_os_error();
        LocalFree(descriptor);
        if handle as isize == -1 {
            Err(error)
        } else {
            Ok(OwnedHandle::from_raw_handle(handle))
        }
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const SecurityAttributes, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(
        job: *mut c_void,
        class: u32,
        information: *const c_void,
        size: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn CreateNamedPipeW(
        name: *const u16,
        mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        security: *const SecurityAttributes,
    ) -> *mut c_void;
    fn ConnectNamedPipe(pipe: *mut c_void, overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(pipe: *mut c_void) -> i32;
    fn GetNamedPipeClientProcessId(pipe: *mut c_void, pid: *mut u32) -> i32;
    fn ReadFile(
        handle: *mut c_void,
        bytes: *mut u8,
        length: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn WriteFile(
        handle: *mut c_void,
        bytes: *const u8,
        length: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}
#[link(name = "advapi32")]
extern "system" {
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_failures_reject_only_the_launch_and_can_be_retried() {
        let mut session = None;
        let attempts = std::cell::Cell::new(0);
        for _ in 0..2 {
            let error = open_in_session(&mut session, 0, Page::General, || {
                attempts.set(attempts.get() + 1);
                Err("simulated extraction failure".into())
            })
            .unwrap_err();
            assert_eq!(error, "simulated extraction failure");
            assert!(
                session.is_none(),
                "Failed preparation must not install a session"
            );
        }
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn active_child_activation_does_not_prepare_payload_again() {
        let activation = Arc::new(Mutex::new(None));
        let mut session = Some(Session {
            pid: std::process::id(),
            stop: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(true)),
            page: activation.clone(),
            state: Arc::new(Mutex::new(None)),
        });
        open_in_session(&mut session, 0, Page::About, || {
            panic!("An active settings child already owns its prepared payload")
        })
        .unwrap();
        assert!(matches!(*activation.lock().unwrap(), Some(Page::About)));
    }

    #[test]
    fn payload_errors_never_fall_back_and_missing_release_payload_is_rejected() {
        let development = PathBuf::from("development-only");
        assert!(resolve_launch_payload(
            || Err("corrupt payload".into()),
            Some(development.clone())
        )
        .is_err());
        assert!(resolve_launch_payload(|| Ok(None), None).is_err());
        let payload = resolve_launch_payload(|| Ok(None), Some(development.clone())).unwrap();
        assert_eq!(payload.directory, development);
        assert!(payload._prepared.is_none());
    }
    #[test]
    fn strict_requests() {
        assert!(parse_request(br#"{"v":1,"id":1,"op":"get"}"#).is_ok());
        assert!(parse_request(
            br#"{"v":1,"id":1,"op":"set_batch","changes":[{"key":"show_codex","value":false}]}"#
        )
        .is_ok());
        assert!(parse_request(
            br#"{"v":1,"id":2,"op":"set_batch","changes":[{"key":"show_tray_icon","value":false},{"key":"show_provider_icons","value":false},{"key":"claude_accent","value":null}]}"#
        )
        .is_ok());
        assert!(parse_request(
            br#"{"v":1,"id":3,"op":"set_batch","changes":[{"key":"settings_pane_open","value":false}]}"#
        )
        .is_ok());
        assert!(parse_request(br#"{"v":1,"id":1,"op":"persist"}"#).is_ok());
        for input in [
            r#"{"v":2,"id":1,"op":"get"}"#,
            r#"{"v":1,"id":1,"op":"get","path":"x"}"#,
            r#"{"v":1,"id":1,"op":"action","action":"shell"}"#,
            r#"{"v":1,"id":1,"op":"set","key":"path","value":"x"}"#,
            r#"{"v":1,"id":1,"op":"set_batch","changes":[{"key":"show_codex","value":false,"extra":true}]}"#,
            r#"{"v":1,"id":1,"op":"set_batch","changes":[],"extra":true}"#,
        ] {
            assert!(parse_request(input.as_bytes()).is_err(), "{input}");
        }
        assert!(parse_request(&vec![b' '; MAX_FRAME + 1]).is_err());
        assert!(parse_request(&[0xff]).is_err());
        let too_many = json!({
            "v": 1,
            "id": 1,
            "op": "set_batch",
            "changes": (0..=MAX_BATCH_CHANGES).map(|_| json!({"key":"show_codex","value":true})).collect::<Vec<_>>()
        });
        assert!(parse_request(&serde_json::to_vec(&too_many).unwrap()).is_err());
    }
    #[test]
    fn little_endian_framing() {
        let value = json!({"v":1,"event":"shutdown"});
        let bytes = frame(&value).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize,
            bytes.len() - 4
        );
        assert_eq!(serde_json::from_slice::<Value>(&bytes[4..]).unwrap(), value);
        assert!(frame(&json!("x".repeat(MAX_FRAME))).is_ok());
        assert!(frame(&json!("x".repeat(MAX_RESPONSE_FRAME))).is_err());
    }

    #[test]
    fn frame_limits_are_checked_before_allocation() {
        for length in [0u32, MAX_FRAME as u32 + 1, u32::MAX] {
            assert!(payload_length(&length.to_le_bytes()).is_err());
        }
        assert_eq!(payload_length(&[0, 1, 0]).unwrap(), None);
        assert_eq!(
            payload_length(&(MAX_FRAME as u32).to_le_bytes()).unwrap(),
            Some(MAX_FRAME)
        );
    }

    #[test]
    fn windows_exit_codes_include_unsigned_decimal_and_hex_values() {
        assert_eq!(format_windows_exit_code(Some(0)), "0 (0x00000000)");
        assert_eq!(
            format_windows_exit_code(Some(0xC0000409_u32 as i32)),
            "3221226505 (0xC0000409)"
        );
        assert_eq!(format_windows_exit_code(None), "unavailable");
    }

    #[test]
    fn child_exit_errors_identify_phase_pid_and_elapsed_time() {
        let before_connect =
            format_child_exit_error(1234, Some(0), 17, ChildExitPhase::BeforePipeConnected);
        assert!(before_connect.contains("PID 1234"));
        assert!(before_connect.contains("before connecting to the widget pipe after 17 ms"));
        assert!(before_connect.contains("Windows exit code 0 (0x00000000)"));

        let during_connection = format_child_exit_error(
            5678,
            Some(0xC0000409_u32 as i32),
            501,
            ChildExitPhase::DuringActiveConnection {
                connected_after_ms: 23,
            },
        );
        assert!(during_connection.contains("PID 5678"));
        assert!(during_connection
            .contains("during the active pipe connection after 501 ms (connected after 23 ms)"));
        assert!(during_connection.contains("3221226505 (0xC0000409)"));
        assert!(during_connection.contains(
            "if it points to a bootstrap/dependency failure, verify Windows App Runtime 1.8 x64"
        ));

        let unavailable =
            format_child_exit_error(9012, None, 8, ChildExitPhase::BeforePipeConnected);
        assert!(unavailable.contains("Windows exit code unavailable"));
    }

    #[test]
    fn private_pipe_accepts_current_user_and_reports_client_pid() {
        use std::io::Write;
        let name = format!(r"\\.\pipe\CodexUsage.Settings.Test.{}", std::process::id());
        let pipe = create_pipe(&name).unwrap();
        // SAFETY: the server handle is valid and uses nonblocking mode.
        unsafe {
            ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut());
        }
        let mut client = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        let mut pid = 0;
        assert_ne!(
            unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) },
            0
        );
        assert_eq!(pid, std::process::id());
        client.write_all(&[1, 2, 3]).unwrap();
        let mut bytes = [0; 3];
        let mut count = 0;
        assert_ne!(
            unsafe {
                ReadFile(
                    pipe.as_raw_handle(),
                    bytes.as_mut_ptr(),
                    3,
                    &mut count,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(count, 3);
        assert_eq!(bytes, [1, 2, 3]);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn job_limits_match_windows_x64_abi() {
        assert_eq!(std::mem::size_of::<BasicJobLimits>(), 64);
        assert_eq!(std::mem::size_of::<ExtendedJobLimits>(), 144);
    }

    #[test]
    fn ipc_child_fixture() {
        use std::io::{Read, Write};
        let Some(name) = std::env::var_os("CODEX_SETTINGS_TEST_PIPE") else {
            return;
        };
        let mut pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap();
        let request = frame(&json!({"v":1,"id":7,"op":"get"})).unwrap();
        // Exercise fragmented headers and payloads across real kernel I/O.
        for part in request.chunks(3) {
            pipe.write_all(part).unwrap();
        }
        loop {
            let mut prefix = [0; 4];
            pipe.read_exact(&mut prefix).unwrap();
            let length = payload_length(&prefix).unwrap().unwrap();
            let mut bytes = vec![0; length];
            pipe.read_exact(&mut bytes).unwrap();
            let message: Value = serde_json::from_slice(&bytes).unwrap();
            if message.get("id") == Some(&json!(7)) {
                assert_eq!(message["state"]["answer"], 42);
                return;
            }
        }
    }

    #[test]
    fn authenticated_child_round_trips_through_bounded_queue() {
        let name = format!(
            r"\\.\pipe\CodexUsage.Settings.RoundTrip.{}",
            std::process::id()
        );
        let pipe = create_pipe(&name).unwrap();
        let launch_started = Instant::now();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "settings_ui::tests::ipc_child_fixture",
                "--nocapture",
            ])
            .env("CODEX_SETTINGS_TEST_PIPE", &name)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut child = ChildGuard(child);
        let pid = child.0.id();
        let job = child_job(&child.0).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let _job = job;
            let result = serve(
                &pipe,
                &mut child.0,
                pid,
                launch_started,
                0,
                &worker_stop,
                &Mutex::new(Some(Page::General)),
                &Mutex::new(None),
            );
            drop(pipe);
            result?;
            if !child.0.wait()?.success() {
                return Err(io::Error::other("IPC child assertions failed"));
            }
            Ok(())
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut replied = false;
        while Instant::now() < deadline && !worker.is_finished() {
            for pending in drain() {
                assert_eq!(pending.request.id, 7);
                pending
                    .reply
                    .send(json!({"v":1,"id":7,"ok":true,"revision":1,"state":{"answer":42}}))
                    .unwrap();
                replied = true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        assert!(replied, "Authenticated child did not deliver its request");
    }
    #[test]
    fn preview_requests_accept_only_a_bounded_known_changes_object() {
        let request = parse_request(br#"{"v":1,"id":7,"op":"preview","changes":{"show_bar":false,"usage_text_color":[1,2,3],"window_label_font_size":9}}"#).unwrap();
        let Operation::Preview { changes } = request.operation else {
            panic!("expected preview");
        };
        assert_eq!(changes.len(), 3);
        for wire in [
            br#"{"v":1,"id":7,"op":"preview","changes":{"arbitrary":false}}"#.as_slice(),
            br#"{"v":1,"id":7,"op":"preview","changes":[]}"#,
            br#"{"v":1,"id":7,"op":"preview","changes":{},"extra":true}"#,
            br#"{"v":2,"id":7,"op":"preview","changes":{}}"#,
        ] {
            assert!(parse_request(wire).is_err());
        }
        let changes: serde_json::Map<String, Value> = (0..=MAX_BATCH_CHANGES)
            .map(|i| (format!("key_{i}"), json!(true)))
            .collect();
        assert!(parse_request(
            &serde_json::to_vec(&json!({"v":1,"id":7,"op":"preview","changes":changes})).unwrap()
        )
        .is_err());
    }
}
