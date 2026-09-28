use crate::prelude::*;

// A family the system cannot resolve drops every glyph to Helvetica without
// warning, so each platform names fonts it actually ships. gpui turns
// ".SystemUIFont" into the platform's own UI face, but on Linux it matches
// other names literally and never expands fontconfig generics like
// "monospace", so Linux names DejaVu Sans Mono, gpui-component's own default.
#[cfg(target_os = "macos")]
pub(crate) const HEADING_FONT: &str = ".SystemUIFont";
#[cfg(target_os = "macos")]
pub(crate) const BODY_FONT: &str = ".SystemUIFont";
#[cfg(target_os = "macos")]
pub(crate) const MONO_FONT: &str = "Menlo";

#[cfg(not(target_os = "macos"))]
pub(crate) const HEADING_FONT: &str = "Segoe UI Variable Display";
#[cfg(not(target_os = "macos"))]
pub(crate) const BODY_FONT: &str = "Segoe UI Variable Text";
#[cfg(windows)]
pub(crate) const MONO_FONT: &str = "Cascadia Mono";
#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) const MONO_FONT: &str = "DejaVu Sans Mono";

pub(crate) const WORDMARK_FONT: &str = "Georgia";

// letronna.ai's Night palette: the site's deep blue frames the window and a
// calm near-black page holds the work. The colours are plain numbers so the
// test below can check each text colour against the surface it is drawn on.

/// The page, the site's near-black moved to the brand's 230° hue.
const NIGHT: u32 = 0x0a0d1a;
/// Body text, a paper-toned white: 15.5:1 on night without pure white's glare.
const TEXT: u32 = 0xe3e6f0;
/// Quieter text, 6.35:1 on night where the old grey reached 5.10.
const MUTED: u32 = 0x8c93ad;
/// Code blocks and menus, one step lighter than night; inputs stay on night.
const RAISED: u32 = 0x121729;
/// Borders, a blue hairline like the site's `--hair-blue`.
const HAIRLINE: u32 = 0x1f2745;
/// The sidebar and title bar, the brand's deep blue.
const DEEP: u32 = 0x000b66;
/// Sidebar text, on-blue at 85% over deep, as the dashboard draws its links.
const SIDEBAR_TEXT: u32 = 0xced1e8;
/// The selected sidebar row, on-blue at 10% over deep, as the dashboard marks its page.
const SIDEBAR_ON: u32 = 0x182275;
/// The brand's electric blue, for fills only: as text on night it reads at 2.9:1.
const BLUE: u32 = 0x0040ff;
/// Pressed blue, the fill at 80% brightness.
const BLUE_PRESSED: u32 = 0x0033cc;
/// Text on a blue fill, the site's on-blue: 6.03:1 on blue.
const ON_BLUE: u32 = 0xf2f4ff;
/// Blue that is read as text (links, the focus ring, the caret): 6.28:1 on night.
const SIGNAL: u32 = 0x6b8cff;
/// One "selected" colour for menus, lists and tabs: blue at 18% over night.
const SELECTED: u32 = 0x081643;
/// Errors and destructive buttons: 7.0:1 on night, and night text on it reads the same.
const DANGER: u32 = 0xff6b7a;
/// Pressed danger, the fill at 80% brightness like blue's: night text on it reads at 4.7:1.
const DANGER_PRESSED: u32 = 0xcc5662;

pub(crate) fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let t = Theme::global_mut(cx);
    let raised: Hsla = rgb(RAISED).into();
    let hairline: Hsla = rgb(HAIRLINE).into();
    let deep: Hsla = rgb(DEEP).into();
    let blue: Hsla = rgb(BLUE).into();
    let signal: Hsla = rgb(SIGNAL).into();
    let selected: Hsla = rgb(SELECTED).into();
    t.background = rgb(NIGHT).into();
    t.foreground = rgb(TEXT).into();
    // gpui-component's dark theme sets these to its own near-white, so button
    // labels, menu items and the title bar's hovered glyphs follow text here.
    t.secondary_foreground = t.foreground;
    t.popover_foreground = t.foreground;
    t.accent_foreground = t.foreground;
    t.muted_foreground = rgb(MUTED).into();
    t.secondary = raised;
    t.muted = raised;
    t.popover = raised;
    t.list = raised;
    t.table = raised;
    t.border = hairline;
    t.input = hairline;
    t.sidebar = deep;
    t.title_bar = deep;
    t.title_bar_border = deep;
    t.sidebar_border = deep;
    t.sidebar_foreground = rgb(SIDEBAR_TEXT).into();
    t.sidebar_accent = rgb(SIDEBAR_ON).into();
    t.primary = blue;
    t.progress_bar = blue;
    t.primary_active = rgb(BLUE_PRESSED).into();
    t.primary_foreground = rgb(ON_BLUE).into();
    t.link = signal;
    t.ring = signal;
    t.caret = signal;
    t.selection = blue.opacity(0.4);
    t.accent = selected;
    t.list_active = selected;
    t.tab_active = selected;
    // A selected ghost button (the model picker's kind tabs) and a pressed
    // outline or caption button fill with this, a neutral grey by default.
    t.secondary_active = selected;
    t.danger = rgb(DANGER).into();
    t.danger_active = rgb(DANGER_PRESSED).into();
    // gpui-component's near-white danger text reads at 2.5:1 on this lighter
    // red; night reads at 7:1, on the title bar's close button too.
    t.danger_foreground = rgb(NIGHT).into();
    // A hover colour equal to its base draws no hover: list rows and blue
    // and red buttons stay still under the pointer, and a scrollbar's thumb
    // keeps its colour, though gpui-component still widens it and fills its
    // track. Ghost and outline buttons work out their own hover, so Letronna
    // draws its own as `plain` and `framed` (ui::NoHover); menu items and
    // gpui-component's own ghost buttons still hover. On Windows and Linux
    // the title bar's minimise and maximise hover to secondary_hover, a
    // darker raised square on deep behind a white glyph, and close hovers to
    // danger.
    t.primary_hover = t.primary;
    t.secondary_hover = t.secondary;
    t.list_hover = t.list;
    t.accordion_hover = t.accordion;
    t.table_hover = t.table;
    t.danger_hover = t.danger;
    t.scrollbar_thumb_hover = t.scrollbar_thumb;
    // Hairlines rather than shadows, and near-square corners, like the site.
    t.shadow = false;
    t.radius = px(2.);
    t.radius_lg = px(4.);
    t.font_family = BODY_FONT.into();
    t.mono_font_family = MONO_FONT.into();
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: the prelude re-exports gpui, whose own
    // `test` macro would shadow the built-in attribute here.
    use super::{
        BLUE, BLUE_PRESSED, DANGER, DANGER_PRESSED, DEEP, MUTED, NIGHT, ON_BLUE, RAISED, SELECTED,
        SIDEBAR_ON, SIDEBAR_TEXT, SIGNAL, TEXT,
    };

    /// WCAG 2's relative luminance of an 0xrrggbb colour.
    fn luminance(colour: u32) -> f64 {
        let channel = |shift: u32| {
            let c = f64::from((colour >> shift) & 0xff) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    /// WCAG 2's contrast ratio between two colours, from 1 to 21.
    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_text_colour_passes_aa_on_its_surface() {
        // Black on white is the scale's top, so a broken formula cannot pass.
        assert!((contrast(0x000000, 0xffffff) - 21.0).abs() < 0.01);
        let pairs = [
            ("text on night", TEXT, NIGHT),
            ("muted on night", MUTED, NIGHT),
            ("text on raised", TEXT, RAISED),
            ("muted on raised", MUTED, RAISED),
            ("text on a selected row", TEXT, SELECTED),
            ("muted on a selected row", MUTED, SELECTED),
            ("signal on night", SIGNAL, NIGHT),
            ("signal on raised", SIGNAL, RAISED),
            ("on-blue on blue", ON_BLUE, BLUE),
            ("on-blue on pressed blue", ON_BLUE, BLUE_PRESSED),
            ("sidebar text on deep", SIDEBAR_TEXT, DEEP),
            ("sidebar text on its selected row", SIDEBAR_TEXT, SIDEBAR_ON),
            ("muted on deep", MUTED, DEEP),
            ("muted on the selected sidebar row", MUTED, SIDEBAR_ON),
            ("text on deep, the title bar's glyphs", TEXT, DEEP),
            ("danger on night, and night on danger", DANGER, NIGHT),
            ("night on pressed danger", NIGHT, DANGER_PRESSED),
        ];
        for (what, text, surface) in pairs {
            let ratio = contrast(text, surface);
            // AA asks 4.5:1 of body text.
            assert!(ratio >= 4.5, "{what} reads at {ratio:.2}:1, under 4.5:1");
        }
    }
}
