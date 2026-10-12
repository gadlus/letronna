//! Whether the person at the computer said yes, and not a program typing
//! into a terminal, which a fake one (`script`, a ConPTY) lets it do: the
//! system asks them itself, with Windows Hello, or Touch ID or the login
//! password on macOS. Neither is a prompt the agent can read or answer.

/// Ask the system to check that the user is here, showing `why`.
pub fn confirm(why: &str) -> Result<(), String> {
    imp::confirm(why)
}

#[cfg(windows)]
mod imp {
    use windows::Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    };
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
    use windows::core::{HSTRING, factory};

    pub fn confirm(why: &str) -> Result<(), String> {
        let fail =
            |e: windows::core::Error| format!("Windows Hello could not be asked: {}", e.message());
        let ready = UserConsentVerifier::CheckAvailabilityAsync()
            .and_then(|asked| asked.get())
            .map_err(fail)?;
        if ready != UserConsentVerifierAvailability::Available {
            return Err(super::UNAVAILABLE.into());
        }
        let why = HSTRING::from(why);
        // Over the console's window, so the prompt is not hidden behind it.
        let asked = factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
            .and_then(|interop| unsafe {
                interop.RequestVerificationForWindowAsync(GetConsoleWindow(), &why)
            })
            .or_else(|_| UserConsentVerifier::RequestVerificationAsync(&why))
            .map_err(fail)?;
        match asked.get().map_err(fail)? {
            UserConsentVerificationResult::Verified => Ok(()),
            _ => Err(super::NOT_CONFIRMED.into()),
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use std::ffi::CString;

    #[link(name = "LocalAuthentication", kind = "framework")]
    unsafe extern "C" {}

    /// LAPolicyDeviceOwnerAuthentication: Touch ID, else the login password.
    const OWNER: isize = 2;

    pub fn confirm(why: &str) -> Result<(), String> {
        let why = CString::new(why.replace('\0', "")).unwrap_or_default();
        let (said, heard) = std::sync::mpsc::channel();
        let reply = RcBlock::new(move |ok: Bool, _: *mut AnyObject| {
            let _ = said.send(ok.as_bool());
        });
        // Kept until the reply comes, which arrives on a queue of its own.
        let context: Retained<AnyObject> = unsafe { msg_send![class!(LAContext), new] };
        unsafe {
            let reason: *mut AnyObject =
                msg_send![class!(NSString), stringWithUTF8String: why.as_ptr()];
            let () = msg_send![&*context, evaluatePolicy: OWNER, localizedReason: reason, reply: &*reply];
        }
        let yes = heard.recv().unwrap_or(false);
        drop(context);
        match yes {
            true => Ok(()),
            false => Err(super::NOT_CONFIRMED.into()),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    pub fn confirm(_: &str) -> Result<(), String> {
        Err(super::UNAVAILABLE.into())
    }
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
const NOT_CONFIRMED: &str = "The system did not confirm it was you. Nothing was done.";
const UNAVAILABLE: &str = "This needs the system to confirm it is you (Windows Hello, or Touch ID or the login password on macOS), which is not set up here. Nothing was done; use the Wallet page in the window (letronna gui) instead.";

#[cfg(test)]
mod tests {
    // No system prompt in a test run: on Linux there is none to give.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn with_no_system_check_nothing_is_confirmed() {
        let why = super::confirm("letronna wallet reveal Main").unwrap_err();
        assert!(why.contains("Nothing was done"), "{why}");
    }
}
