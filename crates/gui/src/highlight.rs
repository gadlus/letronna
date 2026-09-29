use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};

/// A grammar plus the fence names that should resolve to it, e.g. a Rust
/// block may be tagged ```rust or ```rs.
struct Grammar {
    names: &'static [&'static str],
    config: LanguageConfig,
}

/// Registers the eight grammars the roadmap asks for (Rust, TypeScript,
/// JavaScript, Python, JSON, TOML, YAML, Bash, Diff), instead of turning on
/// gpui-component's `tree-sitter-languages` feature, which links all 27 of
/// its bundled grammars into the binary whether a chat ever shows them or
/// not. JSON needs no entry: `tree-sitter-json` is gpui-component's one
/// unconditional grammar, already registered before this runs.
///
/// `LanguageRegistry` is a process-wide singleton with no per-window scope,
/// so this only needs to run once, at startup.
pub(crate) fn register() {
    let registry = LanguageRegistry::singleton();
    for grammar in grammars() {
        for name in grammar.names {
            registry.register(name, &grammar.config);
        }
    }
}

fn grammars() -> Vec<Grammar> {
    vec![
        Grammar {
            names: &["rust", "rs"],
            config: LanguageConfig::new(
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
                vec![],
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        },
        Grammar {
            names: &["typescript", "ts"],
            config: LanguageConfig::new(
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                vec![],
                // The TypeScript grammar's own query only names what
                // JavaScript lacks (types, `interface`); keywords, strings and
                // functions come from JavaScript's, which has to come first.
                &format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
                "",
                &format!(
                    "{}\n{}",
                    tree_sitter_javascript::LOCALS_QUERY,
                    tree_sitter_typescript::LOCALS_QUERY
                ),
            ),
        },
        Grammar {
            names: &["javascript", "js", "jsx"],
            config: LanguageConfig::new(
                "javascript",
                tree_sitter_javascript::LANGUAGE.into(),
                vec![],
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                "",
                tree_sitter_javascript::LOCALS_QUERY,
            ),
        },
        Grammar {
            names: &["python", "py"],
            config: LanguageConfig::new(
                "python",
                tree_sitter_python::LANGUAGE.into(),
                vec![],
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        },
        Grammar {
            names: &["toml"],
            config: LanguageConfig::new(
                "toml",
                tree_sitter_toml_ng::LANGUAGE.into(),
                vec![],
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        },
        Grammar {
            names: &["yaml", "yml"],
            config: LanguageConfig::new(
                "yaml",
                tree_sitter_yaml::LANGUAGE.into(),
                vec![],
                tree_sitter_yaml::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        },
        Grammar {
            names: &["bash", "sh", "shell", "zsh"],
            config: LanguageConfig::new(
                "bash",
                tree_sitter_bash::LANGUAGE.into(),
                vec![],
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
            ),
        },
        Grammar {
            names: &["diff", "patch"],
            config: LanguageConfig::new(
                "diff",
                tree_sitter_diff::LANGUAGE.into(),
                vec![],
                tree_sitter_diff::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::register;
    use gpui_component::highlighter::LanguageRegistry;

    #[test]
    fn the_registry_holds_exactly_the_chosen_languages_and_their_aliases() {
        register();
        let mut names: Vec<String> = LanguageRegistry::singleton()
            .languages()
            .iter()
            .map(|name| name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "bash",
                "diff",
                "javascript",
                "js",
                "json",
                "jsx",
                "patch",
                "py",
                "python",
                "rs",
                "rust",
                "sh",
                "shell",
                "toml",
                "ts",
                "typescript",
                "yaml",
                "yml",
                "zsh",
            ]
        );
    }

    #[test]
    fn a_fence_alias_resolves_to_its_full_grammar_name() {
        register();
        let registry = LanguageRegistry::singleton();
        let aliases = [
            ("ts", "typescript"),
            ("js", "javascript"),
            ("py", "python"),
            ("sh", "bash"),
            ("yml", "yaml"),
            ("jsx", "javascript"),
            ("shell", "bash"),
            ("zsh", "bash"),
            ("patch", "diff"),
        ];
        for (alias, canonical) in aliases {
            let config = registry
                .language(alias)
                .unwrap_or_else(|| panic!("{alias:?} did not resolve to a grammar"));
            assert_eq!(
                config.name.as_ref(),
                canonical,
                "{alias:?} resolved to {:?}, not {canonical:?}",
                config.name
            );
        }
    }

    /// Which capture names a fence of `lang` colours, so a query that fails to
    /// compile (the highlighter then silently falls back to JSON) is caught.
    fn coloured(lang: &str, code: &str) -> Vec<String> {
        use gpui_component::Rope;
        use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
        register();
        let mut highlighter = SyntaxHighlighter::new(lang);
        highlighter.update(None, &Rope::from_str(code));
        highlighter
            .styles(&(0..code.len()), &HighlightTheme::default_dark())
            .iter()
            .map(|(range, _)| code[range.clone()].to_string())
            .collect()
    }

    #[test]
    fn every_grammar_colours_its_own_keywords_and_strings() {
        let samples = [
            ("rust", "fn main() { let x = \"hi\"; }", "fn"),
            ("typescript", "const x: string = \"hi\";", "const"),
            ("typescript", "interface A { b: number }", "interface"),
            ("javascript", "function f() { return \"hi\"; }", "function"),
            ("python", "def f():\n    return \"hi\"", "def"),
            ("toml", "[a]\nb = \"hi\"", "\"hi\""),
            ("yaml", "a: \"hi\"", "\"hi\""),
            ("bash", "if true; then echo \"hi\"; fi", "if"),
            ("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-x\n+y\n", "+y"),
        ];
        for (lang, code, want) in samples {
            let parts = coloured(lang, code);
            assert!(
                parts.iter().any(|part| part == want),
                "{lang}: {want:?} was not coloured, got {parts:?}"
            );
        }
    }

    // gpui-component's own fallback, in a build with the `tree-sitter-languages`
    // feature off, is `Language::from_str` always returning `Json` and
    // `Language::name` always returning "json" (crates/ui/src/highlighter/
    // languages.rs), so `LanguageRegistry::language` never returns `None`: a
    // name nothing registered still resolves to whatever is registered under
    // "json". A code fence tagged with a language this app does not know
    // about is therefore drawn with JSON's grammar rather than left plain.
    // Fixing that needs a `Plain` entry in that no-feature build of
    // `Language` (mirroring the one the feature-gated build already has),
    // which is a change inside the gpui-kit fork; this test records the
    // current behaviour rather than the one the roadmap item asked for.
    #[test]
    fn a_language_nothing_registered_still_borrows_the_json_grammar() {
        register();
        let registry = LanguageRegistry::singleton();
        let unknown = registry
            .language("not-a-real-language")
            .expect("the no-feature build's registry always resolves a name");
        assert_eq!(unknown.name.as_ref(), "json");
    }
}
