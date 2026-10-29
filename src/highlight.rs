//! Gruvbox-dark syntax highlighting for the bounded file preview.
//!
//! Grammar state and token colors are computed on selection changes, never
//! in ratatui's draw loop. All colors come from this small, local theme.
//! syntect uses the pure-Rust fancy-regex backend (no Oniguruma/C dependency).
use std::{path::Path, str::FromStr, sync::OnceLock};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{
        Color as SynColor, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem,
        ThemeSettings,
    },
    parsing::SyntaxSet,
};

const TEXT: Color = Color::Rgb(235, 219, 178);
const COMMENT: (u8, u8, u8) = (146, 131, 116);
const RED: (u8, u8, u8) = (251, 73, 52);
const ORANGE: (u8, u8, u8) = (254, 128, 25);
const YELLOW: (u8, u8, u8) = (250, 189, 47);
const GREEN: (u8, u8, u8) = (184, 187, 38);
const AQUA: (u8, u8, u8) = (142, 192, 124);
const BLUE: (u8, u8, u8) = (131, 165, 152);
const PURPLE: (u8, u8, u8) = (211, 134, 155);

fn syn_color((r, g, b): (u8, u8, u8)) -> SynColor {
    SynColor { r, g, b, a: 255 }
}

fn rule(selector: &str, color: (u8, u8, u8), font: Option<FontStyle>) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(selector).expect("valid built-in scope"),
        style: StyleModifier {
            foreground: Some(syn_color(color)),
            background: None,
            font_style: font,
        },
    }
}

fn gruvbox_theme() -> Theme {
    Theme {
        name: Some("rzpk Gruvbox Dark".into()),
        author: Some("rzpk".into()),
        settings: ThemeSettings {
            foreground: Some(syn_color((235, 219, 178))),
            background: Some(syn_color((40, 40, 40))),
            ..ThemeSettings::default()
        },
        scopes: vec![
            rule("comment", COMMENT, Some(FontStyle::ITALIC)),
            rule("string", GREEN, None),
            rule("constant.numeric", PURPLE, None),
            rule("constant.language", PURPLE, None),
            rule("constant.character.escape", ORANGE, None),
            rule("keyword", RED, None),
            rule("storage", RED, None),
            rule("storage.type", YELLOW, None),
            rule("entity.name.type", YELLOW, None),
            rule("entity.name.class", YELLOW, None),
            rule("entity.name.function", AQUA, None),
            rule("support.function", AQUA, None),
            rule("support.type", YELLOW, None),
            rule("variable.parameter", BLUE, None),
            rule("variable.language", PURPLE, None),
            rule("keyword.operator", ORANGE, None),
            rule("entity.name.tag", BLUE, None),
            rule("entity.other.attribute-name", YELLOW, None),
            rule("markup.heading", YELLOW, Some(FontStyle::BOLD)),
            rule("markup.bold", (235, 219, 178), Some(FontStyle::BOLD)),
            rule("markup.italic", BLUE, Some(FontStyle::ITALIC)),
            rule("markup.raw", GREEN, None),
            rule("markup.quote", COMMENT, None),
        ],
    }
}

// Regular text does not need syntect at all, and a large binary syntax set
// must only be loaded once even when the user moves the cursor rapidly.
static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(gruvbox_theme)
}

fn grammar_extension(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    match name {
        "Dockerfile" | "Containerfile" => Some("sh"),
        "Makefile" | "GNUmakefile" => Some("sh"),
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => Some("sh"),
        "Cargo.lock" => Some("toml"),
        ".gitconfig" => Some("ini"),
        _ => path.extension()?.to_str(),
    }
}

fn token_style(token: syntect::highlighting::Style) -> Style {
    let mut style = Style::default().fg(Color::Rgb(
        token.foreground.r,
        token.foreground.g,
        token.foreground.b,
    ));
    if token.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if token.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if token.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

/// Compact fallback for configuration languages not included in syntect's
/// bundled Sublime grammars (in particular TOML, Nix and Terraform/HCL).
/// This deliberately tokenizes only simple keys, strings, numbers and comments;
/// it never tries to parse/validate the configuration file.
fn config_language(path: &Path) -> Option<&'static str> {
    match grammar_extension(path)? {
        "toml" => Some("TOML"),
        "nix" => Some("Nix"),
        "tf" | "tfvars" | "hcl" => Some("HCL"),
        "env" => Some("Env"),
        _ => None,
    }
}

fn config_comment_at(line: &str, nix: bool) -> Option<usize> {
    let mut quote = None;
    let mut escape = false;
    let mut chars = line.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if escape {
            escape = false;
            continue;
        }
        if quote.is_some() && character == '\\' {
            escape = true;
            continue;
        }
        if quote == Some(character) {
            quote = None;
        } else if quote.is_none() {
            if character == '"' || character == '\'' {
                quote = Some(character);
            } else if character == '#' || (nix && character == '/' && chars.peek().is_some_and(|(_, ch)| *ch == '/')) {
                return Some(index);
            }
        }
    }
    None
}

fn push_plain(spans: &mut Vec<Span<'static>>, plain: &mut String) {
    if !plain.is_empty() {
        spans.push(Span::styled(std::mem::take(plain), Style::default().fg(TEXT)));
    }
}

fn config_value_spans(source: &str, spans: &mut Vec<Span<'static>>) {
    let mut chars = source.chars().peekable();
    let mut plain = String::new();
    while let Some(ch) = chars.next() {
        if ch == '"' || ch == '\'' {
            push_plain(spans, &mut plain);
            let mut string = String::from(ch);
            let mut escaped = false;
            for next in chars.by_ref() {
                string.push(next);
                if escaped {
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                } else if next == ch {
                    break;
                }
            }
            spans.push(Span::styled(string, Style::default().fg(Color::Rgb(GREEN.0, GREEN.1, GREEN.2))));
        } else if ch.is_ascii_digit() {
            push_plain(spans, &mut plain);
            let mut number = String::from(ch);
            while chars.peek().is_some_and(|next| next.is_ascii_digit() || *next == '_' || *next == '.') {
                number.push(chars.next().unwrap());
            }
            spans.push(Span::styled(number, Style::default().fg(Color::Rgb(PURPLE.0, PURPLE.1, PURPLE.2))));
        } else if ch.is_ascii_alphabetic() {
            let mut word = String::from(ch);
            while chars.peek().is_some_and(|next| next.is_ascii_alphanumeric() || *next == '_') {
                word.push(chars.next().unwrap());
            }
            if matches!(word.as_str(), "true" | "false" | "null" | "let" | "in" | "with" | "inherit") {
                push_plain(spans, &mut plain);
                spans.push(Span::styled(word, Style::default().fg(Color::Rgb(RED.0, RED.1, RED.2))));
            } else {
                plain.push_str(&word);
            }
        } else {
            plain.push(ch);
        }
    }
    push_plain(spans, &mut plain);
}

fn highlight_config(path: &Path, lines: &[String]) -> Option<(String, Vec<Line<'static>>)> {
    let language = config_language(path)?;
    let nix = language == "Nix";
    let output = lines.iter().map(|line| {
        if line == "… preview truncated" || line == "(empty file)" {
            return Line::from(Span::styled(line.clone(), Style::default().fg(Color::Rgb(146, 131, 116))));
        }
        let (code, comment) = match config_comment_at(line, nix) {
            Some(index) => (&line[..index], Some(&line[index..])),
            None => (line.as_str(), None),
        };
        let mut spans = Vec::new();
        if code.trim_start().starts_with('[') && code.trim_end().ends_with(']') {
            spans.push(Span::styled(code.to_owned(), Style::default().fg(Color::Rgb(YELLOW.0, YELLOW.1, YELLOW.2))));
        } else if let Some(index) = code.find('=') {
            let (key, value) = code.split_at(index);
            spans.push(Span::styled(key.to_owned(), Style::default().fg(Color::Rgb(BLUE.0, BLUE.1, BLUE.2))));
            config_value_spans(value, &mut spans);
        } else {
            config_value_spans(code, &mut spans);
        }
        if let Some(comment) = comment {
            spans.push(Span::styled(comment.to_owned(), Style::default().fg(Color::Rgb(COMMENT.0, COMMENT.1, COMMENT.2)).add_modifier(Modifier::ITALIC)));
        }
        Line::from(spans)
    }).collect();
    Some((language.to_owned(), output))
}

pub fn highlight(path: &Path, lines: &[String]) -> Option<(String, Vec<Line<'static>>)> {
    let syntax_set = syntaxes();
    let syntax = grammar_extension(path)
        .and_then(|extension| syntax_set.find_syntax_by_extension(extension))
        .or_else(|| lines.first().and_then(|line| syntax_set.find_syntax_by_first_line(line)));
    let Some(syntax) = syntax else {
        return highlight_config(path, lines);
    };
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut styled = Vec::with_capacity(lines.len());
    for line in lines {
        if line == "… preview truncated" || line == "(empty file)" {
            styled.push(Line::from(Span::styled(line.clone(), Style::default().fg(Color::Rgb(146, 131, 116)))));
            continue;
        }
        // The default syntect syntax dump expects newline-terminated lines.
        let source = format!("{line}\n");
        let highlighted = match highlighter.highlight_line(&source, syntax_set) {
            Ok(highlighted) => highlighted,
            // A malformed grammar must never break the file browser or preview.
            Err(_) => {
                styled.push(Line::styled(line.clone(), Style::default().fg(TEXT)));
                continue;
            }
        };
        let spans = highlighted
            .into_iter()
            .filter_map(|(style, text)| {
                let text = text.trim_end_matches('\n');
                (!text.is_empty()).then(|| Span::styled(text.to_owned(), token_style(style)))
            })
            .collect::<Vec<_>>();
        styled.push(Line::from(spans));
    }
    Some((syntax.name.clone(), styled))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_keywords_and_strings_use_distinct_gruvbox_colors() {
        let input = vec!["fn main() { let a = \"hello\"; }".to_owned()];
        let (syntax, lines) = highlight(Path::new("main.rs"), &input).unwrap();
        assert!(!syntax.is_empty());
        let spans = &lines[0].spans;
        assert_eq!(spans.iter().map(|span| span.content.as_ref()).collect::<String>(), input[0]);
        let unique = spans.iter().map(|span| span.style.fg).collect::<std::collections::HashSet<_>>();
        assert!(unique.len() > 1, "syntax highlighting must apply multiple colors");
    }

    #[test]
    fn toml_fallback_colors_keys_strings_values_and_comments() {
        let lines = vec!["theme = \"gruvbox#dark\" # comment".to_owned(), "[jumps]".to_owned(), "max = 12".to_owned()];
        let (language, output) = highlight(Path::new("config.toml"), &lines).unwrap();
        assert!(!language.is_empty());
        assert_eq!(output.len(), lines.len());
        for (line, original) in output.iter().zip(&lines) {
            assert_eq!(line.spans.iter().map(|span| span.content.as_ref()).collect::<String>(), *original);
        }
        let colors = output[0].spans.iter().filter_map(|span| span.style.fg).collect::<std::collections::HashSet<_>>();
        assert!(colors.len() >= 3);
    }

    #[test]
    fn config_fallback_handles_unicode_without_splitting_bytes() {
        let line = "project = \"München 🍀\" # Grüße".to_owned();
        let (_, out) = highlight(Path::new("flake.nix"), &[line.clone()]).unwrap();
        assert_eq!(out[0].spans.iter().map(|span| span.content.as_ref()).collect::<String>(), line);
    }

    #[test]
    fn plain_text_has_no_syntax_decoration() {
        assert!(highlight(Path::new("unrecognised.zzzzunknown"), &["hello".to_owned()]).is_none());
    }

    #[test]
    fn notes_and_truncation_marker_are_neutral() {
        let input = vec!["fn main() {}".to_owned(), "… preview truncated".to_owned()];
        let (_, lines) = highlight(Path::new("test.rs"), &input).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans[0].content, "… preview truncated");
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Rgb(146, 131, 116)));
    }

    #[test]
    fn selected_syntax_is_bounded_by_input_and_uses_no_ansi_codes() {
        let lines = (0..120).map(|_| "let x = 42;".to_owned()).collect::<Vec<_>>();
        let (_, highlighted) = highlight(Path::new("code.rs"), &lines).unwrap();
        assert_eq!(highlighted.len(), 120);
        assert!(highlighted.iter().flat_map(|line| &line.spans).all(|span| !span.content.contains('\x1b')));
    }
}
