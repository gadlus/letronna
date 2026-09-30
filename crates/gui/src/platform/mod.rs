//! OS integration: install and uninstall, the tray icon, window control, the
//! modal backdrop, notifications, and playing and picturing what a tool made.
//!
//! Every module here compiles on every platform and exposes the same API; what
//! differs is how much of it is real. Windows has all of it. Linux has install,
//! single instance, a tray and sound, but no backdrop blur, no video stills or
//! playback, and no true hide-to-tray. macOS plays sound, notifies when it runs
//! as an app bundle, and otherwise falls back to doing nothing, which leaves a
//! working app that simply runs in place.
//! Callers never branch on the platform.

pub(crate) mod audio;
pub(crate) mod backdrop;
pub(crate) mod hotkey;
pub(crate) mod install;
pub(crate) mod mic;
pub(crate) mod notify;
pub(crate) mod poster;
#[cfg(windows)]
mod toast;
pub(crate) mod tray;
pub(crate) mod video;
pub(crate) mod window;
