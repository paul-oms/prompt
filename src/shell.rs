use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, Paths};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
    Other,
}

impl ShellKind {
    pub fn name(self) -> &'static str {
        match self {
            ShellKind::Zsh => "zsh",
            ShellKind::Bash => "bash",
            ShellKind::Fish => "fish",
            ShellKind::Other => "shell",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShellLaunch {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub kind: ShellKind,
    /// True when we installed the OSC 133 integration that powers the
    /// composer's prompt/command state machine.
    pub integrated: bool,
}

const PROMPT_ZSH: &str = include_str!("../assets/shell/prompt.zsh");
const PROMPT_BASH: &str = include_str!("../assets/shell/prompt.bash");
const ZSHRC: &str = include_str!("../assets/shell/zshrc");
const ZSHENV: &str = include_str!("../assets/shell/zshenv");
const ZPROFILE: &str = include_str!("../assets/shell/zprofile");
const ZLOGIN: &str = include_str!("../assets/shell/zlogin");
const BASHRC: &str = include_str!("../assets/shell/bashrc");

/// Pick a shell and write Prompt's integration scripts. This is the whole of
/// first-run shell setup: no rc files are modified, no dotfiles are moved.
pub fn prepare(paths: &Paths, config: &Config) -> ShellLaunch {
    let _ = fs::create_dir_all(paths.shell_dir().join("zsh"));
    let _ = fs::create_dir_all(paths.shell_dir().join("bash"));
    let _ = fs::write(paths.shell_dir().join("prompt.zsh"), PROMPT_ZSH);
    let _ = fs::write(paths.shell_dir().join("prompt.bash"), PROMPT_BASH);
    let _ = fs::write(paths.shell_dir().join("zsh/.zshrc"), ZSHRC);
    let _ = fs::write(paths.shell_dir().join("zsh/.zshenv"), ZSHENV);
    let _ = fs::write(paths.shell_dir().join("zsh/.zprofile"), ZPROFILE);
    let _ = fs::write(paths.shell_dir().join("zsh/.zlogin"), ZLOGIN);
    let _ = fs::write(paths.shell_dir().join("bash/bashrc"), BASHRC);

    let cli_shell = env::var("PROMPT_SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let chosen = if let Some(path) = cli_shell {
        Some(PathBuf::from(path))
    } else if !config.behavior.shell.trim().is_empty() {
        Some(PathBuf::from(config.behavior.shell.trim()))
    } else {
        bundled_zsh()
            .or_else(|| find_in_path("zsh"))
            .or_else(|| find_in_path("bash"))
            .or_else(|| env::var_os("SHELL").map(PathBuf::from))
            .or_else(|| Some(PathBuf::from("/bin/sh")))
    };
    let program = chosen.unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let kind = detect_kind(&program);

    let program_str = program.to_string_lossy().into_owned();
    let home = env::var("HOME").unwrap_or_else(|_| "/".into());
    let user_zdotdir = env::var("ZDOTDIR").unwrap_or_else(|_| home.clone());

    let mut args = Vec::new();
    let mut integrated = true;
    let mut envs: Vec<(String, String)> = vec![
        ("SHELL".into(), program_str.clone()),
        ("PROMPT_TERMINAL".into(), "1".into()),
        (
            "PROMPT_CONFIG_DIR".into(),
            paths.config_dir.to_string_lossy().into_owned(),
        ),
        ("PROMPT_USER_HOME".into(), home),
        ("PROMPT_USER_ZDOTDIR".into(), user_zdotdir),
        ("PROMPT_SHELL_INTEGRATION".into(), "1".into()),
        ("PROMPT_SHELL".into(), kind.name().into()),
    ];

    match kind {
        ShellKind::Zsh => {
            args.push("-l".into());
            args.push("-i".into());
            let zsh_dir = paths.shell_dir().join("zsh").to_string_lossy().into_owned();
            envs.push(("ZDOTDIR".into(), zsh_dir.clone()));
            envs.push(("PROMPT_ZDOTDIR".into(), zsh_dir));
            if let Some(prefix) = zsh_prefix(&program) {
                envs.push((
                    "PROMPT_ZSH_PREFIX".into(),
                    prefix.to_string_lossy().into_owned(),
                ));
            }
        }
        ShellKind::Bash => {
            args.push("--rcfile".into());
            args.push(
                paths
                    .shell_dir()
                    .join("bash/bashrc")
                    .to_string_lossy()
                    .into_owned(),
            );
            args.push("-i".into());
        }
        ShellKind::Fish => {
            args.push("-i".into());
            integrated = false;
        }
        ShellKind::Other => {
            args.push("-i".into());
            integrated = false;
        }
    }

    if env::var_os("LANG").is_none() && env::var_os("LC_ALL").is_none() {
        let locale = match kind {
            ShellKind::Zsh if cfg!(target_os = "macos") => "en_US.UTF-8",
            _ => "C.UTF-8",
        };
        envs.push(("LANG".into(), locale.into()));
    }

    if integrated {
        envs.push(("PROMPT_SHELL_INTEGRATION".into(), "1".into()));
    } else {
        envs.push(("PROMPT_SHELL_INTEGRATION".into(), "0".into()));
    }

    ShellLaunch {
        program: program_str,
        args,
        env: envs,
        kind,
        integrated,
    }
}

fn detect_kind(program: &Path) -> ShellKind {
    let name = program
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.contains("zsh") {
        ShellKind::Zsh
    } else if name.contains("bash") {
        ShellKind::Bash
    } else if name.contains("fish") {
        ShellKind::Fish
    } else {
        ShellKind::Other
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Release builds can ship zsh next to the executable (`vendor/zsh`). During
/// development the same layout works from the repository root.
fn bundled_zsh() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("PROMPT_ZSH") {
        let path = PathBuf::from(explicit);
        if is_executable(&path) {
            return Some(path);
        }
    }

    let mut roots = Vec::new();
    if let Ok(exe) = env::current_exe() {
        for ancestor in exe.ancestors().take(6) {
            roots.push(ancestor.to_path_buf());
        }
    }
    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    for root in roots {
        for rel in [
            "vendor/zsh/bin/zsh",
            "vendor/zsh/usr/bin/zsh",
            "vendor/zsh",
            "../vendor/zsh/bin/zsh",
        ] {
            let candidate = root.join(rel);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// When zsh is shipped in `vendor/zsh` (or comes from a distro package), its
/// compiled-in fpath/module_path may point at the build machine. Locate the
/// prefix next to the binary; the zshenv shim expands it.
fn zsh_prefix(program: &Path) -> Option<PathBuf> {
    if !program.is_absolute() {
        return None;
    }
    let root = program.parent()?.parent()?;
    if root.join("share/zsh").is_dir() || root.join("lib/zsh").is_dir() {
        Some(root.to_path_buf())
    } else {
        None
    }
}

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}
