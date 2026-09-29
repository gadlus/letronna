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
    // Taken rather than unwrapped: a backend that keeps its callback alive
    // past the stream would otherwise hand back an empty recording.
    let mut samples = std::mem::take(&mut *samples.lock().unwrap_or_else(|e| e.into_inner()));
    fit(&mut samples, channels, max_bytes);
    Ok(Captured {
        samples,
        sample_rate,
        channels,
    })
}

/// Cut `samples` to the whole frames that fit in `max_bytes`: the stream
/// keeps sending between the last check and closing, and a file over the cap
/// is refused outright.
fn fit(samples: &mut Vec<i16>, channels: u16, max_bytes: u64) {
    let frame = usize::from(channels.max(1));
    let most = usize::try_from(max_bytes / 2).unwrap_or(usize::MAX) / frame * frame;
    samples.truncate(most);
}

/// Why a stream error ends the recording, or `None` for an overrun: ALSA and
/// JACK report one whenever the machine is briefly too busy, and it costs a
/// few milliseconds of sound, not everything heard so far.
fn stopped(e: cpal::StreamError) -> Option<String> {
    match e {
        cpal::StreamError::BufferUnderrun => None,
        e => Some(format!("The microphone stopped: {e}")),
    }
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
        if let Some(why) = stopped(e) {
            *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
        }
    };
    // Every format cpal can hand over, not just the common three: with no
    // float input on offer, cpal's ALSA default picks 32- or 24-bit before
    // 16-bit, and refusing those would leave the mic dead on that machine.
    use cpal::SampleFormat as F;
    let built = match config.sample_format() {
        F::I8 => input::<i8>(device, &stream_config, samples, on_error),
        F::I16 => input::<i16>(device, &stream_config, samples, on_error),
        F::I24 => input::<cpal::I24>(device, &stream_config, samples, on_error),
        F::I32 => input::<i32>(device, &stream_config, samples, on_error),
        F::I64 => input::<i64>(device, &stream_config, samples, on_error),
        F::U8 => input::<u8>(device, &stream_config, samples, on_error),
        F::U16 => input::<u16>(device, &stream_config, samples, on_error),
        F::U24 => input::<cpal::U24>(device, &stream_config, samples, on_error),
        F::U32 => input::<u32>(device, &stream_config, samples, on_error),
        F::U64 => input::<u64>(device, &stream_config, samples, on_error),
        F::F32 => input::<f32>(device, &stream_config, samples, on_error),
        F::F64 => input::<f64>(device, &stream_config, samples, on_error),
        other => return Err(format!("Letronna cannot record in {other:?}.")),
    };
    built.map_err(|e| format!("Could not start recording: {e}"))
}

/// An input stream in the device's own sample type, appending what it hears
/// to `samples`.
fn input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Mutex<Vec<i16>>>,
    on_error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    i16: cpal::FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _| heard(data, &mut samples.lock().unwrap_or_else(|e| e.into_inner())),
        on_error,
        None,
    )
}

/// Append `data`, in whatever sample type the device sends, as 16-bit.
fn heard<T>(data: &[T], samples: &mut Vec<i16>)
where
    T: cpal::Sample,
    i16: cpal::FromSample<T>,
{
    samples.extend(data.iter().map(|&s| s.to_sample::<i16>()));
}

#[cfg(test)]
mod tests {
    use super::{fit, heard, stopped};

    #[test]
    fn every_sample_type_lands_as_16_bit_at_the_same_level() {
        fn as_16_bit<T>(data: &[T]) -> Vec<i16>
        where
            T: cpal::Sample,
            i16: cpal::FromSample<T>,
        {
            let mut samples = Vec::new();
            heard(data, &mut samples);
            samples
        }
        let full = [-32_768, 0, 32_767];
        assert_eq!(as_16_bit(&[i32::MIN, 0, i32::MAX]), full);
        assert_eq!(as_16_bit(&[0u16, 32_768, u16::MAX]), full);
        assert_eq!(as_16_bit(&[-1.0f32, 0.0, 1.0]), full);
        let lowest = cpal::I24::new(-(1 << 23)).expect("in range");
        assert_eq!(as_16_bit(&[lowest]), [-32_768]);
    }

    #[test]
    fn a_recording_past_the_cap_is_cut_to_whole_frames_within_it() {
        let mut samples = vec![0i16; 100];
        fit(&mut samples, 2, 101);
        assert_eq!(samples.len(), 50);
        let mut samples = vec![0i16; 100];
        fit(&mut samples, 3, 100);
        assert_eq!(samples.len(), 48);
        let mut samples = vec![0i16; 10];
        fit(&mut samples, 1, 1_000);
        assert_eq!(samples.len(), 10);
    }

    #[test]
    fn an_overrun_keeps_recording_and_a_lost_device_ends_it() {
        assert_eq!(stopped(cpal::StreamError::BufferUnderrun), None);
        assert!(
            stopped(cpal::StreamError::DeviceNotAvailable)
                .is_some_and(|why| why.starts_with("The microphone stopped"))
        );
    }
}
