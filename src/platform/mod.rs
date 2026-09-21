//! OS integration: install and uninstall, the tray icon, window control and
//! the modal backdrop.
//!
//! Every module here compiles on every platform and exposes the same API; what
//! differs is how much of it is real. Windows has all of it. Linux has install,
//! single instance and a tray, but no backdrop blur and no true hide-to-tray.
//! macOS falls back to doing nothing, which leaves a working app that simply
//! runs in place. Callers never branch on the platform.

pub(crate) mod backdrop;
pub(crate) mod install;
pub(crate) mod tray;
pub(crate) mod window;
