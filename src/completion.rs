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
    loaded: Instant,
}

impl Default for CompletionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionEngine {
    pub fn new() -> Self {
        let mut engine = Self {
            commands: Vec::new(),
            loaded: Instant::now() - Duration::from_secs(3600),
        };
        engine.refresh();
        engine
    }

    pub fn refresh(&mut self) {
        let mut set = BTreeSet::new();
        for builtin in [
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
        ] {
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
        self.loaded = Instant::now();
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
                if !first.is_empty() && first.starts_with(token) && seen.insert(first.to_string()) {
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
                if command.starts_with(token) && !items.iter().any(|c| &c.value == command) {
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
                if key.starts_with(needle) {
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
        if !name.starts_with(&name_prefix) {
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

/// Longest common prefix, used to extend the token before showing a menu.
pub fn common_prefix(items: &[Candidate]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut prefix = first.value.clone();
    for item in &items[1..] {
        while !item.value.starts_with(&prefix) {
            if prefix.pop().is_none() {
                return String::new();
            }
        }
    }
    prefix
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
}
