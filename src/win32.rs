//! Small Win32 helpers for the borderless Slint window (v1.0):
//! drag to move, edge resize, rounded corners (Windows 11) and the
//! clipboard. All best-effort; no-ops on other platforms.

/// The window's HWND as an integer, once the window exists.
#[cfg(windows)]
pub fn hwnd_of(window: &slint::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle();
    let wh = handle.window_handle().ok()?;
    match wh.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
pub fn hwnd_of(_window: &slint::Window) -> Option<isize> {
    None
}

/// Hit-test code for an edge id from the UI:
/// 0 caption, 1 left, 2 right, 3 top, 4 bottom, 5 top-left,
/// 6 top-right, 7 bottom-left, 8 bottom-right. Pure — unit-tested.
pub fn hit_test_for(edge: i32) -> u32 {
    match edge {
        1 => 10, // HTLEFT
        2 => 11, // HTRIGHT
        3 => 12, // HTTOP
        4 => 15, // HTBOTTOM
        5 => 13, // HTTOPLEFT
        6 => 14, // HTTOPRIGHT
        7 => 16, // HTBOTTOMLEFT
        8 => 17, // HTBOTTOMRIGHT
        _ => 2,  // HTCAPTION
    }
}

/// Start a native move (edge 0) or resize, as if the user grabbed the
/// title bar / border. Same technique winit's drag_window uses.
#[cfg(windows)]
pub fn begin_drag(hwnd: isize, edge: i32) {
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, PostMessageW, WM_NCLBUTTONDOWN};
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = ReleaseCapture();
        let lparam = ((pt.y as u32 & 0xFFFF) << 16) | (pt.x as u32 & 0xFFFF);
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_NCLBUTTONDOWN,
            WPARAM(hit_test_for(edge) as usize),
            LPARAM(lparam as isize),
        );
    }
}

#[cfg(not(windows))]
pub fn begin_drag(_hwnd: isize, _edge: i32) {}

/// Rounded corners + DWM shadow on Windows 11 (ignored on Windows 10).
#[cfg(windows)]
pub fn round_corners(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
    let pref: i32 = 2; // DWMWCP_ROUND
    unsafe {
        let _ = DwmSetWindowAttribute(
            HWND(hwnd as *mut core::ffi::c_void),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const i32 as *const core::ffi::c_void,
            4,
        );
    }
}

#[cfg(not(windows))]
pub fn round_corners(_hwnd: isize) {}

/// Put text on the clipboard (CF_UNICODETEXT).
#[cfg(windows)]
pub fn copy_to_clipboard(text: &str) {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let ptr = GlobalLock(mem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(mem);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0)));
            } else {
                let _ = windows::Win32::Foundation::GlobalFree(Some(HGLOBAL(mem.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

#[cfg(not(windows))]
pub fn copy_to_clipboard(_text: &str) {}

/// Open Explorer with the file selected.
pub fn show_in_folder(path: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer.exe")
            .arg(format!("/select,{path}"))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_map_to_win32_hit_tests() {
        assert_eq!(hit_test_for(0), 2);
        assert_eq!(hit_test_for(1), 10);
        assert_eq!(hit_test_for(8), 17);
        assert_eq!(hit_test_for(99), 2);
    }
}
