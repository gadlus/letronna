//! The first frame of a video a tool made, to show it by in the chat.
//!
//! gpui cannot play video, so the chat shows a still and the system player
//! plays it. Windows makes the still with the shell's own thumbnailer, the
//! one Explorer uses, so any video Explorer can show has one. Elsewhere there
//! is none, and the chat shows a plain tile instead.

use crate::prelude::*;
use std::future::Future;
use std::sync::Arc;

/// The still, loaded once per file and kept by gpui's asset cache.
pub(crate) struct Poster;

impl Asset for Poster {
    type Source = PathBuf;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(path: PathBuf, _: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        // A thread of its own: the shell wants its COM apartment set up just
        // so, which a thread shared with other work may already have done
        // differently.
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = sender.send_blocking(first_frame(&path));
        });
        async move {
            receiver
                .recv()
                .await
                .ok()
                .flatten()
                .map(Arc::new)
                .ok_or_else(|| ImageCacheError::Asset("no still for this video".into()))
        }
    }
}

#[cfg(windows)]
fn first_frame(path: &std::path::Path) -> Option<RenderImage> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
        SIIGBF_THUMBNAILONLY,
    };
    use windows::core::HSTRING;

    // The shell parses the name as Explorer would, which is backslashes only.
    let name = HSTRING::from(path.to_string_lossy().replace('/', "\\"));
    // SAFETY: COM is set up and torn down on this thread alone, and the
    // bitmap the shell hands back is read and freed before it returns.
    unsafe {
        let started = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let frame = SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(&name, None)
            .and_then(|item| {
                // Only a real thumbnail: without it the shell falls back to the
                // player's icon, which is no picture of the video.
                item.GetImage(
                    SIZE { cx: 640, cy: 640 },
                    SIIGBF_BIGGERSIZEOK | SIIGBF_THUMBNAILONLY,
                )
            })
            .ok()
            .and_then(|bitmap| {
                let pixels = pixels(bitmap);
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(bitmap.into());
                pixels
            });
        if started {
            CoUninitialize();
        }
        frame
    }
}

/// A bitmap's pixels, in the BGRA order gpui draws, top row first.
#[cfg(windows)]
unsafe fn pixels(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<RenderImage> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetDIBits, GetObjectW,
        ReleaseDC,
    };

    let mut header = BITMAP::default();
    // SAFETY: `header` is a BITMAP, and the size says so.
    let read = unsafe {
        GetObjectW(
            bitmap.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some((&raw mut header).cast()),
        )
    };
    let (width, height) = (header.bmWidth, header.bmHeight);
    if read == 0 || width <= 0 || height <= 0 {
        return None;
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative: rows top to bottom, as an image is read.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut data = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: `data` holds every row of 32-bit pixels `info` asks for.
    let rows = unsafe {
        let screen = GetDC(None);
        let rows = GetDIBits(
            screen,
            bitmap,
            0,
            height as u32,
            Some(data.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, screen);
        rows
    };
    if rows == 0 {
        return None;
    }
    // GDI leaves the alpha byte at zero, which gpui would draw as nothing.
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    let image = image::RgbaImage::from_raw(width as u32, height as u32, data)?;
    Some(RenderImage::new([image::Frame::new(image)]))
}

#[cfg(not(windows))]
fn first_frame(_: &std::path::Path) -> Option<RenderImage> {
    None
}
