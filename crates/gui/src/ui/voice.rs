//! The mic button in the composer: one press to record, another to have
//! what was said typed in.
//!
//! Recording happens on its own thread, in `platform::mic`, since a cpal
//! stream cannot cross threads on every platform. What comes back is turned
//! into a WAV file just long enough to be transcribed, on the same hosted
//! account `speak` and `transcribe` already use, and is gone again before the
//! text lands in the composer -- successful or not.

use crate::platform::mic::{self, Captured};
use crate::prelude::*;
use letronna_core::cancel::Cancel;
use std::path::Path;
use std::time::{Duration, Instant};

actions!(voice, [ToggleVoice]);

/// A recording longer than this is cut off and sent as it is.
const MAX_DURATION: Duration = Duration::from_secs(120);
/// The upload cap the transcription API takes, less the WAV header that goes
/// in front of the samples; recording stops at it too, rather than filling
/// the cap and having the upload refused anyway.
const MAX_BYTES: u64 = letronna_core::media::UPLOAD_BYTES - WAV_HEADER as u64;
/// The bytes `encode_wav` writes ahead of the samples.
const WAV_HEADER: usize = 44;

/// Where the mic button stands: silent until pressed, recording until
/// pressed again or a cap is hit, then away being transcribed.
pub(crate) enum Voice {
    Recording { cancel: Cancel, started: Instant },
    Transcribing,
}

/// Whether anything set up here can turn speech into text: Letronna's hosted
/// transcription, same as `speak` and the `transcribe` tool, needs an
/// account signed in.
pub(crate) fn available() -> bool {
    letronna_core::auth::signed_in()
}

impl Chat {
    /// Start recording, or stop it and have what was said typed in, one
    /// press either way.
    pub(crate) fn toggle_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.voice {
            Some(Voice::Recording { cancel, .. }) => {
                cancel.set();
                self.voice = Some(Voice::Transcribing);
                cx.notify();
            }
            // Already finishing the last one; a second press does nothing
            // until it lands.
            Some(Voice::Transcribing) => {}
            None => self.start_recording(window, cx),
        }
    }

    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !available() {
            self.error = Some(
                "Voice input is transcribed on Letronna's hosted models, which need a \
                 signed-in account. Sign in first."
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        let cancel = Cancel::default();
        let (tx, rx) = async_channel::bounded(1);
        let recording = cancel.clone();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(mic::record(&recording, MAX_DURATION, MAX_BYTES));
        });
        self.voice = Some(Voice::Recording {
            cancel,
            started: Instant::now(),
        });
        cx.notify();
        // Redraws once a second so the elapsed time moves; stops on its own
        // once recording does, rather than ticking while idle.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let recording = this.update(cx, |this, cx| {
                    let recording = matches!(this.voice, Some(Voice::Recording { .. }));
                    if recording {
                        cx.notify();
                    }
                    recording
                });
                if !matches!(recording, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        // The session whose draft the composer holds, so the words go back
        // to it even if another one is on screen by the time they land.
        let session = self.draft_for;
        cx.spawn_in(window, async move |this, cx| {
            let captured = rx.recv().await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.recorded(captured, session, window, cx);
            });
        })
        .detach();
    }

    /// What the recording thread came back with: heard speech to transcribe,
    /// a reason it could not record, or nothing (the window closed under it).
    fn recorded(
        &mut self,
        captured: Result<Result<Captured, String>, async_channel::RecvError>,
        session: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let audio = match captured {
            Ok(Ok(audio)) => audio,
            Ok(Err(why)) => {
                self.voice = None;
                self.error = Some(why);
                cx.notify();
                return;
            }
            Err(_) => {
                self.voice = None;
                cx.notify();
                return;
            }
        };
        self.voice = Some(Voice::Transcribing);
        cx.notify();
        let transcribing = cx
            .background_executor()
            .spawn(async move { transcribe(&audio) });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = transcribing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.voice = None;
                match outcome {
                    Ok(text) if !text.is_empty() => this.land(session, text, window, cx),
                    Ok(_) => this.error = Some("No speech was heard.".to_owned()),
                    Err(why) => this.error = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Type `text` into the draft of `session`, where the recording began.
    fn land(&mut self, session: u64, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if session != self.draft_for && self.store.index_of(session).is_some() {
            add_to_draft(self.drafts.entry(session).or_default(), &text);
            return;
        }
        // At the caret, not appended, so it lands where the user was about
        // to type and the rest of the draft is untouched either side of it.
        self.composer
            .update(cx, |input, cx| input.insert(text, window, cx));
    }

    /// The mic button: plain for a press to start, red with the time
    /// counting up while recording, a spinner while what was said is sent
    /// off to be transcribed.
    pub(crate) fn mic_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let button = Button::new("mic").plain(cx).xsmall();
        match &self.voice {
            Some(Voice::Recording { started, .. }) => button
                .text_color(cx.theme().danger)
                .tooltip("Stop recording")
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(Icon::new(AppIcon::Stop).size_3())
                        .child(elapsed(started.elapsed())),
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_voice(window, cx))),
            Some(Voice::Transcribing) => button
                .icon(Icon::new(IconName::LoaderCircle))
                .text_color(cx.theme().muted_foreground)
                .tooltip("Transcribing…")
                .disabled(true),
            None if available() => button
                .icon(Icon::new(AppIcon::Mic))
                .tooltip_with_action("Voice input", &ToggleVoice, None)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_voice(window, cx))),
            None => button
                .icon(Icon::new(AppIcon::Mic))
                .text_color(cx.theme().muted_foreground)
                .tooltip("Sign in to use voice input")
                .on_click(cx.listener(|this, _, window, cx| this.toggle_voice(window, cx))),
        }
    }
}

/// "0:07", "1:42": elapsed time as the mic button's tooltip shows it.
fn elapsed(time: Duration) -> String {
    let secs = time.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Add `text` to the end of a draft that is not in the composer, so there is
/// no caret to put it at, keeping a space between it and what was there.
fn add_to_draft(draft: &mut String, text: &str) {
    if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
        draft.push(' ');
    }
    draft.push_str(text);
}

/// The recording as a WAV file, transcribed and gone again, whether or not
/// that worked -- it is only ever a copy of what the user just said out
/// loud, kept no longer than turning it into text takes.
fn transcribe(audio: &Captured) -> Result<String, String> {
    let wav = encode_wav(audio);
    with_temp_wav(&wav, letronna_core::media::transcribe_path)
}

/// Save `wav` to a temp file, run `send` with its path, and remove it
/// whether or not `send` succeeded.
fn with_temp_wav<T>(
    wav: &[u8],
    send: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!(
        "letronna-voice-{}-{stamp:x}.wav",
        std::process::id()
    ));
    let result = std::fs::write(&path, wav)
        .map_err(|e| format!("Could not save the recording: {e}"))
        .and_then(|()| send(&path));
    let _ = std::fs::remove_file(&path);
    result
}

/// A minimal 16-bit PCM WAV file: small enough that pulling in a crate for it
/// is not worth it, as `media::base64` reasons about the same trade.
fn encode_wav(audio: &Captured) -> Vec<u8> {
    let data_len = (audio.samples.len() * 2) as u32;
    let byte_rate = audio.sample_rate * u32::from(audio.channels) * 2;
    let block_align = audio.channels * 2;
    let mut wav = Vec::with_capacity(WAV_HEADER + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&audio.channels.to_le_bytes());
    wav.extend_from_slice(&audio.sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in &audio.samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::{Captured, add_to_draft, elapsed, encode_wav, with_temp_wav};

    #[test]
    fn words_for_a_session_off_screen_join_the_end_of_its_draft() {
        let mut draft = String::new();
        add_to_draft(&mut draft, "hello");
        assert_eq!(draft, "hello");
        add_to_draft(&mut draft, "there");
        assert_eq!(draft, "hello there");
        let mut draft = "first line\n".to_owned();
        add_to_draft(&mut draft, "second");
        assert_eq!(draft, "first line\nsecond");
    }

    #[test]
    fn the_time_reads_minutes_and_seconds() {
        assert_eq!(elapsed(std::time::Duration::from_secs(7)), "0:07");
        assert_eq!(elapsed(std::time::Duration::from_secs(102)), "1:42");
        assert_eq!(elapsed(std::time::Duration::from_secs(0)), "0:00");
    }

    #[test]
    fn the_wav_header_carries_the_recording_s_own_shape() {
        let audio = Captured {
            samples: vec![0, 1_000, -1_000, 32_767],
            sample_rate: 16_000,
            channels: 1,
        };
        let wav = encode_wav(&audio);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1); // channels
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            16_000
        ); // sample rate
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16); // bits per sample
        assert_eq!(&wav[36..40], b"data");
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len as usize, audio.samples.len() * 2);
        assert_eq!(wav.len(), 44 + data_len as usize);
        let first_sample = i16::from_le_bytes([wav[44], wav[45]]);
        assert_eq!(first_sample, 0);
    }

    #[test]
    fn the_caps_match_what_a_recording_may_spend() {
        assert_eq!(super::MAX_DURATION, std::time::Duration::from_secs(120));
        assert_eq!(super::MAX_BYTES, 25 * 1024 * 1024 - 44);
    }

    #[test]
    fn a_recording_filled_to_the_cap_is_still_small_enough_to_send() {
        let audio = Captured {
            samples: vec![0; super::MAX_BYTES as usize / 2],
            sample_rate: 96_000,
            channels: 2,
        };
        let wav = encode_wav(&audio);
        assert_eq!(&wav[super::WAV_HEADER - 8..super::WAV_HEADER - 4], b"data");
        assert!(wav.len() as u64 <= letronna_core::media::UPLOAD_BYTES);
    }

    #[test]
    fn the_temp_recording_is_removed_whether_or_not_it_was_sent() {
        let before = temp_voice_files();
        let sent = with_temp_wav(b"not really a wav", |_| Ok::<_, String>(()));
        assert!(sent.is_ok());
        assert_eq!(temp_voice_files(), before);
        let failed = with_temp_wav(b"not really a wav", |_| Err::<(), _>("nope".to_owned()));
        assert_eq!(failed, Err("nope".to_owned()));
        assert_eq!(temp_voice_files(), before);
    }

    fn temp_voice_files() -> Vec<std::path::PathBuf> {
        std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("letronna-voice-"))
            })
            .collect()
    }
}
