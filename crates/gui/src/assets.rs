use crate::prelude::*;

/// letronna.ai's faces, unmodified, under the SIL Open Font License
/// (assets/fonts/OFL.txt): Barlow for text, Barlow Condensed for titles and
/// labels, DM Mono for micro labels and data. About 0.68 MB in all.
pub(crate) const FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Barlow-Regular.ttf"),
    include_bytes!("../assets/fonts/Barlow-Italic.ttf"),
    include_bytes!("../assets/fonts/Barlow-Medium.ttf"),
    include_bytes!("../assets/fonts/Barlow-SemiBold.ttf"),
    include_bytes!("../assets/fonts/BarlowCondensed-ExtraLight.ttf"),
    include_bytes!("../assets/fonts/BarlowCondensed-Medium.ttf"),
    include_bytes!("../assets/fonts/DMMono-Regular.ttf"),
];

pub(crate) struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        macro_rules! svg { (($($n:literal),*), ($($b:literal),*)) => { match path {
            $(concat!("icons/", $n, ".svg") => Some(Cow::Borrowed(include_bytes!(concat!("../assets/icons/", $n, ".svg")))),)*
            $(concat!("icons/brands/", $b, ".svg") => Some(Cow::Borrowed(include_bytes!(concat!("../assets/icons/brands/", $b, ".svg")))),)*
            "images/letronna.png" => Some(Cow::Borrowed(&include_bytes!("../assets/letronna-front.png")[..])),
            _ => None,
        } } }
        Ok(svg!(
            (
                "plus",
                "close",
                "window-close",
                "window-minimize",
                "window-maximize",
                "window-restore",
                "minus",
                "settings",
                "arrow-up",
                "arrow-down",
                "copy",
                "redo",
                "undo",
                "git-branch",
                "panel-left-close",
                "panel-left-open",
                "chevron-down",
                "chevron-right",
                "check",
                "loader-circle",
                "loader",
                "bot",
                "mic",
                "chevrons-up-down",
                "inbox",
                "send",
                "sparkles",
                "message-square",
                "file",
                "file-text",
                "clock",
                "search",
                "pin",
                "folder",
                "image",
                "play",
                "stop",
                "square-pen",
                "info",
                "pin",
                "trash",
                "export",
                "pencil",
                "circle-x",
                "circle-check",
                "triangle-alert",
                "shield",
                "puzzle",
                "circle-user",
                "plugs",
                "palette",
                "globe",
                "arrow-left",
                "arrow-right",
                "rotate-cw",
                "pop-out",
                "pop-in",
                "view",
                "list-checks",
                "circle",
                "list-filter",
                "envelope",
                "eye",
                "brand-star",
                "wordmark"
            ),
            ("telegram", "discord", "slack", "whatsapp")
        ))
    }
    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum AppIcon {
    Mic,
    Sparkles,
    MessageSquare,
    File,
    FileText,
    Clock,
    Search,
    Folder,
    Image,
    SquarePen,
    Info,
    Pin,
    Trash,
    Export,
    Pencil,
    Shield,
    Puzzle,
    Robot,
    Plugs,
    Palette,
    Checklist,
    Circle,
    User,
    Play,
    Stop,
    Globe,
    Reload,
    PopOut,
    PopIn,
    View,
    Branch,
}
impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        match self {
            AppIcon::Mic => "icons/mic.svg",
            AppIcon::Sparkles => "icons/sparkles.svg",
            AppIcon::MessageSquare => "icons/message-square.svg",
            AppIcon::File => "icons/file.svg",
            AppIcon::FileText => "icons/file-text.svg",
            AppIcon::Clock => "icons/clock.svg",
            AppIcon::Search => "icons/search.svg",
            AppIcon::Folder => "icons/folder.svg",
            AppIcon::Image => "icons/image.svg",
            AppIcon::SquarePen => "icons/square-pen.svg",
            AppIcon::Info => "icons/info.svg",
            AppIcon::Pin => "icons/pin.svg",
            AppIcon::Trash => "icons/trash.svg",
            AppIcon::Export => "icons/export.svg",
            AppIcon::Pencil => "icons/pencil.svg",
            AppIcon::Shield => "icons/shield.svg",
            AppIcon::Puzzle => "icons/puzzle.svg",
            AppIcon::Robot => "icons/bot.svg",
            AppIcon::User => "icons/circle-user.svg",
            AppIcon::Plugs => "icons/plugs.svg",
            AppIcon::Palette => "icons/palette.svg",
            AppIcon::Checklist => "icons/list-checks.svg",
            AppIcon::Circle => "icons/circle.svg",
            AppIcon::Play => "icons/play.svg",
            AppIcon::Stop => "icons/stop.svg",
            AppIcon::Globe => "icons/globe.svg",
            AppIcon::Reload => "icons/rotate-cw.svg",
            AppIcon::PopOut => "icons/pop-out.svg",
            AppIcon::PopIn => "icons/pop-in.svg",
            AppIcon::View => "icons/view.svg",
            AppIcon::Branch => "icons/git-branch.svg",
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::FONTS;
    use crate::theme::{EM, EMBEDDED};

    /// A big-endian number `bytes` long at `at`, as font files store them.
    fn number(data: &[u8], at: usize, bytes: usize) -> Option<usize> {
        let field = data.get(at..at + bytes)?;
        Some(field.iter().fold(0, |n, &b| (n << 8) | usize::from(b)))
    }

    /// The table tagged `tag`, found through the file's table directory.
    fn table<'a>(font: &'a [u8], tag: &[u8]) -> Option<&'a [u8]> {
        (0..number(font, 4, 2)?).find_map(|i| {
            let record = 12 + 16 * i;
            if font.get(record..record + 4)? != tag {
                return None;
            }
            let start = number(font, record + 8, 4)?;
            font.get(start..start + number(font, record + 12, 4)?)
        })
    }

    /// The typographic family a font is looked up by, in Windows' English
    /// names: name 16 where it has one, else name 1.
    fn family(font: &[u8]) -> Option<String> {
        let names = table(font, b"name")?;
        let strings = number(names, 4, 2)?;
        let named = |id: usize| {
            (0..number(names, 2, 2)?).find_map(|i| {
                let record = 6 + 12 * i;
                let key: Option<Vec<usize>> =
                    (0..4).map(|f| number(names, record + 2 * f, 2)).collect();
                // Platform 3 (Windows), encoding 1 (UTF-16), US English.
                if key? != [3, 1, 0x409, id] {
                    return None;
                }
                let at = strings + number(names, record + 10, 2)?;
                let units: Option<Vec<u16>> = (0..number(names, record + 8, 2)? / 2)
                    .map(|u| number(names, at + 2 * u, 2).map(|n| n as u16))
                    .collect();
                String::from_utf16(&units?).ok()
            })
        };
        named(16).or_else(|| named(1))
    }

    /// The advance of `ch` in the font's own units, found through its Unicode
    /// map (cmap format 4) and its horizontal metrics.
    fn advance(font: &[u8], ch: char) -> Option<usize> {
        let cmap = table(font, b"cmap")?;
        let map = (0..number(cmap, 2, 2)?).find_map(|i| {
            let record = 4 + 8 * i;
            // Platform 3 (Windows), encoding 1 (Unicode's first plane).
            if number(cmap, record, 4)? != 0x0003_0001 {
                return None;
            }
            cmap.get(number(cmap, record + 4, 4)?..)
        })?;
        assert_eq!(number(map, 0, 2), Some(4), "cmap format");
        let (code, segments) = (ch as usize, number(map, 6, 2)? / 2);
        let segment =
            (0..segments).find(|&s| number(map, 14 + 2 * s, 2).is_some_and(|end| end >= code))?;
        // After the end codes and a pad: start codes, deltas, range offsets.
        let row = |r: usize| 16 + 2 * (r * segments + segment);
        let start = number(map, row(1), 2)?;
        let (delta, range) = (number(map, row(2), 2)?, number(map, row(3), 2)?);
        if code < start {
            return None;
        }
        let glyph = match range {
            0 => code,
            _ => number(map, row(3) + range + 2 * (code - start), 2)?,
        };
        let glyph = (glyph + delta) & 0xffff;
        // Glyphs past the last metric share its advance.
        let metrics = number(table(font, b"hhea")?, 34, 2)?;
        number(table(font, b"hmtx")?, 4 * glyph.min(metrics - 1), 2)
    }

    #[test]
    fn every_embedded_font_is_a_font_file() {
        for font in FONTS {
            // TrueType outlines, as every static file in google/fonts has.
            assert_eq!(number(font, 0, 4), Some(0x0001_0000));
            for tag in [
                "cmap", "glyf", "head", "hhea", "hmtx", "loca", "maxp", "name", "OS/2",
            ] {
                assert!(table(font, tag.as_bytes()).is_some(), "no {tag} table");
            }
            let head = table(font, b"head").unwrap_or_default();
            assert_eq!(
                number(head, 12, 4),
                Some(0x5f0f_3cf5),
                "head's magic number"
            );
            // The startup check knows each file by its em and its "a".
            assert_eq!(number(head, 18, 2), Some(EM as usize), "the em");
            let (family, a) = (family(font).unwrap_or_default(), advance(font, 'a'));
            assert!(
                EMBEDDED.iter().any(|embedded| embedded.family == family
                    && embedded
                        .files
                        .iter()
                        .any(|&(_, _, known)| Some(usize::from(known)) == a)),
                "{family:?} with an \"a\" of {a:?} is not a face theme.rs checks"
            );
        }
        for embedded in &EMBEDDED {
            for &(_, _, wanted) in embedded.files {
                assert!(
                    FONTS.iter().any(|&font| {
                        family(font).as_deref() == Some(embedded.family)
                            && advance(font, 'a') == Some(usize::from(wanted))
                    }),
                    "no {} file with an \"a\" of {wanted}",
                    embedded.family
                );
            }
        }
    }
}
