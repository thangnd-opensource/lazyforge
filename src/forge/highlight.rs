//! Syntax colors for diffs, by file extension (with the `syntax-highlight` feature).
//!
//! Colors are terminal palette indexes (0–15), not RGB, so they follow the user's terminal
//! theme, light or dark, like `bat --theme ansi`. Highlighting runs once, in the background,
//! when a diff is loaded; drawing only reads the tokens.

use super::FileDiff;

/// A piece of a line and its color: an index into the terminal's 16-color palette, or `None`
/// for the default text color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub color: Option<u8>,
    pub text: String,
}

/// Fills `tokens` on the lines of each file whose language is known. Can take a while on a
/// large diff: call it off the async threads.
pub fn highlight_files(files: &mut [FileDiff]) {
    for file in files {
        imp::highlight_file(file);
    }
}

#[cfg(not(feature = "syntax-highlight"))]
mod imp {
    pub fn highlight_file(_: &mut super::FileDiff) {}
}

#[cfg(feature = "syntax-highlight")]
mod imp {
    use std::path::Path;
    use std::str::FromStr;
    use std::sync::OnceLock;

    use syntect::highlighting::{
        Color, HighlightIterator, HighlightState, Highlighter, ScopeSelectors, StyleModifier,
        Theme, ThemeItem, ThemeSettings,
    };
    use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

    use super::Token;
    use crate::forge::FileDiff;
    use crate::forge::diff::DiffLineKind;

    /// Loaded once (a few milliseconds), then shared.
    fn syntaxes() -> &'static SyntaxSet {
        static SET: OnceLock<SyntaxSet> = OnceLock::new();
        SET.get_or_init(SyntaxSet::load_defaults_newlines)
    }

    fn theme() -> &'static Theme {
        static THEME: OnceLock<Theme> = OnceLock::new();
        THEME.get_or_init(ansi_theme)
    }

    /// Colors with `a == 0` carry a palette index in `r`; `a == 1` means "default color".
    fn palette(index: u8) -> Color {
        Color {
            r: index,
            g: 0,
            b: 0,
            a: 0,
        }
    }

    const DEFAULT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 1,
    };

    /// Which palette color each kind of token gets.
    fn ansi_theme() -> Theme {
        let rule = |scopes: &str, color: Color| ThemeItem {
            scope: ScopeSelectors::from_str(scopes).expect("valid scope selector"),
            style: StyleModifier {
                foreground: Some(color),
                background: None,
                font_style: None,
            },
        };
        Theme {
            settings: ThemeSettings {
                foreground: Some(DEFAULT),
                ..ThemeSettings::default()
            },
            scopes: vec![
                rule("comment, punctuation.definition.comment", palette(8)),
                rule(
                    "string, constant.character, punctuation.definition.string",
                    palette(2),
                ),
                rule(
                    "constant.numeric, constant.language, constant.other",
                    palette(3),
                ),
                // `fn`, `let`, `class`, `int`: the words of the language.
                rule("keyword, storage, variable.language", palette(5)),
                rule("keyword.operator, punctuation", DEFAULT),
                rule(
                    "entity.name.type, entity.name.class, support.type, support.class",
                    palette(6),
                ),
                rule(
                    "entity.name.function, support.function, meta.function-call variable.function",
                    palette(4),
                ),
                rule("entity.name.tag", palette(4)),
                rule("entity.other.attribute-name", palette(3)),
                rule("constant.character.escape", palette(6)),
                rule("markup.heading", palette(4)),
            ],
            ..Theme::default()
        }
    }

    /// The syntax for a path, by extension. Close relatives stand in for languages the
    /// default set lacks (TypeScript is highlighted as JavaScript, Vue and Svelte as HTML).
    fn syntax_for(path: &str) -> Option<&'static SyntaxReference> {
        let set = syntaxes();
        let path = Path::new(path);
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let extension = match extension {
            "ts" | "tsx" | "mts" | "cts" | "jsx" | "mjs" | "cjs" => "js",
            "vue" | "svelte" => "html",
            other => other,
        };
        set.find_syntax_by_extension(extension).or_else(|| {
            // Files known by name: `Makefile`, `Dockerfile`…
            let name = path.file_name()?.to_str()?;
            set.find_syntax_by_extension(name)
        })
    }

    /// One side of a diff (old or new), highlighted line by line. The parser state carries
    /// over between lines (multi-line strings and comments) and restarts at each hunk, since
    /// the lines between hunks are not in the diff.
    struct Side {
        syntax: &'static SyntaxReference,
        parse: ParseState,
        highlight: HighlightState,
    }

    impl Side {
        fn new(syntax: &'static SyntaxReference) -> Self {
            let highlighter = Highlighter::new(theme());
            Self {
                syntax,
                parse: ParseState::new(syntax),
                highlight: HighlightState::new(&highlighter, ScopeStack::new()),
            }
        }

        fn restart(&mut self) {
            *self = Self::new(self.syntax);
        }

        fn tokens(&mut self, text: &str) -> Option<Vec<Token>> {
            let line = format!("{text}\n");
            let ops = self.parse.parse_line(&line, syntaxes()).ok()?;
            let highlighter = Highlighter::new(theme());
            let tokens = HighlightIterator::new(&mut self.highlight, &ops, &line, &highlighter)
                .filter_map(|(style, piece)| {
                    let text = piece.trim_end_matches('\n');
                    (!text.is_empty()).then(|| Token {
                        color: (style.foreground.a == 0).then_some(style.foreground.r),
                        text: text.to_string(),
                    })
                })
                .collect();
            Some(tokens)
        }
    }

    pub fn highlight_file(file: &mut FileDiff) {
        if file.too_large || file.lines.is_empty() {
            return;
        }
        let Some(syntax) = syntax_for(&file.new_path).or_else(|| syntax_for(&file.old_path)) else {
            return;
        };
        let (mut old, mut new) = (Side::new(syntax), Side::new(syntax));
        for line in &mut file.lines {
            let tokens = match line.kind {
                DiffLineKind::HunkHeader => {
                    old.restart();
                    new.restart();
                    continue;
                }
                DiffLineKind::Meta => continue,
                DiffLineKind::Added => new.tokens(&line.text),
                DiffLineKind::Removed => old.tokens(&line.text),
                // Both sides see unchanged lines.
                DiffLineKind::Context => {
                    old.tokens(&line.text);
                    new.tokens(&line.text)
                }
            };
            match tokens {
                Some(tokens) => line.tokens = tokens,
                // The grammar choked: leave this file plain rather than half colored.
                None => {
                    for line in &mut file.lines {
                        line.tokens.clear();
                    }
                    return;
                }
            }
        }
    }
}

#[cfg(all(test, feature = "syntax-highlight"))]
mod tests {
    use super::*;
    use crate::forge::diff::parse_unified_diff;

    fn file(path: &str, diff: &str) -> FileDiff {
        FileDiff {
            old_path: path.into(),
            new_path: path.into(),
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            too_large: false,
            lines: parse_unified_diff(diff),
            added: 0,
            removed: 0,
        }
    }

    fn color_of(tokens: &[Token], text: &str) -> Option<u8> {
        tokens
            .iter()
            .find(|t| t.text.trim() == text)
            .and_then(|t| t.color)
    }

    #[test]
    fn colors_rust_by_kind_of_token() {
        let mut files = vec![file(
            "src/lib.rs",
            "@@ -1 +1,2 @@\n // a comment\n+fn greet() -> &'static str { \"hi\" }\n",
        )];
        highlight_files(&mut files);
        let lines = &files[0].lines;
        assert!(lines[0].tokens.is_empty(), "no tokens for the @@ header");
        assert_eq!(lines[1].tokens[0].color, Some(8), "comment");
        let added = &lines[2].tokens;
        assert_eq!(color_of(added, "fn"), Some(5), "keyword");
        assert_eq!(color_of(added, "greet"), Some(4), "function name");
        // Nothing is lost: the tokens spell the line.
        let text: String = added.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(text, lines[2].text);
    }

    #[test]
    fn a_comment_spanning_lines_stays_a_comment() {
        let mut files = vec![file(
            "a.js",
            "@@ -1,3 +1,3 @@\n /* start\n+   still inside\n */\n",
        )];
        highlight_files(&mut files);
        assert_eq!(files[0].lines[2].tokens[0].color, Some(8));
    }

    #[test]
    fn typescript_uses_javascript_and_unknown_files_stay_plain() {
        let mut files = vec![
            file("app.ts", "@@ -0,0 +1 @@\n+const x = 1;\n"),
            file("notes.unknownext", "@@ -0,0 +1 @@\n+const x = 1;\n"),
        ];
        highlight_files(&mut files);
        assert_eq!(color_of(&files[0].lines[1].tokens, "1"), Some(3), "number");
        assert!(files[1].lines[1].tokens.is_empty());
    }
}
