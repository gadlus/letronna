//! The Accessibility API, through which Letronna reads another app's
//! elements, presses them and sets their values. The few calls computer use
//! makes are declared here, as no crate in the lock file has them. Nothing
//! here asks macOS for the permission: `trusted` only looks, so no system
//! prompt is ever raised on the screen.

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFHash, CFType, CFTypeID, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::{declare_TCFType, impl_TCFType};
use core_graphics::geometry::{CGPoint, CGSize};
use std::ffi::c_void;
use std::ptr;

/// What an element reference points at, which only the system reads.
#[repr(C)]
pub struct OpaqueElement(c_void);
type ElementRef = *const OpaqueElement;

/// An Accessibility call's result: 0, or why it failed.
pub type Code = i32;
/// The element has gone from the app's interface.
pub const GONE: Code = -25202;
/// The app did not answer in time, or is busy.
const NO_ANSWER: Code = -25204;
/// The element has no such attribute or action.
const UNSUPPORTED: Code = -25205;
const ACTION_UNSUPPORTED: Code = -25206;
/// This process has not been given the Accessibility permission.
pub const NOT_ALLOWED: Code = -25211;
/// The attribute has no value.
const NO_VALUE: Code = -25212;

/// The kinds of `AXValue` read here: a point, a size, and a failure.
const POINT: u32 = 1;
const SIZE: u32 = 2;
const FAILURE: u32 = 5;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXUIElementCreateApplication(pid: i32) -> ElementRef;
    fn AXUIElementCreateSystemWide() -> ElementRef;
    fn AXUIElementCopyAttributeValue(
        element: ElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> Code;
    fn AXUIElementCopyMultipleAttributeValues(
        element: ElementRef,
        attributes: CFArrayRef,
        options: u32,
        values: *mut CFArrayRef,
    ) -> Code;
    fn AXUIElementSetAttributeValue(
        element: ElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> Code;
    fn AXUIElementIsAttributeSettable(
        element: ElementRef,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> Code;
    fn AXUIElementCopyActionNames(element: ElementRef, names: *mut CFArrayRef) -> Code;
    fn AXUIElementPerformAction(element: ElementRef, action: CFStringRef) -> Code;
    fn AXUIElementGetPid(element: ElementRef, pid: *mut i32) -> Code;
    fn AXUIElementSetMessagingTimeout(element: ElementRef, seconds: f32) -> Code;
    fn AXValueGetTypeID() -> CFTypeID;
    fn AXValueGetType(value: CFTypeRef) -> u32;
    fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> u8;
}

declare_TCFType!(
    /// One element of an app's interface, or the app itself.
    AxElement, ElementRef
);
impl_TCFType!(AxElement, ElementRef, AXUIElementGetTypeID);

// An element is a Core Foundation object, whose counting is thread safe, and
// each call about it is a message to its app, answered on no thread of ours.
unsafe impl Send for AxElement {}
unsafe impl Sync for AxElement {}

/// Whether macOS has given this process, or the app it is counted under,
/// the Accessibility permission. Only looks: it never asks.
pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// What a failed call's `code` means, as a refusal says it.
pub fn why(code: Code) -> String {
    match code {
        GONE => "that element is no longer on screen; list the app's elements again".into(),
        NO_ANSWER => "the app did not answer; it may be busy".into(),
        UNSUPPORTED | ACTION_UNSUPPORTED => "the element does not take that".into(),
        NOT_ALLOWED => "macOS has not given Letronna the Accessibility permission".into(),
        NO_VALUE => "the element has no such value".into(),
        other => format!("the Accessibility API failed ({other})"),
    }
}

impl AxElement {
    /// The app with process `pid`, which answers for its windows and focus.
    pub fn app(pid: u32) -> Option<AxElement> {
        let raw = unsafe { AXUIElementCreateApplication(pid as i32) };
        (!raw.is_null()).then(|| unsafe { AxElement::wrap_under_create_rule(raw) })
    }

    /// The whole system, which knows which app has the keyboard.
    pub fn system() -> Option<AxElement> {
        let raw = unsafe { AXUIElementCreateSystemWide() };
        (!raw.is_null()).then(|| unsafe { AxElement::wrap_under_create_rule(raw) })
    }

    /// The value of `attribute`, or why there is none.
    pub fn get(&self, attribute: &str) -> Result<CFType, Code> {
        let name = CFString::new(attribute);
        let mut value: CFTypeRef = ptr::null();
        let code = unsafe {
            AXUIElementCopyAttributeValue(self.0, name.as_concrete_TypeRef(), &mut value)
        };
        if code != 0 {
            return Err(code);
        }
        if value.is_null() {
            return Err(NO_VALUE);
        }
        Ok(unsafe { CFType::wrap_under_create_rule(value) })
    }

    /// The element `attribute` names, such as the app's focused window.
    pub fn element(&self, attribute: &str) -> Option<AxElement> {
        self.get(attribute).ok()?.downcast_into::<AxElement>()
    }

    /// The elements `attribute` lists, such as an element's children.
    pub fn elements(&self, attribute: &str) -> Option<Vec<AxElement>> {
        let array = self.get(attribute).ok()?.downcast_into::<CFArray>()?;
        Some(
            items(&array)
                .into_iter()
                .filter_map(|item| item.downcast_into::<AxElement>())
                .collect(),
        )
    }

    /// Several attributes in one message to the app, each None where the
    /// element lacks it.
    pub fn get_many(&self, attributes: &[&str]) -> Result<Vec<Option<CFType>>, Code> {
        let names: Vec<CFString> = attributes.iter().map(|name| CFString::new(name)).collect();
        let names = CFArray::from_CFTypes(&names);
        let mut values: CFArrayRef = ptr::null();
        let code = unsafe {
            AXUIElementCopyMultipleAttributeValues(
                self.0,
                names.as_concrete_TypeRef(),
                0,
                &mut values,
            )
        };
        if code != 0 {
            return Err(code);
        }
        if values.is_null() {
            return Err(NO_VALUE);
        }
        let values: CFArray = unsafe { CFArray::wrap_under_create_rule(values) };
        // Each value stays in its attribute's place.
        let mut out: Vec<Option<CFType>> = values
            .get_all_values()
            .into_iter()
            .map(|value| {
                let value =
                    (!value.is_null()).then(|| unsafe { CFType::wrap_under_get_rule(value) });
                value.filter(|value| !failure(value))
            })
            .collect();
        out.resize(attributes.len(), None);
        Ok(out)
    }

    /// Sets `attribute` to `value`.
    pub fn set(&self, attribute: &str, value: &CFType) -> Result<(), Code> {
        let name = CFString::new(attribute);
        let code = unsafe {
            AXUIElementSetAttributeValue(self.0, name.as_concrete_TypeRef(), value.as_CFTypeRef())
        };
        if code != 0 { Err(code) } else { Ok(()) }
    }

    /// Sets a yes/no `attribute`, such as whether the element has the focus.
    pub fn set_flag(&self, attribute: &str, on: bool) -> Result<(), Code> {
        self.set(attribute, &CFBoolean::from(on).as_CFType())
    }

    /// Whether the app lets `attribute` be set.
    pub fn settable(&self, attribute: &str) -> bool {
        let name = CFString::new(attribute);
        let mut settable = 0u8;
        let code = unsafe {
            AXUIElementIsAttributeSettable(self.0, name.as_concrete_TypeRef(), &mut settable)
        };
        code == 0 && settable != 0
    }

    /// The actions the element takes, such as `AXPress`.
    pub fn actions(&self) -> Vec<String> {
        let mut names: CFArrayRef = ptr::null();
        let code = unsafe { AXUIElementCopyActionNames(self.0, &mut names) };
        if code != 0 || names.is_null() {
            return Vec::new();
        }
        let names: CFArray = unsafe { CFArray::wrap_under_create_rule(names) };
        items(&names).iter().filter_map(text).collect()
    }

    /// Does `action` on the element, as a screen reader would.
    pub fn perform(&self, action: &str) -> Result<(), Code> {
        let name = CFString::new(action);
        let code = unsafe { AXUIElementPerformAction(self.0, name.as_concrete_TypeRef()) };
        if code != 0 { Err(code) } else { Ok(()) }
    }

    /// The process whose element this is.
    pub fn pid(&self) -> Option<u32> {
        let mut pid = 0i32;
        let code = unsafe { AXUIElementGetPid(self.0, &mut pid) };
        (code == 0 && pid > 0).then_some(pid as u32)
    }

    /// How long a call about this element, or about any for the system
    /// element, waits for the app to answer.
    pub fn set_timeout(&self, seconds: f32) {
        unsafe { AXUIElementSetMessagingTimeout(self.0, seconds) };
    }

    /// A number equal elements share, which finds them again among many.
    pub fn hash(&self) -> u64 {
        unsafe { CFHash(self.as_CFTypeRef()) as u64 }
    }
}

/// The values in `array`, each held.
fn items(array: &CFArray) -> Vec<CFType> {
    array
        .get_all_values()
        .into_iter()
        .filter(|value| !value.is_null())
        .map(|value| unsafe { CFType::wrap_under_get_rule(value) })
        .collect()
}

/// Whether `value` is the failure a batch read puts where a value is missing.
fn failure(value: &CFType) -> bool {
    is_ax_value(value) && unsafe { AXValueGetType(value.as_CFTypeRef()) } == FAILURE
}

fn is_ax_value(value: &CFType) -> bool {
    unsafe { CFGetTypeID(value.as_CFTypeRef()) == AXValueGetTypeID() }
}

/// `value` as words: a string, a number, or on and off for a yes/no.
pub fn text(value: &CFType) -> Option<String> {
    if let Some(string) = value.downcast::<CFString>() {
        return Some(string.to_string());
    }
    if let Some(flag) = value.downcast::<CFBoolean>() {
        return Some(if bool::from(flag) { "on" } else { "off" }.into());
    }
    let number = value.downcast::<CFNumber>()?;
    number
        .to_i64()
        .map(|n| n.to_string())
        .or_else(|| number.to_f64().map(|n| n.to_string()))
}

/// `value` as a point, as `AXPosition` gives it.
pub fn point(value: &CFType) -> Option<CGPoint> {
    let mut out = CGPoint::default();
    let ok = is_ax_value(value)
        && unsafe { AXValueGetValue(value.as_CFTypeRef(), POINT, (&raw mut out).cast()) } != 0;
    ok.then_some(out)
}

/// `value` as a size, as `AXSize` gives it.
pub fn size(value: &CFType) -> Option<CGSize> {
    let mut out = CGSize::default();
    let ok = is_ax_value(value)
        && unsafe { AXValueGetValue(value.as_CFTypeRef(), SIZE, (&raw mut out).cast()) } != 0;
    ok.then_some(out)
}
