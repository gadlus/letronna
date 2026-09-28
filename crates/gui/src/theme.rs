use crate::prelude::*;
use gpui_component::highlighter::HighlightTheme;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
/// Success, gpui-component's own dark green, named so a palette can carry it.
const GREEN: u32 = 0x22c55e;

// Paper, the light palette: the site's ink on its paper. The frame stays
// light too, blue laid thinly over paper, because the sidebar and title bar
// draw the same text colours as the page (and gpui-component draws the
// search field's): on the dashboard's full blue, ink and ink-mute would not
// read at all.

/// The page, the site's paper: never pure white, which glares all day.
const PAPER: u32 = 0xf2f2f0;
/// Body text, the site's ink: 14.4:1 on paper.
const INK: u32 = 0x00126b;
/// Quieter text, the site's ink-mute: 7.3:1 on paper.
const INK_MUTE: u32 = 0x33499b;
/// Menus, tables and cards, a step lighter than paper.
const PAPER_RAISED: u32 = 0xfafaf8;
/// Code blocks, a step darker than paper so they stand apart from cards.
const PAPER_CODE: u32 = 0xe9e9e5;
/// Borders, the site's `--hair-ink` blended onto paper.
const PAPER_HAIRLINE: u32 = 0xc6cad8;
/// The sidebar and title bar, blue at 8% over paper: ink-mute still reads at 6.4:1.
const PAPER_FRAME: u32 = 0xdfe4f1;
/// The selected sidebar row, blue at 16% over paper: blue on it still reads at 4.5:1.
const PAPER_FRAME_ON: u32 = 0xcbd6f2;
/// One "selected" colour for menus, lists and tabs: blue at 10% over paper.
const PAPER_SELECTED: u32 = 0xdae0f2;
/// Errors, the site's crimson: 6.5:1 on paper, and paper on it reads the same.
const CRIMSON: u32 = 0xb00020;
/// Pressed crimson, the fill at 80% brightness.
const CRIMSON_PRESSED: u32 = 0x8d001a;
/// Success on paper, where gpui-component's light green reads at 2:1; this reads at 6.4:1.
const FOREST: u32 = 0x166534;
/// Attributes in Paper's code, gpui-component's gold darkened from 3.4:1 to
/// 5.3:1 on a code block.
const OCHRE: u32 = 0x735c23;

/// gpui-component's light syntax colours that read under 4.5:1 on a Paper
/// code block, each with the Paper colour drawn in its place.
const PAPER_SYNTAX: [(&str, u32); 6] = [
    ("comment", INK_MUTE),
    ("comment.doc", INK_MUTE),
    ("attribute", OCHRE),
    ("link_uri", INK_MUTE),
    ("string.special", CRIMSON),
    ("string.special.symbol", CRIMSON),
];

/// Every colour that differs between Night and Paper; blue fills are shared.
struct Palette {
    mode: ThemeMode,
    page: u32,
    text: u32,
    muted: u32,
    raised: u32,
    code: u32,
    hairline: u32,
    frame: u32,
    frame_text: u32,
    frame_on: u32,
    link: u32,
    selected: u32,
    /// How much blue a text selection lays over the page.
    selection: f32,
    danger: u32,
    danger_pressed: u32,
    on_danger: u32,
    green: u32,
}

const NIGHT_PALETTE: Palette = Palette {
    mode: ThemeMode::Dark,
    page: NIGHT,
    text: TEXT,
    muted: MUTED,
    raised: RAISED,
    code: RAISED,
    hairline: HAIRLINE,
    frame: DEEP,
    frame_text: SIDEBAR_TEXT,
    frame_on: SIDEBAR_ON,
    link: SIGNAL,
    selected: SELECTED,
    selection: 0.4,
    danger: DANGER,
    danger_pressed: DANGER_PRESSED,
    on_danger: NIGHT,
    green: GREEN,
};

const PAPER_PALETTE: Palette = Palette {
    mode: ThemeMode::Light,
    page: PAPER,
    text: INK,
    muted: INK_MUTE,
    raised: PAPER_RAISED,
    code: PAPER_CODE,
    hairline: PAPER_HAIRLINE,
    frame: PAPER_FRAME,
    frame_text: INK,
    frame_on: PAPER_FRAME_ON,
    // Blue itself reads as text on paper, at 5.9:1.
    link: BLUE,
    selected: PAPER_SELECTED,
    // Lighter than night's, so ink on it still reads at 9.5:1.
    selection: 0.25,
    danger: CRIMSON,
    danger_pressed: CRIMSON_PRESSED,
    on_danger: PAPER,
    green: FOREST,
};

/// The palette a setting asks for: "paper", "system" to match the
/// computer's light or dark mode, and anything else Night.
fn palette_for(setting: &str, system: WindowAppearance) -> &'static Palette {
    let light = matches!(
        system,
        WindowAppearance::Light | WindowAppearance::VibrantLight
    );
    match setting {
        "paper" => &PAPER_PALETTE,
        "system" if light => &PAPER_PALETTE,
        _ => &NIGHT_PALETTE,
    }
}

/// Whether a setting pins the native window chrome dark or light; following
/// the system it is left to the system, so the window hears when that changes.
fn pinned_dark(setting: &str) -> Option<bool> {
    match setting {
        "system" => None,
        "paper" => Some(false),
        _ => Some(true),
    }
}

/// Whether a setting changes with the computer's light or dark mode.
pub(crate) fn follows_system(setting: &str) -> bool {
    pinned_dark(setting).is_none()
}

/// Counts the times the theme was applied. A text view keeps the syntax
/// colours of its first draw, so its id carries this and a new theme draws
/// a new view.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// How many times the theme has been applied, for the ids of text views.
pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The syntax colours with Paper's swapped in; unchanged if gpui-component
/// ever stores them in a shape this cannot rewrite.
fn paper_highlight(base: &HighlightTheme) -> HighlightTheme {
    // The styles' fields are private to gpui-component, but they round-trip
    // through JSON, which keeps each style's slant and weight.
    let recoloured = serde_json::to_value(&base.style).and_then(|mut style| {
        for (name, colour) in PAPER_SYNTAX {
            let mut entry = style["syntax"][name].take();
            entry["color"] = format!("#{colour:06x}").into();
            style["syntax"][name] = entry;
        }
        serde_json::from_value(style)
    });
    let mut theme = base.clone();
    if let Ok(style) = recoloured {
        theme.style = style;
    }
    theme
}

/// Colour the window as the appearance setting asks.
pub(crate) fn apply(appearance: &str, window: &mut Window, cx: &mut App) {
    crate::platform::window::pin_chrome(window, pinned_dark(appearance));
    // The app's appearance, not the window's: gpui caches the window's and
    // only updates it once this change has settled.
    let p = palette_for(appearance, cx.window_appearance());
    Theme::change(p.mode, None, cx);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    let t = Theme::global_mut(cx);
    if !p.mode.is_dark() {
        t.highlight_theme = Arc::new(paper_highlight(&t.highlight_theme));
    }
    let raised: Hsla = rgb(p.raised).into();
    let hairline: Hsla = rgb(p.hairline).into();
    let frame: Hsla = rgb(p.frame).into();
    let blue: Hsla = rgb(BLUE).into();
    let link: Hsla = rgb(p.link).into();
    let selected: Hsla = rgb(p.selected).into();
    t.background = rgb(p.page).into();
    t.foreground = rgb(p.text).into();
    // gpui-component's dark theme sets these to its own near-white, so button
    // labels, menu items and the title bar's hovered glyphs follow text here.
    t.secondary_foreground = t.foreground;
    t.popover_foreground = t.foreground;
    t.accent_foreground = t.foreground;
    t.muted_foreground = rgb(p.muted).into();
    t.secondary = raised;
    t.muted = rgb(p.code).into();
    t.popover = raised;
    t.list = raised;
    t.table = raised;
    t.border = hairline;
    t.input = hairline;
    t.sidebar = frame;
    t.title_bar = frame;
    t.title_bar_border = frame;
    t.sidebar_border = frame;
    t.sidebar_foreground = rgb(p.frame_text).into();
    t.sidebar_accent = rgb(p.frame_on).into();
    t.primary = blue;
    t.progress_bar = blue;
    t.primary_active = rgb(BLUE_PRESSED).into();
    t.primary_foreground = rgb(ON_BLUE).into();
    t.link = link;
    t.ring = link;
    t.caret = link;
    t.selection = blue.opacity(p.selection);
    t.accent = selected;
    t.list_active = selected;
    t.tab_active = selected;
    // A selected ghost button (the model picker's kind tabs) and a pressed
    // outline or caption button fill with this, a neutral grey by default.
    t.secondary_active = selected;
    t.danger = rgb(p.danger).into();
    t.danger_active = rgb(p.danger_pressed).into();
    // gpui-component's near-white danger text reads at 2.5:1 on this lighter
    // red; night reads at 7:1, on the title bar's close button too, as paper
    // does on crimson.
    t.danger_foreground = rgb(p.on_danger).into();
    t.green = rgb(p.green).into();
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
    window.refresh();
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: the prelude re-exports gpui, whose own
    // `test` macro would shadow the built-in attribute here.
    use super::{
        BLUE, BLUE_PRESSED, DANGER, DANGER_PRESSED, DEEP, MUTED, NIGHT, NIGHT_PALETTE, ON_BLUE,
        PAPER_PALETTE, RAISED, SELECTED, SIDEBAR_ON, SIDEBAR_TEXT, SIGNAL, TEXT, follows_system,
        palette_for, paper_highlight, pinned_dark,
    };
    use gpui::{Rgba, WindowAppearance};
    use gpui_component::highlighter::HighlightTheme;

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

    /// An 0xrrggbb colour laid over another at some opacity.
    fn over(colour: u32, ground: u32, opacity: f32) -> u32 {
        [16, 8, 0].into_iter().fold(0, |mixed, shift| {
            let (c, g) = ((colour >> shift) & 0xff, (ground >> shift) & 0xff);
            let channel = c as f32 * opacity + g as f32 * (1. - opacity);
            mixed | ((channel.round() as u32) << shift)
        })
    }

    #[test]
    fn every_paper_text_colour_passes_aa_on_its_surface() {
        let p = &PAPER_PALETTE;
        let selection = over(BLUE, p.page, p.selection);
        let pairs = [
            ("ink on paper", p.text, p.page),
            ("ink-mute on paper", p.muted, p.page),
            ("ink on raised", p.text, p.raised),
            ("ink-mute on raised", p.muted, p.raised),
            ("ink on a code block", p.text, p.code),
            ("ink-mute on a code block", p.muted, p.code),
            ("ink on a selected row", p.text, p.selected),
            ("ink-mute on a selected row", p.muted, p.selected),
            ("ink on selected text", p.text, selection),
            ("blue on paper", p.link, p.page),
            ("blue on raised", p.link, p.raised),
            ("blue on a code block", p.link, p.code),
            ("blue on a selected row", p.link, p.selected),
            ("frame text on the frame", p.frame_text, p.frame),
            ("ink-mute on the frame", p.muted, p.frame),
            ("frame text on its selected row", p.frame_text, p.frame_on),
            ("ink-mute on the frame's selected row", p.muted, p.frame_on),
            ("blue on the frame's selected row", p.link, p.frame_on),
            ("on-blue on blue", ON_BLUE, BLUE),
            ("on-blue on pressed blue", ON_BLUE, BLUE_PRESSED),
            ("crimson on paper", p.danger, p.page),
            ("crimson on raised", p.danger, p.raised),
            ("paper on crimson", p.on_danger, p.danger),
            ("paper on pressed crimson", p.on_danger, p.danger_pressed),
            ("green on paper", p.green, p.page),
            ("green on raised", p.green, p.raised),
        ];
        for (what, text, surface) in pairs {
            let ratio = contrast(text, surface);
            assert!(ratio >= 4.5, "{what} reads at {ratio:.2}:1, under 4.5:1");
        }
    }

    #[test]
    fn every_paper_syntax_colour_passes_aa_on_a_code_block() {
        let paper = paper_highlight(&HighlightTheme::default_light());
        let syntax = &paper.style.syntax;
        let Ok(serde_json::Value::Object(names)) = serde_json::to_value(syntax) else {
            panic!("the syntax colours no longer serialize as a map");
        };
        let mut checked = 0;
        for name in names.keys() {
            let Some(colour) = syntax.style(name).and_then(|style| style.color) else {
                continue;
            };
            let colour = Rgba::from(colour);
            let hex = [colour.r, colour.g, colour.b]
                .into_iter()
                .fold(0, |hex, c| hex << 8 | (c * 255.).round() as u32);
            let ratio = contrast(hex, PAPER_PALETTE.code);
            assert!(ratio >= 4.5, "{name} reads at {ratio:.2}:1, under 4.5:1");
            checked += 1;
        }
        // The light theme colours two dozen kinds; none may slip through.
        assert!(checked >= 20, "only {checked} syntax colours checked");
        // A swapped colour keeps its style: links stay italic.
        let link = syntax.style("link_uri").and_then(|style| style.font_style);
        assert_eq!(link, Some(gpui::FontStyle::Italic));
    }

    #[test]
    fn the_system_setting_follows_the_windows_appearance() {
        let page = |setting, system| palette_for(setting, system).page;
        for system in [WindowAppearance::Light, WindowAppearance::VibrantLight] {
            assert_eq!(page("system", system), PAPER_PALETTE.page);
        }
        for system in [WindowAppearance::Dark, WindowAppearance::VibrantDark] {
            assert_eq!(page("system", system), NIGHT_PALETTE.page);
        }
        // The other settings hold whatever the computer does.
        for system in [WindowAppearance::Light, WindowAppearance::Dark] {
            assert_eq!(page("", system), NIGHT_PALETTE.page);
            assert_eq!(page("paper", system), PAPER_PALETTE.page);
            assert_eq!(page("from a newer build", system), NIGHT_PALETTE.page);
        }
        // Only following the system leaves the native chrome to it, so the
        // window hears when the computer changes.
        assert_eq!(pinned_dark("system"), None);
        assert_eq!(pinned_dark("paper"), Some(false));
        assert_eq!(pinned_dark(""), Some(true));
        // So a flip of the computer's mode is ignored under Night and Paper.
        assert!(follows_system("system"));
        assert!(!follows_system("paper") && !follows_system(""));
    }
}
