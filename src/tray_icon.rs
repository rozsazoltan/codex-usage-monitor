use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::UI::Shell::{
    ExtractIconExW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::native_interop::WM_APP_TRAY;

const APP_TRAY_ICON_ID: u32 = 1;
const LEGACY_CODEX_TRAY_ICON_ID: u32 = 2;
const LEGACY_ANTIGRAVITY_TRAY_ICON_ID: u32 = 3;

#[derive(Clone, Copy)]
struct Registration {
    hwnd: isize,
    // The shell may continue using this handle after Shell_NotifyIconW returns.
    // Keep it alive until the icon is deleted or the window is destroyed.
    icon: isize,
}

static REGISTRATION: std::sync::Mutex<Option<Registration>> = std::sync::Mutex::new(None);

/// Menu item ID for toggling widget visibility (used by window.rs context menu).
pub const IDM_TOGGLE_WIDGET: u16 = 70;

/// Actions the tray message handler can request from the main window.
pub enum TrayAction {
    None,
    ToggleWidget,
    ShowContextMenu,
}

/// Identifies the provider that originated a notification. All notifications
/// now use the one application tray icon rather than provider-specific icons.
#[derive(Clone, Copy)]
pub enum TrayIconKind {
    Claude,
    Codex,
    Antigravity,
}

pub struct TrayIconData {
    pub tooltip: String,
}

/// Load the same embedded icon used by the executable and desktop shortcut.
pub fn create_icon() -> HICON {
    load_embedded_app_icon()
}

fn load_embedded_app_icon() -> HICON {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            crate::diagnose::log_error(
                "tray icon executable path could not be read",
                std::io::Error::last_os_error(),
            );
            return HICON::default();
        }

        let mut small_icon = HICON::default();
        let mut large_icon = HICON::default();
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(exe_buf.as_ptr()),
            0,
            Some(&mut large_icon),
            Some(&mut small_icon),
            1,
        );

        if extracted == 0 {
            crate::diagnose::log_error(
                "embedded tray icon could not be extracted",
                std::io::Error::last_os_error(),
            );
            return HICON::default();
        }

        if !small_icon.is_invalid() {
            if !large_icon.is_invalid() {
                let _ = DestroyIcon(large_icon);
            }
            small_icon
        } else {
            large_icon
        }
    }
}

/// Show a Windows balloon notification from the single application tray icon.
pub fn notify_balloon(hwnd: HWND, _kind: TrayIconKind, title: &str, message: &str) {
    if !is_registered(hwnd) {
        return;
    }
    unsafe {
        let mut nid = notify_data(hwnd, APP_TRAY_ICON_ID);
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_WARNING;
        copy_wide(title, &mut nid.szInfoTitle);
        copy_wide(message, &mut nid.szInfo);
        if !Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
            log_shell_failure("NIM_MODIFY balloon");
        }
    }
}

fn copy_wide<const N: usize>(s: &str, buf: &mut [u16; N]) {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let mut len = wide.len().min(N - 1);
    if len > 0 && (0xD800..=0xDBFF).contains(&wide[len - 1]) {
        len -= 1;
    }
    buf[..len].copy_from_slice(&wide[..len]);
    buf[len] = 0;
}

unsafe fn notify_data(hwnd: HWND, id: u32) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = id;
    nid
}

fn log_shell_failure(operation: &str) {
    crate::diagnose::log(format!(
        "Shell_NotifyIconW {operation} failed: {}",
        std::io::Error::last_os_error()
    ));
}

fn is_registered(hwnd: HWND) -> bool {
    REGISTRATION
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|registration| registration.hwnd == hwnd.0 as isize)
}

fn icon_data(hwnd: HWND, tooltip: &str, icon: HICON) -> NOTIFYICONDATAW {
    unsafe {
        let mut nid = notify_data(hwnd, APP_TRAY_ICON_ID);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = icon;
        copy_wide(tooltip, &mut nid.szTip);
        nid
    }
}

fn add(hwnd: HWND, tooltip: &str) {
    let icon = create_icon();
    if icon.is_invalid() {
        crate::diagnose::log("tray icon registration skipped: no valid application icon");
        return;
    }
    let nid = icon_data(hwnd, tooltip, icon);
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() };
    if added {
        *REGISTRATION
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Registration {
            hwnd: hwnd.0 as isize,
            icon: icon.0 as isize,
        });
    } else {
        log_shell_failure("NIM_ADD application icon");
        unsafe {
            let _ = DestroyIcon(icon);
        }
    }
}

fn update(hwnd: HWND, tooltip: &str, icon: HICON) -> bool {
    let mut nid = icon_data(hwnd, tooltip, icon);
    nid.uFlags = NIF_TIP;
    let modified = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() };
    if modified {
        return true;
    }
    log_shell_failure("NIM_MODIFY application icon");

    // Explorer may have restarted and forgotten an otherwise-live icon.
    // NIM_ADD restores it while reusing the still-owned HICON.
    let added = unsafe {
        let nid = icon_data(hwnd, tooltip, icon);
        Shell_NotifyIconW(NIM_ADD, &nid).as_bool()
    };
    if added {
        return true;
    }
    log_shell_failure("NIM_ADD application icon after modify failure");
    false
}

fn remove_id(hwnd: HWND, id: u32, report_failure: bool) -> bool {
    unsafe {
        let nid = notify_data(hwnd, id);
        let removed = Shell_NotifyIconW(NIM_DELETE, &nid).as_bool();
        if !removed && report_failure {
            log_shell_failure("NIM_DELETE application icon");
        }
        removed
    }
}

pub fn sync(hwnd: HWND, icon: Option<&TrayIconData>) {
    // Remove provider-specific icons left by versions before v1.9.
    remove_id(hwnd, LEGACY_CODEX_TRAY_ICON_ID, false);
    remove_id(hwnd, LEGACY_ANTIGRAVITY_TRAY_ICON_ID, false);

    let mut current = *REGISTRATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(registration) = current {
        if registration.hwnd != hwnd.0 as isize {
            let old_hwnd = HWND(registration.hwnd as *mut _);
            remove_id(old_hwnd, APP_TRAY_ICON_ID, true);
            unsafe {
                let _ = DestroyIcon(HICON(registration.icon as *mut _));
            }
            *REGISTRATION
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
            current = None;
        }
    }

    match (icon, current) {
        (Some(icon_data), Some(current)) => {
            let handle = HICON(current.icon as *mut _);
            let _ = update(hwnd, &icon_data.tooltip, handle);
        }
        (Some(icon_data), None) => add(hwnd, &icon_data.tooltip),
        (None, Some(current)) => {
            if remove_id(hwnd, APP_TRAY_ICON_ID, true) {
                let removed = {
                    let mut registration = REGISTRATION
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if registration.is_some_and(|registered| registered.icon == current.icon) {
                        registration.take()
                    } else {
                        None
                    }
                };
                if let Some(removed) = removed {
                    unsafe {
                        let _ = DestroyIcon(HICON(removed.icon as *mut _));
                    }
                }
            }
        }
        (None, None) => {
            remove_id(hwnd, APP_TRAY_ICON_ID, false);
        }
    }
}

pub fn remove_all(hwnd: HWND) {
    sync(hwnd, None);
    remove_id(hwnd, LEGACY_CODEX_TRAY_ICON_ID, false);
    remove_id(hwnd, LEGACY_ANTIGRAVITY_TRAY_ICON_ID, false);
}

/// Interpret a tray callback message and return the action to take.
pub fn handle_message(lparam: LPARAM) -> TrayAction {
    let mouse_msg = lparam.0 as u32;
    match mouse_msg {
        WM_LBUTTONUP => TrayAction::ToggleWidget,
        WM_RBUTTONUP => TrayAction::ShowContextMenu,
        _ => TrayAction::None,
    }
}
