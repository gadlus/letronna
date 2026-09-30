//! Pictures attached to the next message: pasted, dropped on the chat or
//! picked from the attach menu.
//!
//! Each is scaled down here, with the image crate the window already has,
//! and saved in Letronna's `media/` before it is shown in the composer, so
//! the core sends the same small file on every request and the session keeps
//! it as it was sent. They wait per session, as drafts do.

use crate::prelude::*;
use gpui_component::input::Paste;
use image::{DynamicImage, ImageFormat};
use letronna_core::media;

/// The long side a picture is scaled down to before it is attached: the most
/// Anthropic looks at before it scales one down itself, and more than OpenAI
/// keeps of it (768 on the short side).
const LONG_SIDE: u32 = 1_568;
/// The height of a picture waiting in the composer or shown in a prompt.
const THUMB: f32 = 56.;

impl Chat {
    /// Attach the pictures among `paths` to the active session's next message.
    pub(crate) fn attach_files(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attach_pictures(
            move || {
                paths
                    .into_iter()
                    .map(|path| {
                        let name = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let bytes = std::fs::read(&path).map_err(|e| e.to_string());
                        (name, bytes)
                    })
                    .collect()
            },
            window,
            cx,
        );
    }

    /// Attach the picture on the clipboard, in place of the paste, when it
    /// holds one and no text; otherwise the box pastes as it always has.
    pub(crate) fn paste_picture(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        // Copied text can bring a picture of itself along, as Office's does.
        if item
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::String(_)))
        {
            return;
        }
        let Some(bytes) = item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::Image(image) => Some(image.bytes.clone()),
            _ => None,
        }) else {
            return;
        };
        cx.stop_propagation();
        self.attach_pictures(move || vec![("pasted".to_owned(), Ok(bytes))], window, cx);
    }

    /// Ask for picture files and attach them.
    pub(crate) fn pick_pictures(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                let _ = this.update_in(cx, |this, window, cx| this.attach_files(paths, window, cx));
            }
        })
        .detach();
    }

    /// Scale down and save what `load` reads, off the window's thread, then
    /// add each to the session's waiting pictures or say why it could not be.
    fn attach_pictures(
        &mut self,
        load: impl FnOnce() -> Vec<(String, Result<Vec<u8>, String>)> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.store.conversations[self.store.active].id;
        let saving = cx.background_executor().spawn(async move {
            load()
                .into_iter()
                .map(|(name, bytes)| {
                    bytes
                        .and_then(|bytes| fit(&bytes))
                        .and_then(|bytes| media::attach(&name, &bytes))
                        .map_err(|e| format!("Cannot attach {name}: {e}"))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update(cx, |this, cx| {
                for picture in saved {
                    match picture {
                        Ok(path) => this.attachments.entry(session).or_default().push(path),
                        Err(error) => this.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The pictures waiting to go with the active session's next message.
    pub(crate) fn attached(&self) -> &[PathBuf] {
        let session = self.store.conversations[self.store.active].id;
        self.attachments.get(&session).map_or(&[], Vec::as_slice)
    }

    /// The pictures waiting to go with the active session's next message,
    /// each with a button that takes it off; none when there are none.
    pub(crate) fn attached_row(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let session = self.store.conversations[self.store.active].id;
        let waiting = Some(self.attached()).filter(|w| !w.is_empty())?;
        let pictures = waiting.iter().enumerate().map(|(i, path)| {
            h_flex()
                .gap_0p5()
                .items_start()
                .child(thumbnail(path, cx))
                .child(
                    Button::new(("unattach", i))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(IconName::Close))
                        .tooltip("Take this picture off")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(waiting) = this.attachments.get_mut(&session)
                                && i < waiting.len()
                            {
                                waiting.remove(i);
                            }
                            cx.notify();
                        })),
                )
        });
        Some(h_flex().flex_wrap().gap_2().pb_1().children(pictures))
    }
}

/// The pictures sent with a prompt, small, under its words; one since
/// deleted is left out.
pub(crate) fn sent_row(paths: &[PathBuf], cx: &App) -> impl IntoElement {
    let kept = paths.iter().filter(|path| path.exists());
    h_flex()
        .flex_wrap()
        .gap_2()
        .children(kept.map(|path| thumbnail(path, cx)))
}

fn thumbnail(path: &std::path::Path, cx: &App) -> impl IntoElement {
    img(path.to_path_buf())
        .h(px(THUMB))
        .max_w(px(THUMB * 2.))
        .object_fit(ObjectFit::Contain)
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
}

/// `bytes` as a picture goes to the model: as they are when they are a kind
/// every provider takes and small enough; otherwise scaled to `LONG_SIDE`, as
/// a PNG, which keeps a screenshot's text sharp, unless that is still too
/// big or the picture was a photo to begin with, then as a JPEG.
fn fit(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let picture = image::load_from_memory(bytes)
        .map_err(|_| "it is not a picture this can read.".to_owned())?;
    let long = picture.width().max(picture.height());
    let kind = media::picture_kind(bytes);
    if long <= LONG_SIDE && kind.is_some() && bytes.len() <= media::PICTURE_BYTES {
        return Ok(bytes.to_vec());
    }
    let picture = if long > LONG_SIDE {
        picture.thumbnail(LONG_SIDE, LONG_SIDE)
    } else {
        picture
    };
    if matches!(kind, Some("png") | None) {
        let png = encode(&picture, ImageFormat::Png)?;
        if png.len() <= media::PICTURE_BYTES {
            return Ok(png);
        }
    }
    encode(
        &DynamicImage::ImageRgb8(picture.to_rgb8()),
        ImageFormat::Jpeg,
    )
}

fn encode(picture: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    picture
        .write_to(&mut out, format)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{DynamicImage, ImageFormat, LONG_SIDE, encode, fit, media};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let picture = DynamicImage::ImageRgb8(image::RgbImage::new(width, height));
        encode(&picture, ImageFormat::Png).unwrap()
    }

    #[test]
    fn a_big_picture_is_scaled_to_the_long_side_and_a_small_one_kept() {
        let fitted = image::load_from_memory(&fit(&png(4_000, 1_000)).unwrap()).unwrap();
        assert_eq!((fitted.width(), fitted.height()), (LONG_SIDE, 392));
        let tall = image::load_from_memory(&fit(&png(600, 3_136)).unwrap()).unwrap();
        assert_eq!((tall.width(), tall.height()), (300, LONG_SIDE));
        let small = png(800, 600);
        assert_eq!(fit(&small).unwrap(), small);
        // A kind not every provider takes is sent as one that is.
        let bmp = encode(
            &DynamicImage::ImageRgb8(image::RgbImage::new(8, 8)),
            ImageFormat::Bmp,
        );
        assert_eq!(
            media::picture_kind(&fit(&bmp.unwrap()).unwrap()),
            Some("png")
        );
        assert!(fit(b"not a picture").is_err());
    }
}
