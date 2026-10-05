//! Backdrop blur for modal overlays.
//!
//! GPUI has no backdrop-filter: its renderer only draws quads, paths, text and
//! sprites, and `Style` exposes opacity but no filters. So we make the frosted
//! panel ourselves. When a modal opens we grab the window's pixels, shrink and
//! blur them on the CPU, and hand the result back as an image the overlay
//! draws behind itself. The snapshot is taken once, which is what a modal
//! wants anyway: the content behind it is frozen.
//!
//! Only the app's own window is ever read, which no platform asks the user
//! about: `PrintWindow` on Windows, the window server's picture of the window
//! on macOS, which needs no Screen Recording permission, and `GetImage` on
//! X11. Wayland has no portable way for a client to read its own surface
//! back, and from macOS 26 the window server no longer gives that picture, so
//! there the overlay keeps its plain scrim.

use crate::prelude::*;
use std::sync::Arc;

/// How much the snapshot is shrunk before blurring. The GPU smooths it back up,
/// which does most of the blurring for free and keeps the CPU work tiny.
const SHRINK: u32 = 4;
/// Box-blur passes over the shrunk image. Three approximates a gaussian.
const PASSES: usize = 2;
/// Blur radius in shrunk pixels.
const RADIUS: i32 = 1;

/// Sample the window and return a blurred copy for a modal to sit on, or
/// `None` when the platform cannot do it. The overlay treats `None` as "just
/// use the plain scrim", so there is nothing else to handle.
pub(crate) fn capture(window: &Window) -> Option<Arc<RenderImage>> {
    let (width, height, bgra) = read(window)?;
    let (small_w, small_h, mut small) = shrink(width, height, &bgra);
    for _ in 0..PASSES {
        small = box_blur(small_w, small_h, &small);
    }
    lift(&mut small);
    let buffer = image::RgbaImage::from_raw(small_w, small_h, small)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

#[cfg(windows)]
use windows_impl::read;

#[cfg(target_os = "macos")]
use mac::read;

#[cfg(target_os = "linux")]
use x11::read;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn read(_window: &Window) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// A blurred near-black UI is just black. Lift it so the backdrop reads as
/// frosted glass rather than a void, leaving alpha alone.
fn lift(pixels: &mut [u8]) {
    for chunk in pixels.as_chunks_mut::<4>().0 {
        for c in chunk.iter_mut().take(3) {
            *c = ((*c as f32 * 1.18) + 6.).min(255.) as u8;
        }
    }
}

/// Average blocks of `SHRINK` pixels into one. Cheap and already a soft blur.
fn shrink(width: u32, height: u32, bgra: &[u8]) -> (u32, u32, Vec<u8>) {
    let out_w = (width / SHRINK).max(1);
    let out_h = (height / SHRINK).max(1);
    let mut out = vec![0u8; (out_w * out_h * 4) as usize];
    for y in 0..out_h {
        for x in 0..out_w {
            let mut sum = [0u32; 4];
            let mut count = 0u32;
            for dy in 0..SHRINK {
                for dx in 0..SHRINK {
                    let sx = x * SHRINK + dx;
                    let sy = y * SHRINK + dy;
                    if sx >= width || sy >= height {
                        continue;
                    }
                    let i = ((sy * width + sx) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += bgra[i + c] as u32;
                    }
                    count += 1;
                }
            }
            let o = ((y * out_w + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (sum[c] / count.max(1)) as u8;
            }
        }
    }
    (out_w, out_h, out)
}

/// One box-blur pass. Repeating it approximates a gaussian.
fn box_blur(width: u32, height: u32, src: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let mut sum = [0u32; 4];
            let mut count = 0u32;
            for dy in -RADIUS..=RADIUS {
                for dx in -RADIUS..=RADIUS {
                    let (sx, sy) = (x + dx, y + dy);
                    if sx < 0 || sy < 0 || sx >= width as i32 || sy >= height as i32 {
                        continue;
                    }
                    let i = ((sy as u32 * width + sx as u32) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += src[i + c] as u32;
                    }
                    count += 1;
                }
            }
            let o = ((y as u32 * width + x as u32) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (sum[c] / count.max(1)) as u8;
            }
        }
    }
    out
}

#[cfg(windows)]
mod windows_impl {
    use crate::prelude::*;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
    };
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

    /// The window's pixels, or `None` if it cannot be read.
    pub(super) fn read(window: &Window) -> Option<(u32, u32, Vec<u8>)> {
        let hwnd = crate::platform::window::hwnd(window)?;
        unsafe { read_pixels(hwnd) }
    }

    /// Read the window's pixels as BGRA, which is the order `RenderImage` wants.
    unsafe fn read_pixels(hwnd: *mut core::ffi::c_void) -> Option<(u32, u32, Vec<u8>)> {
        unsafe {
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetClientRect(hwnd, &mut rect) == 0 {
                return None;
            }
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            if w <= 0 || h <= 0 {
                return None;
            }

            let screen = GetDC(std::ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let previous = SelectObject(dc, bitmap);

            // PW_RENDERFULLCONTENT (2) is what makes this work for a GPU-drawn window.
            let drawn = PrintWindow(hwnd, dc, 2) != 0;

            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // Negative height asks for a top-down buffer.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            let read = GetDIBits(
                dc,
                bitmap,
                0,
                h as u32,
                pixels.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            );

            SelectObject(dc, previous);
            DeleteObject(bitmap);
            DeleteDC(dc);
            ReleaseDC(std::ptr::null_mut(), screen);

            if !drawn || read == 0 {
                return None;
            }
            // BitBlt leaves alpha at zero; the backdrop must be opaque.
            for chunk in pixels.as_chunks_mut::<4>().0 {
                chunk[3] = 255;
            }
            Some((w as u32, h as u32, pixels))
        }
    }
}

// objc 0.2's msg_send! checks a `cargo-clippy` feature this crate never declares.
// Built for tests everywhere, so the layouts are checked where CI runs.
#[cfg(any(target_os = "macos", test))]
#[allow(unexpected_cfgs)]
mod mac {
    #[cfg(target_os = "macos")]
    use crate::prelude::*;

    /// kCGBitmapAlphaInfoMask: where alpha sits in a pixel, and what it means.
    const ALPHA: u32 = 0x1f;
    /// kCGBitmapFloatComponents and kCGImagePixelFormatMask: float or packed
    /// pixels, which a window's picture never has.
    const UNREAD: u32 = 0x100 | 0xf_0000;
    /// kCGBitmapByteOrderMask, and the two orders of a 32-bit pixel in it.
    const ORDER: u32 = 0x7000;
    const LITTLE: u32 = 2 << 12;
    const BIG: u32 = 4 << 12;

    /// How a Core Graphics picture lays out its pixels.
    pub(super) struct Layout {
        pub width: usize,
        pub height: usize,
        pub bits_per_component: usize,
        pub bits_per_pixel: usize,
        pub bytes_per_row: usize,
        pub bitmap_info: u32,
    }

    /// NSOperatingSystemVersion.
    #[cfg(target_os = "macos")]
    #[repr(C)]
    struct Version {
        major: isize,
        minor: isize,
        patch: isize,
    }

    #[cfg(target_os = "macos")]
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        static CGRectNull: core_graphics::geometry::CGRect;
        fn CGImageGetBitmapInfo(image: *const std::ffi::c_void) -> u32;
    }

    /// The window's pixels as BGRA, or `None` where they cannot be read.
    #[cfg(target_os = "macos")]
    pub(super) fn read(window: &Window) -> Option<(u32, u32, Vec<u8>)> {
        use core_graphics::image::CGImageRef;
        use core_graphics::window::{
            create_image, kCGWindowImageBoundsIgnoreFraming, kCGWindowImageNominalResolution,
            kCGWindowListOptionIncludingWindow,
        };
        use objc::runtime::{BOOL, NO, Object};
        use objc::{class, msg_send, sel, sel_impl};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        let view = h.ns_view.as_ptr() as *mut Object;
        let (number, tahoe) = unsafe {
            let ns_window: *mut Object = msg_send![view, window];
            if ns_window.is_null() {
                return None;
            }
            let number: isize = msg_send![ns_window, windowNumber];
            // macOS 26 calls itself 16 to a program built with an older SDK.
            let version = Version {
                major: 16,
                minor: 0,
                patch: 0,
            };
            let info: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
            let tahoe: BOOL = msg_send![info, isOperatingSystemAtLeastVersion: version];
            (number, tahoe != NO)
        };
        // From macOS 26 the window server gives an app no picture of its own
        // window (Chromium stopped asking it for one there), and a picture has
        // been seen to take thirty seconds, which would hang the window.
        if tahoe {
            return None;
        }
        let number = u32::try_from(number).ok().filter(|&number| number > 0)?;
        // The app's own window alone, which needs no Screen Recording
        // permission, so macOS never asks for it. One pixel a point is
        // plenty for a picture about to be shrunk.
        let image = create_image(
            unsafe { CGRectNull },
            kCGWindowListOptionIncludingWindow,
            number,
            kCGWindowImageBoundsIgnoreFraming | kCGWindowImageNominalResolution,
        )?;
        // A reference to a foreign type is the object's own pointer.
        let image_ref: &CGImageRef = &image;
        let layout = Layout {
            width: image.width(),
            height: image.height(),
            bits_per_component: image.bits_per_component(),
            bits_per_pixel: image.bits_per_pixel(),
            bytes_per_row: image.bytes_per_row(),
            bitmap_info: unsafe { CGImageGetBitmapInfo((image_ref as *const CGImageRef).cast()) },
        };
        let bgra = to_bgra(image.data().bytes(), &layout)?;
        let (width, height) = (layout.width, layout.height);
        Some((
            u32::try_from(width).ok()?,
            u32::try_from(height).ok()?,
            bgra,
        ))
    }

    /// A Core Graphics picture's pixels in `layout` as opaque BGRA, or None
    /// where `data` is too short or the layout is one not read here.
    pub(super) fn to_bgra(data: &[u8], layout: &Layout) -> Option<Vec<u8>> {
        let info = layout.bitmap_info;
        if layout.bits_per_component != 8 || layout.bits_per_pixel != 32 || info & UNREAD != 0 {
            return None;
        }
        // CGImageAlphaInfo: premultiplied, straight or skipped alpha, last or
        // first; no alpha at all, alpha alone and anything newer are refused.
        let (alpha_first, straight) = match info & ALPHA {
            1 | 5 => (false, false),
            2 | 6 => (true, false),
            3 => (false, true),
            4 => (true, true),
            _ => return None,
        };
        let little = match info & ORDER {
            0 | BIG => false,
            LITTLE => true,
            _ => return None,
        };
        // Red, green, blue and alpha in the order the layout names them, which
        // a little-endian layout keeps back to front in memory.
        let named = if alpha_first {
            [1, 2, 3, 0]
        } else {
            [0, 1, 2, 3]
        };
        let [r, g, b, a] = if little { named.map(|i| 3 - i) } else { named };
        let (width, height, stride) = (layout.width, layout.height, layout.bytes_per_row);
        let row = width.checked_mul(4)?;
        let needed = stride
            .checked_mul(height.checked_sub(1)?)?
            .checked_add(row)?;
        if width == 0 || stride < row || data.len() < needed {
            return None;
        }
        let mut out = Vec::with_capacity(row.checked_mul(height)?);
        for line in data.chunks(stride).take(height) {
            out.extend_from_slice(&line[..row]);
        }
        for pixel in out.as_chunks_mut::<4>().0 {
            let read = *pixel;
            *pixel = [read[b], read[g], read[r], 255];
            // Straight colour is laid over black, as premultiplied colour
            // already is, so that both read the same once made opaque.
            if straight {
                for c in &mut pixel[..3] {
                    *c = (u16::from(*c) * u16::from(read[a]) / 255) as u8;
                }
            }
        }
        Some(out)
    }
}

// Built for tests everywhere, so the formats are checked where CI runs.
#[cfg(any(target_os = "linux", test))]
mod x11 {
    #[cfg(target_os = "linux")]
    use crate::prelude::*;
    #[cfg(target_os = "linux")]
    use x11rb::rust_connection::RustConnection;

    /// How the X server lays out a picture's pixels.
    pub(super) struct Format {
        pub bits_per_pixel: u8,
        pub scanline_pad: u8,
        pub msb_first: bool,
        pub red: u32,
        pub green: u32,
        pub blue: u32,
    }

    /// The window's pixels as BGRA, or `None` where they cannot be read.
    #[cfg(target_os = "linux")]
    pub(super) fn read(window: &Window) -> Option<(u32, u32, Vec<u8>)> {
        // Wayland gives a client no portable way to read its own surface
        // back: copying the screen is each compositor's own protocol to offer,
        // and the portal for it asks the user first.
        if gpui::guess_compositor() != "X11" {
            return None;
        }
        let device = |length: Pixels| {
            let length = (f32::from(length) * window.scale_factor()).round();
            u16::try_from(length as i64).ok()
        };
        let size = window.viewport_size();
        let (width, height) = (device(size.width)?, device(size.height)?);
        // gpui 0.2.2 lends out neither its X connection nor its window: both
        // raw handles panic on X11. So the window is looked for over a
        // connection of our own, which opens in well under a millisecond.
        let (conn, _) = x11rb::connect(None).ok()?;
        let id = own_window(&conn, width, height)?;
        pixels(&conn, id, width, height)
    }

    /// Our own viewable X window `width` by `height`, found by the process id
    /// gpui puts on every window it makes.
    #[cfg(target_os = "linux")]
    fn own_window(conn: &RustConnection, width: u16, height: u16) -> Option<u32> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MapState};
        let atom = |name: &[u8]| Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom);
        let (pid, clients) = (atom(b"_NET_WM_PID")?, atom(b"_NET_CLIENT_LIST")?);
        // A window manager lists the windows it manages, wherever it has put
        // them; without one, ours is a child of the root.
        let mut windows = Vec::new();
        for screen in &conn.setup().roots {
            let listed = conn
                .get_property(false, screen.root, clients, AtomEnum::WINDOW, 0, u32::MAX)
                .ok()?
                .reply()
                .ok()?;
            windows.extend(listed.value32().into_iter().flatten());
            windows.extend(conn.query_tree(screen.root).ok()?.reply().ok()?.children);
        }
        // Asked of every window before any answer is read, in one round trip.
        let asked: Vec<_> = (windows.iter())
            .map(|&window| conn.get_property(false, window, pid, AtomEnum::CARDINAL, 0, 1))
            .collect();
        let mut ours = windows
            .into_iter()
            .zip(asked)
            .filter_map(|(window, asked)| {
                let pid = asked.ok()?.reply().ok()?.value32()?.next();
                (pid == Some(std::process::id())).then_some(window)
            });
        ours.find(|&window| {
            let shown = conn
                .get_window_attributes(window)
                .ok()
                .map(|asked| asked.reply());
            let size = conn.get_geometry(window).ok().map(|asked| asked.reply());
            let shown = shown.and_then(Result::ok).map(|shown| shown.map_state);
            let size = size
                .and_then(Result::ok)
                .map(|size| (size.width, size.height));
            shown == Some(MapState::VIEWABLE) && size == Some((width, height))
        })
    }

    /// The pixels of X window `id`, `width` by `height`, as BGRA.
    #[cfg(target_os = "linux")]
    pub(super) fn pixels(
        conn: &RustConnection,
        id: u32,
        width: u16,
        height: u16,
    ) -> Option<(u32, u32, Vec<u8>)> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder};
        let image = conn
            .get_image(ImageFormat::Z_PIXMAP, id, 0, 0, width, height, !0)
            .ok()?
            .reply()
            .ok()?;
        let setup = conn.setup();
        let pixmap = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == image.depth)?;
        let visual = setup
            .roots
            .iter()
            .flat_map(|screen| &screen.allowed_depths)
            .flat_map(|depth| &depth.visuals)
            .find(|found| found.visual_id == image.visual)?;
        let format = Format {
            bits_per_pixel: pixmap.bits_per_pixel,
            scanline_pad: pixmap.scanline_pad,
            msb_first: setup.image_byte_order == ImageOrder::MSB_FIRST,
            red: visual.red_mask,
            green: visual.green_mask,
            blue: visual.blue_mask,
        };
        let bgra = to_bgra(image.data, width.into(), height.into(), &format)?;
        Some((width.into(), height.into(), bgra))
    }

    /// A Z-format picture's pixels in `format` as opaque BGRA, or None where
    /// `data` is too short or the format is one not read here.
    pub(super) fn to_bgra(
        mut data: Vec<u8>,
        width: usize,
        height: usize,
        format: &Format,
    ) -> Option<Vec<u8>> {
        let bits = usize::from(format.bits_per_pixel);
        let pad = usize::from(format.scanline_pad).max(8);
        let masks = [format.blue, format.green, format.red];
        if width == 0 || height == 0 || !matches!(bits, 16 | 24 | 32) || masks.contains(&0) {
            return None;
        }
        let stride = (width * bits).div_ceil(pad) * pad / 8;
        let bytes = bits / 8;
        if data.len() < stride * (height - 1) + width * bytes {
            return None;
        }
        // A depth-32 window keeps alpha in the bits no mask covers, and a
        // depth-24 one leaves them unused; neither reaches the backdrop.
        // Nearly every screen now has four bytes a pixel, least significant
        // first, which is BGRA already: only alpha is set, in place.
        if bits == 32
            && stride == width * 4
            && !format.msb_first
            && masks == [0xff, 0xff00, 0xff_0000]
        {
            data.truncate(width * height * 4);
            for pixel in data.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            return Some(data);
        }
        let channels = masks.map(|mask| {
            let shift = mask.trailing_zeros();
            (shift, (mask >> shift).count_ones())
        });
        let mut out = Vec::with_capacity(width * height * 4);
        for line in data.chunks(stride).take(height) {
            for raw in line[..width * bytes].chunks_exact(bytes) {
                let pixel = match format.msb_first {
                    true => raw.iter().fold(0, |pixel, &b| pixel << 8 | u32::from(b)),
                    false => raw
                        .iter()
                        .rev()
                        .fold(0, |pixel, &b| pixel << 8 | u32::from(b)),
                };
                out.extend(channels.map(|channel| eight_bits(pixel, channel)));
                out.push(255);
            }
        }
        Some(out)
    }

    /// The colour `bits` wide at `shift` in `pixel`, stretched or cut to eight bits.
    fn eight_bits(pixel: u32, (shift, bits): (u32, u32)) -> u8 {
        let value = u64::from(pixel >> shift) & ((1 << bits) - 1);
        match bits {
            8.. => (value >> (bits - 8)) as u8,
            _ => (value * 255 / ((1 << bits) - 1)) as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mac::{self, Layout};
    use super::x11::{self, Format};

    /// The usual true-colour masks: red, green and blue in one byte each.
    fn format(bits_per_pixel: u8, scanline_pad: u8, msb_first: bool) -> Format {
        Format {
            bits_per_pixel,
            scanline_pad,
            msb_first,
            red: 0xff_0000,
            green: 0x00_ff00,
            blue: 0x00_00ff,
        }
    }

    fn layout(width: usize, height: usize, bytes_per_row: usize, bitmap_info: u32) -> Layout {
        Layout {
            width,
            height,
            bits_per_component: 8,
            bits_per_pixel: 32,
            bytes_per_row,
            bitmap_info,
        }
    }

    #[test]
    fn a_32_bit_x_pixel_comes_out_bgra_in_either_byte_order() {
        // Red 0x11, green 0x22, blue 0x33, least significant byte first.
        let read = x11::to_bgra(vec![0x33, 0x22, 0x11, 0x00], 1, 1, &format(32, 32, false));
        assert_eq!(read, Some(vec![0x33, 0x22, 0x11, 255]));
        let read = x11::to_bgra(vec![0x00, 0x11, 0x22, 0x33], 1, 1, &format(32, 32, true));
        assert_eq!(read, Some(vec![0x33, 0x22, 0x11, 255]));
    }

    #[test]
    fn a_depth_32_windows_alpha_is_left_out_as_a_depth_24_windows_spare_byte_is() {
        // Half-transparent and spare: neither byte reaches the backdrop.
        let data = vec![0x33, 0x22, 0x11, 0x80, 0x66, 0x55, 0x44, 0x00];
        let read = x11::to_bgra(data, 2, 1, &format(32, 32, false));
        assert_eq!(
            read,
            Some(vec![0x33, 0x22, 0x11, 255, 0x66, 0x55, 0x44, 255])
        );
        let read = x11::to_bgra(vec![0x80, 0x11, 0x22, 0x33], 1, 1, &format(32, 32, true));
        assert_eq!(read, Some(vec![0x33, 0x22, 0x11, 255]));
    }

    #[test]
    fn a_padded_row_is_read_without_its_padding() {
        // Three bytes a pixel, rows of two padded out from six bytes to eight.
        let data = [
            1, 2, 3, 4, 5, 6, 0xee, 0xee, 7, 8, 9, 10, 11, 12, 0xee, 0xee,
        ];
        let tight = vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        let read = x11::to_bgra(data.to_vec(), 2, 2, &format(24, 32, false));
        assert_eq!(read, Some(tight.clone()));
        // The last row need not carry its padding.
        let read = x11::to_bgra(data[..14].to_vec(), 2, 2, &format(24, 32, false));
        assert_eq!(read, Some(tight));
    }

    #[test]
    fn a_16_bit_x_pixel_is_stretched_to_eight_bits_a_colour() {
        let wide = Format {
            red: 0xf800,
            green: 0x07e0,
            blue: 0x001f,
            ..format(16, 32, false)
        };
        let read = x11::to_bgra(vec![0xff, 0xff, 0, 0, 0x00, 0xf8, 0, 0], 1, 2, &wide);
        assert_eq!(read, Some(vec![255, 255, 255, 255, 0, 0, 255, 255]));
    }

    #[test]
    fn an_x_picture_cut_short_empty_or_in_a_format_not_read_here_gives_none() {
        let plain = format(32, 32, false);
        assert_eq!(x11::to_bgra(vec![0; 7], 2, 1, &plain), None);
        assert_eq!(x11::to_bgra(vec![], 0, 0, &plain), None);
        assert_eq!(x11::to_bgra(vec![0; 4], 1, 0, &plain), None);
        assert_eq!(x11::to_bgra(vec![0; 8], 2, 1, &format(8, 32, false)), None);
        let maskless = Format {
            green: 0,
            ..format(32, 32, false)
        };
        assert_eq!(x11::to_bgra(vec![0; 4], 1, 1, &maskless), None);
    }

    #[test]
    fn a_core_graphics_picture_comes_out_bgra_whatever_its_byte_order() {
        // Premultiplied ARGB little-endian, the window server's own layout,
        // is already BGRA in memory.
        let bgra = [0x33, 0x22, 0x11, 0xff];
        let read = mac::to_bgra(&bgra, &layout(1, 1, 4, 2 | 2 << 12));
        assert_eq!(read, Some(vec![0x33, 0x22, 0x11, 255]));
        // Premultiplied RGBA in the default order, which is big-endian.
        let rgba = [0x11, 0x22, 0x33, 0xff];
        let read = mac::to_bgra(&rgba, &layout(1, 1, 4, 1));
        assert_eq!(read, Some(vec![0x33, 0x22, 0x11, 255]));
        // ARGB big-endian with alpha skipped, and RGBX little-endian.
        let xrgb = [0x00, 0x11, 0x22, 0x33];
        let read = mac::to_bgra(&xrgb, &layout(1, 1, 4, 6 | 4 << 12));
        assert_eq!(read, Some(vec![0x33, 0x22, 0x11, 255]));
        let xbgr = [0x00, 0x33, 0x22, 0x11];
        let read = mac::to_bgra(&xbgr, &layout(1, 1, 4, 5 | 2 << 12));
        assert_eq!(read, Some(vec![0x33, 0x22, 0x11, 255]));
    }

    #[test]
    fn the_padding_at_the_end_of_a_core_graphics_row_is_skipped() {
        let data = [1, 2, 3, 255, 0xee, 0xee, 0xee, 0xee, 4, 5, 6, 255];
        let read = mac::to_bgra(&data, &layout(1, 2, 8, 2 | 2 << 12));
        assert_eq!(read, Some(vec![1, 2, 3, 255, 4, 5, 6, 255]));
    }

    #[test]
    fn straight_alpha_is_laid_over_black_and_every_pixel_comes_out_opaque() {
        // Non-premultiplied RGBA at half alpha, and premultiplied at none.
        let read = mac::to_bgra(&[200, 100, 50, 128], &layout(1, 1, 4, 3));
        assert_eq!(read, Some(vec![25, 50, 100, 255]));
        let read = mac::to_bgra(&[0, 0, 0, 0], &layout(1, 1, 4, 1));
        assert_eq!(read, Some(vec![0, 0, 0, 255]));
    }

    #[test]
    fn a_core_graphics_picture_malformed_empty_or_not_read_here_gives_none() {
        let argb = 2 | 2 << 12;
        assert_eq!(mac::to_bgra(&[0; 7], &layout(2, 1, 8, argb)), None);
        assert_eq!(mac::to_bgra(&[0; 4], &layout(2, 1, 4, argb)), None);
        assert_eq!(mac::to_bgra(&[], &layout(0, 1, 0, argb)), None);
        assert_eq!(mac::to_bgra(&[0; 4], &layout(1, 0, 4, argb)), None);
        assert_eq!(mac::to_bgra(&[0; 4], &layout(usize::MAX, 2, 4, argb)), None);
        // No alpha at all, alpha only, a 16-bit byte order, float components.
        for info in [0, 7, 2 | 1 << 12, argb | 0x100] {
            assert_eq!(
                mac::to_bgra(&[0; 4], &layout(1, 1, 4, info)),
                None,
                "{info:#x}"
            );
        }
        let wide = Layout {
            bits_per_component: 16,
            bits_per_pixel: 64,
            ..layout(1, 1, 8, argb)
        };
        assert_eq!(mac::to_bgra(&[0; 8], &wide), None);
    }
}
