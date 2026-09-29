//! Recording the microphone into memory, for voice input.
//!
//! Cpal's stream is not safe to send anywhere else on every platform, same as
//! rodio's device in `audio.rs`, so `record` opens it, blocks the thread it
//! was called on until told to stop or a cap is hit, and closes it again
//! before returning. Nothing here runs unless a recording is in progress.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use letronna_core::cancel::Cancel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a recording came back as: 16-bit PCM samples, interleaved by
/// channel, at the device's own rate.
pub(crate) struct Captured {
    pub(crate) samples: Vec<i16>,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
}

/// Open the default microphone and record until `cancel` is set or `cap` (a
/// duration and a byte count, whichever comes first) is reached, then close
/// it. Blocks the calling thread, so call it from one made for this.
pub(crate) fn record(
    cancel: &Cancel,
    max_duration: Duration,
    max_bytes: u64,
) -> Result<Captured, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No microphone was found.")?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("Could not read the microphone's settings: {e}"))?;
    let (sample_rate, channels) = (config.sample_rate(), config.channels());
    let samples: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stream = build_stream(&device, &config, samples.clone(), failure.clone())?;
    stream
        .play()
        .map_err(|e| format!("Could not start recording: {e}"))?;
    let deadline = Instant::now() + max_duration;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // A cap stops the recording as surely as the caller asking does.
            cancel.set();
            break;
        }
        // Woken at once by `cancel.set()`; otherwise checked a few times a
        // second, for the byte cap and a permission failure heard mid-stream.
        if !cancel.sleep(left.min(Duration::from_millis(200))) {
            break;
        }
        if let Some(why) = failure.lock().unwrap_or_else(|e| e.into_inner()).take() {
            drop(stream);
            return Err(why);
        }
        let bytes = (samples.lock().unwrap_or_else(|e| e.into_inner()).len() * 2) as u64;
        if bytes >= max_bytes {
            cancel.set();
            break;
        }
    }
    drop(stream);
    let samples = Arc::try_unwrap(samples)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();
    Ok(Captured {
        samples,
        sample_rate,
        channels,
    })
}

/// A stream that appends what the microphone sends to `samples`, in the
/// device's own sample format, converted to 16-bit; `failure` catches a
/// device error heard after recording has already started, such as the OS
/// revoking permission mid-stream.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    samples: Arc<Mutex<Vec<i16>>>,
    failure: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let on_error = move |e: cpal::StreamError| {
        *failure.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(format!("The microphone stopped: {e}"));
    };
    let built = match config.sample_format() {
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                samples
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(data);
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _| {
                let mut samples = samples.lock().unwrap_or_else(|e| e.into_inner());
                samples.extend(data.iter().map(|&s| (i32::from(s) - 32_768) as i16));
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| {
                let mut samples = samples.lock().unwrap_or_else(|e| e.into_inner());
                samples.extend(
                    data.iter()
                        .map(|s| (s.clamp(-1., 1.) * f32::from(i16::MAX)) as i16),
                );
            },
            on_error,
            None,
        ),
        other => return Err(format!("Letronna cannot record in {other:?}.")),
    };
    built.map_err(|e| format!("Could not start recording: {e}"))
}
