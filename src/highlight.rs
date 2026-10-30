//! Shell-ish syntax highlighting for the composer.
//!
//! This is intentionally a lightweight lexer, not a shell parser. It only
//! needs to make command lines easier to scan: the command name, flags,
//! strings, variables, paths and operators all get distinct colours.

use crate::font::Span;
use crate::theme::{Rgb, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Command,
    Flag,
    String,
    Variable,
    Operator,
    Path,
    Comment,
    Text,
}

pub fn line_spans(line: &str, theme: &Theme) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut command_position = true;

    while i < bytes.len() {
        let c = line[i..].chars().next().unwrap();
        let start = i;

        if c.is_whitespace() {
            while i < bytes.len() {
                let ch = line[i..].chars().next().unwrap();
                if !ch.is_whitespace() {
                    break;
                }
                i += ch.len_utf8();
            }
            push(&mut spans, &line[start..i], TokenKind::Text, theme);
            continue;
        }

        if c == '#' {
            i = line.len();
            push(&mut spans, &line[start..i], TokenKind::Comment, theme);
            continue;
        }

        if c == '\'' || c == '"' {
            let quote = c;
            i += c.len_utf8();
            while i < bytes.len() {
                let ch = line[i..].chars().next().unwrap();
                i += ch.len_utf8();
                if quote == '"' && ch == '\\' && i < bytes.len() {
                    let escaped = line[i..].chars().next().unwrap();
                    i += escaped.len_utf8();
                    continue;
                }
                if ch == quote {
                    break;
                }
            }
            push(&mut spans, &line[start..i], TokenKind::String, theme);
            command_position = false;
            continue;
        }

        if c == '$' {
            i += 1;
            if i < bytes.len() && line[i..].starts_with('{') {
                if let Some(end) = line[i..].find('}') {
                    i += end + 1;
                } else {
                    i = line.len();
                }
            } else {
                while i < bytes.len() {
                    let ch = line[i..].chars().next().unwrap();
                    if ch.is_alphanumeric() || ch == '_' {
                        i += ch.len_utf8();
                    } else {
                        break;
                    }
                }
            }
            push(&mut spans, &line[start..i], TokenKind::Variable, theme);
            command_position = false;
            continue;
        }

        if matches!(c, '|' | '&' | ';' | '>' | '<' | '(' | ')') {
            i += c.len_utf8();
            while i < bytes.len() {
                let ch = line[i..].chars().next().unwrap();
                if matches!(ch, '|' | '&' | ';' | '>' | '<') {
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
            push(&mut spans, &line[start..i], TokenKind::Operator, theme);
            command_position = true;
            continue;
        }

        while i < bytes.len() {
            let ch = line[i..].chars().next().unwrap();
            if ch.is_whitespace()
                || matches!(
                    ch,
                    '\'' | '"' | '$' | '|' | '&' | ';' | '>' | '<' | '(' | ')'
                )
            {
                break;
            }
            i += ch.len_utf8();
        }
        let token = &line[start..i];
        let kind = if command_position {
            TokenKind::Command
        } else if token.starts_with('-') && token.len() > 1 {
            TokenKind::Flag
        } else if token.contains('/') || token.starts_with('~') || token.starts_with('.') {
            TokenKind::Path
        } else {
            TokenKind::Text
        };
        push(&mut spans, token, kind, theme);
        command_position = false;
    }

    spans
}

fn push(spans: &mut Vec<Span>, text: &str, kind: TokenKind, theme: &Theme) {
    if text.is_empty() {
        return;
    }
    let color = color_for(kind, theme);
    if let Some(last) = spans.last_mut() {
        if last.color == color && !last.bold {
            last.text.push_str(text);
            return;
        }
    }
    spans.push(Span {
        text: text.to_string(),
        color,
        bold: kind == TokenKind::Command,
        italic: false,
    });
}

fn color_for(kind: TokenKind, theme: &Theme) -> Rgb {
    match kind {
        TokenKind::Command => theme.accent,
        TokenKind::Flag => theme.yellow,
        TokenKind::String => theme.green,
        TokenKind::Variable => theme.purple,
        TokenKind::Operator => theme.red,
        TokenKind::Path => theme.cyan,
        TokenKind::Comment => theme.faint,
        TokenKind::Text => theme.fg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_shell_tokens() {
        let theme = Theme::default();
        let spans = line_spans("git commit -m 'hello world' | grep hello > out.txt", &theme);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "git commit -m 'hello world' | grep hello > out.txt");
        assert_eq!(spans[0].text, "git");
        assert!(spans[0].bold);
        assert!(spans.iter().any(|s| s.text == "-m"));
        assert!(spans.iter().any(|s| s.text == "'hello world'"));
        assert!(spans.iter().any(|s| s.text == "grep" && s.bold));
        assert!(spans.iter().any(|s| s.text == "out.txt"));
    }

    #[test]
    fn comments_and_variables() {
        let theme = Theme::default();
        let spans = line_spans("echo $HOME # where", &theme);
        assert!(spans.iter().any(|s| s.text == "$HOME"));
        assert!(spans.iter().any(|s| s.text == "# where"));
    }

    #[test]
    fn quotes_are_atomic() {
        let theme = Theme::default();
        let spans = line_spans("echo 'a # b'", &theme);
        assert!(spans.iter().any(|s| s.text == "'a # b'"));
    }

    #[test]
    fn round_trip_text() {
        let theme = Theme::default();
        for line in [
            "cargo run --release",
            "echo \"unterminated",
            "printf '%s\\n' $PATH",
            "a && b || c; d",
        ] {
            let spans = line_spans(line, &theme);
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, line);
        }
    }
}
