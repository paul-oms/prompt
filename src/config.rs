use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub behavior: Behavior,
    pub ui: Ui,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Preferred font family. Empty means "pick the best available".
    pub font_family: String,
    pub font_size: f32,
    pub line_height: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 14.0,
            line_height: 1.38,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Behavior {
    /// Shell to launch. Empty means auto-detect (bundled zsh -> system zsh ->
    /// bash -> $SHELL).
    pub shell: String,
    pub scrollback_lines: usize,
    /// Copy terminal selections to the system clipboard automatically.
    pub copy_on_select: bool,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            shell: String::new(),
            scrollback_lines: 10_000,
            copy_on_select: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub show_hints: bool,
    /// Show dim ghost text for the history/command completion. Right (or Tab
    /// when the completion is unambiguous) accepts it.
    pub inline_suggestions: bool,
    pub start_maximized: bool,
    pub window_width: u32,
    pub window_height: u32,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            show_hints: true,
            inline_suggestions: true,
            start_maximized: false,
            window_width: 1080,
            window_height: 700,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let config_base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let data_base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        // A terminal should feel at home on both macOS and Linux; keep the
        // XDG-style dotfile layout everywhere instead of Application Support.
        let config_dir = std::env::var_os("PROMPT_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| config_base.join("prompt"));
        let data_dir = std::env::var_os("PROMPT_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_base.join("prompt"));
        Self {
            config_dir,
            data_dir,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn shell_dir(&self) -> PathBuf {
        self.config_dir.join("shell")
    }

    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("history.jsonl")
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.toml")
    }
}

const CONFIG_TEMPLATE: &str = r#"# Prompt configuration
# Every setting has a sensible default: delete this file to start over.

[appearance]
# Leave empty to pick the best monospace font installed on this machine.
font_family = ""
font_size = 14.0
line_height = 1.38

[behavior]
# Leave empty to auto-detect: bundled zsh, system zsh, bash, then $SHELL.
shell = ""
scrollback_lines = 10000
copy_on_select = false

[ui]
show_hints = true
# Dim completion preview; Right (or an unambiguous Tab) accepts it.
inline_suggestions = true
start_maximized = false
window_width = 1080
window_height = 700
"#;

/// Load config, creating the config directory and a commented default file on
/// first run. A broken config never prevents Prompt from starting: we fall back
/// to defaults and keep the file untouched so the user can fix it.
pub fn load(paths: &Paths) -> (Config, Option<String>) {
    let _ = fs::create_dir_all(&paths.config_dir);
    let _ = fs::create_dir_all(&paths.data_dir);

    let file = paths.config_file();
    if !file.exists() {
        if let Err(err) = write_new(&file, CONFIG_TEMPLATE) {
            return (
                Config::default(),
                Some(format!("could not write config: {err}")),
            );
        }
    }

    match fs::read_to_string(&file) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(cfg) => (sanitize(cfg), None),
            Err(err) => (
                Config::default(),
                Some(format!("config.toml has an error, using defaults: {err}")),
            ),
        },
        Err(err) => (
            Config::default(),
            Some(format!("could not read config.toml, using defaults: {err}")),
        ),
    }
}

fn write_new(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

fn sanitize(mut cfg: Config) -> Config {
    cfg.appearance.font_size = cfg.appearance.font_size.clamp(8.0, 48.0);
    cfg.appearance.line_height = cfg.appearance.line_height.clamp(1.0, 2.5);
    cfg.behavior.scrollback_lines = cfg.behavior.scrollback_lines.clamp(100, 1_000_000);
    cfg.ui.window_width = cfg.ui.window_width.clamp(480, 8000);
    cfg.ui.window_height = cfg.ui.window_height.clamp(320, 6000);
    cfg
}

/// True the first time Prompt is run in a config directory.
pub fn is_first_run(paths: &Paths) -> bool {
    !paths.state_file().exists()
}

pub fn mark_started(paths: &Paths) {
    let text = format!(
        "first_run = false\nversion = \"{}\"\n",
        env!("CARGO_PKG_VERSION")
    );
    let _ = fs::create_dir_all(&paths.data_dir);
    let _ = fs::write(paths.state_file(), text);
}
