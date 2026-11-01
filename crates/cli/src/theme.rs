//! The colours the terminal draws with, kept in one table so `/theme` can
//! change them all at once.
//!
//! Dark is the 256 colours Letronna has always used, and stays the default,
//! so nothing looks different until a theme is picked. Light suits a light
//! background. The colour-blind pair never tells a removed line from an added
//! one, or a failure from a success, by red and green alone: orange and blue
//! do it instead. ANSI uses the terminal's own sixteen colours, so the
//! terminal's palette decides, and its diffs mark lines with ink alone,
//! since a background from someone else's palette can hide the text on it.
//!
//! Auto asks the terminal what its background is (OSC 11), once, as the
//! terminal starts and before anything else reads the keys: asked later, the
//! answer would arrive as typing. Without an answer it goes by `COLORFGBG`,
//! and failing that it is dark.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(unix)]
use std::time::Duration;

/// A diff row's colours: a background, or none for ink alone, and the ink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    pub back: Option<&'static str>,
    pub ink: &'static str,
}

/// One theme, each colour given as the parameters of an SGR escape.
#[derive(Debug, PartialEq)]
pub struct Theme {
    /// What it is called in the settings and after `/theme`.
    pub name: &'static str,
    pub about: &'static str,
    pub blue: &'static str,
    pub red: &'static str,
    pub green: &'static str,
    pub amber: &'static str,
    /// `Inline code` in a reply.
    pub code: &'static str,
    pub gone: Paint,
    pub new: Paint,
    pub keyword: &'static str,
    /// Quoted text in code.
    pub text: &'static str,
    /// Comments in code.
    pub note: &'static str,
    pub number: &'static str,
}

pub const DARK: Theme = Theme {
    name: "dark",
    about: "for a dark background, as Letronna has always looked",
    blue: "38;5;69",
    red: "38;5;167",
    green: "38;5;114",
    amber: "38;5;179",
    code: "38;5;180",
    gone: Paint {
        back: Some("48;5;52"),
        ink: "38;5;210",
    },
    new: Paint {
        back: Some("48;5;22"),
        ink: "38;5;157",
    },
    keyword: "38;5;176",
    text: "38;5;144",
    note: "38;5;102",
    number: "38;5;180",
};

const LIGHT: Theme = Theme {
    name: "light",
    about: "for a light background",
    blue: "38;5;26",
    red: "38;5;160",
    green: "38;5;28",
    amber: "38;5;130",
    code: "38;5;94",
    gone: Paint {
        back: Some("48;5;224"),
        ink: "38;5;88",
    },
    new: Paint {
        back: Some("48;5;194"),
        ink: "38;5;22",
    },
    keyword: "38;5;90",
    text: "38;5;64",
    note: "38;5;244",
    number: "38;5;130",
};

const DARK_COLOUR_BLIND: Theme = Theme {
    name: "dark-colour-blind",
    about: "a dark background, orange and blue where others use red and green",
    blue: "38;5;147",
    red: "38;5;208",
    green: "38;5;75",
    amber: "38;5;222",
    code: "38;5;180",
    gone: Paint {
        back: Some("48;5;94"),
        ink: "38;5;223",
    },
    new: Paint {
        back: Some("48;5;24"),
        ink: "38;5;153",
    },
    keyword: "38;5;176",
    text: "38;5;144",
    note: "38;5;102",
    number: "38;5;180",
};

const LIGHT_COLOUR_BLIND: Theme = Theme {
    name: "light-colour-blind",
    about: "a light background, orange and blue where others use red and green",
    blue: "38;5;61",
    red: "38;5;166",
    green: "38;5;25",
    amber: "38;5;136",
    code: "38;5;94",
    gone: Paint {
        back: Some("48;5;223"),
        ink: "38;5;94",
    },
    new: Paint {
        back: Some("48;5;153"),
        ink: "38;5;18",
    },
    keyword: "38;5;90",
    text: "38;5;64",
    note: "38;5;244",
    number: "38;5;130",
};

const ANSI: Theme = Theme {
    name: "ansi",
    about: "the terminal's own sixteen colours",
    blue: "34",
    red: "31",
    green: "32",
    amber: "33",
    code: "36",
    gone: Paint {
        back: None,
        ink: "31",
    },
    new: Paint {
        back: None,
        ink: "32",
    },
    keyword: "35",
    text: "32",
    note: "90",
    number: "33",
};

/// Every theme, dark first: an index into this is what is drawn with.
pub const THEMES: [&Theme; 5] = [
    &DARK,
    &LIGHT,
    &DARK_COLOUR_BLIND,
    &LIGHT_COLOUR_BLIND,
    &ANSI,
];

/// The setting that picks light or dark by the terminal's background.
pub const AUTO: &str = "auto";

/// How long the terminal is given to say what its background is. Its answer
/// comes in a few milliseconds and ends the wait; this bounds one that never
/// answers, with room for one over a slow ssh link, whose late answer would
/// otherwise land in the box as typing.
#[cfg(unix)]
const WAIT: Duration = Duration::from_secs(1);

/// Which of `THEMES` is drawn with now.
static NOW: AtomicUsize = AtomicUsize::new(0);

/// Whether the terminal said its background is light, when it was asked at
/// start.
static TOLD: OnceLock<Option<bool>> = OnceLock::new();

/// The theme drawn with now.
pub fn now() -> &'static Theme {
    THEMES[NOW.load(Ordering::Relaxed).min(THEMES.len() - 1)]
}

/// Set up the theme the setting names as the terminal starts, asking the
/// terminal its background first for auto when there is a keyboard to ask.
pub fn start(setting: &str, ask: bool) {
    if ask && is_auto(setting) {
        let _ = TOLD.set(asked());
    }
    choose(setting);
}

/// Draw with the theme `setting` names from now on.
pub fn choose(setting: &str) {
    let name = resolve(setting, light_background());
    let at = THEMES.iter().position(|theme| theme.name == name);
    NOW.store(at.unwrap_or(0), Ordering::Relaxed);
}

/// A name as typed, the way the settings keep it: lower case, "color" as
/// "colour", and the default dark one as nothing. None for no theme.
pub fn setting_for(typed: &str) -> Option<String> {
    let name = typed.trim().to_lowercase().replace("color", "colour");
    match name.as_str() {
        "" | "dark" => Some(String::new()),
        // The colour-blind one alone is the dark one, as dark is the default.
        "colour-blind" => Some(DARK_COLOUR_BLIND.name.to_owned()),
        AUTO => Some(AUTO.to_owned()),
        name => THEMES
            .iter()
            .find(|theme| theme.name == name)
            .map(|theme| theme.name.to_owned()),
    }
}

/// Whether the terminal was asked its background as it started.
pub fn asked_at_start() -> bool {
    TOLD.get().is_some()
}

/// Whether `setting` is auto.
pub fn is_auto(setting: &str) -> bool {
    setting.trim().eq_ignore_ascii_case(AUTO)
}

/// What auto draws with here: light, dark, or none when nothing has said.
pub fn light_background() -> Option<bool> {
    TOLD.get()
        .copied()
        .flatten()
        .or_else(|| light_from_colorfgbg(&std::env::var("COLORFGBG").ok()?))
}

/// The name of the theme a setting draws with, where `light` is what is
/// known of the background: anything not a theme is the dark default.
fn resolve(setting: &str, light: Option<bool>) -> &'static str {
    let setting = setting_for(setting).unwrap_or_default();
    match setting.as_str() {
        AUTO if light == Some(true) => LIGHT.name,
        "" | AUTO => DARK.name,
        name => THEMES
            .iter()
            .find(|theme| theme.name == name)
            .map_or(DARK.name, |theme| theme.name),
    }
}

/// Whether `COLORFGBG`, "foreground;background" as rxvt and others set it,
/// names a light background: 7 and 9 to 15 are light, the rest dark.
fn light_from_colorfgbg(value: &str) -> Option<bool> {
    let back: u8 = value.rsplit(';').next()?.trim().parse().ok()?;
    Some(back == 7 || (9..=15).contains(&back))
}

/// Ask the terminal its background. A device-attributes request goes behind
/// the question, which every terminal answers, so one that ignores the
/// question is not waited on past that answer. Each byte is read alone, so
/// nothing typed after the answers is taken.
#[cfg(unix)]
fn asked() -> Option<bool> {
    use std::io::{IsTerminal, Write};
    use std::os::fd::AsRawFd;
    let stdin = std::io::stdin();
    if !stdin.is_terminal() || !std::io::stdout().is_terminal() || !crate::render::ansi() {
        return None;
    }
    let raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    if !raw {
        crossterm::terminal::enable_raw_mode().ok()?;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b]11;?\x1b\\\x1b[c");
    let _ = out.flush();
    let fd = stdin.as_raw_fd();
    let until = std::time::Instant::now() + WAIT;
    let mut said = Vec::new();
    while !answered(&said) {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, waited on for at most the time left.
        if unsafe { libc::poll(&mut ready, 1, left.as_millis() as libc::c_int) } <= 0 {
            break;
        }
        let mut byte = 0u8;
        // SAFETY: reads at most one byte into `byte`, which outlives the call.
        if unsafe { libc::read(fd, (&raw mut byte).cast(), 1) } != 1 {
            break;
        }
        said.push(byte);
    }
    if !raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    light_from_answer(&said)
}

#[cfg(not(unix))]
fn asked() -> Option<bool> {
    None
}

/// Whether the device-attributes answer, `ESC [ ? ... c`, has come.
#[cfg(any(unix, test))]
fn answered(said: &[u8]) -> bool {
    said.windows(3)
        .position(|three| three == b"\x1b[?")
        .is_some_and(|at| said[at..].contains(&b'c'))
}

/// Whether the terminal's answer to OSC 11, `ESC ] 11 ; rgb:R/G/B`, names a
/// light background, each part being one to four hex digits.
#[cfg(any(unix, test))]
fn light_from_answer(said: &[u8]) -> Option<bool> {
    let said = String::from_utf8_lossy(said);
    let (_, rgb) = said.split_once("]11;rgb:")?;
    let rgb = rgb.split(['\x07', '\x1b']).next()?;
    let parts: Vec<f64> = rgb
        .split('/')
        .map(|part| {
            let value = u32::from_str_radix(part, 16).ok()?;
            let most = 16u32.checked_pow(u32::try_from(part.len()).ok()?)? - 1;
            (1..=4)
                .contains(&part.len())
                .then(|| f64::from(value) / f64::from(most))
        })
        .collect::<Option<_>>()?;
    let [red, green, blue] = parts[..] else {
        return None;
    };
    Some(0.2126 * red + 0.7152 * green + 0.0722 * blue > 0.5)
}

/// A few rows drawn in `theme` whatever is drawn with now, so `/theme` can
/// show each before one is picked.
pub fn sample(theme: &Theme) -> Vec<String> {
    let ink = |code: &str, text: &str| format!("\x1b[{code}m{text}\x1b[0m");
    let row = |paint: Paint, text: &str| match paint.back {
        Some(back) => format!("\x1b[{back}m\x1b[{}m{text}\x1b[0m", paint.ink),
        None => ink(paint.ink, text),
    };
    vec![
        format!(
            "{} {}  {}  {}  {}",
            ink(theme.blue, "●"),
            ink(theme.green, "✓ passed"),
            ink(theme.red, "✗ failed"),
            ink(theme.amber, "src/main.rs"),
            ink(theme.code, "cargo test"),
        ),
        format!(
            "{}  {}",
            row(theme.gone, "- let total = 0; "),
            row(theme.new, "+ let total = sum(); "),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_theme_draws_with_the_colours_the_terminal_always_used() {
        // The values the renderers had written into them before themes.
        let before = Theme {
            name: "dark",
            about: DARK.about,
            blue: "38;5;69",
            red: "38;5;167",
            green: "38;5;114",
            amber: "38;5;179",
            code: "38;5;180",
            gone: Paint {
                back: Some("48;5;52"),
                ink: "38;5;210",
            },
            new: Paint {
                back: Some("48;5;22"),
                ink: "38;5;157",
            },
            keyword: "38;5;176",
            text: "38;5;144",
            note: "38;5;102",
            number: "38;5;180",
        };
        assert_eq!(DARK, before);
        assert_eq!(resolve("", None), "dark");
        assert_eq!(resolve("no such theme", Some(true)), "dark");
        assert_eq!(THEMES[0], &DARK);
    }

    #[test]
    fn auto_follows_the_background_and_is_dark_when_nothing_says() {
        assert_eq!(resolve("auto", Some(true)), "light");
        assert_eq!(resolve("Auto", Some(false)), "dark");
        assert_eq!(resolve("auto", None), "dark");
        assert_eq!(resolve("light", Some(false)), "light");
    }

    #[test]
    fn a_theme_is_named_however_it_is_spelt() {
        assert_eq!(setting_for("Dark").as_deref(), Some(""));
        assert_eq!(setting_for("light").as_deref(), Some("light"));
        assert_eq!(
            setting_for("light-color-blind").as_deref(),
            Some("light-colour-blind")
        );
        assert_eq!(
            setting_for("colour-blind").as_deref(),
            Some("dark-colour-blind")
        );
        assert_eq!(setting_for("ANSI").as_deref(), Some("ansi"));
        assert_eq!(setting_for("neon"), None);
    }

    #[test]
    fn the_terminal_s_answer_says_whether_its_background_is_light() {
        let white = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c";
        assert_eq!(light_from_answer(white), Some(true));
        let dark = b"\x1b]11;rgb:1e1e/1f1f/2828\x07\x1b[?1;2c";
        assert_eq!(light_from_answer(dark), Some(false));
        assert_eq!(light_from_answer(b"\x1b]11;rgb:f0/f0/e8\x07"), Some(true));
        // Only the device attributes: the question went unanswered.
        assert_eq!(light_from_answer(b"\x1b[?62;c"), None);
        assert!(answered(b"\x1b]11;rgb:0/0/0\x07\x1b[?62;22c"));
        assert!(!answered(b"\x1b]11;rgb:0/0/0\x07\x1b[?62;22"));
    }

    #[test]
    fn colorfgbg_names_the_background_by_its_last_number() {
        assert_eq!(light_from_colorfgbg("15;0"), Some(false));
        assert_eq!(light_from_colorfgbg("0;15"), Some(true));
        assert_eq!(light_from_colorfgbg("12;default;7"), Some(true));
        assert_eq!(light_from_colorfgbg("7;8"), Some(false));
        assert_eq!(light_from_colorfgbg("default"), None);
    }

    #[test]
    fn diffs_in_ansi_are_marked_by_ink_alone() {
        assert_eq!(ANSI.gone.back, None);
        assert_eq!(ANSI.new.back, None);
        for theme in THEMES {
            assert_eq!(sample(theme).len(), 2, "{}", theme.name);
        }
    }
}
