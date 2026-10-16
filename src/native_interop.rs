use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(test)]
use std::{cell::Cell, thread_local};

use crate::diagnose;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, FILETIME, HWND, LPARAM, POINT, RECT, SYSTEMTIME};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, ScreenToClient, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

// Window style constants
pub const WS_POPUP_STYLE: u32 = 0x80000000;
pub const WS_CHILD_STYLE: u32 = 0x40000000;
pub const WS_CLIPSIBLINGS_STYLE: u32 = 0x04000000;

// Win event constants
pub const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
pub const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;

// Timer IDs
pub const TIMER_POLL: usize = 1;
pub const TIMER_COUNTDOWN: usize = 2;
pub const TIMER_UPDATE_CHECK: usize = 4;
pub const TIMER_FRESHNESS: usize = 5;
pub const TIMER_TASKBAR_RETRY: usize = 6;

// Custom messages
pub const WM_APP: u32 = 0x8000;
pub const WM_APP_USAGE_UPDATED: u32 = WM_APP + 1;
pub const WM_APP_TRAY: u32 = WM_APP + 3;

const WINDOWS_TO_UNIX_EPOCH_SECONDS: u64 = 11_644_473_600;

pub fn system_time_to_local(value: SystemTime) -> Option<SYSTEMTIME> {
    let unix_seconds = value.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let file_ticks = unix_seconds
        .checked_add(WINDOWS_TO_UNIX_EPOCH_SECONDS)?
        .checked_mul(10_000_000)?;
    let file_time = FILETIME {
        dwLowDateTime: file_ticks as u32,
        dwHighDateTime: (file_ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        FileTimeToSystemTime(&file_time, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(local)
}

#[derive(Clone, Copy, Debug)]
pub struct TaskbarWindow {
    pub hwnd: HWND,
    pub rect: RECT,
}

/// An active display returned by EnumDisplayMonitors.
#[derive(Clone, Debug)]
pub struct DisplayMonitor {
    pub device: String,
    pub rect: RECT,
    pub is_primary: bool,
}

/// Enumerate active displays independently of whether Explorer created a
/// taskbar window for each one.
pub fn find_monitors() -> Vec<DisplayMonitor> {
    unsafe extern "system" fn enum_proc(
        monitor: windows::Win32::Graphics::Gdi::HMONITOR,
        _dc: windows::Win32::Graphics::Gdi::HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let monitors = &mut *(lparam.0 as *mut Vec<DisplayMonitor>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            return BOOL(1);
        }
        let len = info
            .szDevice
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(info.szDevice.len());
        let device = String::from_utf16_lossy(&info.szDevice[..len]);
        if !device.is_empty() {
            monitors.push(DisplayMonitor {
                device,
                rect: info.monitorInfo.rcMonitor,
                // MONITORINFOF_PRIMARY is 1 in the Win32 GDI contract.
                is_primary: info.monitorInfo.dwFlags & 1 != 0,
            });
        }
        BOOL(1)
    }

    let mut monitors: Vec<DisplayMonitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM(&mut monitors as *mut _ as isize),
        );
    }
    monitors.sort_by_key(|monitor| {
        (
            monitor.rect.top,
            monitor.rect.left,
            monitor.rect.bottom,
            monitor.rect.right,
        )
    });
    monitors
}

/// Windows display device name (for example \\.\DISPLAY2) for a taskbar.
/// Unlike enumeration order, this survives taskbar recreation and rearrangement.
pub fn taskbar_monitor_device(taskbar: &TaskbarWindow) -> Option<String> {
    unsafe {
        // Resolve the monitor from the taskbar HWND itself instead of a point
        // derived from its rectangle. Auto-hidden taskbars can have a clipped
        // or transient rectangle that puts its center outside the display.
        let monitor = MonitorFromWindow(taskbar.hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_invalid() {
            return None;
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            return None;
        }
        let len = info
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szDevice.len());
        Some(String::from_utf16_lossy(&info.szDevice[..len]))
    }
}

pub fn find_taskbars() -> Vec<TaskbarWindow> {
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let taskbars = &mut *(lparam.0 as *mut Vec<TaskbarWindow>);
        let mut class_name = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut class_name) };
        if len > 0 {
            let class_name = String::from_utf16_lossy(&class_name[..len as usize]);
            if class_name == "Shell_TrayWnd" || class_name == "Shell_SecondaryTrayWnd" {
                if let Some(rect) = get_taskbar_rect(hwnd).or_else(|| get_window_rect_safe(hwnd)) {
                    taskbars.push(TaskbarWindow { hwnd, rect });
                }
            }
        }
        BOOL(1)
    }

    let mut taskbars: Vec<TaskbarWindow> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut taskbars as *mut _ as isize));
    }
    taskbars.sort_by_key(|taskbar| {
        (
            taskbar.rect.top,
            taskbar.rect.left,
            taskbar.rect.bottom,
            taskbar.rect.right,
        )
    });
    taskbars
}

/// Find a child window by class name
pub fn find_child_window(parent: HWND, class_name: &str) -> Option<HWND> {
    unsafe {
        let class = wide_str(class_name);
        match FindWindowExW(
            parent,
            HWND::default(),
            PCWSTR::from_raw(class.as_ptr()),
            PCWSTR::null(),
        ) {
            Ok(h) if h != HWND::default() => Some(h),
            _ => None,
        }
    }
}

/// Query the taskbar without synchronously waiting for Explorer's message loop.
pub fn get_taskbar_rect(taskbar_hwnd: HWND) -> Option<RECT> {
    get_window_rect_safe(taskbar_hwnd)
}

/// Get the bounding rectangle of a window
pub fn get_window_rect_safe(hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            Some(rect)
        } else {
            None
        }
    }
}

/// Convert a screen-space point to the specified window's client coordinates.
pub fn screen_to_client(hwnd: HWND, x: i32, y: i32) -> Option<POINT> {
    unsafe {
        let mut point = POINT { x, y };
        ScreenToClient(hwnd, &mut point).as_bool().then_some(point)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowPosition {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy)]
struct WindowSnapshot {
    parent: HWND,
    style: i32,
    ex_style: i32,
    rect: RECT,
    visible: bool,
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_MODE_CHANGE_AFTER_PARENT_MUTATION: Cell<bool> = const { Cell::new(false) };
}

unsafe fn has_parent(hwnd: HWND, parent: Option<HWND>) -> bool {
    if !IsWindow(hwnd).as_bool() {
        return false;
    }
    match parent {
        Some(parent) => GetParent(hwnd).is_ok_and(|actual| actual == parent),
        // GetParent reports a null parent as an error in windows 0.58. IsWindow
        // above distinguishes that expected null from an invalid child HWND.
        None => GetParent(hwnd).is_err(),
    }
}

unsafe fn has_style(hwnd: HWND, expected: i32, is_child: bool) -> bool {
    let actual = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    let expected = expected as u32;
    if !is_child && expected & WS_POPUP_STYLE != 0 {
        actual & !WS_CLIPSIBLINGS_STYLE == expected & !WS_CLIPSIBLINGS_STYLE
    } else {
        actual == expected
    }
}

unsafe fn restore_window(hwnd: HWND, old: WindowSnapshot) -> bool {
    log_window_transition("rollback begin", hwnd, Some(old.parent));
    let old_parent = (!old.parent.is_invalid()).then_some(old.parent);
    let current_parent = GetParent(hwnd).unwrap_or_default();
    let parent_changed = current_parent != old.parent;
    let topmost_cleared = if old_parent.is_some() && current_parent.is_invalid() {
        let cleared = SetWindowPos(
            hwnd,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        );
        if diagnose::is_enabled() {
            diagnose::log(format!("rollback: HWND_NOTOPMOST result={cleared:?}"));
        }
        cleared.is_ok()
    } else {
        true
    };

    if GetWindowLongW(hwnd, GWL_STYLE) != old.style {
        let _ = SetWindowLongW(hwnd, GWL_STYLE, old.style);
    }
    if parent_changed && old_parent.is_none() {
        if old.ex_style & WS_EX_LAYERED.0 as i32 != 0 {
            let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, old.ex_style & !(WS_EX_LAYERED.0 as i32));
        }
        let restored_parent = SetParent(hwnd, HWND::default());
        if diagnose::is_enabled() {
            diagnose::log(format!("rollback: detach SetParent result={restored_parent:?}"));
        }
        if GetWindowLongW(hwnd, GWL_STYLE) != old.style {
            let _ = SetWindowLongW(hwnd, GWL_STYLE, old.style);
        }
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, old.ex_style);
    } else {
        if parent_changed
            && current_parent.is_invalid()
            && old.ex_style & WS_EX_LAYERED.0 as i32 != 0
        {
            let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, old.ex_style & !(WS_EX_LAYERED.0 as i32));
        }
        if GetWindowLongW(hwnd, GWL_EXSTYLE) != old.ex_style {
            let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, old.ex_style);
        }
        if parent_changed {
            let restored_parent = SetParent(hwnd, old.parent);
            if diagnose::is_enabled() {
                diagnose::log(format!("rollback: restore SetParent result={restored_parent:?}"));
            }
        }
    }
    let parent = has_parent(hwnd, old_parent);

    if old_parent.is_none() && old.ex_style & WS_EX_LAYERED.0 as i32 != 0 {
        let _ = SetLayeredWindowAttributes(
            hwnd,
            windows::Win32::Foundation::COLORREF(0),
            255,
            LWA_ALPHA,
        );
    }

    let mut point = POINT {
        x: old.rect.left,
        y: old.rect.top,
    };
    let coordinates = if old_parent.is_some() {
        ScreenToClient(old.parent, &mut point).as_bool()
    } else {
        true
    };
    let insert_after = if old.ex_style & WS_EX_TOPMOST.0 as i32 != 0 {
        HWND_TOPMOST
    } else if old_parent.is_some() {
        HWND_TOP
    } else {
        HWND_NOTOPMOST
    };
    let positioned = coordinates && {
        let result = SetWindowPos(
            hwnd,
            insert_after,
            point.x,
            point.y,
            old.rect.right - old.rect.left,
            old.rect.bottom - old.rect.top,
            SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        if diagnose::is_enabled() {
            diagnose::log(format!("rollback: final SetWindowPos result={result:?}"));
        }
        result.is_ok()
    };
    let style = has_style(hwnd, old.style, old_parent.is_some());
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) == old.ex_style;
    if old.visible {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    } else {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    let restored = style && ex_style && parent && positioned && topmost_cleared;
    if diagnose::is_enabled() {
        diagnose::log(format!("rollback result: restored={restored} style={style} ex_style={ex_style} parent={parent} positioned={positioned} topmost_cleared={topmost_cleared}"));
    }
    log_window_transition("rollback end", hwnd, Some(old.parent));
    restored
}

/// Snapshot only explicit recovery/transition boundaries, never routine paints.
pub fn log_window_transition(phase: &str, hwnd: HWND, target_parent: Option<HWND>) {
    if !diagnose::is_enabled() {
        return;
    }
    unsafe {
        diagnose::log(format!(
            "window transition: phase={phase} hwnd={hwnd:?} alive={} parent={:?} owner={:?} style={:#x} exstyle={:#x} target_parent={:?}",
            IsWindow(hwnd).as_bool(),
            GetParent(hwnd).unwrap_or_default(),
            GetWindow(hwnd, GW_OWNER).unwrap_or_default(),
            GetWindowLongW(hwnd, GWL_STYLE),
            GetWindowLongW(hwnd, GWL_EXSTYLE),
            target_parent.unwrap_or_default(),
        ));
    }
}

/// Apply a style change so it takes effect immediately. SetWindowLongW only
/// stores the new bits; without SWP_FRAMECHANGED the window manager can keep
/// acting on the previous state (observed as a stuck topmost band after a
/// Task View detach).
unsafe fn apply_style(hwnd: HWND, style: i32) -> bool {
    if has_style(hwnd, style, false) {
        return true;
    }
    let _ = SetWindowLongW(hwnd, GWL_STYLE, style);
    SetWindowPos(
        hwnd,
        None,
        0,
        0,
        0,
        0,
        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
    )
    .is_ok()
        && has_style(hwnd, style, false)
}

/// Clear a WS_EX_TOPMOST bit left behind by shell transitions (notably Task
/// View). SetWindowPos(HWND_NOTOPMOST) alone can report success while the bit
/// stays set on a window the shell is still settling; clearing the bit
/// explicitly and re-applying with SWP_FRAMECHANGED recovers. A stuck bit
/// poisons later UpdateLayeredWindow calls, so callers must fail rather than
/// attach a poisoned window. Returns true when the bit is gone.
unsafe fn clear_stuck_topmost(hwnd: HWND) -> bool {
    for _ in 0..3 {
        if GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as i32 == 0 {
            return true;
        }
        let cleared = GetWindowLongW(hwnd, GWL_EXSTYLE) & !(WS_EX_TOPMOST.0 as i32);
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, cleared);
        let repositioned = SetWindowPos(
            hwnd,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
        );
        if diagnose::is_enabled() {
            diagnose::log(format!(
                "topmost clear attempt: result={repositioned:?} exstyle={:#x}",
                GetWindowLongW(hwnd, GWL_EXSTYLE)
            ));
        }
        if repositioned.is_err() {
            return false;
        }
    }
    GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as i32 == 0
}

/// Clear incompatible constant-alpha state before retrying a per-pixel update.
pub fn reset_layered_surface(hwnd: HWND) -> bool {
    log_window_transition("surface reset begin", hwnd, None);
    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return false;
        }
        let parent = GetParent(hwnd).unwrap_or_default();
        if !parent.is_invalid() {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return false;
            }
            let old = WindowSnapshot {
                parent,
                style: GetWindowLongW(hwnd, GWL_STYLE),
                ex_style: GetWindowLongW(hwnd, GWL_EXSTYLE),
                rect,
                visible: IsWindowVisible(hwnd).as_bool(),
            };
            let mut point = POINT { x: rect.left, y: rect.top };
            if !ScreenToClient(parent, &mut point).as_bool() {
                return false;
            }
            let position = WindowPosition {
                x: rect.left,
                y: rect.top,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
            };
            // Explorer may refuse to reset the layered style on its child;
            // transition through top-level mode without applying constant alpha.
            if !set_window_mode_inner(hwnd, None, position, false)
                || !set_window_mode_inner(
                    hwnd,
                    Some(parent),
                    WindowPosition { x: point.x, y: point.y, ..position },
                    false,
                )
            {
                log_window_transition("surface reset transition failed", hwnd, Some(parent));
                let _ = restore_window(hwnd, old);
                return false;
            }
            log_window_transition("surface reset child complete", hwnd, Some(parent));
            return true;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let old = WindowSnapshot {
            parent: GetParent(hwnd).unwrap_or_default(),
            style: GetWindowLongW(hwnd, GWL_STYLE),
            ex_style: GetWindowLongW(hwnd, GWL_EXSTYLE),
            rect,
            visible: IsWindowVisible(hwnd).as_bool(),
        };
        let without_layered = old.ex_style & !(WS_EX_LAYERED.0 as i32);
        let position = || {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
            )
            .is_ok()
        };

        // Reset the layered style in place. Routing a child through floating
        // mode calls SetLayeredWindowAttributes, which makes subsequent
        // UpdateLayeredWindow calls incompatible with the per-pixel surface.
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, without_layered);
        log_window_transition("surface reset layered cleared", hwnd, None);
        if GetWindowLongW(hwnd, GWL_EXSTYLE) != without_layered || !position() {
            let _ = restore_window(hwnd, old);
            return false;
        }
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, old.ex_style);
        log_window_transition("surface reset layered restored", hwnd, None);
        if GetWindowLongW(hwnd, GWL_EXSTYLE) != old.ex_style || !position() {
            let _ = restore_window(hwnd, old);
            return false;
        }
        log_window_transition("surface reset popup complete", hwnd, None);
        true
    }
}

/// Change between an embedded child window and a floating popup, placing it in
/// the coordinate space of its new parent. Native failures restore the prior
/// parent, styles, rectangle, z-order, and visibility.
pub fn set_window_mode(hwnd: HWND, parent: Option<HWND>, position: WindowPosition) -> bool {
    set_window_mode_inner(hwnd, parent, position, true)
}

fn set_window_mode_inner(
    hwnd: HWND,
    parent: Option<HWND>,
    position: WindowPosition,
    set_constant_alpha: bool,
) -> bool {
    log_window_transition("mode change begin", hwnd, parent);
    unsafe {
        if !IsWindow(hwnd).as_bool() || position.width <= 0 || position.height <= 0 {
            diagnose::log(format!(
                "set_window_mode: rejected (window_alive={}, size={}x{})",
                IsWindow(hwnd).as_bool(),
                position.width,
                position.height
            ));
            return false;
        }
        if parent.is_some_and(|parent| !IsWindow(parent).as_bool()) {
            diagnose::log("set_window_mode: rejected (taskbar parent window is gone)");
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            diagnose::log(format!(
                "set_window_mode: GetWindowRect failed: {}",
                std::io::Error::last_os_error()
            ));
            return false;
        }
        let old_parent = GetParent(hwnd).unwrap_or_default();
        let new_parent = parent.unwrap_or_default();
        let parent_changed = old_parent != new_parent;
        let old = WindowSnapshot {
            parent: old_parent,
            style: GetWindowLongW(hwnd, GWL_STYLE),
            ex_style: GetWindowLongW(hwnd, GWL_EXSTYLE),
            rect,
            visible: IsWindowVisible(hwnd).as_bool(),
        };

        let (style, ex_style, insert_after) = if parent.is_some() {
            (
                ((old.style as u32 & !WS_POPUP_STYLE) | WS_CHILD_STYLE | WS_CLIPSIBLINGS_STYLE)
                    as i32,
                (old.ex_style & !WS_EX_TOPMOST.0 as i32)
                    | WS_EX_LAYERED.0 as i32
                    | WS_EX_TOOLWINDOW.0 as i32
                    | WS_EX_NOACTIVATE.0 as i32,
                HWND_TOP,
            )
        } else {
            // Taskbar visibility comes from the Explorer parent in child mode.
            // A floating fallback is ordinary non-activating UI, not a global
            // always-on-top window that can cover capture/system overlays.
            (
                ((old.style as u32 & !WS_CHILD_STYLE) | WS_POPUP_STYLE) as i32,
                (old.ex_style & !WS_EX_TOPMOST.0 as i32)
                    | WS_EX_LAYERED.0 as i32
                    | WS_EX_TOOLWINDOW.0 as i32
                    | WS_EX_NOACTIVATE.0 as i32,
                HWND_NOTOPMOST,
            )
        };

        // Detach while still WS_CHILD. Changing it to WS_POPUP first can
        // reinterpret the Explorer parent as an owner (including its topmost
        // band), and layered-style changes may be refused while attached.
        if parent_changed && old.style as u32 & WS_CHILD_STYLE != 0 {
            log_window_transition("before detach", hwnd, parent);
            let detached = SetParent(hwnd, HWND::default());
            if diagnose::is_enabled() {
                // A null previous parent is ambiguous in this API; retain the
                // raw binding result alongside the actual parent snapshot.
                diagnose::log(format!("mode change: detach SetParent result={detached:?}"));
            }
            log_window_transition("after detach", hwnd, parent);
            let popup_style = ((old.style as u32 & !WS_CHILD_STYLE) | WS_POPUP_STYLE) as i32;
            if !apply_style(hwnd, popup_style) {
                diagnose::log("set_window_mode: detached popup style refused");
                let _ = restore_window(hwnd, old);
                return false;
            }
            log_window_transition("after detached popup style", hwnd, parent);
            if !has_parent(hwnd, None) || !has_style(hwnd, popup_style, false) {
                diagnose::log("set_window_mode: detach verification failed");
                let _ = restore_window(hwnd, old);
                return false;
            }
            // The shell can leave WS_EX_TOPMOST behind on the detached window
            // (observed after Task View); carrying it along poisons the
            // surface, so clear it while top-level and re-verify the detach.
            if !clear_stuck_topmost(hwnd) || !has_parent(hwnd, None) {
                diagnose::log("set_window_mode: detached topmost state could not be cleared");
                let _ = restore_window(hwnd, old);
                return false;
            }
        }
        // Clear the z-order band while top-level, before setting WS_CHILD or
        // attaching to a topmost Explorer window. Clearing the bit alone does
        // not change the actual z-order band, and a reported success alone
        // does not prove the bit is gone.
        if parent_changed && !clear_stuck_topmost(hwnd) {
            diagnose::log("set_window_mode: pre-attach topmost state could not be cleared");
            let _ = restore_window(hwnd, old);
            return false;
        }

        if parent.is_some() {
            // Reset WS_EX_LAYERED while still top-level so the next
            // UpdateLayeredWindow call can replace the popup's alpha mode.
            if parent_changed
                && old.ex_style & WS_EX_LAYERED.0 as i32 != 0
            {
                let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, old.ex_style & !(WS_EX_LAYERED.0 as i32));
                if GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED.0 as i32 != 0 {
                    diagnose::log("set_window_mode: layered reset refused while detached");
                    let _ = restore_window(hwnd, old);
                    return false;
                }
            }
            if GetWindowLongW(hwnd, GWL_EXSTYLE) != ex_style {
                let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style);
            }
            if GetWindowLongW(hwnd, GWL_STYLE) != style {
                let _ = SetWindowLongW(hwnd, GWL_STYLE, style);
            }
        } else {
            if GetWindowLongW(hwnd, GWL_STYLE) != style {
                let _ = SetWindowLongW(hwnd, GWL_STYLE, style);
            }
            if GetWindowLongW(hwnd, GWL_EXSTYLE) != ex_style {
                let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style);
            }
        }
        let ex_style_before_position = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let expected_ex_style_before_position = ex_style & !(WS_EX_TOPMOST.0 as i32);
        if ex_style_before_position & !(WS_EX_TOPMOST.0 as i32) != expected_ex_style_before_position
            || (parent.is_some() && GetWindowLongW(hwnd, GWL_STYLE) != style)
        {
            diagnose::log("set_window_mode: style change refused before positioning");
            let _ = restore_window(hwnd, old);
            return false;
        }

        // Drop a topmost band picked up while attached before re-attaching:
        // the band comes from the Explorer parent, while an explicit bit on
        // the child breaks UpdateLayeredWindow.
        if parent.is_some()
            && GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as i32 != 0
            && !clear_stuck_topmost(hwnd)
        {
            diagnose::log("set_window_mode: pre-parent topmost state could not be cleared");
            let _ = restore_window(hwnd, old);
            return false;
        }

        // SetParent may return NULL both for success (the previous parent was
        // null) and failure. Verify the resulting parent instead of trusting
        // the windows crate's Result conversion of that ambiguous return.
        if parent_changed {
            let attached = SetParent(hwnd, new_parent);
            if diagnose::is_enabled() {
                diagnose::log(format!("mode change: target SetParent result={attached:?}"));
            }
            log_window_transition("after target SetParent", hwnd, parent);
        }
        if !has_parent(hwnd, parent) {
            diagnose::log(format!(
                "set_window_mode: SetParent verification failed: {}",
                std::io::Error::last_os_error()
            ));
            let _ = restore_window(hwnd, old);
            return false;
        }
        // Attaching to a topmost Explorer window can force the band bit back
        // onto the child. The band comes from the parent; an explicit bit on
        // the child breaks UpdateLayeredWindow, so clear and re-verify it.
        if parent.is_some()
            && GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as i32 != 0
            && !clear_stuck_topmost(hwnd)
        {
            diagnose::log("set_window_mode: attach forced an uncleared topmost bit");
            let _ = restore_window(hwnd, old);
            return false;
        }
        #[cfg(test)]
        if FAIL_NEXT_MODE_CHANGE_AFTER_PARENT_MUTATION.with(|fail| fail.replace(false)) {
            let _ = restore_window(hwnd, old);
            return false;
        }
        if parent.is_none() && GetWindowLongW(hwnd, GWL_STYLE) != style {
            let _ = SetWindowLongW(hwnd, GWL_STYLE, style);
        }
        if GetWindowLongW(hwnd, GWL_STYLE) != style {
            diagnose::log("set_window_mode: child style verification failed");
            let _ = restore_window(hwnd, old);
            return false;
        }
        if set_constant_alpha
            && parent.is_none()
            && SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0),
                255,
                LWA_ALPHA,
            )
            .is_err()
        {
            diagnose::log(format!(
                "set_window_mode: SetLayeredWindowAttributes failed: {}",
                std::io::Error::last_os_error()
            ));
            let _ = restore_window(hwnd, old);
            return false;
        }

        let positioned = SetWindowPos(
            hwnd,
            insert_after,
            position.x,
            position.y,
            position.width,
            position.height,
            SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        if diagnose::is_enabled() {
            diagnose::log(format!("mode change: final SetWindowPos insert_after={insert_after:?} result={positioned:?}"));
        }
        log_window_transition("after final SetWindowPos", hwnd, parent);
        if let Err(error) = positioned {
            diagnose::log(format!(
                "set_window_mode: final SetWindowPos failed: {error}"
            ));
            let _ = restore_window(hwnd, old);
            return false;
        }
        if GetWindowLongW(hwnd, GWL_EXSTYLE) != ex_style || GetWindowLongW(hwnd, GWL_STYLE) != style
        {
            log_window_transition("final verification failed", hwnd, parent);
            diagnose::log(format!(
                "set_window_mode: final style verification failed (exstyle_actual={:#x} expected={:#x} style_actual={:#x} expected={:#x})",
                GetWindowLongW(hwnd, GWL_EXSTYLE),
                ex_style,
                GetWindowLongW(hwnd, GWL_STYLE),
                style
            ));
            let _ = restore_window(hwnd, old);
            return false;
        }
        log_window_transition("mode change complete", hwnd, parent);
        true
    }
}

/// Reposition without activating or raising the widget over transient UI.
pub fn position_window(hwnd: HWND, _parent: Option<HWND>, position: WindowPosition) -> bool {
    if position.width <= 0 || position.height <= 0 {
        return false;
    }
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            position.x,
            position.y,
            position.width,
            position.height,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
        )
        .is_ok()
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    use windows::core::w;
    use windows::Win32::Foundation::{COLORREF, POINT, SIZE};
    use windows::Win32::Graphics::Gdi::*;

    #[test]
    fn repeated_placement_preserves_floating_and_child_z_order() {
        unsafe {
            let widget = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("placement-test"),
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
            let position = WindowPosition {
                x: 10,
                y: 20,
                width: 100,
                height: 30,
            };
            assert!(set_window_mode(widget, None, position));
            let overlay = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("overlay-test"),
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
            let previous = GetWindow(widget, GW_HWNDPREV).unwrap();
            for offset in 0..5 {
                assert!(position_window(
                    widget,
                    None,
                    WindowPosition {
                        x: 10 + offset,
                        ..position
                    }
                ));
                assert_eq!(
                    GetWindowLongW(widget, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                    0
                );
                assert_eq!(GetWindow(widget, GW_HWNDPREV).unwrap(), previous);
            }
            assert!(set_window_mode(widget, Some(overlay), position));
            let sibling = CreateWindowExW(
                WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("sibling-test"),
                WS_CHILD,
                0,
                0,
                100,
                30,
                overlay,
                None,
                None,
                None,
            )
            .unwrap();
            SetWindowPos(
                sibling,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            )
            .unwrap();
            let previous = GetWindow(widget, GW_HWNDPREV).unwrap_or_default();
            assert_eq!(previous, sibling);
            for offset in 0..5 {
                assert!(position_window(
                    widget,
                    Some(overlay),
                    WindowPosition {
                        x: 10 + offset,
                        ..position
                    }
                ));
                assert_eq!(GetWindow(widget, GW_HWNDPREV).unwrap_or_default(), previous);
            }
            let _ = DestroyWindow(sibling);
            let _ = DestroyWindow(widget);
            let _ = DestroyWindow(overlay);
        }
    }

    unsafe fn update_test_layered_surface(hwnd: HWND) -> windows::core::Result<()> {
        let mut client_rect = RECT::default();
        GetClientRect(hwnd, &mut client_rect)?;
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;
        assert!(
            width > 0 && height > 0,
            "test child has an empty client area"
        );
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: 40,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .expect("CreateDIBSection should create a test surface");
        std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize)
            .fill(0xffff_ffff);
        let old = SelectObject(dc, bitmap);
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
            ..Default::default()
        };
        let rendered = UpdateLayeredWindow(
            hwnd,
            None,
            None,
            None,
            dc,
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        let _ = SelectObject(dc, old);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(dc);
        rendered
    }

    #[test]
    fn startup_popup_can_switch_to_layered_child_rendering() {
        unsafe {
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("attachment-test"),
                WS_POPUP,
                0,
                0,
                20,
                20,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let child = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("widget-test"),
                WS_POPUP,
                0,
                0,
                2,
                2,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            SetLayeredWindowAttributes(child, COLORREF(0), 255, LWA_ALPHA).unwrap();
            assert!(set_window_mode(
                child,
                Some(parent),
                WindowPosition {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                }
            ));
            assert_eq!(GetParent(child).unwrap(), parent);
            update_test_layered_surface(child).unwrap();
            let style_before = GetWindowLongW(child, GWL_STYLE);
            let extended_style_before = GetWindowLongW(child, GWL_EXSTYLE);
            let mut rect_before = RECT::default();
            GetWindowRect(child, &mut rect_before).unwrap();
            SetLayeredWindowAttributes(child, COLORREF(0), 255, LWA_ALPHA).unwrap();
            assert!(update_test_layered_surface(child).is_err());
            assert!(reset_layered_surface(child));
            update_test_layered_surface(child).unwrap();
            assert_eq!(GetParent(child).unwrap(), parent);
            assert_eq!(GetWindowLongW(child, GWL_STYLE), style_before);
            assert_eq!(GetWindowLongW(child, GWL_EXSTYLE), extended_style_before);
            let mut rect_after = RECT::default();
            GetWindowRect(child, &mut rect_after).unwrap();
            assert_eq!(rect_after, rect_before);
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: 40,
                    biWidth: 2,
                    biHeight: -2,
                    biPlanes: 1,
                    biBitCount: 32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            std::slice::from_raw_parts_mut(bits as *mut u32, 4).fill(0xffffffff);
            let old = SelectObject(dc, bitmap);
            let point = POINT { x: 0, y: 0 };
            let size = SIZE { cx: 2, cy: 2 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
                ..Default::default()
            };
            let rendered = UpdateLayeredWindow(
                child,
                None,
                Some(&point),
                Some(&size),
                dc,
                Some(&point),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            let _ = SelectObject(dc, old);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(dc);
            assert!(
                rendered.is_ok(),
                "fallback alpha mode must not prevent taskbar rendering: {rendered:?}"
            );

            let placement = WindowPosition {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            };
            assert!(set_window_mode(child, None, placement));
            assert!(set_window_mode(child, Some(parent), placement));
            let second_render = update_test_layered_surface(child);
            assert!(
                second_render.is_ok(),
                "UpdateLayeredWindow must work after a second child re-embedding: {second_render:?}"
            );

            let _ = DestroyWindow(child);
            let _ = DestroyWindow(parent);
        }
    }

    #[test]
    fn failed_window_mode_transition_restores_popup_styles_and_parent() {
        unsafe {
            let popup = CreateWindowExW(
                WS_EX_LAYERED,
                w!("STATIC"),
                w!("attachment-failure-test"),
                WS_POPUP,
                0,
                0,
                20,
                20,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let original_style = GetWindowLongW(popup, GWL_STYLE);
            let original_ex_style = GetWindowLongW(popup, GWL_EXSTYLE);

            assert!(!set_window_mode(
                popup,
                Some(HWND(std::ptr::dangling_mut::<std::ffi::c_void>())),
                WindowPosition {
                    x: 0,
                    y: 0,
                    width: 20,
                    height: 20,
                }
            ));
            assert_eq!(GetParent(popup).unwrap_or_default(), HWND::default());
            assert_eq!(GetWindowLongW(popup, GWL_STYLE), original_style);
            assert_eq!(GetWindowLongW(popup, GWL_EXSTYLE), original_ex_style);

            let _ = DestroyWindow(popup);
        }
    }

    #[test]
    fn post_mutation_window_mode_failure_restores_geometry_visibility_and_redraw() {
        unsafe {
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("post-mutation-parent"),
                WS_POPUP,
                0,
                0,
                80,
                80,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let popup = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("post-mutation-popup"),
                WS_POPUP,
                127,
                139,
                24,
                26,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            SetLayeredWindowAttributes(popup, COLORREF(0), 255, LWA_ALPHA).unwrap();
            let _ = ShowWindow(popup, SW_SHOWNOACTIVATE);

            let original_style = GetWindowLongW(popup, GWL_STYLE);
            let original_ex_style = GetWindowLongW(popup, GWL_EXSTYLE);
            let mut original_rect = RECT::default();
            GetWindowRect(popup, &mut original_rect).unwrap();
            assert!(IsWindowVisible(popup).as_bool());

            FAIL_NEXT_MODE_CHANGE_AFTER_PARENT_MUTATION.with(|fail| fail.set(true));
            assert!(!set_window_mode(
                popup,
                Some(parent),
                WindowPosition {
                    x: 7,
                    y: 9,
                    width: 28,
                    height: 30,
                }
            ));

            assert!(GetParent(popup).is_err());
            assert_eq!(GetWindowLongW(popup, GWL_STYLE), original_style);
            assert_eq!(GetWindowLongW(popup, GWL_EXSTYLE), original_ex_style);
            let mut restored_rect = RECT::default();
            GetWindowRect(popup, &mut restored_rect).unwrap();
            assert_eq!(restored_rect, original_rect);
            assert!(IsWindowVisible(popup).as_bool());

            assert!(InvalidateRect(popup, None, false).as_bool());
            assert!(UpdateWindow(popup).as_bool());

            let _ = DestroyWindow(popup);
            let _ = DestroyWindow(parent);
        }
    }

    #[test]
    fn popup_child_popup_child_round_trip_preserves_visibility_and_render_modes() {
        unsafe {
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("round-trip-parent"),
                WS_POPUP,
                100,
                120,
                80,
                80,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let mut screen_point = POINT { x: 7, y: 9 };
            assert!(ClientToScreen(parent, &mut screen_point).as_bool());
            let client_point = screen_to_client(parent, screen_point.x, screen_point.y).unwrap();
            assert_eq!((client_point.x, client_point.y), (7, 9));
            let child = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("round-trip-child"),
                WS_POPUP,
                0,
                0,
                20,
                20,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            SetLayeredWindowAttributes(child, COLORREF(0), 255, LWA_ALPHA).unwrap();
            let _ = ShowWindow(parent, SW_SHOWNOACTIVATE);
            let _ = ShowWindow(child, SW_SHOWNOACTIVATE);

            let embedded = WindowPosition {
                x: 4,
                y: 5,
                width: 21,
                height: 22,
            };
            let floating = WindowPosition {
                x: -1400,
                y: 200,
                width: 23,
                height: 24,
            };
            assert!(set_window_mode(child, Some(parent), embedded));
            assert_eq!(GetParent(child).unwrap(), parent);
            assert_ne!(GetWindowLongW(child, GWL_STYLE) as u32 & WS_CHILD_STYLE, 0);
            assert_eq!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0,
                WS_EX_LAYERED.0
            );
            assert_eq!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                0
            );
            assert_ne!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_NOACTIVATE.0,
                0
            );
            assert!(IsWindowVisible(child).as_bool());

            let child_style = GetWindowLongW(child, GWL_STYLE);
            let child_ex_style = GetWindowLongW(child, GWL_EXSTYLE);
            let mut child_rect = RECT::default();
            GetWindowRect(child, &mut child_rect).unwrap();
            assert!(!set_window_mode(
                child,
                Some(HWND(std::ptr::dangling_mut::<std::ffi::c_void>())),
                WindowPosition {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                }
            ));
            assert_eq!(GetParent(child).unwrap(), parent);
            assert_eq!(GetWindowLongW(child, GWL_STYLE), child_style);
            assert_eq!(GetWindowLongW(child, GWL_EXSTYLE), child_ex_style);
            let mut restored_rect = RECT::default();
            GetWindowRect(child, &mut restored_rect).unwrap();
            assert_eq!(restored_rect, child_rect);

            assert!(set_window_mode(child, None, floating));
            assert!(GetParent(child).is_err());
            assert_eq!(
                GetWindowLongW(child, GWL_STYLE) as u32 & WS_POPUP_STYLE,
                WS_POPUP_STYLE
            );
            assert_eq!(GetWindowLongW(child, GWL_STYLE) as u32 & WS_CHILD_STYLE, 0);
            assert_ne!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0,
                0
            );
            assert_eq!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                0
            );
            assert_ne!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0,
                0
            );
            assert_ne!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_NOACTIVATE.0,
                0
            );
            assert!(IsWindowVisible(child).as_bool());

            assert!(set_window_mode(child, Some(parent), embedded));
            assert_eq!(GetParent(child).unwrap(), parent);
            assert_eq!(
                GetWindowLongW(child, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0,
                WS_EX_LAYERED.0
            );
            assert!(IsWindowVisible(child).as_bool());

            let _ = ShowWindow(child, SW_HIDE);
            assert!(set_window_mode(child, None, floating));
            assert!(!IsWindowVisible(child).as_bool());
            assert!(set_window_mode(child, Some(parent), embedded));
            assert!(!IsWindowVisible(child).as_bool());

            let _ = DestroyWindow(child);
            let _ = DestroyWindow(parent);
        }
    }
}

#[cfg(test)]
mod taskbar_selection_tests {
    use super::*;
    use windows::core::w;

    #[test]
    fn taskbar_monitor_device_comes_from_the_taskbar_window() {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("monitor-resolution-test"),
                WS_POPUP,
                0,
                0,
                20,
                20,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let taskbar = TaskbarWindow {
                hwnd,
                rect: RECT::default(),
            };
            let rect_independent = TaskbarWindow {
                rect: RECT {
                    left: 100_000,
                    top: 100_000,
                    right: 100_001,
                    bottom: 100_001,
                },
                ..taskbar
            };

            assert_eq!(
                taskbar_monitor_device(&taskbar),
                taskbar_monitor_device(&rect_independent)
            );
            let _ = DestroyWindow(hwnd);
        }
    }
}

/// Set up a WinEvent hook for tray location changes
pub fn set_tray_event_hook(
    thread_id: u32,
    callback: unsafe extern "system" fn(HWINEVENTHOOK, u32, HWND, i32, i32, u32, u32),
) -> Option<HWINEVENTHOOK> {
    unsafe {
        let hook = SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(callback),
            0,
            thread_id,
            WINEVENT_OUTOFCONTEXT,
        );
        if hook.is_invalid() {
            None
        } else {
            Some(hook)
        }
    }
}

/// Get the thread ID that owns a window
pub fn get_window_thread_id(hwnd: HWND) -> u32 {
    unsafe { GetWindowThreadProcessId(hwnd, None) }
}

/// Unhook a WinEvent hook
pub fn unhook_win_event(hook: HWINEVENTHOOK) {
    unsafe {
        let _ = UnhookWinEvent(hook);
    }
}

/// Convert a Rust string to a null-terminated wide string
pub fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// COLORREF wrapper (RGB packed into u32)
pub fn colorref(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// Color helper
#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    #[allow(dead_code)]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn from_hex(hex: &str) -> Self {
        let hex = hex.trim_start_matches('#');
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        Self { r, g, b }
    }

    pub fn to_colorref(self) -> u32 {
        colorref(self.r, self.g, self.b)
    }
}
