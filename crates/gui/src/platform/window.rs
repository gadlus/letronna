//! Window control the app needs but gpui does not expose portably.

use crate::prelude::*;

/// Put the window back in front of the user, from the tray, a second launch
/// or a notification.
pub(crate) fn show(window: &mut Window) {
    #[cfg(windows)]
    win::set_visible(window, true);
    #[cfg(target_os = "macos")]
    mac::bring_back(window);
    window.activate_window();
}

/// Make the app the active one, from the menu bar while the user's click on
/// it is still being handled. Elsewhere `show` is enough.
#[cfg(target_os = "macos")]
pub(crate) fn activate_app() {
    mac::activate_app();
}

/// Get the window out of the way when the user closes it while a tray icon is
/// there to bring it back. Windows can hide it outright; elsewhere the closest
/// portable thing is minimising, which at least leaves a way back.
pub(crate) fn hide(window: &mut Window) {
    #[cfg(windows)]
    {
        win::set_visible(window, false);
    }
    #[cfg(target_os = "macos")]
    mac::minimize_later(window);
    #[cfg(not(any(windows, target_os = "macos")))]
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
    use objc::runtime::{BOOL, NO, Object, Sel, YES};
    use objc::{class, msg_send, sel, sel_impl};

    /// The NSWindow behind a gpui window.
    fn ns_window(window: &Window) -> Option<*mut Object> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        let view = h.ns_view.as_ptr() as *mut Object;
        let ns_window: *mut Object = unsafe { msg_send![view, window] };
        (!ns_window.is_null()).then_some(ns_window)
    }

    /// Send `selector` to `target` once the run loop is next free. AppKit
    /// tells gpui about a window that minimises or comes back by calling
    /// into it there and then, and every caller here is in the middle of a
    /// gpui update; gpui's own `activate_window` waits for the same reason.
    unsafe fn later(target: *mut Object, selector: Sel) {
        let nil: *mut Object = std::ptr::null_mut();
        unsafe {
            let _: () = msg_send![target,
                performSelectorOnMainThread: selector
                withObject: nil
                waitUntilDone: NO];
        }
    }

    /// Bring the app forward with its window: out of the Dock if it was
    /// minimised, and out of hiding if the app was hidden. gpui's
    /// `activate_window` orders the window front but leaves a minimised one
    /// in the Dock and the app behind whatever had the focus.
    pub(crate) fn bring_back(window: &Window) {
        let Some(ns_window) = ns_window(window) else {
            return;
        };
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            later(app, sel!(unhide:));
            let minimized: BOOL = msg_send![ns_window, isMiniaturized];
            if minimized == YES {
                later(ns_window, sel!(deminiaturize:));
            }
        }
        activate_app();
    }

    pub(crate) fn activate_app() {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: YES];
        }
    }

    /// Minimise the window into the Dock, after the close that asked for it
    /// has been answered.
    pub(crate) fn minimize_later(window: &Window) {
        if let Some(ns_window) = ns_window(window) {
            unsafe { later(ns_window, sel!(miniaturize:)) };
        }
    }

    pub(crate) fn pin_appearance(window: &Window, dark: Option<bool>) {
        let Some(ns_window) = ns_window(window) else {
            return;
        };
        unsafe {
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
