use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::shell::ShellLaunch;

/// A shell running behind a pseudo-terminal.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    _notify: Option<Box<dyn Fn() + Send + Sync>>,
    exit_code: Option<i32>,
}

impl Pty {
    pub fn spawn(
        launch: &ShellLaunch,
        cwd: &Path,
        cols: u16,
        rows: u16,
        cell_w: u32,
        cell_h: u32,
        notify: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: cell_w.saturating_mul(cols as u32) as u16,
                pixel_height: cell_h.saturating_mul(rows as u32) as u16,
            })
            .context("opening a pseudo-terminal")?;

        let mut cmd = CommandBuilder::new(&launch.program);
        cmd.args(&launch.args);
        if cwd.is_dir() {
            cmd.cwd(cwd);
        }
        for (key, value) in &launch.env {
            cmd.env(key, value);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "prompt");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("starting {}", launch.program))?;
        // The parent side must not keep the slave open or EOF never arrives.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("cloning pty reader")?;
        let writer = pair.master.take_writer().context("taking pty writer")?;

        let (tx, rx) = mpsc::channel();
        let notify = notify;
        let reader_handle = thread::Builder::new()
            .name("prompt-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                            if let Some(notify) = &notify {
                                notify();
                            }
                        }
                        Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .context("spawning pty reader thread")?;

        let killer = child.clone_killer();
        let _ = child.try_wait();

        Ok(Self {
            master: pair.master,
            child,
            killer,
            writer,
            rx,
            reader: Some(reader_handle),
            _notify: None,
            exit_code: None,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // A shell can disappear while the user is still typing; never let a
        // failed write take down the UI.
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: cell_w.saturating_mul(cols as u32) as u16,
            pixel_height: cell_h.saturating_mul(rows as u32) as u16,
        });
    }

    /// Drain all currently available output without blocking.
    pub fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    pub fn try_exit_code(&mut self) -> Option<i32> {
        if self.exit_code.is_some() {
            return self.exit_code;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exit_code = Some(status.exit_code() as i32);
                self.exit_code
            }
            Ok(None) => None,
            Err(_) => {
                self.exit_code = Some(-1);
                self.exit_code
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.killer.kill();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Paths};
    use std::time::{Duration, Instant};

    fn collect_until(pty: &mut Pty, needle: &[u8], timeout: Duration) -> Vec<u8> {
        let start = Instant::now();
        let mut out = Vec::new();
        while start.elapsed() < timeout {
            let chunk = pty.drain();
            if !chunk.is_empty() {
                out.extend_from_slice(&chunk);
                if out.windows(needle.len()).any(|w| w == needle) {
                    return out;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        out
    }

    #[test]
    fn integrated_shell_emits_prompt_marker() {
        let base = std::env::temp_dir().join(format!("prompt-pty-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let paths = Paths {
            config_dir: base.join("config"),
            data_dir: base.join("data"),
        };
        let cfg = Config::default();
        let launch = crate::shell::prepare(&paths, &cfg);
        if !launch.integrated {
            return;
        }
        let cwd = std::env::temp_dir();
        let mut pty = Pty::spawn(&launch, &cwd, 80, 24, 8, 16, None).expect("spawn shell");
        let out = collect_until(&mut pty, b"\x1b]133;A", Duration::from_secs(5));
        assert!(
            out.windows(7).any(|w| w == b"\x1b]133;A"),
            "no prompt marker, got: {:?}",
            String::from_utf8_lossy(&out)
        );

        pty.write(b"echo prompt-ok\r");
        let out = collect_until(&mut pty, b"prompt-ok\r\n", Duration::from_secs(5));
        assert!(
            String::from_utf8_lossy(&out).contains("prompt-ok"),
            "command did not run: {:?}",
            String::from_utf8_lossy(&out)
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
