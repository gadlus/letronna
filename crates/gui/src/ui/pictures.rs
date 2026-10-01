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
use std::collections::HashMap;
use std::sync::Arc;

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
    // At most twice as wide as tall, on a screen up to twice as sharp.
    held_while_shown(path, (THUMB * 4.) as u32).child(
        img(path.to_path_buf())
            .h(px(THUMB))
            .max_w(px(THUMB * 2.))
            .object_fit(ObjectFit::Contain)
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border),
    )
}

/// Where the picture an `img` inside reads from `path` is kept: decoded no
/// larger than `longest` pixels a side, and only while it is drawn, then
/// let go of, from memory and the GPU. Left to gpui, every picture drawn
/// from a file stays decoded at full size until the app quits -- a phone's
/// photo is 48 MB, and the GPU's copy of it as much again.
pub(crate) fn held_while_shown(path: &std::path::Path, longest: u32) -> ImageCacheElement {
    image_cache(Held {
        id: std::sync::Arc::<std::path::Path>::from(path).into(),
        longest,
    })
}

/// The pictures of one `held_while_shown`, kept as the element's state.
struct Held {
    id: ElementId,
    longest: u32,
}

impl ImageCacheProvider for Held {
    fn provide(&mut self, window: &mut Window, cx: &mut App) -> AnyImageCache {
        let longest = self.longest;
        window
            .with_global_id(self.id.clone(), |id, window| {
                window.with_element_state::<Entity<Shrunk>, _>(id, |cache, _| {
                    let cache = cache.unwrap_or_else(|| Shrunk::new(longest, cx));
                    (cache.clone(), cache)
                })
            })
            .into()
    }
}

/// Pictures by the hash of where they were read from: `None` while read.
struct Shrunk {
    this: WeakEntity<Shrunk>,
    longest: u32,
    pictures: HashMap<u64, Option<Result<Arc<RenderImage>, ImageCacheError>>>,
}

impl Shrunk {
    fn new(longest: u32, cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|cx| Self {
            this: cx.weak_entity(),
            longest,
            pictures: HashMap::default(),
        });
        // Not drawn any more: the GPU's copies go too.
        cx.observe_release(&cache, |cache, cx| {
            for (_, picture) in cache.pictures.drain() {
                if let Some(Ok(picture)) = picture {
                    cx.drop_image(picture, None);
                }
            }
        })
        .detach();
        cache
    }
}

impl ImageCache for Shrunk {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            resource.hash(&mut hasher);
            hasher.finish()
        };
        if let Some(picture) = self.pictures.get(&key) {
            return picture.clone();
        }
        self.pictures.insert(key, None);
        // gpui's own reading, for every kind of picture it knows; an SVG is
        // drawn at a scale of its own, which a copy would lose.
        let svg = matches!(resource, Resource::Path(path)
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")));
        let longest = if svg { u32::MAX } else { self.longest };
        let read = ImageAssetLoader::load(resource.clone(), cx);
        let picture = cx
            .background_executor()
            .spawn(async move { read.await.map(|picture| shrunk(picture, longest)) });
        let (this, view) = (self.this.clone(), window.current_view());
        window
            .spawn(cx, async move |cx| {
                let picture = picture.await;
                let _ = this.update(cx, |cache, _| cache.pictures.insert(key, Some(picture)));
                cx.on_next_frame(move |_, cx| cx.notify(view));
            })
            .detach();
        None
    }
}

/// `picture` no larger than `longest` a side; one with frames to play is
/// left as it is.
fn shrunk(picture: Arc<RenderImage>, longest: u32) -> Arc<RenderImage> {
    let size = picture.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    if picture.frame_count() != 1 || width.max(height) <= longest {
        return picture;
    }
    let Some(full) = picture
        .as_bytes(0)
        .and_then(|bytes| image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(width, height, bytes))
    else {
        return picture;
    };
    let scale = f64::from(longest) / f64::from(width.max(height));
    let fits = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
    // Blue before red stays so: each channel is shrunk on its own.
    let small = image::imageops::thumbnail(&full, fits(width), fits(height));
    Arc::new(RenderImage::new([image::Frame::new(small)]))
}

/// `bytes` as a picture goes to the model: as they are when they are a kind
/// every provider takes and small enough; otherwise scaled to `LONG_SIDE`, as
/// a PNG, which keeps a screenshot's text sharp, unless that is still too
/// big or the picture was a photo to begin with, then as a JPEG.
pub(crate) fn fit(bytes: &[u8]) -> Result<Vec<u8>, String> {
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
    use super::{Arc, DynamicImage, ImageFormat, LONG_SIDE, encode, fit, media, shrunk};

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

    #[test]
    fn a_picture_is_drawn_no_larger_than_it_is_shown() {
        let picture = |width, height| {
            let mut bgra = image::RgbaImage::new(width, height);
            for pixel in bgra.pixels_mut() {
                pixel.0 = [10, 20, 250, 255];
            }
            Arc::new(gpui::RenderImage::new([image::Frame::new(bgra)]))
        };
        let wide = shrunk(picture(4_000, 3_000), 720);
        assert_eq!((wide.size(0).width.0, wide.size(0).height.0), (720, 540));
        // Blue before red, as gpui draws it, still.
        assert_eq!(&wide.as_bytes(0).unwrap()[..4], &[10, 20, 250, 255]);
        let tall = shrunk(picture(300, 3_000), 720);
        assert_eq!((tall.size(0).width.0, tall.size(0).height.0), (72, 720));
        let small = picture(640, 480);
        assert!(Arc::ptr_eq(&shrunk(small.clone(), 720), &small));
    }
}
