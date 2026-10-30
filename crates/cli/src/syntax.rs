//! Just enough colour to read code by.
//!
//! Not a parser and not trying to be. A diff is read in a glance to answer
//! one question -- is this the change I meant -- and for that, telling a
//! string from a comment from a keyword is most of the value. Anything that
//! needed a grammar per language would cost more than the glance is worth,
//! and would be wrong on the next language anyway.
//!
//! The rules are the ones nearly every language agrees on: a run of quotes is
//! a string, a comment runs to the end of the line, digits are numbers, and a
//! short list of words are keywords in most things that have keywords.

use crate::render;

/// Words treated as keywords. Drawn from what several languages share rather
/// than any one of them, which is why `fn`, `def` and `function` all appear.
const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "mut",
    "const",
    "static",
    "pub",
    "use",
    "mod",
    "impl",
    "struct",
    "enum",
    "trait",
    "match",
    "if",
    "else",
    "for",
    "while",
    "loop",
    "return",
    "break",
    "continue",
    "as",
    "in",
    "where",
    "async",
    "await",
    "move",
    "ref",
    "dyn",
    "self",
    "Self",
    "def",
    "class",
    "import",
    "from",
    "function",
    "var",
    "new",
    "null",
    "None",
    "True",
    "False",
    "true",
    "false",
    "and",
    "or",
    "not",
    "is",
    "try",
    "catch",
    "except",
    "finally",
    "raise",
    "throw",
    "with",
    "yield",
    "lambda",
    "elif",
    "end",
    "do",
    "then",
    "type",
    "interface",
    "export",
    "default",
    "public",
    "private",
    "void",
    "int",
    "string",
    "bool",
    "float",
    "double",
    "char",
    "unsigned",
];

/// What a run of characters is, as far as this cares.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Plain,
    Keyword,
    Text,
    Note,
    Number,
}

impl Kind {
    /// The colour, or none for ordinary code, which keeps whatever colour the
    /// line already had -- a diff paints its own background and the text has
    /// to stay legible on it.
    fn ink(self) -> Option<&'static str> {
        let theme = crate::theme::now();
        match self {
            Kind::Plain => None,
            Kind::Keyword => Some(theme.keyword),
            Kind::Text => Some(theme.text),
            Kind::Note => Some(theme.note),
            Kind::Number => Some(theme.number),
        }
    }
}

/// Colour a line of code, leaving its length alone: every character in goes
/// out, so a caller that has already fitted the line to a width still has it
/// fitted afterwards.
pub fn paint(line: &str) -> String {
    if !render::colour() {
        return line.to_owned();
    }
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    let mut open: Option<Kind> = None;

    while at < chars.len() {
        let (kind, len) = read(&chars, at);
        if open != Some(kind) {
            if open.is_some_and(|k| k.ink().is_some()) {
                out.push_str("\x1b[39m");
            }
            if let Some(ink) = kind.ink() {
                out.push_str(&format!("\x1b[{ink}m"));
            }
            open = Some(kind);
        }
        out.extend(&chars[at..at + len]);
        at += len;
    }
    if open.is_some_and(|k| k.ink().is_some()) {
        out.push_str("\x1b[39m");
    }
    out
}

/// What starts at `at`, and how long it runs for.
fn read(chars: &[char], at: usize) -> (Kind, usize) {
    let ch = chars[at];

    // A comment takes the rest of the line, whichever way it is opened.
    let rest_is_comment = matches!(ch, '#')
        || (ch == '/' && chars.get(at + 1) == Some(&'/'))
        || (ch == '-' && chars.get(at + 1) == Some(&'-'))
        || ch == ';';
    if rest_is_comment {
        return (Kind::Note, chars.len() - at);
    }

    // A string runs to its closing quote, or to the end of the line if it
    // never comes -- which happens all the time in a diff, where a line can
    // be cut off mid-string.
    if ch == '"' || ch == '\'' || ch == '`' {
        let mut end = at + 1;
        while end < chars.len() {
            // A quote with a backslash in front of it is part of the string.
            if chars[end] == ch && chars.get(end - 1) != Some(&'\\') {
                return (Kind::Text, end - at + 1);
            }
            end += 1;
        }
        return (Kind::Text, chars.len() - at);
    }

    if ch.is_ascii_digit() {
        let len = chars[at..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == '.' || **c == '_')
            .count();
        return (Kind::Number, len);
    }

    if ch.is_alphabetic() || ch == '_' {
        let len = chars[at..]
            .iter()
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .count();
        let word: String = chars[at..at + len].iter().collect();
        let kind = if KEYWORDS.contains(&word.as_str()) {
            Kind::Keyword
        } else {
            Kind::Plain
        };
        return (kind, len);
    }

    (Kind::Plain, 1)
}

#[cfg(test)]
mod tests {
    use super::{Kind, paint, read};

    fn kind(text: &str) -> Kind {
        let chars: Vec<char> = text.chars().collect();
        read(&chars, 0).0
    }

    #[test]
    fn it_tells_the_few_things_apart_that_matter() {
        assert!(kind("fn main") == Kind::Keyword);
        assert!(kind("name") == Kind::Plain);
        assert!(kind("\"hello\"") == Kind::Text);
        assert!(kind("// a note") == Kind::Note);
        assert!(kind("# a note") == Kind::Note);
        assert!(kind("42") == Kind::Number);
    }

    #[test]
    fn a_string_cut_off_by_the_edge_is_still_a_string() {
        let chars: Vec<char> = "\"unterminated".chars().collect();
        let (kind, len) = read(&chars, 0);
        assert_eq!(kind, Kind::Text);
        assert_eq!(len, chars.len());
    }

    #[test]
    fn an_escaped_quote_does_not_end_one() {
        let chars: Vec<char> = "\"a\\\"b\" tail".chars().collect();
        let (_, len) = read(&chars, 0);
        // The string is the six characters up to and including the real quote.
        assert_eq!(len, 6);
    }

    #[test]
    fn nothing_is_added_or_lost() {
        // Colour is off in a test, so this is the plain path: same text back.
        let line = "let x = \"hi\";  // note";
        assert_eq!(paint(line), line);
    }
}
