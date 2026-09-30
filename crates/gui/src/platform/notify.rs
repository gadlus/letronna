//! Notifications on the desktop, and the click that brings Letronna back.
//!
//! Each platform shows them its own way: a toast on Windows, the user
//! notification centre on macOS, notify-send on Linux. What they share is a
//! click that should open the session a notification is about. It arrives
//! on another thread, or from another process, so it comes back through
//! `clicks` for the window to act on rather than through a callback.
//!
//! macOS hands notifications only to an app bundle with an identifier, and
//! asking for them without one throws. A build run in place, as every
//! development build is, has none, so there nothing is shown -- rather than
//! going through osascript, whose notifications belong to another app and
//! lead nowhere when clicked.

use async_channel::{Receiver, Sender};
use letronna_core::notify::{Note, Notifier};
use std::sync::OnceLock;

/// The id Windows shows toasts under, which the Start menu shortcut carries.
#[cfg(windows)]
const APP_ID: &str = "Gadlus.Letronna";

/// Clicks waiting for the window. It takes them as they come, so a few is
/// plenty; more are dropped rather than kept for a window that is gone.
const WAITING: usize = 8;

fn clicked_channel() -> &'static (Sender<u64>, Receiver<u64>) {
    static CLICKED: OnceLock<(Sender<u64>, Receiver<u64>)> = OnceLock::new();
    CLICKED.get_or_init(|| async_channel::bounded(WAITING))
}

/// The sessions whose notifications were clicked, as they are.
pub(crate) fn clicks() -> Receiver<u64> {
    clicked_channel().1.clone()
}

/// Say that the notification about `session` was clicked, from any thread.
#[cfg_attr(
    not(any(windows, target_os = "macos", target_os = "linux")),
    allow(dead_code)
)]
fn clicked(session: u64) {
    let _ = clicked_channel().0.try_send(session);
}

/// The desktop's own notifications, as far as this platform has them.
pub(crate) struct Desktop {
    #[cfg(windows)]
    toasts: super::toast::Toasts,
    /// The notification centre, when this is an app bundle that may use it.
    #[cfg(target_os = "macos")]
    center: Option<mac::Center>,
}

/// Get ready to notify: on Windows, give the Start menu shortcut the app id
/// toasts need; on macOS, ask for permission and listen for clicks.
pub(crate) fn desktop() -> Desktop {
    #[cfg(windows)]
    if let Some(lnk) = super::install::shortcut().filter(|lnk| lnk.exists()) {
        let _ = super::toast::name_shortcut(&lnk, APP_ID);
    }
    Desktop {
        #[cfg(windows)]
        toasts: super::toast::Toasts::default(),
        #[cfg(target_os = "macos")]
        center: mac::Center::start(),
    }
}

impl Notifier for Desktop {
    fn show(&mut self, note: &Note) {
        #[cfg(windows)]
        {
            let session = note.session;
            let xml = toast_xml(note);
            let _ = self
                .toasts
                .show(APP_ID, &xml, &session.to_string(), move || clicked(session));
        }
        #[cfg(target_os = "macos")]
        if let Some(center) = &self.center {
            center.show(note);
        }
        #[cfg(target_os = "linux")]
        linux::show(note);
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        let _ = note;
    }
}

/// A toast's XML for `note`: its title over its body, in the template
/// Windows lays out itself.
#[cfg(any(windows, test))]
fn toast_xml(note: &Note) -> String {
    format!(
        "<toast launch=\"session={}\"><visual><binding template=\"ToastGeneric\">\
         <text>{}</text><text>{}</text></binding></visual></toast>",
        note.session,
        escape(&note.title),
        escape(&note.body)
    )
}

/// `text` safe inside an XML element or attribute.
#[cfg(any(windows, test))]
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// notify-send, which every Linux desktop that shows notifications has.
/// Asked for a default action and to wait, it prints the action's name when
/// the notification is clicked, which is the click without a D-Bus client.
#[cfg(any(target_os = "linux", test))]
mod linux {
    use letronna_core::notify::Note;

    /// notify-send's arguments for `note`: with a click heard when `click`,
    /// and after `--`, so a title starting with a dash is not an option.
    /// Most desktops read the body as markup, where a model's `<a href>` or
    /// `<img src>` would be a link or a picture, so it is escaped.
    pub(super) fn args(note: &Note, click: bool) -> Vec<String> {
        let mut args = vec![format!("--app-name={}", letronna_core::BRAND)];
        if click {
            args.extend(["--action=default=Open".to_owned(), "--wait".to_owned()]);
        }
        let body = note
            .body
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        args.extend(["--".to_owned(), note.title.clone(), body]);
        args
    }

    /// Show `note`, waiting for its click on a thread of its own.
    #[cfg(target_os = "linux")]
    pub(super) fn show(note: &Note) {
        use std::process::{Command, Stdio};
        let session = note.session;
        let (clickable, plain) = (args(note, true), args(note, false));
        std::thread::spawn(move || {
            let Ok(out) = Command::new("notify-send")
                .args(&clickable)
                .stderr(Stdio::null())
                .output()
            else {
                return;
            };
            if out.status.success() {
                if String::from_utf8_lossy(&out.stdout).trim() == "default" {
                    super::clicked(session);
                }
                return;
            }
            // Before libnotify 0.7.10 there were no actions, and an option
            // it does not know is refused: shown again without them.
            let _ = Command::new("notify-send")
                .args(&plain)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }
}

// objc 0.2's msg_send! checks a `cargo-clippy` feature this crate never declares.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod mac {
    use block::{Block, ConcreteBlock};
    use letronna_core::notify::Note;
    use objc::declare::ClassDecl;
    use objc::runtime::{BOOL, Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::c_void;

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    /// The class that hears clicks, declared here since Rust has no other.
    const DELEGATE: &str = "LetronnaNotificationDelegate";
    /// The key the session's id is kept under in a notification.
    const SESSION: &str = "session";
    /// UNAuthorizationOptionSound | UNAuthorizationOptionAlert.
    const ASK_FOR: usize = (1 << 1) | (1 << 2);
    /// UNNotificationPresentationOptionSound | List | Banner: shown even
    /// while Letronna is the active app, as it is with its window minimised.
    const PRESENT: usize = (1 << 1) | (1 << 3) | (1 << 4);

    /// The user notification centre, asked for permission once.
    pub(super) struct Center(*mut Object);

    impl Center {
        /// The centre, with a delegate that hears clicks, or None where
        /// this is not an app bundle with an identifier.
        pub(super) fn start() -> Option<Self> {
            if !bundled() {
                return None;
            }
            unsafe {
                let center: *mut Object = msg_send![
                    Class::get("UNUserNotificationCenter")?,
                    currentNotificationCenter
                ];
                if center.is_null() {
                    return None;
                }
                // The centre holds its delegate weakly; this one is never let go.
                let delegate: *mut Object = msg_send![delegate_class()?, new];
                let _: () = msg_send![center, setDelegate: delegate];
                // The answer comes later, and the system keeps it: a
                // notification sent while it is still no is dropped.
                let asked = ConcreteBlock::new(|_: BOOL, _: *mut Object| {}).copy();
                let _: () = msg_send![center, requestAuthorizationWithOptions: ASK_FOR
                                              completionHandler: &*asked];
                Some(Self(center))
            }
        }

        /// Post `note`, in place of any earlier one about its session.
        pub(super) fn show(&self, note: &Note) {
            let (Some(content), Some(request)) = (
                Class::get("UNMutableNotificationContent"),
                Class::get("UNNotificationRequest"),
            ) else {
                return;
            };
            unsafe {
                let content: *mut Object = msg_send![content, new];
                if content.is_null() {
                    return;
                }
                let _: () = msg_send![content, setTitle: ns_string(&note.title)];
                let _: () = msg_send![content, setBody: ns_string(&note.body)];
                let id: *mut Object =
                    msg_send![class!(NSNumber), numberWithUnsignedLongLong: note.session];
                let info: *mut Object = msg_send![class!(NSDictionary),
                    dictionaryWithObject: id forKey: ns_string(SESSION)];
                let _: () = msg_send![content, setUserInfo: info];
                if let Some(sound) = Class::get("UNNotificationSound") {
                    let sound: *mut Object = msg_send![sound, defaultSound];
                    let _: () = msg_send![content, setSound: sound];
                }
                let name = ns_string(&format!("letronna-session-{}", note.session));
                let request: *mut Object = msg_send![request, requestWithIdentifier: name
                    content: content trigger: std::ptr::null_mut::<Object>()];
                let _: () = msg_send![content, release];
                if request.is_null() {
                    return;
                }
                let _: () = msg_send![self.0, addNotificationRequest: request
                    withCompletionHandler: std::ptr::null_mut::<c_void>()];
            }
        }
    }

    /// Whether this runs as an app bundle with an identifier, which is what
    /// the notification centre needs; asked without one, it throws.
    fn bundled() -> bool {
        let Some(bundle) = Class::get("NSBundle") else {
            return false;
        };
        unsafe {
            let bundle: *mut Object = msg_send![bundle, mainBundle];
            let id: *mut Object = msg_send![bundle, bundleIdentifier];
            !id.is_null()
        }
    }

    /// The delegate's class, declared the first time it is asked for.
    fn delegate_class() -> Option<&'static Class> {
        let Some(mut decl) = ClassDecl::new(DELEGATE, class!(NSObject)) else {
            return Class::get(DELEGATE);
        };
        unsafe {
            decl.add_method(
                sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                clicked as extern "C" fn(&Object, Sel, *mut Object, *mut Object, *mut c_void),
            );
            decl.add_method(
                sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                present as extern "C" fn(&Object, Sel, *mut Object, *mut Object, *mut c_void),
            );
        }
        Some(decl.register())
    }

    /// A notification was clicked: say which session it was about.
    extern "C" fn clicked(
        _: &Object,
        _: Sel,
        _: *mut Object,
        response: *mut Object,
        done: *mut c_void,
    ) {
        unsafe {
            // A message to nil answers nil, so a missing link ends at no id.
            let notification: *mut Object = msg_send![response, notification];
            let request: *mut Object = msg_send![notification, request];
            let content: *mut Object = msg_send![request, content];
            let info: *mut Object = msg_send![content, userInfo];
            let id: *mut Object = msg_send![info, objectForKey: ns_string(SESSION)];
            if !id.is_null() {
                let id: u64 = msg_send![id, unsignedLongLongValue];
                super::clicked(id);
            }
            if let Some(done) = (done as *const Block<(), ()>).as_ref() {
                done.call(());
            }
        }
    }

    /// A notification is about to show while Letronna is the active app.
    extern "C" fn present(_: &Object, _: Sel, _: *mut Object, _: *mut Object, done: *mut c_void) {
        unsafe {
            if let Some(done) = (done as *const Block<(usize,), ()>).as_ref() {
                done.call((PRESENT,));
            }
        }
    }

    /// `text` as an autoreleased NSString.
    unsafe fn ns_string(text: &str) -> *mut Object {
        let text = std::ffi::CString::new(text.replace('\0', "")).unwrap_or_default();
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(title: &str, body: &str) -> Note {
        Note {
            session: 42,
            title: title.into(),
            body: body.into(),
        }
    }

    #[test]
    fn a_toast_carries_its_title_and_body_as_text_and_never_as_markup() {
        let xml = toast_xml(&note("Fix <b> & \"c\"", "Asks: it's </text><text>forged"));
        assert_eq!(
            xml,
            "<toast launch=\"session=42\"><visual><binding template=\"ToastGeneric\">\
             <text>Fix &lt;b&gt; &amp; &quot;c&quot;</text>\
             <text>Asks: it&apos;s &lt;/text&gt;&lt;text&gt;forged</text>\
             </binding></visual></toast>"
        );
    }

    #[test]
    fn notify_send_is_asked_for_a_click_and_reads_the_text_as_neither_options_nor_markup() {
        let shown = note("--urgency=critical", "-h <a href='x'>&</a>");
        assert_eq!(
            linux::args(&shown, true),
            [
                "--app-name=Letronna",
                "--action=default=Open",
                "--wait",
                "--",
                "--urgency=critical",
                "-h &lt;a href='x'&gt;&amp;&lt;/a&gt;"
            ]
        );
        assert_eq!(
            linux::args(&shown, false),
            [
                "--app-name=Letronna",
                "--",
                "--urgency=critical",
                "-h &lt;a href='x'&gt;&amp;&lt;/a&gt;"
            ]
        );
    }
}
