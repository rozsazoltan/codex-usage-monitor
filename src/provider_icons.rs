use std::sync::OnceLock;

use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DrawIconEx, DI_NORMAL, HICON, LR_DEFAULTCOLOR,
};

use crate::native_interop::Color;

pub const SIZE: i32 = 16;
pub const RIGHT_MARGIN: i32 = 5;

#[derive(Clone, Copy)]
pub enum Provider {
    Claude,
    Codex,
    Antigravity,
    OpenCode,
    Cursor,
    Grok,
    Copilot,
}

pub fn codex_color(is_dark: bool) -> Color {
    Color::from_hex(if is_dark { "#34D399" } else { "#0F8F70" })
}

pub fn draw(hdc: HDC, x: i32, y: i32, size: i32, provider: Provider, tint: Option<[u8; 3]>) {
    // These two handles live for the process lifetime. DrawIconEx scales the
    // embedded 64px artwork to the current monitor's DPI without disk access.
    static CLAUDE: OnceLock<isize> = OnceLock::new();
    static CODEX: OnceLock<isize> = OnceLock::new();
    static ANTIGRAVITY: OnceLock<isize> = OnceLock::new();
    static OPENCODE: OnceLock<isize> = OnceLock::new();
    static CURSOR: OnceLock<isize> = OnceLock::new();
    static GROK: OnceLock<isize> = OnceLock::new();
    static COPILOT: OnceLock<isize> = OnceLock::new();
    let (cache, png): (&OnceLock<isize>, &[u8]) = match provider {
        Provider::Claude => (&CLAUDE, include_bytes!("icons/providers/claude.png")),
        Provider::Codex => (&CODEX, include_bytes!("icons/providers/openai.png")),
        Provider::Antigravity => (
            &ANTIGRAVITY,
            include_bytes!("icons/providers/antigravity.png"),
        ),
        Provider::OpenCode => (&OPENCODE, include_bytes!("icons/providers/opencode.png")),
        Provider::Cursor => (&CURSOR, include_bytes!("icons/providers/cursor.png")),
        Provider::Grok => (&GROK, include_bytes!("icons/providers/grok.png")),
        Provider::Copilot => (&COPILOT, include_bytes!("icons/providers/copilot.png")),
    };
    let handle = *cache.get_or_init(|| unsafe {
        CreateIconFromResourceEx(png, true, 0x00030000, 64, 64, LR_DEFAULTCOLOR)
            .map(|icon| icon.0 as isize)
            .unwrap_or(0)
    });
    if handle != 0 {
        unsafe {
            if let Some(tint) = tint {
                draw_tinted(hdc, x, y, size, HICON(handle as *mut _), tint);
                return;
            }
            let _ = DrawIconEx(
                hdc,
                x,
                y,
                HICON(handle as *mut _),
                size,
                size,
                0,
                HBRUSH::default(),
                DI_NORMAL,
            );
        }
    }
}

/// Preserve the artwork's alpha coverage while recoloring its visible pixels.
unsafe fn draw_tinted(hdc: HDC, x: i32, y: i32, size: i32, icon: HICON, [r, g, b]: [u8; 3]) {
    if !(1..=256).contains(&size) {
        return;
    }
    let dc = CreateCompatibleDC(hdc);
    if dc.is_invalid() {
        return;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
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
        return;
    }
    let old = SelectObject(dc, bitmap);
    // SAFETY: the DIB owns size * size writable 32-bit pixels until DeleteObject.
    let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (size * size) as usize);
    pixels.fill(0);
    let _ = DrawIconEx(dc, 0, 0, icon, size, size, 0, HBRUSH::default(), DI_NORMAL);
    let _ = GdiFlush();
    for pixel in pixels {
        let alpha = *pixel >> 24;
        *pixel = (alpha << 24)
            | ((u32::from(r) * alpha / 255) << 16)
            | ((u32::from(g) * alpha / 255) << 8)
            | (u32::from(b) * alpha / 255);
    }
    let _ = GdiAlphaBlend(
        hdc,
        x,
        y,
        size,
        size,
        dc,
        0,
        0,
        size,
        size,
        BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1,
        },
    );
    SelectObject(dc, old);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(dc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tinted_marks_keep_visible_coverage_and_use_the_selected_rgb() {
        unsafe {
            let dc = CreateCompatibleDC(HDC::default());
            assert!(!dc.is_invalid());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 16,
                    biHeight: -16,
                    biPlanes: 1,
                    biBitCount: 32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bitmap);
            let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), 256);
            for provider in [
                Provider::Claude,
                Provider::Codex,
                Provider::Antigravity,
                Provider::OpenCode,
                Provider::Cursor,
                Provider::Grok,
                Provider::Copilot,
            ] {
                pixels.fill(0);
                draw(dc, 0, 0, 16, provider, Some([255, 0, 255]));
                let _ = GdiFlush();
                assert!(pixels.iter().any(|pixel| pixel & 0x00ffffff != 0));
                for pixel in pixels.iter() {
                    assert_eq!(pixel & 0x0000ff00, 0);
                    assert_eq!((pixel >> 16) & 255, pixel & 255);
                }
            }
            SelectObject(dc, old);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(dc);
        }
    }
}
