mod app;
mod completion;
mod config;
mod editor;
mod ffi;
mod font;
mod gpu;
mod highlight;
mod history;
mod keys;
mod pty;
mod render;
mod shell;
mod term;
mod theme;

use std::process::ExitCode;

use winit::event_loop::{ControlFlow, EventLoop};

const HELP: &str = "\
Prompt — a terminal that types like an editor.

Usage:
  prompt [options]

Options:
  --shell <path>    Shell to launch (default: bundled zsh, system zsh, bash)
  --config <dir>    Config directory (default: ~/.config/prompt)
  --data <dir>      Data directory (default: ~/.local/share/prompt)
  -h, --help        Show this help
  -V, --version     Show the version

Run with no arguments for a zero-config first launch. Prompt creates its
config, shell integration and history automatically.
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("prompt {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--shell" => match args.next() {
                Some(path) => std::env::set_var("PROMPT_SHELL", path),
                None => {
                    eprintln!("prompt: --shell needs a path");
                    return ExitCode::from(2);
                }
            },
            "--config" => match args.next() {
                Some(path) => std::env::set_var("PROMPT_CONFIG_DIR", path),
                None => {
                    eprintln!("prompt: --config needs a directory");
                    return ExitCode::from(2);
                }
            },
            "--data" => match args.next() {
                Some(path) => std::env::set_var("PROMPT_DATA_DIR", path),
                None => {
                    eprintln!("prompt: --data needs a directory");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("prompt: unknown argument '{other}'\n");
                eprint!("{HELP}");
                return ExitCode::from(2);
            }
        }
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("prompt: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = app::PromptApp::new(proxy)?;
    event_loop.run_app(&mut app)?;
    Ok(())
}
