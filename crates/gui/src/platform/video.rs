//! Playing a video a tool made, in the chat, with its sound.
//!
//! gpui draws pictures, not video, so the frames are decoded here and handed
//! to the chat one at a time as images, each when it is due, while the sound
//! plays through the output speech uses. Windows decodes both with Media
//! Foundation, so what Windows can play plays here. Elsewhere there is no
//! decoder to lean on: `play` says so, and the chat opens the system player.

use crate::prelude::*;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a playing video sends the chat. Only Windows decodes video, so only
/// Windows sends these; elsewhere `play` refuses before anything is sent.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum Event {
    /// The next frame, due on screen now.
    Frame(Arc<RenderImage>),
    /// The video is over: played to the end, stopped, or failed and why.
    Ended(Option<String>),
}

/// A video playing. Its frames arrive on `events`; dropping it stops it.
pub(crate) struct Playing {
    /// Which playing this is, so what a stopped one still sends is not taken
    /// for the next, even of the same video.
    pub(crate) id: u64,
    pub(crate) path: PathBuf,
    pub(crate) events: async_channel::Receiver<Event>,
    stop: Arc<AtomicBool>,
}

impl Drop for Playing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Start playing `path`, or say why it cannot be played here.
pub(crate) fn play(path: &Path) -> Result<Playing, String> {
    static PLAYED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (sender, events) = async_channel::bounded(2);
    let stop = Arc::new(AtomicBool::new(false));
    start(path.to_owned(), sender, stop.clone())?;
    Ok(Playing {
        id: PLAYED.fetch_add(1, Ordering::Relaxed),
        path: path.to_owned(),
        events,
        stop,
    })
}

#[cfg(not(windows))]
fn start(_: PathBuf, _: async_channel::Sender<Event>, _: Arc<AtomicBool>) -> Result<(), String> {
    Err("Videos play in the system player here.".into())
}

#[cfg(windows)]
fn start(
    path: PathBuf,
    sender: async_channel::Sender<Event>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::spawn(move || {
        let why = decode::run(&path, &sender, &stop).err();
        let _ = sender.send_blocking(Event::Ended(why));
    });
    Ok(())
}

#[cfg(windows)]
mod decode {
    use super::Event;
    use crate::prelude::*;
    use std::num::{NonZeroU16, NonZeroU32};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::core::HSTRING;

    /// Frames are made no wider than this. The chat shows them small, and a
    /// full-size frame would cost its copy and its upload for nothing.
    const WIDTH: u32 = 640;
    const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

    /// Play `path` to the end, or until `stop` is set.
    pub(super) fn run(
        path: &Path,
        sender: &async_channel::Sender<Event>,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        // SAFETY: COM and Media Foundation are started and stopped on this
        // thread alone, and nothing they hand out outlives `play`.
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let played = match MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                Ok(()) => {
                    let played = play(path, sender, stop);
                    let _ = MFShutdown();
                    played
                }
                Err(e) => Err(e),
            };
            if com {
                CoUninitialize();
            }
            played.map_err(|e| format!("This video cannot be played here: {}", e.message()))
        }
    }

    unsafe fn play(
        path: &Path,
        sender: &async_channel::Sender<Event>,
        stop: &AtomicBool,
    ) -> windows::core::Result<()> {
        let name = HSTRING::from(path.to_string_lossy().replace('/', "\\"));
        // The sound is read whole first: a clip is short, and with all of it
        // in hand the picture can follow the clock the sound starts.
        let sound = unsafe { sound(&name) }.ok();
        let (reader, width, height, stride) = unsafe { pictures(&name) }?;

        let speaker = sound.and_then(|(samples, channels, rate)| {
            let mut device = rodio::DeviceSinkBuilder::open_default_sink().ok()?;
            device.log_on_drop(false);
            let player = rodio::Player::connect_new(device.mixer());
            player.append(rodio::buffer::SamplesBuffer::new(channels, rate, samples));
            Some((device, player))
        });
        let started = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
            unsafe {
                reader.ReadSample(
                    VIDEO,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
            }?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else {
                continue;
            };
            let frame = unsafe { frame(&sample, width, height, stride) }?;
            // Media Foundation counts time in hundreds of nanoseconds.
            let due = Duration::from_nanos(time.max(0) as u64 * 100);
            while let Some(wait) = due.checked_sub(started.elapsed()) {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                std::thread::sleep(wait.min(Duration::from_millis(10)));
            }
            match sender.try_send(Event::Frame(Arc::new(frame))) {
                // A chat too busy to draw the last frame skips this one.
                Ok(()) | Err(async_channel::TrySendError::Full(_)) => {}
                Err(async_channel::TrySendError::Closed(_)) => return Ok(()),
            }
        }
        // The sound can run on past the last frame.
        while speaker.as_ref().is_some_and(|(_, p)| !p.empty()) && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// A reader for the video's frames, as BGRA no wider than `WIDTH`, and
    /// the frames' width, height and row length.
    unsafe fn pictures(name: &HSTRING) -> windows::core::Result<(IMFSourceReader, u32, u32, i32)> {
        let mut settings = None;
        unsafe { MFCreateAttributes(&mut settings, 1) }?;
        let settings = settings.ok_or_else(windows::core::Error::empty)?;
        // The advanced processor converts to RGB and scales, both.
        unsafe { settings.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1) }?;
        let reader = unsafe { MFCreateSourceReaderFromURL(name, &settings) }?;
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(VIDEO, true)?;
        }
        let native = unsafe { reader.GetNativeMediaType(VIDEO, 0) }?;
        let (width, height) = split(unsafe { native.GetUINT64(&MF_MT_FRAME_SIZE) }?);
        let wanted = unsafe { MFCreateMediaType() }?;
        unsafe {
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        }
        if width > WIDTH && height > 0 {
            let small = (WIDTH, (height * WIDTH / width) & !1);
            unsafe {
                wanted.SetUINT64(&MF_MT_FRAME_SIZE, ((small.0 as u64) << 32) | small.1 as u64)
            }?;
        }
        if unsafe { reader.SetCurrentMediaType(VIDEO, None, &wanted) }.is_err() {
            // Not every decoder can be scaled from; take the size it makes.
            unsafe {
                wanted.DeleteItem(&MF_MT_FRAME_SIZE)?;
                reader.SetCurrentMediaType(VIDEO, None, &wanted)?;
            }
        }
        let made = unsafe { reader.GetCurrentMediaType(VIDEO) }?;
        let (width, height) = split(unsafe { made.GetUINT64(&MF_MT_FRAME_SIZE) }?);
        let stride = unsafe { made.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
            .map(|s| s as i32)
            .unwrap_or(width as i32 * 4);
        Ok((reader, width, height, stride))
    }

    /// The video's sound, decoded whole, as interleaved samples.
    unsafe fn sound(name: &HSTRING) -> windows::core::Result<(Vec<f32>, NonZeroU16, NonZeroU32)> {
        let reader = unsafe { MFCreateSourceReaderFromURL(name, None) }?;
        let wanted = unsafe { MFCreateMediaType() }?;
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(AUDIO, true)?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
            reader.SetCurrentMediaType(AUDIO, None, &wanted)?;
        }
        let made = unsafe { reader.GetCurrentMediaType(AUDIO) }?;
        let channels = unsafe { made.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }?;
        let rate = unsafe { made.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }?;
        let (Some(channels), Some(rate)) =
            (NonZeroU16::new(channels as u16), NonZeroU32::new(rate))
        else {
            return Err(windows::core::Error::empty());
        };
        let mut samples = Vec::new();
        loop {
            let (mut flags, mut sample) = (0u32, None);
            unsafe {
                reader.ReadSample(AUDIO, 0, None, Some(&mut flags), None, Some(&mut sample))
            }?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else {
                continue;
            };
            let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
            unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }?;
            // SAFETY: the buffer holds `length` bytes of 32-bit float samples
            // while it is locked.
            let floats =
                unsafe { std::slice::from_raw_parts(data.cast::<f32>(), length as usize / 4) };
            samples.extend_from_slice(floats);
            unsafe { buffer.Unlock() }?;
        }
        Ok((samples, channels, rate))
    }

    /// One frame's pixels, top row first, as gpui draws them.
    unsafe fn frame(
        sample: &IMFSample,
        width: u32,
        height: u32,
        stride: i32,
    ) -> windows::core::Result<RenderImage> {
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
        let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }?;
        let row = width as usize * 4;
        let step = stride.unsigned_abs() as usize;
        let mut pixels = vec![0u8; row * height as usize];
        if step >= row && step * height as usize <= length as usize {
            // SAFETY: the check above keeps every row read inside the
            // `length` bytes the locked buffer holds.
            let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) };
            for y in 0..height as usize {
                // A negative stride means the rows are stored bottom up.
                let from = if stride < 0 {
                    height as usize - 1 - y
                } else {
                    y
                } * step;
                pixels[y * row..(y + 1) * row].copy_from_slice(&bytes[from..from + row]);
            }
        }
        unsafe { buffer.Unlock() }?;
        // RGB32 is BGRX: the fourth byte is not alpha, and gpui would draw
        // whatever is in it as see-through.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        let image = image::RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(windows::core::Error::empty)?;
        Ok(RenderImage::new([image::Frame::new(image)]))
    }

    /// A frame size, which Media Foundation packs as width << 32 | height.
    fn split(size: u64) -> (u32, u32) {
        ((size >> 32) as u32, size as u32)
    }
}
