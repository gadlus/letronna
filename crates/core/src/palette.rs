//! What the window's command palette finds for what was typed.
//!
//! The rows themselves are the window's, since only it knows its actions and
//! pages; how well a row answers the typing, which folders are recent and
//! whether the typing names a session or a folder are the same questions
//! for any front end, and live here.

use crate::store::Conversation;
use std::path::PathBuf;

/// How well a row's text answers what was typed, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// The whole text, ignoring case.
    Exact,
    /// The start of the text.
    Prefix,
    /// A whole word inside it.
    Word,
    /// Anywhere inside it.
    Substring,
}

/// How well `text` answers `query`, ignoring case, or `None` when it does not
/// contain it. An empty query answers everything, as a substring.
pub fn rank(query: &str, text: &str) -> Option<Rank> {
    let (query, text) = (query.trim().to_lowercase(), text.to_lowercase());
    if query.is_empty() {
        return Some(Rank::Substring);
    }
    if text == query {
        return Some(Rank::Exact);
    }
    if text.starts_with(&query) {
        return Some(Rank::Prefix);
    }
    let mut substring = false;
    for (at, _) in text.match_indices(&query) {
        substring = true;
        let before = text[..at].chars().next_back();
        let after = text[at + query.len()..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return Some(Rank::Word);
        }
    }
    substring.then_some(Rank::Substring)
}

/// The best `rank` of `query` among `texts`.
pub fn best<'a>(query: &str, texts: impl IntoIterator<Item = &'a str>) -> Option<Rank> {
    texts.into_iter().filter_map(|text| rank(query, text)).min()
}

/// The places in a list of ranks that answered, best first. The sort is
/// stable, so rows that rank alike keep the order they were listed in.
pub fn order(ranks: &[Option<Rank>]) -> Vec<usize> {
    let mut places: Vec<usize> = (0..ranks.len()).filter(|&i| ranks[i].is_some()).collect();
    places.sort_by_key(|&i| ranks[i]);
    places
}

/// The session id pasted in, as the sidebar's export and the logs write it.
pub fn session_id(query: &str) -> Option<u64> {
    query.trim().trim_start_matches('#').parse().ok()
}

/// The project folders sessions were attached to, most recently used first,
/// each once.
pub fn recent_folders(conversations: &[Conversation]) -> Vec<PathBuf> {
    let mut by_age: Vec<&Conversation> = conversations
        .iter()
        .filter(|c| c.workspace.is_some())
        .collect();
    by_age.sort_by_key(|c| std::cmp::Reverse(c.updated));
    let mut folders: Vec<PathBuf> = Vec::new();
    for folder in by_age.into_iter().filter_map(|c| c.workspace.clone()) {
        if !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    folders
}

/// The folder a typed or pasted path names, when there is one there. Only a
/// full path counts, or one from the home folder with `~`: a bare name would
/// be read against wherever the app happened to start.
pub fn typed_folder(query: &str) -> Option<PathBuf> {
    // Explorer's "Copy as path" puts quotes around it.
    let typed = query.trim().trim_matches('"');
    let path = match typed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            crate::tools::home()?.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(typed),
    };
    (path.is_absolute() && path.is_dir()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_ranks_exact_then_prefix_then_whole_word_then_substring() {
        assert_eq!(rank("usage", "Usage"), Some(Rank::Exact));
        assert_eq!(rank("new", "New session"), Some(Rank::Prefix));
        assert_eq!(rank("session", "New session"), Some(Rank::Word));
        assert_eq!(rank("ess", "New session"), Some(Rank::Substring));
        assert_eq!(rank("xyz", "New session"), None);
    }

    #[test]
    fn a_whole_word_is_found_after_an_earlier_part_match() {
        // "tab" first appears inside "table", then as a word of its own.
        assert_eq!(rank("tab", "a table tab"), Some(Rank::Word));
        assert_eq!(rank("gpt", "openai/gpt-5"), Some(Rank::Word));
    }

    #[test]
    fn an_empty_query_keeps_every_row_in_its_order() {
        let ranks = [rank("", "b"), rank("", "a")];
        assert_eq!(order(&ranks), vec![0, 1]);
    }

    #[test]
    fn rows_are_ordered_best_first_and_alike_ones_keep_their_order() {
        let texts = ["Open settings", "Settings", "Usage settings", "Reset"];
        let ranks: Vec<_> = texts.iter().map(|t| rank("set", t)).collect();
        // Only "Settings" starts with it; the rest have it inside a word.
        assert_eq!(order(&ranks), vec![1, 0, 2, 3]);
        let ranks: Vec<_> = texts.iter().map(|t| rank("settings", t)).collect();
        assert_eq!(order(&ranks), vec![1, 0, 2]);
    }

    #[test]
    fn the_best_of_several_texts_counts() {
        assert_eq!(best("mcp", ["Servers", "MCP"]), Some(Rank::Exact));
        assert_eq!(best("mcp", ["Servers", "Tools"]), None);
    }

    #[test]
    fn a_pasted_session_id_is_read_with_or_without_its_hash() {
        assert_eq!(session_id(" 1234 "), Some(1234));
        assert_eq!(session_id("#1234"), Some(1234));
        assert_eq!(session_id("12a"), None);
    }

    #[test]
    fn recent_folders_come_newest_first_and_once_each() {
        let session = |updated, folder: Option<&str>| Conversation {
            updated,
            workspace: folder.map(PathBuf::from),
            ..Default::default()
        };
        let sessions = [
            session(1, Some("/old")),
            session(3, Some("/new")),
            session(2, None),
            session(4, Some("/old")),
        ];
        assert_eq!(
            recent_folders(&sessions),
            vec![PathBuf::from("/old"), PathBuf::from("/new")]
        );
    }

    #[test]
    fn a_typed_folder_counts_only_when_it_is_a_full_path_to_a_folder() {
        let dir = std::env::temp_dir().join(format!("palette-folder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file.txt");
        std::fs::write(&file, "").unwrap();
        let typed = dir.display().to_string();
        assert_eq!(typed_folder(&typed), Some(dir.clone()));
        assert_eq!(typed_folder(&format!("\"{typed}\"")), Some(dir.clone()));
        assert_eq!(typed_folder(&file.display().to_string()), None);
        assert_eq!(typed_folder("crates"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
