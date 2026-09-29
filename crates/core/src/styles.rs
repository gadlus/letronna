//! Output styles: the tone replies are written in, chosen by name.
//!
//! A few are built in, and the user can write more as Markdown files in
//! `<data>/styles`, the file's name being the style's. A file may open with a
//! `# Title` line or frontmatter with a `description:`, which lists show; the
//! rest is the text the system prompt carries right after who Letronna is.
//!
//! "Default" is no style at all. The choice is saved by name in the settings,
//! so a style whose file has gone, or a name nobody knows, reads as default
//! rather than failing a request.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The most characters of a style the prompt carries: a tone takes a few
/// sentences, and every request pays for it.
pub const LIMIT: usize = 4_000;

/// The name that means no style.
pub const DEFAULT: &str = "default";

/// The styles that come with Letronna: name, line for lists, text.
const BUILT_IN: [(&str, &str, &str); 3] = [
    (
        "concise",
        "Short and direct",
        "Answer in as few words as do the job. Lead with the answer, skip preamble and recaps, and leave out anything the user did not ask for. Use a list only when the content is a list.",
    ),
    (
        "explanatory",
        "Says why, not only what",
        "Explain your reasoning as you go. When you make a choice, say briefly why, and point out the trade-offs or the parts of the code that matter. Keep each explanation short and tied to the work at hand.",
    ),
    (
        "teacher",
        "Helps the user learn",
        "Act as a patient teacher. Explain ideas in plain words, one step at a time, and check what the user already knows before going deep. When a task is a chance to learn, suggest the user try a small part themselves and offer to help.",
    ),
];

/// One style as lists show it and the prompt carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub name: String,
    /// The title or description the file gives, empty when it gives none.
    pub about: String,
    pub text: String,
    /// The file was longer than `LIMIT`, so only its start is used.
    pub cut: bool,
    pub built_in: bool,
}

/// Where the user's own style files live.
pub fn dir() -> PathBuf {
    crate::chat::data_dir().join("styles")
}

/// Every style, built-in first, then the user's by name.
pub fn all() -> Vec<Style> {
    all_in(&dir())
}

/// The style saved as `name`, or `None` for default and for a name no style
/// has.
pub fn find(name: &str) -> Option<Style> {
    find_in(&dir(), name)
}

/// The text the system prompt carries for the style in the settings, if any.
pub(crate) fn chosen() -> Option<String> {
    find(&crate::store::current_settings().style).map(|style| style.text)
}

fn all_in(dir: &Path) -> Vec<Style> {
    let mut styles: Vec<Style> = BUILT_IN
        .iter()
        .map(|(name, about, text)| Style {
            name: (*name).to_owned(),
            about: (*about).to_owned(),
            text: (*text).to_owned(),
            cut: false,
            built_in: true,
        })
        .collect();
    let mut own: Vec<Style> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| from_file(&entry.path()))
        .collect();
    own.sort_by_key(|style| style.name.to_lowercase());
    for style in own {
        // A file named like a built-in is the user rewriting it, so it wins.
        match styles
            .iter_mut()
            .find(|s| s.name.eq_ignore_ascii_case(&style.name))
        {
            Some(built_in) => *built_in = style,
            None => styles.push(style),
        }
    }
    styles
}

fn find_in(dir: &Path, name: &str) -> Option<Style> {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case(DEFAULT) {
        return None;
    }
    all_in(dir)
        .into_iter()
        .find(|style| style.name.eq_ignore_ascii_case(name))
}

/// A style from a Markdown file, or `None` for anything that is not one.
fn from_file(path: &Path) -> Option<Style> {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return None;
    }
    let name = path.file_stem()?.to_str()?.trim();
    // Default means no style, so a file by that name could never be chosen.
    if name.is_empty() || name.starts_with('.') || name.eq_ignore_ascii_case(DEFAULT) {
        return None;
    }
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    // Enough for the cap in any width with frontmatter before it, without
    // reading a huge file whole only to cut it.
    let most = (LIMIT as u64 + 1) * 4 + 4_096;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(most)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let (about, text) = parse(&text);
    if text.is_empty() {
        return None;
    }
    let cut = text.chars().count() > LIMIT;
    let text = if cut {
        text.chars()
            .take(LIMIT)
            .collect::<String>()
            .trim_end()
            .to_owned()
    } else {
        text
    };
    Some(Style {
        name: name.to_owned(),
        about,
        text,
        cut,
        built_in: false,
    })
}

/// A file's line for lists and the text that follows it.
fn parse(file: &str) -> (String, String) {
    let file = file.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if let Some(rest) = file.strip_prefix("---\n")
        && let Some((front, body)) = rest.split_once("\n---")
    {
        let about = front
            .lines()
            .find_map(|line| line.trim().strip_prefix("description:"))
            .map(|value| value.trim().trim_matches(['"', '\'']).to_owned())
            .unwrap_or_default();
        return (about, body.trim().to_owned());
    }
    let file = file.trim();
    let Some(title) = file.lines().next().and_then(|l| l.strip_prefix("# ")) else {
        return (String::new(), file.to_owned());
    };
    let body = file.split_once('\n').map_or("", |(_, body)| body);
    (title.trim().to_owned(), body.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-styles-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_built_in_styles_are_there_with_no_folder_at_all() {
        let styles = all_in(Path::new("/nowhere/letronna-styles"));
        let names: Vec<&str> = styles.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["concise", "explanatory", "teacher"]);
        assert!(styles.iter().all(|s| s.built_in && !s.text.is_empty()));
    }

    #[test]
    fn default_empty_and_unknown_names_mean_no_style() {
        let dir = folder("unknown");
        assert_eq!(find_in(&dir, ""), None);
        assert_eq!(find_in(&dir, "Default"), None);
        assert_eq!(find_in(&dir, "pirate"), None);
        assert_eq!(find_in(&dir, "Teacher").unwrap().name, "teacher");
    }

    #[test]
    fn a_file_is_a_style_named_after_it_with_its_title_shown() {
        let dir = folder("titled");
        std::fs::write(dir.join("Pirate.md"), "# Talks like a pirate\n\nSay arr.\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a style").unwrap();
        std::fs::write(dir.join("default.md"), "never chosen").unwrap();
        let style = find_in(&dir, "pirate").unwrap();
        assert_eq!(style.name, "Pirate");
        assert_eq!(style.about, "Talks like a pirate");
        assert_eq!(style.text, "Say arr.");
        assert!(!style.built_in && !style.cut);
        assert_eq!(all_in(&dir).len(), 4);
    }

    #[test]
    fn frontmatter_gives_the_description_and_stays_out_of_the_text() {
        let dir = folder("front");
        let file = "---\nname: ignored\ndescription: \"Very brief\"\n---\nOne line answers.\n";
        std::fs::write(dir.join("brief.md"), file).unwrap();
        let style = find_in(&dir, "brief").unwrap();
        assert_eq!(style.about, "Very brief");
        assert_eq!(style.text, "One line answers.");
    }

    #[test]
    fn a_long_file_is_cut_to_the_limit_and_says_so() {
        let dir = folder("long");
        std::fs::write(dir.join("long.md"), "é".repeat(LIMIT + 50)).unwrap();
        let style = find_in(&dir, "long").unwrap();
        assert!(style.cut);
        assert_eq!(style.text.chars().count(), LIMIT);
    }

    #[test]
    fn a_file_named_like_a_built_in_replaces_it() {
        let dir = folder("override");
        std::fs::write(dir.join("Concise.md"), "Three words, tops.").unwrap();
        let styles = all_in(&dir);
        assert_eq!(styles.len(), 3);
        let concise = find_in(&dir, "concise").unwrap();
        assert_eq!(concise.text, "Three words, tops.");
        assert!(!concise.built_in);
    }

    #[test]
    fn an_empty_file_is_no_style() {
        let dir = folder("empty");
        std::fs::write(dir.join("blank.md"), "# Only a title\n").unwrap();
        assert_eq!(find_in(&dir, "blank"), None);
    }
}
