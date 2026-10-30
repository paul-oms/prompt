//! Command and path completion for the composer.
//!
//! Deliberately dependency-free and shell-agnostic: Prompt completes from PATH,
//! the current directory and its own history. The shell still owns execution.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Text that replaces the token.
    pub value: String,
    /// Text shown in the popup (usually the basename).
    pub display: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub struct CompletionSet {
    pub start: usize,
    pub end: usize,
    pub items: Vec<Candidate>,
    pub command: bool,
    /// The token as typed, used to decide whether an accepted candidate needs
    /// a trailing space.
    pub token: String,
}

pub struct CompletionEngine {
    commands: Vec<String>,
    /// The curated command list, kept in priority order so inline suggestions
    /// prefer common tools over whatever happens to sort first on PATH.
    builtins: Vec<String>,
    loaded: Instant,
}

/// Common commands, in the order inline suggestions should prefer them.
const BUILTINS: &[&str] = &[
    "cd",
    "pwd",
    "ls",
    "echo",
    "cat",
    "grep",
    "rg",
    "git",
    "cargo",
    "rustc",
    "npm",
    "pnpm",
    "bun",
    "yarn",
    "node",
    "python",
    "python3",
    "vim",
    "nvim",
    "nano",
    "code",
    "ssh",
    "scp",
    "curl",
    "wget",
    "mkdir",
    "rm",
    "mv",
    "cp",
    "touch",
    "chmod",
    "chown",
    "tar",
    "unzip",
    "docker",
    "kubectl",
    "systemctl",
    "brew",
    "apt",
    "pacman",
    "yay",
    "make",
    "just",
    "go",
    "sed",
    "awk",
    "find",
    "which",
    "open",
    "man",
    "less",
    "jq",
    "tmux",
    "htop",
    "btop",
    "top",
    "kill",
    "ps",
    "df",
    "du",
    "env",
    "export",
    "source",
    "alias",
    "history",
    "clear",
    "exit",
    "zsh",
    "bash",
    "fish",
    "sudo",
    "prompt",
];

impl Default for CompletionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionEngine {
    pub fn new() -> Self {
        let mut engine = Self {
            commands: Vec::new(),
            builtins: Vec::new(),
            loaded: Instant::now() - Duration::from_secs(3600),
        };
        engine.refresh();
        engine
    }

    pub fn refresh(&mut self) {
        let mut set = BTreeSet::new();
        for builtin in BUILTINS {
            set.insert(builtin.to_string());
        }
        for dir in command_dirs() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(ft) = entry.file_type() else {
                    continue;
                };
                if !ft.is_file() && !ft.is_symlink() {
                    continue;
                }
                if let Some(name) = entry.file_name().to_str() {
                    if !name.is_empty() && !name.starts_with('.') {
                        set.insert(name.to_string());
                    }
                }
            }
        }
        self.commands = set.into_iter().collect();
        self.builtins = BUILTINS.iter().map(|s| (*s).to_string()).collect();
        self.loaded = Instant::now();
    }

    /// The first command on PATH that extends `text`, used for the inline
    /// suggestion. Only call this for a single command token.
    pub fn command_suggestion(&mut self, text: &str) -> Option<String> {
        if text.is_empty()
            || text.chars().any(char::is_whitespace)
            || text.contains('/')
            || text.starts_with('$')
        {
            return None;
        }
        if self.loaded.elapsed() > Duration::from_secs(30) {
            self.refresh();
        }
        // If the token is already a complete command, adding more letters would
        // feel like the editor is second-guessing the user.
        if self.commands.iter().any(|command| command == text) {
            return None;
        }
        self.builtins
            .iter()
            .chain(self.commands.iter())
            .find(|command| command.len() > text.len() && command.starts_with(text))
            .map(|command| command[text.len()..].to_string())
    }

    /// Compute completions for the token under `caret`.
    pub fn complete(
        &mut self,
        text: &str,
        caret: usize,
        cwd: &Path,
        history: &[String],
    ) -> Option<CompletionSet> {
        if self.loaded.elapsed() > Duration::from_secs(30) {
            self.refresh();
        }
        let caret = caret.min(text.len());
        let start = token_start(text, caret);
        let end = token_end(text, caret);
        let token = &text[start..caret];

        let prefix = &text[..start];
        let command = is_command_position(prefix);

        let mut items: Vec<Candidate> = Vec::new();
        if command && !token.starts_with('$') {
            // History first: the commands someone actually uses should rank
            // above a random binary on PATH.
            let mut seen = BTreeSet::new();
            for entry in history.iter().rev() {
                let first = entry.split_whitespace().next().unwrap_or("");
                if !first.is_empty()
                    && starts_with_ignore_ascii_case(first, token)
                    && seen.insert(first.to_string())
                {
                    items.push(Candidate {
                        value: first.to_string(),
                        display: first.to_string(),
                        is_dir: false,
                    });
                    if items.len() >= 12 {
                        break;
                    }
                }
            }
            for command in &self.commands {
                if items.len() >= 200 {
                    break;
                }
                if starts_with_ignore_ascii_case(command, token)
                    && !items.iter().any(|c| &c.value == command)
                {
                    items.push(Candidate {
                        value: command.clone(),
                        display: command.clone(),
                        is_dir: false,
                    });
                }
            }
        } else if token.starts_with('$') {
            let needle = &token[1..];
            for (key, _) in env::vars() {
                if starts_with_ignore_ascii_case(&key, needle) {
                    items.push(Candidate {
                        value: format!("${key}"),
                        display: key.clone(),
                        is_dir: false,
                    });
                }
            }
            items.sort_by(|a, b| a.value.cmp(&b.value));
        } else {
            items = path_candidates(token, cwd);
        }

        if items.is_empty() {
            return None;
        }
        Some(CompletionSet {
            start,
            end,
            items,
            command,
            token: token.to_string(),
        })
    }
}

/// PATH plus the places version managers commonly live. The shell's rc files
/// may add to PATH after Prompt starts, so scanning these keeps completion
/// useful for cargo/mise/nvm/bun users without trusting stale environment.
fn command_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        for rel in [
            ".cargo/bin",
            ".local/bin",
            ".bun/bin",
            ".deno/bin",
            ".local/share/mise/shims",
            ".local/share/pnpm",
            ".volta/bin",
        ] {
            dirs.push(home.join(rel));
        }
        let nvm = home.join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(&nvm) {
            for entry in entries.flatten() {
                dirs.push(entry.path().join("bin"));
            }
        }
        let asdf = home.join(".asdf/shims");
        dirs.push(asdf);
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/local/sbin"] {
        dirs.push(PathBuf::from(dir));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn token_start(text: &str, caret: usize) -> usize {
    let mut start = caret;
    for (i, ch) in text[..caret].char_indices().rev() {
        if ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '<' | '>' | '(' | ')') {
            return i + ch.len_utf8();
        }
        start = i;
    }
    start
}

fn token_end(text: &str, caret: usize) -> usize {
    for (i, ch) in text[caret..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '<' | '>' | '(' | ')') {
            return caret + i;
        }
    }
    text.len()
}

fn is_command_position(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    if trimmed.is_empty() {
        return true;
    }
    let last = trimmed.chars().last().unwrap();
    if matches!(last, '|' | '&' | ';' | '(') {
        return true;
    }
    let mut words = trimmed.split_whitespace().rev();
    match words.next() {
        Some("sudo") | Some("env") | Some("command") | Some("exec") | Some("time")
        | Some("nohup") => true,
        _ => false,
    }
}

fn path_candidates(token: &str, cwd: &Path) -> Vec<Candidate> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let (display_prefix, base, name_prefix) = if let Some(rest) = token.strip_prefix("~/") {
        let base = home.clone().unwrap_or_else(|| PathBuf::from("/"));
        let (dir, prefix) = split_dir(rest);
        (format!("~/{dir}"), base.join(dir), prefix.to_string())
    } else if token == "~" {
        (
            String::new(),
            home.unwrap_or_else(|| PathBuf::from("/")),
            String::new(),
        )
    } else if token.starts_with('/') {
        let (dir, prefix) = split_dir(token);
        (dir.to_string(), PathBuf::from(dir), prefix.to_string())
    } else {
        let (dir, prefix) = split_dir(token);
        (dir.to_string(), cwd.join(dir), prefix.to_string())
    };

    let Ok(entries) = fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !starts_with_ignore_ascii_case(&name, &name_prefix) {
            continue;
        }
        if name.starts_with('.') && !name_prefix.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let mut value = format!("{display_prefix}{name}");
        if is_dir {
            value.push('/');
        }
        // Keep paths with spaces/specials usable without forcing the user to
        // learn shell quoting. Backslashes preserve ~ and / expansion.
        if name
            .chars()
            .any(|c| matches!(c, ' ' | '(' | ')' | '[' | ']' | '{' | '}' | '\'' | '"'))
        {
            value = value
                .chars()
                .map(|c| {
                    if matches!(c, ' ' | '(' | ')' | '[' | ']' | '{' | '}' | '\'' | '"') {
                        format!("\\{c}")
                    } else {
                        c.to_string()
                    }
                })
                .collect();
        }
        out.push(Candidate {
            value,
            display: if is_dir { format!("{name}/") } else { name },
            is_dir,
        });
    }
    out.sort_by(|a, b| a.display.cmp(&b.display));
    out.truncate(200);
    out
}

fn split_dir(token: &str) -> (&str, &str) {
    match token.rfind('/') {
        Some(i) => (&token[..=i], &token[i + 1..]),
        None => ("", token),
    }
}

/// Prefix comparison used by tab completion. Commands, environment variables
/// and paths are matched without regard to ASCII case, while the accepted
/// candidate keeps its real casing so the shell still sees a valid name.
fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Longest common prefix, used to extend the token before showing a menu.
pub fn common_prefix(items: &[Candidate]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    // Walk the first candidate's characters so the result keeps real casing
    // even when the comparison ignores it.
    let mut prefix: Vec<char> = first.value.chars().collect();
    for item in &items[1..] {
        let shared = prefix
            .iter()
            .zip(item.value.chars())
            .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
            .count();
        prefix.truncate(shared);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.into_iter().collect()
}

/// Inline history suggestion ("ghost text") for the composer.
///
/// Only offered when the caret sits at the end of a single-line buffer, which
/// keeps the preview unambiguous: it always occupies the space to the right of
/// the caret and never covers existing text.
pub fn history_suggestion(text: &str, caret: usize, history: &[String]) -> Option<String> {
    if caret != text.len() || text.is_empty() || text.contains('\n') {
        return None;
    }
    history.iter().rev().find_map(|entry| {
        if entry.len() > text.len() && entry.starts_with(text) && !entry.contains('\n') {
            Some(entry[text.len()..].to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bounds() {
        let text = "git status --short";
        assert_eq!(token_start(text, 12), 11);
        assert_eq!(token_end(text, 12), 18);
        assert_eq!(token_start(text, 3), 0);
        assert_eq!(token_end(text, 3), 3);
    }

    #[test]
    fn command_position_detection() {
        assert!(is_command_position(""));
        assert!(is_command_position("git status | "));
        assert!(is_command_position("echo hi && "));
        assert!(!is_command_position("git "));
        assert!(is_command_position("sudo "));
    }

    #[test]
    fn paths_with_spaces_are_escaped() {
        let dir = std::env::temp_dir().join(format!("prompt comp {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("my dir")).unwrap();
        std::fs::write(dir.join("my file.txt"), "").unwrap();
        let items = path_candidates("", &dir);
        assert!(
            items.iter().any(|c| c.value == "my\\ file.txt"),
            "{items:?}"
        );
        assert!(items.iter().any(|c| c.value == "my\\ dir/"), "{items:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn common_prefix_of_candidates() {
        let items = vec![
            Candidate {
                value: "cargo".into(),
                display: "cargo".into(),
                is_dir: false,
            },
            Candidate {
                value: "cargo-clippy".into(),
                display: "cargo-clippy".into(),
                is_dir: false,
            },
        ];
        assert_eq!(common_prefix(&items), "cargo");
    }

    #[test]
    fn common_prefix_ignores_case() {
        let items = vec![
            Candidate {
                value: "Cargo.toml".into(),
                display: "Cargo.toml".into(),
                is_dir: false,
            },
            Candidate {
                value: "cargo.lock".into(),
                display: "cargo.lock".into(),
                is_dir: false,
            },
        ];
        assert_eq!(common_prefix(&items), "Cargo.");
    }

    #[test]
    fn tab_completion_ignores_case() {
        let mut engine = CompletionEngine::new();
        engine.builtins = vec!["cargo".into(), "cat".into()];
        engine.commands = vec!["cargo".into(), "cat".into()];
        let set = engine
            .complete("CAR", 3, Path::new("."), &[])
            .expect("case-insensitive command match");
        assert!(set.items.iter().any(|c| c.value == "cargo"), "{set:?}");
    }

    #[test]
    fn history_completion_ignores_case() {
        let mut engine = CompletionEngine::new();
        engine.builtins.clear();
        engine.commands.clear();
        let history = vec!["Git status".to_string()];
        let set = engine
            .complete("git", 3, Path::new("."), &history)
            .expect("case-insensitive history match");
        assert!(set.items.iter().any(|c| c.value == "Git"), "{set:?}");
    }

    #[test]
    fn path_completion_ignores_case() {
        let dir = std::env::temp_dir().join(format!("prompt comp case {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "").unwrap();
        let items = path_candidates("rea", &dir);
        assert!(items.iter().any(|c| c.value == "README.md"), "{items:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inline_history_suggestion() {
        let history = vec![
            "cargo build".to_string(),
            "git status".to_string(),
            "git commit -m wip".to_string(),
        ];
        assert_eq!(
            history_suggestion("git ", 4, &history).as_deref(),
            Some("commit -m wip")
        );
        assert_eq!(
            history_suggestion("git s", 5, &history).as_deref(),
            Some("tatus")
        );
        assert_eq!(history_suggestion("git status", 10, &history), None);
        assert_eq!(history_suggestion("git", 1, &history), None);
    }

    #[test]
    fn inline_suggestion_requires_caret_at_end() {
        let history = vec!["echo hello".to_string()];
        assert_eq!(history_suggestion("echo", 2, &history), None);
        assert_eq!(
            history_suggestion("echo", 4, &history).as_deref(),
            Some(" hello")
        );
    }

    #[test]
    fn command_suggestion_prefers_longer_prefix() {
        let mut engine = CompletionEngine::new();
        engine.builtins = vec!["cargo".into(), "cat".into(), "cd".into()];
        engine.commands = vec!["cargo".into(), "cat".into(), "cd".into()];
        assert_eq!(engine.command_suggestion("car").as_deref(), Some("go"));
        assert_eq!(engine.command_suggestion("c"), Some("argo".into()));
        assert_eq!(engine.command_suggestion("cargo"), None);
        assert_eq!(engine.command_suggestion("cargo "), None);
    }
}
