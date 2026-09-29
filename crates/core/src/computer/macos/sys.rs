//! The macOS calls computer use makes besides the Accessibility API: the
//! window server's list of windows, pictures of them, made-up input and the
//! process table, through Core Graphics and libc, with ImageIO, declared
//! here, to turn a picture into a PNG or a JPEG. The Screen Recording
//! permission is only looked at, never asked for, and no picture is taken
//! without it: taking one would raise the system's prompt.

use super::{Proc, Window};
use crate::computer::{Bounds, Point};
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::bundle::{CFBundle, CFBundleGetIdentifier};
use core_foundation::data::{CFData, CFDataCreateMutable, CFDataRef};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::CFURL;
use core_graphics::access::ScreenCaptureAccess;
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::window::{
    self, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowImageBoundsIgnoreFraming,
    kCGWindowImageNominalResolution, kCGWindowLayer, kCGWindowListExcludeDesktopElements,
    kCGWindowListOptionOnScreenOnly, kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
};
use std::ffi::{c_char, c_void};
use std::ptr;

/// The JPEG qualities tried, best first, when a PNG of the picture is too
/// large to send: a busy full screen can run past the limit as a PNG.
const JPEG_QUALITIES: [f64; 3] = [0.8, 0.6, 0.4];

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    // Declared here too, returning the picture as a plain Core Foundation
    // object that ImageIO takes as it is.
    fn CGWindowListCreateImage(area: CGRect, list: u32, window: u32, options: u32) -> CFTypeRef;
    fn CGWindowListCreateImageFromArray(
        area: CGRect,
        windows: CFArrayRef,
        options: u32,
    ) -> CFTypeRef;
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageDestinationLossyCompressionQuality: CFStringRef;
    fn CGImageDestinationCreateWithData(
        data: CFTypeRef,
        kind: CFStringRef,
        count: usize,
        options: CFDictionaryRef,
    ) -> CFTypeRef;
    fn CGImageDestinationAddImage(
        destination: CFTypeRef,
        image: CFTypeRef,
        properties: CFDictionaryRef,
    );
    fn CGImageDestinationFinalize(destination: CFTypeRef) -> bool;
}

/// The windows on screen, the one in front first, menus and panels among
/// them; the window server gives these without any permission.
pub fn windows() -> Result<Vec<Window>, String> {
    let list = window::copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )
    .ok_or("macOS did not list the windows on screen; is anyone signed in at the screen?")?;
    Ok(list
        .get_all_values()
        .into_iter()
        .filter(|info| !info.is_null())
        .filter_map(|info| {
            let info: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(info as CFDictionaryRef) };
            window_of(&info)
        })
        .collect())
}

/// One window of the window server's list.
fn window_of(info: &CFDictionary<CFString, CFType>) -> Option<Window> {
    let number = |key: CFStringRef| info.find(key)?.downcast::<CFNumber>()?.to_f64();
    let (id, pid, layer, bounds) = unsafe {
        (
            number(kCGWindowNumber)?,
            number(kCGWindowOwnerPID)?,
            number(kCGWindowLayer).unwrap_or(0.0),
            info.find(kCGWindowBounds)?.downcast::<CFDictionary>()?,
        )
    };
    let bounds = CGRect::from_dict_representation(&bounds)?;
    let owner = unsafe { info.find(kCGWindowOwnerName) }
        .and_then(|name| name.downcast::<CFString>())
        .map(|name| name.to_string())
        .unwrap_or_default();
    Some(Window {
        id: id as u32,
        pid: pid as u32,
        owner,
        layer: layer as i64,
        bounds: bounds_of(bounds),
        alpha: unsafe { number(kCGWindowAlpha) }.unwrap_or(1.0),
    })
}

fn bounds_of(rect: CGRect) -> Bounds {
    Bounds {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    }
}

/// Every display, one rectangle holding them all.
pub fn screen() -> Option<Bounds> {
    let displays = CGDisplay::active_displays().ok()?;
    super::union(
        displays
            .into_iter()
            .map(|id| bounds_of(CGDisplay::new(id).bounds())),
    )
}

/// Whether macOS lets this process take pictures of the screen. Only looks:
/// it never asks, and never shows the system's prompt.
pub fn may_capture() -> bool {
    ScreenCaptureAccess.preflight()
}

/// Whether macOS lets this process send made-up input, which it counts
/// under Accessibility. Only looks: sending without it could raise a prompt.
pub fn may_post() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

/// A picture of `area`, one pixel a point, of the windows `ids` alone or of
/// everything on screen there, as a PNG, or a JPEG where a PNG is too large
/// for a model. Call only after `may_capture`.
pub fn capture(area: Bounds, ids: Option<&[u32]>) -> Result<Vec<u8>, String> {
    let rect = CGRect::new(
        &CGPoint::new(area.x, area.y),
        &CGSize::new(area.width, area.height),
    );
    let image = match ids {
        Some(ids) => {
            let ids = id_array(ids);
            let options = kCGWindowImageBoundsIgnoreFraming | kCGWindowImageNominalResolution;
            unsafe { CGWindowListCreateImageFromArray(rect, ids.as_concrete_TypeRef(), options) }
        }
        None => unsafe {
            CGWindowListCreateImage(
                rect,
                kCGWindowListOptionOnScreenOnly,
                kCGNullWindowID,
                kCGWindowImageNominalResolution,
            )
        },
    };
    if image.is_null() {
        return Err("macOS gave no picture of the screen.".into());
    }
    let image = unsafe { CFType::wrap_under_create_rule(image) };
    let limit = crate::media::PICTURE_BYTES;
    if let Some(png) = encode(&image, "public.png", None).filter(|png| png.len() <= limit) {
        return Ok(png);
    }
    JPEG_QUALITIES
        .iter()
        .find_map(|&quality| {
            encode(&image, "public.jpeg", Some(quality)).filter(|jpeg| jpeg.len() <= limit)
        })
        .ok_or_else(|| {
            "The picture is too large to send a model; take one app's windows instead.".into()
        })
}

/// Window ids as the window server takes a list of them: each in the
/// array's own slot, as CGWindowListCreate gives them, not a boxed number.
fn id_array(ids: &[u32]) -> CFArray {
    let slots: Vec<*const c_void> = ids.iter().map(|&id| id as usize as *const c_void).collect();
    CFArray::from_copyable(&slots)
}

/// `image` as a file of type `kind`, at `quality` where it is lossy.
fn encode(image: &CFType, kind: &str, quality: Option<f64>) -> Option<Vec<u8>> {
    let data = unsafe { CFDataCreateMutable(ptr::null(), 0) };
    if data.is_null() {
        return None;
    }
    let data = unsafe { CFData::wrap_under_create_rule(data as CFDataRef) };
    let kind = CFString::new(kind);
    let destination = unsafe {
        CGImageDestinationCreateWithData(
            data.as_CFTypeRef(),
            kind.as_concrete_TypeRef(),
            1,
            ptr::null(),
        )
    };
    if destination.is_null() {
        return None;
    }
    let destination = unsafe { CFType::wrap_under_create_rule(destination) };
    let properties = quality.map(|quality| {
        let key =
            unsafe { CFString::wrap_under_get_rule(kCGImageDestinationLossyCompressionQuality) };
        CFDictionary::from_CFType_pairs(&[(key, CFNumber::from(quality))])
    });
    let properties = properties
        .as_ref()
        .map_or(ptr::null(), |p| p.as_concrete_TypeRef());
    let done = unsafe {
        CGImageDestinationAddImage(destination.as_CFTypeRef(), image.as_CFTypeRef(), properties);
        CGImageDestinationFinalize(destination.as_CFTypeRef())
    };
    done.then(|| data.bytes().to_vec())
}

/// What a made-up mouse event does, with the left button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouse {
    Move,
    Down,
    Up,
}

/// A source whose events carry only the modifiers set on them, not those
/// the user happens to hold.
fn source() -> Result<CGEventSource, String> {
    CGEventSource::new(CGEventSourceStateID::Private).map_err(|()| not_made())
}

fn not_made() -> String {
    "macOS would not make the input event.".into()
}

/// Sends one mouse event at `at`, to whichever window the window server
/// finds there.
pub fn mouse(kind: Mouse, at: Point) -> Result<(), String> {
    let (kind, clicks) = match kind {
        Mouse::Move => (CGEventType::MouseMoved, 0),
        Mouse::Down => (CGEventType::LeftMouseDown, 1),
        Mouse::Up => (CGEventType::LeftMouseUp, 1),
    };
    let at = CGPoint::new(at.x, at.y);
    let event = CGEvent::new_mouse_event(source()?, kind, at, CGMouseButton::Left)
        .map_err(|()| not_made())?;
    if clicks > 0 {
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, clicks);
    }
    event.post(CGEventTapLocation::HID);
    Ok(())
}

/// Sends key `code` going down or up, with `flags` held, to process `pid`
/// alone, so that it reaches no other app whatever comes to the front; the
/// character it types, where given, goes with it.
pub fn key(pid: u32, code: u16, flags: u64, typed: &str, down: bool) -> Result<(), String> {
    let event = CGEvent::new_keyboard_event(source()?, code, down).map_err(|()| not_made())?;
    event.set_flags(CGEventFlags::from_bits_truncate(flags));
    if !typed.is_empty() {
        event.set_string(typed);
    }
    event.post_to_pid(pid as i32);
    Ok(())
}

/// Sends `units` of text as one key going down or up, to process `pid` alone.
pub fn text(pid: u32, units: &[u16], down: bool) -> Result<(), String> {
    let event = CGEvent::new_keyboard_event(source()?, 0, down).map_err(|()| not_made())?;
    event.set_flags(CGEventFlags::CGEventFlagNull);
    event.set_string_from_utf16_unchecked(units);
    event.post_to_pid(pid as i32);
    Ok(())
}

/// Scrolls by lines, up and left where positive, at the pointer.
pub fn scroll(vertical: i32, horizontal: i32) -> Result<(), String> {
    let event =
        CGEvent::new_scroll_event(source()?, ScrollEventUnit::LINE, 2, vertical, horizontal, 0)
            .map_err(|()| not_made())?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

/// Every process this user can see, with its parent and name.
pub fn processes() -> Vec<Proc> {
    let count = unsafe { libc::proc_listallpids(ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    // Room for processes started between the two calls.
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let room = (pids.len() * size_of::<libc::pid_t>()) as i32;
    let found = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), room) };
    pids.truncate(usize::try_from(found).unwrap_or(0).min(pids.len()));
    pids.into_iter()
        .filter(|&pid| pid > 0)
        .filter_map(|pid| process(pid as u32))
        .collect()
}

fn process(pid: u32) -> Option<Proc> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    let name = Some(words(&info.pbi_name))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| words(&info.pbi_comm));
    Some(Proc {
        pid,
        ppid: info.pbi_ppid,
        name,
    })
}

/// A fixed field of C characters as a string, up to its first NUL.
fn words(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The path of process `pid`'s program.
pub fn path_of(pid: u32) -> Option<String> {
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    path.truncate(usize::try_from(len).ok().filter(|&len| len > 0)?);
    String::from_utf8(path).ok()
}

/// The identifier in the Info.plist of the app bundle at `bundle`.
pub fn bundle_id(bundle: &str) -> Option<String> {
    let bundle = CFBundle::new(CFURL::from_path(bundle, true)?)?;
    let id = unsafe { CFBundleGetIdentifier(bundle.as_concrete_TypeRef()) };
    (!id.is_null()).then(|| unsafe { CFString::wrap_under_get_rule(id) }.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graphics::base::kCGImageAlphaPremultipliedLast;
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::CGContext;
    use core_graphics::image::CGImageRef;

    #[test]
    fn a_picture_of_some_windows_asks_for_those_windows_alone() {
        // Describing windows takes no permission and changes nothing on screen.
        let Ok(on_screen) = windows() else {
            return;
        };
        let ids: Vec<u32> = on_screen.iter().map(|window| window.id).take(5).collect();
        if ids.is_empty() {
            return;
        }
        let array = id_array(&ids);
        let array = unsafe { CFArray::<u32>::wrap_under_get_rule(array.as_concrete_TypeRef()) };
        let described = window::create_description_from_array(array).expect("a description");
        let described: Vec<u32> = described
            .iter()
            .filter_map(|info| window_of(&info))
            .map(|window| window.id)
            .collect();
        assert!(!described.is_empty());
        assert!(
            described.iter().all(|id| ids.contains(id)),
            "{described:?} of {ids:?}"
        );
    }

    #[test]
    fn a_drawn_picture_comes_out_as_a_png_or_a_jpeg() {
        let space = CGColorSpace::create_device_rgb();
        let context = CGContext::create_bitmap_context(
            None,
            64,
            32,
            8,
            0,
            &space,
            kCGImageAlphaPremultipliedLast,
        );
        context.set_rgb_fill_color(0.2, 0.4, 0.6, 1.0);
        context.fill_rect(CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(32.0, 32.0),
        ));
        let image = context.create_image().expect("a drawn picture");
        // A reference to a foreign type is the object's own pointer.
        let image: &CGImageRef = &image;
        let image = unsafe { CFType::wrap_under_get_rule(image as *const CGImageRef as CFTypeRef) };
        let png = encode(&image, "public.png", None).expect("a PNG");
        assert_eq!(crate::media::picture_kind(&png), Some("png"));
        let jpeg = encode(&image, "public.jpeg", Some(0.6)).expect("a JPEG");
        assert_eq!(crate::media::picture_kind(&jpeg), Some("jpg"));
    }
}
