//! Toasts on Windows, and the id they are shown under.
//!
//! A program that is not packaged shows toasts only under an
//! AppUserModelID that a Start menu shortcut carries, and the shortcut the
//! installer makes -- through PowerShell, which cannot set one -- carries
//! none, so it is given one here through its property store. A click is
//! heard through the toast's Activated event while the app runs; no COM
//! server is registered for a click after it has quit, since that would
//! start a second copy only to hand over to the first.
//!
//! It uses nothing else of the app's, so it can be type-checked for Windows
//! on its own.

use std::collections::VecDeque;
use std::path::Path;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::Foundation::E_OUTOFMEMORY;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemAlloc, CoTaskMemFree, IPersistFile, STGM_READWRITE,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, IInspectable, Interface, PCWSTR, PWSTR, Ref, Result};

/// Toasts kept after they are shown, so their click handlers live while
/// they can still be clicked in the notification centre; older ones have
/// usually been read or dismissed.
const KEPT: usize = 8;

/// Give the shortcut at `lnk` the AppUserModelID `id`, unless it has it.
pub(crate) fn name_shortcut(lnk: &Path, id: &str) -> Result<()> {
    unsafe {
        // The window's thread has COM already; this only makes sure.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READWRITE)?;
        let store: IPropertyStore = link.cast()?;
        let mut current = store.GetValue(&PKEY_AppUserModel_ID)?;
        let named = PropVariantToStringAlloc(&current).ok().map(|text| {
            let named = text.to_string().unwrap_or_default();
            CoTaskMemFree(Some(text.0 as *const _));
            named
        });
        let _ = PropVariantClear(&mut current);
        if named.as_deref() == Some(id) {
            return Ok(());
        }
        let mut value = text_value(id)?;
        let set = store
            .SetValue(&PKEY_AppUserModel_ID, &value)
            .and_then(|()| store.Commit())
            .and_then(|()| file.Save(PCWSTR::null(), true));
        let _ = PropVariantClear(&mut value);
        set
    }
}

/// `text` as a property value, in memory `PropVariantClear` frees.
fn text_value(text: &str) -> Result<PROPVARIANT> {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        let buffer = CoTaskMemAlloc(wide.len() * 2) as *mut u16;
        if buffer.is_null() {
            return Err(E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), buffer, wide.len());
        let mut value = PROPVARIANT::default();
        let inner = &mut value.Anonymous.Anonymous;
        inner.vt = VT_LPWSTR;
        inner.Anonymous.pwszVal = PWSTR(buffer);
        Ok(value)
    }
}

/// The toasts shown lately.
#[derive(Default)]
pub(crate) struct Toasts(VecDeque<ToastNotification>);

impl Toasts {
    /// Show the toast `xml` under the AppUserModelID `id`, in place of any
    /// earlier one tagged `tag`; `clicked` runs on another thread when it
    /// is clicked.
    pub(crate) fn show(
        &mut self,
        id: &str,
        xml: &str,
        tag: &str,
        clicked: impl Fn() + Send + 'static,
    ) -> Result<()> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        toast.SetTag(&HSTRING::from(tag))?;
        toast.SetGroup(&HSTRING::from("letronna"))?;
        let handler =
            TypedEventHandler::new(move |_: Ref<ToastNotification>, _: Ref<IInspectable>| {
                clicked();
                Ok(())
            });
        toast.Activated(&handler)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(id))?.Show(&toast)?;
        self.0.push_back(toast);
        if self.0.len() > KEPT {
            self.0.pop_front();
        }
        Ok(())
    }
}
