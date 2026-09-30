//! A faint film grain for the window's chrome and empty pages.
//!
//! letronna.ai draws noise over its whole page. The window keeps it to places
//! nobody reads for long -- the title bar, the sidebar, the empty chat and the
//! settings backdrop -- and never lays it over the transcript, code or diffs.
//! gpui has no tiled image, so a canvas paints one 512px tile across its
//! bounds, a tile pixel to each screen pixel. The tile comes from a fixed seed
//! rather than an asset or the clock, so it is the same every start. Each
//! surface's grain is a cached view of its own: a caret blink or a streamed
//! token redraws the chat, and the grain's recorded tiles are reused rather
//! than painted again. Static, never animated.

use crate::prelude::*;
use std::sync::{Arc, LazyLock};

/// The tile's side in pixels: 1 MB of RGBA, uploaded once, so a large
/// window takes a few dozen draws where a 128px tile took hundreds.
const SIDE: u32 = 512;
/// How much the grain covers what is under it: 9 of 255 is about 3.5%.
const STRENGTH: u8 = 9;

/// Grey noise at one faint alpha; grey, so gpui's BGRA order cannot swap it.
fn tile_pixels() -> Vec<u8> {
    // xorshift32 from a fixed seed: cheap, and the same noise every run.
    let mut state: u32 = 0x9e37_79b9;
    let mut pixels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for _ in 0..SIDE * SIDE {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let grey = (state >> 24) as u8;
        pixels.extend_from_slice(&[grey, grey, grey, STRENGTH]);
    }
    pixels
}

/// The tile, made the first time the grain is drawn and kept from then on.
static TILE: LazyLock<Option<Arc<RenderImage>>> = LazyLock::new(|| {
    let image = image::RgbaImage::from_raw(SIDE, SIDE, tile_pixels())?;
    Some(Arc::new(RenderImage::new([image::Frame::new(image)])))
});

/// One surface's grain, tiled across the whole of its bounds.
pub(crate) struct Grain;

impl Render for Grain {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        canvas(
            |_, _, _| {},
            |bounds, _, window, _| {
                let Some(tile) = TILE.clone() else {
                    return;
                };
                // One tile pixel to each screen pixel keeps the grain fine.
                let side = px(SIDE as f32 / window.scale_factor());
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    let mut y = bounds.top();
                    while y < bounds.bottom() {
                        let mut x = bounds.left();
                        while x < bounds.right() {
                            let at = Bounds::new(point(x, y), size(side, side));
                            let _ =
                                window.paint_image(at, Corners::default(), tile.clone(), 0, false);
                            x += side;
                        }
                        y += side;
                    }
                });
            },
        )
        .size_full()
    }
}

/// A grain view for each surface that has one; a cached view keeps one
/// recording, so two surfaces cannot share it.
pub(crate) struct Grains {
    pub(crate) title_bar: Entity<Grain>,
    pub(crate) sidebar: Entity<Grain>,
    pub(crate) empty_chat: Entity<Grain>,
    pub(crate) settings: Entity<Grain>,
}

impl Grains {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            title_bar: cx.new(|_| Grain),
            sidebar: cx.new(|_| Grain),
            empty_chat: cx.new(|_| Grain),
            settings: cx.new(|_| Grain),
        }
    }
}

impl Chat {
    /// A surface's grain over the whole of its parent, or nothing when
    /// Appearance has it off.
    pub(crate) fn grain(&self, layer: &Entity<Grain>) -> Option<AnyView> {
        let over_parent = StyleRefinement::default()
            .absolute()
            .top_0()
            .left_0()
            .size_full();
        self.store
            .settings
            .grain
            .then(|| AnyView::from(layer.clone()).cached(over_parent))
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude's gpui `test` macro would shadow the
    // built-in attribute here.
    use super::{STRENGTH, tile_pixels};

    #[test]
    fn the_grain_tile_is_the_same_every_start() {
        let tile = tile_pixels();
        assert_eq!(tile, tile_pixels());
        assert_eq!(tile.len(), 1024 * 1024);
        // Pinned, so noise that changes between builds is a change someone meant.
        assert_eq!(
            tile[..16],
            [
                81, 81, 81, 9, 224, 224, 224, 9, 123, 123, 123, 9, 1, 1, 1, 9
            ]
        );
        // Grey at one faint alpha, with the noise in the grey.
        assert!(
            tile.chunks(4)
                .all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == STRENGTH)
        );
        let greys: std::collections::HashSet<u8> = tile.chunks(4).map(|p| p[0]).collect();
        assert!(greys.len() > 250, "only {} greys in the tile", greys.len());
    }
}
