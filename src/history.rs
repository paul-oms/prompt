use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

/// Persistent command history. Stored as JSON lines so commands with embedded
/// newlines survive a round trip. Prompt keeps its own history (rather than
/// scraping the shell's) because the composer is the source of truth for what
/// the user actually typed.
pub struct History {
    entries: Vec<String>,
    path: PathBuf,
    limit: usize,
}

impl History {
    pub fn load(path: PathBuf, limit: usize) -> Self {
        let mut entries = Vec::new();
        if let Ok(file) = fs::File::open(&path) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(cmd) = serde_json::from_str::<String>(&line) {
                    entries.push(cmd);
                }
            }
        }
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        Self {
            entries,
            path,
            limit,
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(String::as_str)
    }

    /// Add a command, most recent last. Duplicates are moved to the top, the
    /// way zsh's `hist_ignore_all_dups` behaves.
    pub fn add(&mut self, command: &str) {
        let command = command.trim_end_matches(['\n', '\r']);
        if command.trim().is_empty() {
            return;
        }
        if let Some(pos) = self.entries.iter().rposition(|e| e == command) {
            self.entries.remove(pos);
        }
        self.entries.push(command.to_string());
        if self.entries.len() > self.limit {
            let excess = self.entries.len() - self.limit;
            self.entries.drain(..excess);
        }
        self.append(command);
    }

    fn append(&self, command: &str) {
        let _ = command;
        self.rewrite();
    }

    fn rewrite(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut out = String::new();
        for entry in &self.entries {
            if let Ok(line) = serde_json::to_string(entry) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        let _ = fs::write(&self.path, out);
    }

    /// All entries matching `query`, newest first, up to `limit`.
    pub fn search<'a>(&'a self, query: &str, limit: usize) -> Vec<&'a str> {
        let q = query.to_lowercase();
        self.entries
            .iter()
            .rev()
            .filter(|e| q.is_empty() || e.to_lowercase().contains(&q))
            .take(limit)
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "prompt-history-test-{}-{}.jsonl",
            name,
            std::process::id()
        ))
    }

    #[test]
    fn round_trip_and_dedupe() {
        let path = temp_path("dedupe");
        let _ = fs::remove_file(&path);
        {
            let mut h = History::load(path.clone(), 100);
            h.add("echo one");
            h.add("echo two");
            h.add("echo one");
        }
        let h = History::load(path.clone(), 100);
        assert_eq!(h.entries(), &["echo two", "echo one"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn preserves_newlines() {
        let path = temp_path("multiline");
        let _ = fs::remove_file(&path);
        {
            let mut h = History::load(path.clone(), 100);
            h.add("for x in a b\ndo echo $x\ndone");
        }
        let h = History::load(path.clone(), 100);
        assert_eq!(h.entries(), &["for x in a b\ndo echo $x\ndone"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_blank_and_trims_limit() {
        let path = temp_path("limit");
        let _ = fs::remove_file(&path);
        let mut h = History::load(path.clone(), 2);
        h.add("   ");
        h.add("a");
        h.add("b");
        h.add("c");
        assert_eq!(h.entries(), &["b", "c"]);
        let _ = fs::remove_file(path);
    }
}
