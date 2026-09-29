//! Window control the app needs but gpui does not expose portably.

use crate::prelude::*;

/// Put the window back in front of the user, from the tray or a second launch.
/// Kept everywhere per this module's contract, though only the tray and the
/// single-instance check ever call it.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
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

/// Match the native window chrome to the app's theme: pinned dark or light,
/// or with None left to follow the system. On macOS the traffic lights follow
/// the window's appearance, not the content under them: a light window draws
/// them inactive as dark discs, which vanish on a dark title bar whenever
/// another app has focus. Other platforms draw their own controls.
pub(crate) fn pin_chrome(window: &Window, dark: Option<bool>) {
    #[cfg(target_os = "macos")]
    mac::pin_appearance(window, dark);
    #[cfg(not(target_os = "macos"))]
    let _ = (window, dark);
}

#[cfg(windows)]
pub(crate) use win::{hwnd, make_topmost};

// objc 0.2's msg_send! checks a `cargo-clippy` feature this crate never declares.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod mac {
    use crate::prelude::*;
    use objc::{class, msg_send, runtime::Object, sel, sel_impl};

    pub(crate) fn pin_appearance(window: &Window, dark: Option<bool>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        let view = h.ns_view.as_ptr() as *mut Object;
        unsafe {
            let ns_window: *mut Object = msg_send![view, window];
            if ns_window.is_null() {
                return;
            }
            // A nil appearance is the system's own.
            let mut appearance: *mut Object = std::ptr::null_mut();
            if let Some(dark) = dark {
                let name = if dark {
                    c"NSAppearanceNameDarkAqua"
                } else {
                    c"NSAppearanceNameAqua"
                };
                let name: *mut Object =
                    msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
                appearance = msg_send![class!(NSAppearance), appearanceNamed: name];
                if appearance.is_null() {
                    return;
                }
            }
            let _: () = msg_send![ns_window, setAppearance: appearance];
        }
    }
}

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
