use crate::prelude::*;

// letronna.ai's three faces ship inside the app (assets.rs), so every
// computer draws the same letters. Code and tool output keep the platform's
// mono: DM Mono has no box drawing or arrows, and tool output is full of them.

/// Text: chat, menus, inputs and settings.
pub(crate) const BODY_FONT: &str = "Barlow";
/// Page titles and section labels, always in capitals.
pub(crate) const DISPLAY_FONT: &str = "Barlow Condensed";
/// Micro labels and data: the model over a reply, key hints, counts and ids.
pub(crate) const DATA_FONT: &str = "DM Mono";

// A family the system cannot resolve drops every glyph to Helvetica without
// warning, so each platform names fonts it actually ships. gpui turns
// ".SystemUIFont" into the platform's own UI face, but on Linux it matches
// other names literally and never expands fontconfig generics like
// "monospace", so Linux names DejaVu Sans Mono, gpui-component's own default.
// The display and text faces are the ones the window drew with before the
// brand faces, and stand in for any brand face that does not resolve.
#[cfg(target_os = "macos")]
const PLATFORM_DISPLAY: &str = ".SystemUIFont";
#[cfg(target_os = "macos")]
const PLATFORM_TEXT: &str = ".SystemUIFont";
#[cfg(target_os = "macos")]
pub(crate) const MONO_FONT: &str = "Menlo";

#[cfg(not(target_os = "macos"))]
const PLATFORM_DISPLAY: &str = "Segoe UI Variable Display";
#[cfg(not(target_os = "macos"))]
const PLATFORM_TEXT: &str = "Segoe UI Variable Text";
#[cfg(windows)]
pub(crate) const MONO_FONT: &str = "Cascadia Mono";
#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) const MONO_FONT: &str = "DejaVu Sans Mono";

// Letters the brand faces lack -- arrows, Cyrillic, CJK, emoji -- come from
// these first, then from the system's own list, which gpui appends on macOS
// and Windows and which already chooses by script and language. Windows
// names Segoe UI so those letters match the rest of the system; CJK is left
// to the system list, as one named face would give Japanese readers Chinese
// forms. On Linux gpui ignores the list and cosmic-text finds its own.
#[cfg(windows)]
const FALLBACKS: &[&str] = &["Segoe UI", "Segoe UI Emoji"];
#[cfg(not(windows))]
const FALLBACKS: &[&str] = &[];

/// Every embedded file draws on a 1000-unit em, the scale the advances below
/// are given in; most platform faces use 2048.
pub(crate) const EM: u32 = 1000;

/// A brand family, each of its files by weight, style and the advance of "a"
/// in units of its em, and the platform face that stands in for it.
pub(crate) struct Embedded {
    pub(crate) family: &'static str,
    pub(crate) files: &'static [(FontWeight, FontStyle, u16)],
    stand_in: &'static str,
}

// The advances of "a" differ between every file shipped, so the one a face
// draws tells which file it came from; the test in assets.rs reads them from
// the files.
pub(crate) const EMBEDDED: [Embedded; 3] = [
    Embedded {
        family: BODY_FONT,
        files: &[
            (FontWeight::NORMAL, FontStyle::Normal, 511),
            (FontWeight::NORMAL, FontStyle::Italic, 529),
            (FontWeight::MEDIUM, FontStyle::Normal, 517),
            (FontWeight::SEMIBOLD, FontStyle::Normal, 522),
        ],
        stand_in: PLATFORM_TEXT,
    },
    Embedded {
        family: DISPLAY_FONT,
        files: &[
            (FontWeight::EXTRA_LIGHT, FontStyle::Normal, 397),
            (FontWeight::MEDIUM, FontStyle::Normal, 425),
        ],
        stand_in: PLATFORM_DISPLAY,
    },
    Embedded {
        family: DATA_FONT,
        files: &[(FontWeight::NORMAL, FontStyle::Normal, 600)],
        stand_in: MONO_FONT,
    },
];

/// The families titles and data are drawn in, each a brand face or its
/// platform stand-in, the faces that fill in letters the brand faces lack,
/// and the brand families that did not load. Text's own family is the
/// theme's `font_family`.
struct Faces {
    display: SharedString,
    data: SharedString,
    fallbacks: FontFallbacks,
    missing: Vec<&'static str>,
}
impl Global for Faces {}

/// The brand families that did not load and are drawn in a stand-in, for
/// About to name: on Windows the window has no console to print to.
pub(crate) fn missing_fonts(cx: &App) -> &[&'static str] {
    cx.try_global::<Faces>()
        .map_or(&[], |faces| faces.missing.as_slice())
}

/// The family page titles and section labels are drawn in.
pub(crate) fn display_font(cx: &App) -> SharedString {
    cx.try_global::<Faces>()
        .map_or(DISPLAY_FONT.into(), |faces| faces.display.clone())
}

/// The family micro labels and data are drawn in.
pub(crate) fn data_font(cx: &App) -> SharedString {
    cx.try_global::<Faces>()
        .map_or(DATA_FONT.into(), |faces| faces.data.clone())
}

/// Text's font with the fallbacks for letters it lacks, for a window's root,
/// where every element below inherits the fallbacks.
pub(crate) fn text_font(cx: &App) -> Font {
    Font {
        fallbacks: cx
            .try_global::<Faces>()
            .map(|faces| faces.fallbacks.clone()),
        ..font(cx.theme().font_family.clone())
    }
}

/// The brand family when each of its files resolves to that very file, else
/// its stand-in, noted in `missing`: a family that did not load would be
/// drawn in whatever face gpui falls back to, with no word why.
fn checked(
    Embedded {
        family,
        files,
        stand_in,
    }: Embedded,
    missing: &mut Vec<&'static str>,
    cx: &App,
) -> SharedString {
    let text = cx.text_system();
    // Names and ids cannot tell whether a family loaded: DirectWrite may list
    // the static condensed files under Barlow's name, and it hands out a
    // fresh id for every request, even one it answers with a face already
    // given. A face's em and its advance of "a" tell the file it came from
    // on every platform, so a family it lacks or a weight folded onto
    // another fails alike.
    let loaded = files.iter().all(|&(weight, style, advance)| {
        let id = text.resolve_font(&Font {
            weight,
            style,
            ..font(family)
        });
        text.units_per_em(id) == EM
            && text
                .advance(id, px(EM as f32), 'a')
                .is_ok_and(|size| size.width.round() == px(f32::from(advance)))
    });
    if loaded {
        return family.into();
    }
    eprintln!("The {family} font did not load; drawing {stand_in} in its place.");
    missing.push(family);
    stand_in.into()
}

/// The site's expo-out, `cubic-bezier(.16, 1, .3, 1)`: three quarters of the
/// way in the first fifth of the time, then a long settle, so things arrive
/// rather than slide.
pub(crate) fn expo_out(t: f32) -> f32 {
    let bezier = |a: f32, b: f32, s: f32| {
        3. * a * s * (1. - s).powi(2) + 3. * b * s * s * (1. - s) + s.powi(3)
    };
    let (x1, x2) = (0.16, 0.3);
    // CSS runs time along the curve's x, so find where x reaches `t` by
    // Newton's method -- x's slope never drops below .47, so eight steps
    // land it -- and read the curve's y there.
    let mut s = t;
    for _ in 0..8 {
        let slope =
            3. * x1 * (1. - s).powi(2) + 6. * (x2 - x1) * s * (1. - s) + 3. * (1. - x2) * s * s;
        s = (s - (bezier(x1, x2, s) - t) / slope).clamp(0., 1.);
    }
    bezier(1., 1., s)
}

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
    let app: &App = cx;
    let mut missing = Vec::new();
    let [text, display, data] = EMBEDDED.map(|embedded| checked(embedded, &mut missing, app));
    let fallbacks =
        FontFallbacks::from_fonts(FALLBACKS.iter().map(|&face| face.to_owned()).collect());
    cx.set_global(Faces {
        display,
        data,
        fallbacks,
        missing,
    });
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
    t.font_family = text;
    t.mono_font_family = MONO_FONT.into();
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: the prelude re-exports gpui, whose own
    // `test` macro would shadow the built-in attribute here.
    use super::{
        BLUE, BLUE_PRESSED, DANGER, DANGER_PRESSED, DEEP, MUTED, NIGHT, ON_BLUE, RAISED, SELECTED,
        SIDEBAR_ON, SIDEBAR_TEXT, SIGNAL, TEXT, expo_out,
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

    #[test]
    fn the_expo_out_curve_starts_fast_and_ends_at_one() {
        assert!(expo_out(0.0).abs() < 1e-4);
        assert!((expo_out(1.0) - 1.0).abs() < 1e-4);
        // A fifth of the time in, the site's curve is three quarters there.
        assert!(expo_out(0.2) > 0.7, "{} at a fifth", expo_out(0.2));
        let steps: Vec<f32> = (0..=100).map(|i| expo_out(i as f32 / 100.0)).collect();
        assert!(
            steps.windows(2).all(|pair| pair[0] <= pair[1] + 1e-6),
            "it never turns back"
        );
        assert!(steps.iter().all(|&y| y <= 1.0 + 1e-4), "nor overshoots");
    }
}
