//! Window control the app needs but gpui does not expose portably.

use crate::prelude::*;

/// Put the window back in front of the user, from the tray or a second launch.
pub(crate) fn show(window: &mut Window) {
    #[cfg(windows)]
    win::set_visible(window, true);
    window.activate_window();
}

/// Get the window out of the way when the user closes it while a tray icon is
/// there to bring it back. Windows can hide it outright; elsewhere the closest
/// portable thing is minimising, which at least leaves a way back.
pub(crate) fn hide(window: &mut Window) {
    #[cfg(windows)]
    {
        win::set_visible(window, false);
    }
    #[cfg(not(windows))]
    {
        window.minimize_window();
    }
}

#[cfg(windows)]
pub(crate) use win::{hwnd, make_topmost};

#[cfg(windows)]
mod win {
    use crate::prelude::*;

    pub(crate) fn hwnd(window: &Window) -> Option<*mut core::ffi::c_void> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut core::ffi::c_void),
            _ => None,
        }
    }

    /// Bring the window in front of the notification-area flyout.
    pub(crate) fn make_topmost(window: &Window) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos,
        };
        let Some(hwnd) = hwnd(window) else {
            return;
        };
        unsafe {
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            SetForegroundWindow(hwnd);
        }
    }

    pub(crate) fn set_visible(window: &Window, visible: bool) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SW_HIDE, SW_SHOW, SetForegroundWindow, ShowWindow,
        };
        let Some(hwnd) = hwnd(window) else {
            return;
        };
        unsafe {
            ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
            if visible {
                SetForegroundWindow(hwnd);
            }
        }
    }
}
