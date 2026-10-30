use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use arboard::Clipboard;
use softbuffer::{Context, Surface};
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::completion::{common_prefix, CompletionEngine, CompletionSet};
use crate::config::{self, Config, Paths};
use crate::editor::Editor;
use crate::font::{Clip, Span, TextEngine};
use crate::highlight;
use crate::history::History;
use crate::keys;
use crate::pty::Pty;
use crate::render::Painter;
use crate::shell::{self, ShellKind, ShellLaunch};
use crate::term::{CursorStyle, TermEvent, TerminalSession};
use crate::theme::{Rgb, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Compose,
    Running,
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Composer,
    Grid,
}

#[derive(Clone, Copy, Debug, Default)]
struct GridLayout {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    cols: u16,
    rows: u16,
}

#[derive(Clone, Copy, Debug, Default)]
struct CardLayout {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    text_x: f32,
    text_y: f32,
}

struct SearchState {
    query: String,
    selected: usize,
    matches: Vec<String>,
}

struct CompletionState {
    set: CompletionSet,
    selected: usize,
}

struct Toast {
    text: String,
    until: Instant,
}

pub struct PromptApp {
    paths: Paths,
    config: Config,
    theme: Theme,
    launch: ShellLaunch,

    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    proxy: Option<EventLoopProxy<()>>,

    text: TextEngine,
    canvas: Option<Pixmap>,
    grid_cache: Option<Pixmap>,
    grid_dirty: bool,
    size: PhysicalSize<u32>,
    scale: f32,

    session: Option<TerminalSession>,
    pty: Option<Pty>,
    editor: Editor,
    history: History,
    completion: CompletionEngine,
    clipboard: Option<Clipboard>,

    mode: Mode,
    focus: Focus,
    cwd: PathBuf,
    cwd_label: String,
    running_cmd: String,
    running_started: Instant,
    last_status: Option<(i32, Instant)>,
    title: String,

    modifiers: ModifiersState,
    ime_composing: bool,
    ime_preedit: String,
    cursor_pos: PhysicalPosition<f64>,

    grid: GridLayout,
    card: CardLayout,
    hscroll: usize,
    composer_scroll: usize,

    history_index: Option<usize>,
    history_draft: String,
    search: Option<SearchState>,
    completion_popup: Option<CompletionState>,

    welcome: bool,
    toast: Option<Toast>,

    cursor_on: bool,
    last_blink: Instant,

    sel_anchor: Option<(u16, u16)>,
    sel_drag: bool,
    click_count: u8,
    last_click: Option<(Instant, f32, f32)>,

    integration_deadline: Instant,
    got_prompt: bool,
    exited: bool,
    quit_requested: bool,

    autotype: Option<(String, Instant)>,
    screenshot_path: Option<PathBuf>,
    screenshot_at: Option<Instant>,
    exit_at: Option<Instant>,
    last_frame: Instant,
}

impl PromptApp {
    pub fn new(proxy: EventLoopProxy<()>) -> Result<Self> {
        let paths = Paths::discover();
        let (config, warning) = config::load(&paths);
        let first_run = config::is_first_run(&paths);
        let theme = Theme::default();
        let launch = shell::prepare(&paths, &config);
        let history = History::load(paths.history_file(), 5000);
        let text = TextEngine::new(
            1.0,
            &config.appearance.font_family,
            config.appearance.font_size,
            config.appearance.line_height,
        );
        let cwd = shell::home_dir();
        let started = Instant::now();

        let autotype = std::env::var("PROMPT_AUTOTYPE").ok().map(|text| {
            (
                text,
                started
                    + Duration::from_millis(
                        std::env::var("PROMPT_AUTOTYPE_DELAY")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1200),
                    ),
            )
        });
        let screenshot_path = std::env::var_os("PROMPT_SCREENSHOT").map(PathBuf::from);
        let screenshot_at = screenshot_path.as_ref().map(|_| {
            started
                + Duration::from_millis(
                    std::env::var("PROMPT_SCREENSHOT_DELAY")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1800),
                )
        });
        let exit_at = std::env::var("PROMPT_EXIT_AFTER_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|ms| started + Duration::from_millis(ms));

        let launch_integrated = launch.integrated;
        let mut app = Self {
            paths,
            config,
            theme,
            launch,
            window: None,
            context: None,
            surface: None,
            proxy: Some(proxy),
            text,
            canvas: None,
            grid_cache: None,
            grid_dirty: true,
            size: PhysicalSize::new(0, 0),
            scale: 1.0,
            session: None,
            pty: None,
            editor: Editor::default(),
            history,
            completion: CompletionEngine::new(),
            clipboard: None,
            mode: if launch_integrated {
                Mode::Compose
            } else {
                Mode::Direct
            },
            focus: Focus::Composer,
            cwd,
            cwd_label: String::new(),
            running_cmd: String::new(),
            running_started: started,
            last_status: None,
            title: "Prompt".into(),
            modifiers: ModifiersState::empty(),
            ime_composing: false,
            ime_preedit: String::new(),
            cursor_pos: PhysicalPosition::new(0.0, 0.0),
            grid: GridLayout::default(),
            card: CardLayout::default(),
            hscroll: 0,
            composer_scroll: 0,
            history_index: None,
            history_draft: String::new(),
            search: None,
            completion_popup: None,
            welcome: first_run,
            toast: None,
            cursor_on: true,
            last_blink: started,
            sel_anchor: None,
            sel_drag: false,
            click_count: 0,
            last_click: None,
            integration_deadline: started + Duration::from_secs(3),
            got_prompt: false,
            exited: false,
            quit_requested: false,
            autotype,
            screenshot_path,
            screenshot_at,
            exit_at,
            last_frame: started,
        };
        if let Some(warning) = warning {
            app.toast(warning);
        }
        Ok(app)
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            until: Instant::now() + Duration::from_secs(2),
        });
        self.request_redraw();
    }

    fn request_redraw(&mut self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn zoom_font(&mut self, delta: f32) {
        let size = if delta == 0.0 {
            14.0
        } else {
            (self.config.appearance.font_size + delta).clamp(8.0, 48.0)
        };
        if (size - self.config.appearance.font_size).abs() < f32::EPSILON {
            return;
        }
        self.config.appearance.font_size = size;
        self.text = TextEngine::new(
            self.scale,
            &self.config.appearance.font_family,
            self.config.appearance.font_size,
            self.config.appearance.line_height,
        );
        if let Some(session) = &mut self.session {
            session.set_cell_size(
                self.text.cell_w.max(1.0) as u32,
                self.text.cell_h.max(1.0) as u32,
            );
        }
        self.relayout();
        self.grid_dirty = true;
        self.toast(format!("Font size {size:.0}"));
        self.request_redraw();
    }

    fn s(&self, logical: f32) -> f32 {
        (logical * self.scale).round()
    }

    fn init_surface(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let attrs = Window::default_attributes()
            .with_title("Prompt")
            .with_inner_size(LogicalSize::new(
                self.config.ui.window_width,
                self.config.ui.window_height,
            ))
            .with_min_inner_size(LogicalSize::new(520, 320));
        let window = Arc::new(event_loop.create_window(attrs)?);
        window.focus_window();
        self.scale = window.scale_factor() as f32;
        self.text = TextEngine::new(
            self.scale,
            &self.config.appearance.font_family,
            self.config.appearance.font_size,
            self.config.appearance.line_height,
        );
        let context = Context::new(window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let surface = Surface::new(&context, window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        window.set_ime_allowed(true);
        self.context = Some(context);
        self.surface = Some(surface);
        self.window = Some(window);
        self.size = self.window.as_ref().unwrap().inner_size();

        self.relayout();
        let cols = self.grid.cols;
        let rows = self.grid.rows;
        let cell_w = self.text.cell_w.max(1.0) as u32;
        let cell_h = self.text.cell_h.max(1.0) as u32;
        let session = TerminalSession::new(
            &self.theme,
            cols,
            rows,
            cell_w,
            cell_h,
            self.config.behavior.scrollback_lines,
        )?;
        self.session = Some(session);
        let notify: Box<dyn Fn() + Send + Sync> = match self.proxy.clone() {
            Some(proxy) => Box::new(move || {
                let _ = proxy.send_event(());
            }),
            None => Box::new(|| {}),
        };
        let pty = match Pty::spawn(
            &self.launch,
            &self.cwd,
            cols,
            rows,
            cell_w,
            cell_h,
            Some(notify),
        ) {
            Ok(pty) => pty,
            Err(first_error) => {
                let fallback = fallback_shell(&self.launch);
                self.toast(format!(
                    "Could not start {} — using {}",
                    self.launch.program, fallback.program
                ));
                self.mode = Mode::Direct;
                self.launch.kind = ShellKind::Other;
                self.launch.integrated = false;
                self.launch.program = fallback.program.clone();
                let notify: Box<dyn Fn() + Send + Sync> = match self.proxy.clone() {
                    Some(proxy) => Box::new(move || {
                        let _ = proxy.send_event(());
                    }),
                    None => Box::new(|| {}),
                };
                Pty::spawn(
                    &fallback,
                    &self.cwd,
                    cols,
                    rows,
                    cell_w,
                    cell_h,
                    Some(notify),
                )
                .map_err(|second| {
                    anyhow::anyhow!(
                        "could not start shell ({}: {first_error}; fallback {}: {second})",
                        self.launch.program,
                        fallback.program
                    )
                })?
            }
        };
        self.pty = Some(pty);
        self.integration_deadline = Instant::now() + Duration::from_secs(4);
        if self.config.ui.start_maximized {
            if let Some(window) = &self.window {
                window.set_maximized(true);
            }
        }
        config::mark_started(&self.paths);
        self.request_redraw();
        Ok(())
    }

    fn relayout(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        self.size = window.inner_size();
        let w = self.size.width as f32;
        let h = self.size.height as f32;
        if w < 2.0 || h < 2.0 {
            return;
        }
        let scale = window.scale_factor() as f32;
        if (scale - self.scale).abs() > 0.001 {
            self.scale = scale;
            self.text = TextEngine::new(
                scale,
                &self.config.appearance.font_family,
                self.config.appearance.font_size,
                self.config.appearance.line_height,
            );
            if let Some(session) = &mut self.session {
                session.set_cell_size(
                    self.text.cell_w.max(1.0) as u32,
                    self.text.cell_h.max(1.0) as u32,
                );
            }
        }

        let margin = self.s(14.0);
        let padding = self.s(10.0);
        let gap = self.s(10.0);
        let hint_h = if self.config.ui.show_hints {
            self.s(18.0)
        } else {
            0.0
        };
        let base_card = self.s(12.0) * 2.0 + self.text.cell_h + self.s(6.0) + hint_h;

        let grid_x = margin + padding;
        let grid_y = margin + padding;
        let grid_w = (w - 2.0 * grid_x).max(1.0);
        let grid_h = (h - margin - base_card - gap - grid_y).max(1.0);
        let cols = ((grid_w / self.text.cell_w).floor() as u16).max(20);
        let rows = ((grid_h / self.text.cell_h).floor() as u16).max(5);

        self.grid = GridLayout {
            x: grid_x,
            y: grid_y,
            w: cols as f32 * self.text.cell_w,
            h: rows as f32 * self.text.cell_h,
            cols,
            rows,
        };
        self.card = CardLayout {
            x: margin,
            w: (w - margin * 2.0).max(1.0),
            h: base_card,
            y: h - margin - base_card,
            ..CardLayout::default()
        };

        let changed = self
            .session
            .as_ref()
            .map(|s| s.cols != cols || s.rows != rows)
            .unwrap_or(false);
        if changed {
            self.grid_dirty = true;
            if let Some(session) = &mut self.session {
                session.resize(
                    cols,
                    rows,
                    self.text.cell_w.max(1.0) as u32,
                    self.text.cell_h.max(1.0) as u32,
                );
            }
            if let Some(pty) = &self.pty {
                pty.resize(
                    cols,
                    rows,
                    self.text.cell_w.max(1.0) as u32,
                    self.text.cell_h.max(1.0) as u32,
                );
            }
        }
        self.grid_dirty = true;
        self.request_redraw();
    }

    // ---- event processing -------------------------------------------------

    fn drain_pty(&mut self) {
        let Some(pty) = &mut self.pty else { return };
        let data = pty.drain();
        if !data.is_empty() {
            if let Some(session) = &mut self.session {
                let was_at_bottom = session.frame().scrollbar.at_bottom();
                session.write(&data);
                if was_at_bottom {
                    session.scroll_to_bottom();
                }
                let replies = session.take_pty_replies();
                if !replies.is_empty() {
                    pty.write(&replies);
                }
                self.grid_dirty = true;
                self.after_terminal_change();
            }
        }
    }

    fn after_terminal_change(&mut self) {
        let (events, mut pwd, mut title) = {
            let Some(session) = &mut self.session else {
                return;
            };
            (session.drain_events(), session.pwd(), session.title())
        };
        for event in events {
            match event {
                TermEvent::PromptStart => {
                    self.got_prompt = true;
                    self.mode = Mode::Compose;
                    self.focus = Focus::Composer;
                    self.sel_anchor = None;
                    self.sel_drag = false;
                    self.history_index = None;
                    self.completion_popup = None;
                    self.editor.clear_selection();
                }
                TermEvent::InputStart => {}
                TermEvent::OutputStart { command } => {
                    if self.mode == Mode::Compose {
                        self.mode = Mode::Running;
                    }
                    self.running_started = Instant::now();
                    if !command.is_empty() {
                        self.running_cmd = command;
                    }
                }
                TermEvent::CommandEnd { exit_code } => {
                    if let Some(code) = exit_code {
                        self.last_status = Some((code, Instant::now()));
                    }
                }
                TermEvent::Bell => {
                    self.toast("Bell");
                }
                TermEvent::TitleChanged | TermEvent::PwdChanged => {}
            }
        }
        if let Some(session) = &self.session {
            pwd = session.pwd().or(pwd);
            title = session.title().or(title);
        }
        if let Some(pwd) = pwd {
            let path = pwd_to_path(&pwd);
            if path.is_dir() {
                self.cwd = path;
                self.cwd_label.clear();
            }
        }
        if let Some(title) = title {
            let title = title.trim();
            if !title.is_empty() && title != self.title {
                self.title = title.to_string();
                if let Some(window) = &self.window {
                    window.set_title(&format!("{} — Prompt", self.title));
                }
            }
        }
        self.grid_dirty = true;
        self.request_redraw();
    }

    fn check_exit(&mut self, event_loop: &ActiveEventLoop) {
        let Some(pty) = &mut self.pty else { return };
        if let Some(code) = pty.try_exit_code() {
            if !self.exited {
                self.exited = true;
                self.toast(format!("Shell exited ({code})"));
                event_loop.exit();
            }
        }
    }

    fn update_timers(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now.duration_since(self.last_blink) >= Duration::from_millis(530) {
            self.cursor_on = !self.cursor_on;
            self.last_blink = now;
            if self.mode == Mode::Running || self.mode == Mode::Direct {
                self.grid_dirty = true;
            }
            if self.focus == Focus::Composer || self.mode == Mode::Running {
                self.request_redraw();
            }
        }

        if !self.got_prompt && now > self.integration_deadline && self.mode == Mode::Compose {
            self.mode = Mode::Direct;
            self.toast("Shell integration not detected — using classic input");
            self.request_redraw();
        }

        if let Some((_, at)) = &self.autotype {
            if now >= *at {
                let command = self.autotype.take().map(|(cmd, _)| cmd).unwrap_or_default();
                if self.mode == Mode::Compose {
                    self.welcome = false;
                    self.editor.set_text(&command);
                    self.submit();
                }
            }
        }

        if let Some(at) = self.screenshot_at {
            if now >= at {
                self.screenshot_at = None;
                if let Some(path) = self.screenshot_path.clone() {
                    self.save_screenshot(&path);
                }
                if std::env::var("PROMPT_EXIT_AFTER_SCREENSHOT").as_deref() == Ok("1") {
                    event_loop.exit();
                }
            }
        }

        if let Some(at) = self.exit_at {
            if now >= at {
                event_loop.exit();
            }
        }

        let mut next = now + Duration::from_millis(250);
        if self.cursor_on || self.focus == Focus::Composer {
            next = next.min(self.last_blink + Duration::from_millis(530));
        }
        if self.mode == Mode::Running {
            next = next.min(now + Duration::from_millis(500));
        }
        if let Some((_, at)) = &self.autotype {
            next = next.min(*at);
        }
        if let Some(at) = self.screenshot_at {
            next = next.min(at);
        }
        if let Some(at) = self.exit_at {
            next = next.min(at);
        }
        if let Some(toast) = &self.toast {
            next = next.min(toast.until);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }

    fn save_screenshot(&self, path: &Path) {
        if let Some(canvas) = &self.canvas {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = canvas.save_png(path);
        }
    }

    // ---- keyboard ---------------------------------------------------------

    fn handle_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        if self.welcome {
            self.welcome = false;
            self.request_redraw();
        }

        let mods = self.modifiers;
        let primary = if keys::is_macos() {
            mods.super_key()
        } else {
            mods.control_key()
        };
        let shift = mods.shift_key();
        let alt = mods.alt_key();
        let ctrl = mods.control_key();

        if let PhysicalKey::Code(code) = event.physical_key {
            if code == KeyCode::F11 || (keys::is_macos() && primary && code == KeyCode::KeyF) {
                if let Some(window) = &self.window {
                    let fullscreen = window.fullscreen().is_some();
                    window.set_fullscreen(if fullscreen {
                        None
                    } else {
                        Some(winit::window::Fullscreen::Borderless(None))
                    });
                }
                return;
            }
            if shift && matches!(code, KeyCode::PageUp | KeyCode::PageDown) {
                if let Some(session) = &mut self.session {
                    let delta = if code == KeyCode::PageUp {
                        -(self.grid.rows as i32 / 2).max(3)
                    } else {
                        (self.grid.rows as i32 / 2).max(3)
                    };
                    session.scroll(delta);
                    session.update_frame();
                }
                self.grid_dirty = true;
                self.request_redraw();
                return;
            }
            if primary
                && matches!(
                    code,
                    KeyCode::Equal | KeyCode::NumpadAdd | KeyCode::Minus | KeyCode::NumpadSubtract
                )
            {
                let delta = if matches!(code, KeyCode::Equal | KeyCode::NumpadAdd) {
                    1.0
                } else {
                    -1.0
                };
                self.zoom_font(delta);
                return;
            }
            if primary && code == KeyCode::Digit0 {
                self.zoom_font(0.0);
                return;
            }

            // Popups own the keyboard while they are open.
            if self.search.is_some() {
                if self.handle_search_key(code, mods, event) {
                    return;
                }
            }
            if self.completion_popup.is_some() {
                if self.handle_completion_key(code, shift) {
                    return;
                }
            }

            if self.mode == Mode::Compose {
                if self.handle_compose_key(code, mods, primary, shift, ctrl, alt, event) {
                    return;
                }
            }

            if self.mode == Mode::Running || self.mode == Mode::Direct {
                // Application-level shortcuts still work while a program runs.
                // On Linux, Ctrl+C/Ctrl+V belong to the program; copy/paste use
                // Ctrl+Shift+C/V instead.
                if keys::is_macos() && primary && code == KeyCode::KeyC {
                    if self.copy_selection() {
                        return;
                    }
                }
                if keys::is_macos() && primary && code == KeyCode::KeyV {
                    self.paste_to_terminal();
                    return;
                }
                if ctrl && shift && code == KeyCode::KeyC {
                    if self.copy_selection() {
                        return;
                    }
                }
                if ctrl && shift && code == KeyCode::KeyV {
                    self.paste_to_terminal();
                    return;
                }
                if shift && code == KeyCode::Insert {
                    self.paste_to_terminal();
                    return;
                }
                if ctrl && code == KeyCode::Insert {
                    self.copy_selection();
                    return;
                }
                self.forward_key(event, code);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_compose_key(
        &mut self,
        code: KeyCode,
        mods: ModifiersState,
        primary: bool,
        shift: bool,
        ctrl: bool,
        alt: bool,
        event: &KeyEvent,
    ) -> bool {
        // Clipboard and editor shortcuts.
        if primary && !shift {
            match code {
                KeyCode::KeyC => {
                    if self.copy_selection() {
                        return true;
                    }
                }
                KeyCode::KeyV => {
                    self.paste_composer();
                    return true;
                }
                KeyCode::KeyX => {
                    self.cut_selection();
                    return true;
                }
                KeyCode::KeyA => {
                    if self.focus == Focus::Grid {
                        if let Some(session) = &mut self.session {
                            session.select_all();
                        }
                        self.grid_dirty = true;
                    } else {
                        self.editor.select_all();
                        self.focus = Focus::Composer;
                    }
                    self.request_redraw();
                    return true;
                }
                KeyCode::KeyZ => {
                    self.editor.undo();
                    self.history_index = None;
                    self.request_redraw();
                    return true;
                }
                KeyCode::KeyR => {
                    self.open_search();
                    return true;
                }
                KeyCode::KeyK if keys::is_macos() => {
                    self.send_bytes(b"\x0c");
                    return true;
                }
                _ => {}
            }
        }
        if primary && shift && code == KeyCode::KeyZ {
            self.editor.redo();
            self.request_redraw();
            return true;
        }
        if ctrl && shift && code == KeyCode::KeyC {
            self.copy_selection();
            return true;
        }
        if ctrl && shift && code == KeyCode::KeyV {
            self.paste_composer();
            return true;
        }
        if shift && code == KeyCode::Insert {
            self.paste_composer();
            return true;
        }
        if ctrl && code == KeyCode::Insert {
            self.copy_selection();
            return true;
        }
        if keys::is_macos() && primary && code == KeyCode::KeyQ {
            self.quit_requested = true;
            return true;
        }

        // Enter is special: submit, or newline with Shift.
        if code == KeyCode::Enter || code == KeyCode::NumpadEnter {
            if self.completion_popup.is_some() {
                self.accept_completion();
                return true;
            }
            if shift {
                self.editor.newline();
                self.history_index = None;
                self.request_redraw();
            } else {
                self.submit();
            }
            return true;
        }

        if code == KeyCode::Escape {
            if self.search.is_some() {
                self.search = None;
            } else if self.completion_popup.is_some() {
                self.completion_popup = None;
            } else if self.focus == Focus::Grid
                && self
                    .session
                    .as_ref()
                    .map(|s| s.has_selection())
                    .unwrap_or(false)
            {
                if let Some(session) = &mut self.session {
                    session.clear_selection();
                }
                self.focus = Focus::Composer;
                self.grid_dirty = true;
            } else if self.editor.has_selection() {
                self.editor.clear_selection();
            } else if !self.editor.is_empty() {
                self.editor.clear();
            }
            self.request_redraw();
            return true;
        }

        if code == KeyCode::Tab {
            if shift {
                self.cycle_completion(true);
            } else {
                self.tab_complete();
            }
            return true;
        }

        // History navigation.
        if code == KeyCode::ArrowUp && self.completion_popup.is_none() {
            let (line, _) = self.editor.caret_line_col();
            if line == 0 {
                self.history_up();
                return true;
            }
        }
        if code == KeyCode::ArrowDown && self.completion_popup.is_none() {
            let (line, _) = self.editor.caret_line_col();
            if line + 1 >= self.editor.line_count() {
                self.history_down();
                return true;
            }
        }

        let extend = shift;
        let word_mod = alt || (ctrl && !keys::is_macos());
        let line_mod = primary || (ctrl && keys::is_macos());

        match code {
            KeyCode::ArrowLeft => {
                if word_mod {
                    self.editor.move_word_left(extend);
                } else if line_mod {
                    self.editor.move_line_start(extend);
                } else {
                    self.editor.move_left(extend);
                }
                self.after_editor_motion();
                return true;
            }
            KeyCode::ArrowRight => {
                if word_mod {
                    self.editor.move_word_right(extend);
                } else if line_mod {
                    self.editor.move_line_end(extend);
                } else {
                    self.editor.move_right(extend);
                }
                self.after_editor_motion();
                return true;
            }
            KeyCode::ArrowUp => {
                self.editor.move_up(extend);
                self.after_editor_motion();
                return true;
            }
            KeyCode::ArrowDown => {
                self.editor.move_down(extend);
                self.after_editor_motion();
                return true;
            }
            KeyCode::Home => {
                if primary || ctrl {
                    self.editor.move_buffer_start(extend);
                } else {
                    self.editor.move_line_start(extend);
                }
                self.after_editor_motion();
                return true;
            }
            KeyCode::End => {
                if primary || ctrl {
                    self.editor.move_buffer_end(extend);
                } else {
                    self.editor.move_line_end(extend);
                }
                self.after_editor_motion();
                return true;
            }
            KeyCode::Backspace => {
                if keys::is_macos() && primary {
                    self.editor.delete_to_line_start();
                } else if word_mod {
                    self.editor.delete_word_back();
                } else {
                    self.editor.backspace();
                }
                self.after_edit();
                return true;
            }
            KeyCode::Delete => {
                if keys::is_macos() && primary {
                    self.editor.delete_to_line_end();
                } else if word_mod {
                    self.editor.delete_word_forward();
                } else {
                    self.editor.delete_forward();
                }
                self.after_edit();
                return true;
            }
            KeyCode::KeyK if ctrl => {
                self.editor.delete_to_line_end();
                self.after_edit();
                return true;
            }
            KeyCode::KeyU if ctrl => {
                self.editor.delete_to_line_start();
                self.after_edit();
                return true;
            }
            KeyCode::KeyW if ctrl => {
                self.editor.delete_word_back();
                self.after_edit();
                return true;
            }
            _ => {}
        }

        if ctrl {
            match code {
                KeyCode::KeyC => {
                    if self.editor.has_selection() {
                        self.copy_selection();
                    } else {
                        self.editor.clear();
                        self.send_bytes(b"\x03");
                        self.history_index = None;
                    }
                    return true;
                }
                KeyCode::KeyD => {
                    if self.editor.is_empty() {
                        self.send_bytes(b"\x04");
                    } else {
                        self.editor.delete_forward();
                        self.after_edit();
                    }
                    return true;
                }
                KeyCode::KeyL => {
                    self.send_bytes(b"\x0c");
                    return true;
                }
                KeyCode::KeyR => {
                    self.open_search();
                    return true;
                }
                KeyCode::KeyA => {
                    self.editor.select_all();
                    self.request_redraw();
                    return true;
                }
                _ => {}
            }
        }

        // Text input. Option+key on macOS and AltGr on Linux both arrive as
        // text and must be inserted; ordinary Ctrl chords must not.
        let altgr = ctrl && alt;
        if (!ctrl || altgr) && !(keys::is_macos() && primary) {
            if let Some(text) = event.text.as_ref() {
                if !text.is_empty() && !text.chars().any(|c| c.is_control()) {
                    let cleaned = text.replace("\r\n", "\n").replace('\r', "");
                    self.editor.insert(&cleaned);
                    self.history_index = None;
                    self.completion_popup = None;
                    self.focus = Focus::Composer;
                    self.after_edit();
                    return true;
                }
            }
        }
        let _ = mods;
        false
    }

    fn after_edit(&mut self) {
        self.focus = Focus::Composer;
        self.completion_popup = None;
        self.cursor_on = true;
        self.last_blink = Instant::now();
        if let Some(session) = &mut self.session {
            if session.has_selection() {
                session.clear_selection();
                self.grid_dirty = true;
            }
        }
        if let Some(session) = &mut self.session {
            if !session.frame().scrollbar.at_bottom() {
                session.scroll_to_bottom();
                session.update_frame();
                self.grid_dirty = true;
            }
        }
        self.composer_scroll_to_caret();
        self.request_redraw();
    }

    fn after_editor_motion(&mut self) {
        self.focus = Focus::Composer;
        self.cursor_on = true;
        self.last_blink = Instant::now();
        self.composer_scroll_to_caret();
        self.request_redraw();
    }

    fn composer_scroll_to_caret(&mut self) {
        let (line, _) = self.editor.caret_line_col();
        let max_lines = 10usize;
        if line < self.composer_scroll {
            self.composer_scroll = line;
        } else if line >= self.composer_scroll + max_lines {
            self.composer_scroll = line + 1 - max_lines;
        }
    }

    fn forward_key(&mut self, event: &KeyEvent, code: KeyCode) {
        let text = event.text.as_deref();
        let bytes = match &mut self.session {
            Some(session) => keys::encode(session, code, self.modifiers, text),
            None => Vec::new(),
        };
        if !bytes.is_empty() {
            self.write_pty(&bytes);
        }
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        if let Some(pty) = &mut self.pty {
            pty.write(bytes);
        }
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        self.write_pty(bytes);
    }

    // ---- submitting -------------------------------------------------------

    fn submit(&mut self) {
        let text = self.editor.text().to_string();
        if text.trim().is_empty() {
            self.editor.clear();
            self.send_bytes(b"\r");
            self.mode = Mode::Running;
            self.running_cmd.clear();
            self.running_started = Instant::now();
            self.hscroll = 0;
            self.request_redraw();
            return;
        }
        self.history.add(&text);
        self.history_index = None;
        self.running_cmd = text.clone();
        self.running_started = Instant::now();
        self.mode = Mode::Running;
        self.last_status = None;

        let bracketed = self
            .session
            .as_ref()
            .map(|s| s.bracketed_paste())
            .unwrap_or(false);
        let needs_bracketed = text.contains('\n')
            || text
                .chars()
                .any(|c| c.is_control() && c != '\t' && c != '\n');
        let mut payload = Vec::new();
        if needs_bracketed && bracketed {
            payload.extend_from_slice(b"\x1b[200~");
            payload.extend_from_slice(text.as_bytes());
            payload.extend_from_slice(b"\x1b[201~");
        } else {
            payload.extend_from_slice(text.as_bytes());
        }
        payload.push(b'\r');
        self.send_bytes(&payload);

        if let Some(session) = &mut self.session {
            session.clear_selection();
            session.scroll_to_bottom();
        }
        self.grid_dirty = true;
        self.editor.clear();
        self.hscroll = 0;
        self.composer_scroll = 0;
        self.sel_anchor = None;
        self.sel_drag = false;
        self.toast = None;
        self.request_redraw();
    }

    fn paste_composer(&mut self) {
        let Some(text) = self.read_clipboard() else {
            return;
        };
        let cleaned = normalize_paste(&text);
        if cleaned.is_empty() {
            return;
        }
        self.editor.insert(&cleaned);
        self.history_index = None;
        self.completion_popup = None;
        self.focus = Focus::Composer;
        self.after_edit();
    }

    fn paste_to_terminal(&mut self) {
        let Some(text) = self.read_clipboard() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let bracketed = self
            .session
            .as_ref()
            .map(|s| s.bracketed_paste())
            .unwrap_or(false);
        let mut payload = Vec::new();
        if bracketed {
            payload.extend_from_slice(b"\x1b[200~");
            payload.extend_from_slice(text.as_bytes());
            payload.extend_from_slice(b"\x1b[201~");
        } else {
            payload.extend_from_slice(text.as_bytes());
        }
        self.send_bytes(&payload);
    }

    fn read_clipboard(&mut self) -> Option<String> {
        if self.clipboard.is_none() {
            self.clipboard = Clipboard::new().ok();
        }
        self.clipboard.as_mut().and_then(|c| c.get_text().ok())
    }

    fn write_clipboard(&mut self, text: String) {
        if self.clipboard.is_none() {
            self.clipboard = Clipboard::new().ok();
        }
        if let Some(clipboard) = &mut self.clipboard {
            if clipboard.set_text(text).is_ok() {
                self.toast("Copied");
            }
        }
    }

    fn copy_selection(&mut self) -> bool {
        if self.focus == Focus::Grid
            || (self.focus == Focus::Composer && !self.editor.has_selection())
        {
            if let Some(session) = &self.session {
                if session.has_selection() {
                    if let Some(text) = session.selection_text() {
                        if !text.is_empty() {
                            self.write_clipboard(text);
                            return true;
                        }
                    }
                }
            }
        }
        if let Some(text) = self.editor.selected_text().map(str::to_string) {
            if !text.is_empty() {
                self.write_clipboard(text);
                return true;
            }
        }
        false
    }

    fn cut_selection(&mut self) {
        if let Some(text) = self.editor.selected_text().map(str::to_string) {
            if !text.is_empty() {
                self.write_clipboard(text);
                self.editor.backspace();
                self.after_edit();
            }
        }
    }

    // ---- history ----------------------------------------------------------

    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_index {
            Some(0) => 0,
            Some(i) => i - 1,
            None => {
                self.history_draft = self.editor.text().to_string();
                self.history.len() - 1
            }
        };
        self.history_index = Some(next);
        if let Some(cmd) = self.history.get(next) {
            let cmd = cmd.to_string();
            self.editor.set_text(&cmd);
            self.editor.move_buffer_end(false);
        }
        self.after_editor_motion();
    }

    fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            let next = index + 1;
            self.history_index = Some(next);
            if let Some(cmd) = self.history.get(next) {
                let cmd = cmd.to_string();
                self.editor.set_text(&cmd);
                self.editor.move_buffer_end(false);
            }
        } else {
            self.history_index = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.editor.set_text(&draft);
            self.editor.move_buffer_end(false);
        }
        self.after_editor_motion();
    }

    fn open_search(&mut self) {
        let mut search = SearchState {
            query: String::new(),
            selected: 0,
            matches: Vec::new(),
        };
        search.matches = self
            .history
            .search("", 50)
            .into_iter()
            .map(str::to_string)
            .collect();
        self.search = Some(search);
        self.completion_popup = None;
        self.request_redraw();
    }

    fn refresh_search(&mut self) {
        let query = self
            .search
            .as_ref()
            .map(|s| s.query.clone())
            .unwrap_or_default();
        let matches: Vec<String> = self
            .history
            .search(&query, 50)
            .into_iter()
            .map(str::to_string)
            .collect();
        if let Some(search) = &mut self.search {
            search.matches = matches;
            search.selected = search.selected.min(search.matches.len().saturating_sub(1));
        }
    }

    fn handle_search_key(&mut self, code: KeyCode, mods: ModifiersState, event: &KeyEvent) -> bool {
        let shift = mods.shift_key();
        match code {
            KeyCode::Escape => {
                self.search = None;
                self.request_redraw();
                return true;
            }
            KeyCode::Enter => {
                if let Some(search) = &self.search {
                    if let Some(cmd) = search.matches.get(search.selected) {
                        let cmd = cmd.clone();
                        self.editor.set_text(&cmd);
                        self.editor.move_buffer_end(false);
                    }
                }
                self.search = None;
                self.after_editor_motion();
                return true;
            }
            KeyCode::ArrowUp => {
                if let Some(search) = &mut self.search {
                    search.selected = search.selected.saturating_sub(1);
                }
                self.request_redraw();
                return true;
            }
            KeyCode::ArrowDown => {
                if let Some(search) = &mut self.search {
                    if !search.matches.is_empty() {
                        search.selected = (search.selected + 1).min(search.matches.len() - 1);
                    }
                }
                self.request_redraw();
                return true;
            }
            KeyCode::Backspace => {
                if let Some(search) = &mut self.search {
                    search.query.pop();
                }
                self.refresh_search();
                self.request_redraw();
                return true;
            }
            _ => {
                if !mods.control_key() && !mods.super_key() {
                    if let Some(text) = event.text.as_ref() {
                        if let Some(search) = &mut self.search {
                            search.query.push_str(text);
                        }
                        self.refresh_search();
                        self.request_redraw();
                        return true;
                    }
                }
                if mods.control_key() && code == KeyCode::KeyR {
                    if let Some(search) = &mut self.search {
                        if !search.matches.is_empty() {
                            search.selected = (search.selected + 1) % search.matches.len();
                        }
                    }
                    self.request_redraw();
                    return true;
                }
                let _ = shift;
                true
            }
        }
    }

    // ---- completion -------------------------------------------------------

    fn tab_complete(&mut self) {
        if self.completion_popup.is_some() {
            self.cycle_completion(false);
            return;
        }
        let text = self.editor.text().to_string();
        let caret = self.editor.caret();
        let Some(set) = self
            .completion
            .complete(&text, caret, &self.cwd, self.history.entries())
        else {
            return;
        };
        if set.items.len() == 1 {
            self.apply_completion(&set, 0);
            return;
        }
        let prefix = common_prefix(&set.items);
        let mut set = set;
        if prefix.len() > set.token.len() {
            self.editor.replace_range(set.start, set.end, &prefix);
            set.end = set.start + prefix.len();
            set.token = prefix.clone();
            self.after_edit();
        }
        self.completion_popup = Some(CompletionState { set, selected: 0 });
        self.request_redraw();
    }

    fn cycle_completion(&mut self, backwards: bool) {
        let Some(popup) = &mut self.completion_popup else {
            if backwards {
                self.tab_complete();
            }
            return;
        };
        let len = popup.set.items.len();
        if len == 0 {
            return;
        }
        if backwards {
            popup.selected = (popup.selected + len - 1) % len;
        } else {
            popup.selected = (popup.selected + 1) % len;
        }
        self.request_redraw();
    }

    fn handle_completion_key(&mut self, code: KeyCode, shift: bool) -> bool {
        match code {
            KeyCode::Escape => {
                self.completion_popup = None;
                self.request_redraw();
                true
            }
            KeyCode::Tab => {
                self.cycle_completion(shift);
                true
            }
            KeyCode::ArrowUp => {
                self.cycle_completion(true);
                true
            }
            KeyCode::ArrowDown => {
                self.cycle_completion(false);
                true
            }
            KeyCode::Enter => {
                self.accept_completion();
                true
            }
            _ => false,
        }
    }

    fn accept_completion(&mut self) {
        let Some(popup) = self.completion_popup.take() else {
            return;
        };
        self.apply_completion(&popup.set, popup.selected);
    }

    fn apply_completion(&mut self, set: &CompletionSet, index: usize) {
        let Some(item) = set.items.get(index) else {
            return;
        };
        let mut value = item.value.clone();
        let single_command = set.command && set.items.len() == 1;
        self.editor.replace_range(set.start, set.end, &value);
        if single_command && !value.ends_with(' ') && !value.ends_with('/') {
            value.push(' ');
            self.editor.set_caret(set.start + value.len());
        }
        self.completion_popup = None;
        self.after_edit();
    }

    // ---- mouse ------------------------------------------------------------

    fn handle_mouse_button(
        &mut self,
        state: ElementState,
        button: MouseButton,
        pos: PhysicalPosition<f64>,
    ) {
        let x = pos.x as f32;
        let y = pos.y as f32;
        if state == ElementState::Pressed {
            // A click outside a popup is a dismissal in every editor.
            self.completion_popup = None;
            self.search = None;
        }
        if self.mouse_in_grid(x, y) {
            self.focus = Focus::Grid;
            if self.should_forward_mouse(button) {
                self.forward_mouse(button, state, x, y, false);
                return;
            }
            if state == ElementState::Pressed {
                let Some((cx, cy)) = self.viewport_cell(x, y) else {
                    return;
                };
                self.click_count = self.next_click_count(x, y);
                self.editor.clear_selection();
                if self.click_count == 2 {
                    if let Some(session) = &mut self.session {
                        session.select_word(cx, cy);
                    }
                } else if self.click_count >= 3 {
                    if let Some(session) = &mut self.session {
                        session.select_line(cx, cy);
                    }
                } else if let Some(session) = &mut self.session {
                    self.sel_anchor = Some((cx, cy));
                    session.set_selection(Some((cx, cy)), (cx, cy));
                }
                self.grid_dirty = true;
                self.sel_drag = true;
                self.grid_dirty = true;
                self.request_redraw();
            } else {
                self.sel_drag = false;
                if self.config.behavior.copy_on_select {
                    self.copy_selection();
                }
                self.request_redraw();
            }
        } else if self.mouse_in_card(x, y) {
            self.focus = Focus::Composer;
            if let Some(session) = &mut self.session {
                session.clear_selection();
            }
            self.grid_dirty = true;
            if state == ElementState::Pressed {
                let count = self.next_click_count(x, y);
                if let Some(index) = self.card_index(x, y) {
                    if count == 2 {
                        self.editor.select_word_at(index);
                    } else if count >= 3 {
                        self.editor.select_line_at(index);
                    } else {
                        let (line, col) = self.card_line_col(x, y);
                        let extend = self.modifiers.shift_key();
                        self.editor.index_for_line_col(line, col, extend);
                    }
                    self.sel_drag = true;
                    self.history_index = None;
                }
            } else {
                self.sel_drag = false;
            }
            self.request_redraw();
        }
    }

    fn should_forward_mouse(&self, _button: MouseButton) -> bool {
        if self.modifiers.shift_key() {
            return false;
        }
        self.session
            .as_ref()
            .map(|s| s.mouse_tracking() && self.mode != Mode::Compose)
            .unwrap_or(false)
    }

    fn handle_cursor_moved(&mut self, pos: PhysicalPosition<f64>) {
        let x = pos.x as f32;
        let y = pos.y as f32;
        if self.sel_drag {
            if self.mouse_in_grid(x, y) {
                if let Some((cx, cy)) = self.viewport_cell(x, y) {
                    if let Some(anchor) = self.sel_anchor {
                        if let Some(session) = &mut self.session {
                            session.set_selection(Some(anchor), (cx, cy));
                        }
                    }
                }
                self.grid_dirty = true;
            } else if self.mouse_in_card(x, y) {
                if let Some(index) = self.card_index(x, y) {
                    self.editor.drag_to_index(index);
                }
            }
            self.request_redraw();
        } else if self.mouse_in_card(x, y) || self.mouse_in_grid(x, y) {
            if let Some(window) = &self.window {
                window.set_cursor(if self.mouse_in_card(x, y) {
                    winit::window::CursorIcon::Text
                } else {
                    winit::window::CursorIcon::Default
                });
            }
        }
    }

    fn handle_wheel(&mut self, delta: MouseScrollDelta, pos: PhysicalPosition<f64>) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => -y as f32 * 3.0,
            MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / self.text.cell_h,
        };
        let x = pos.x as f32;
        let y = pos.y as f32;
        if self.mouse_in_grid(x, y) {
            if self.should_forward_mouse(MouseButton::Left) && self.mode != Mode::Compose {
                self.forward_wheel(delta, x, y);
                return;
            }
            if let Some(session) = &mut self.session {
                session.scroll(lines.round() as i32);
                session.update_frame();
            }
            self.grid_dirty = true;
            self.request_redraw();
        }
    }

    fn next_click_count(&mut self, x: f32, y: f32) -> u8 {
        let now = Instant::now();
        let count = match &self.last_click {
            Some((at, lx, ly))
                if now.duration_since(*at) < Duration::from_millis(420)
                    && (lx - x).abs() < 6.0
                    && (ly - y).abs() < 6.0 =>
            {
                (self.click_count + 1).min(3)
            }
            _ => 1,
        };
        self.click_count = count;
        self.last_click = Some((now, x, y));
        count
    }

    fn mouse_in_grid(&self, x: f32, y: f32) -> bool {
        x >= self.grid.x
            && y >= self.grid.y
            && x < self.grid.x + self.grid.w
            && y < self.grid.y + self.grid.h
    }

    fn mouse_in_card(&self, x: f32, y: f32) -> bool {
        let card = self.card;
        x >= card.x && x < card.x + card.w && y >= card.y && y < card.y + card.h
    }

    fn viewport_cell(&self, x: f32, y: f32) -> Option<(u16, u16)> {
        if !self.mouse_in_grid(x, y) {
            return None;
        }
        let cx = ((x - self.grid.x) / self.text.cell_w).floor() as i32;
        let cy = ((y - self.grid.y) / self.text.cell_h).floor() as i32;
        if cx < 0 || cy < 0 || cx >= self.grid.cols as i32 || cy >= self.grid.rows as i32 {
            return None;
        }
        Some((cx as u16, cy as u16))
    }

    fn card_line_col(&self, x: f32, y: f32) -> (usize, usize) {
        let line = ((y - self.card.text_y) / self.text.cell_h).floor().max(0.0) as usize
            + self.composer_scroll;
        let col =
            ((x - self.card.text_x) / self.text.cell_w).floor().max(0.0) as usize + self.hscroll;
        (line, col)
    }

    fn card_index(&self, x: f32, y: f32) -> Option<usize> {
        let (line, col) = self.card_line_col(x, y);
        if line >= self.editor.line_count() {
            return None;
        }
        Some(self.editor.index_at_line_col(line, col))
    }

    fn forward_mouse(
        &mut self,
        button: MouseButton,
        state: ElementState,
        x: f32,
        y: f32,
        _drag: bool,
    ) {
        let Some((cx, cy)) = self.viewport_cell(x, y) else {
            return;
        };
        let code = match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        let sgr = self
            .session
            .as_ref()
            .map(|s| s.sgr_mouse())
            .unwrap_or(false);
        let mut out = String::new();
        if sgr {
            let final_char = if state == ElementState::Pressed {
                'M'
            } else {
                'm'
            };
            out.push_str(&format!(
                "\x1b[<{};{};{}{}",
                code,
                cx + 1,
                cy + 1,
                final_char
            ));
        } else {
            let b = 32
                + code as u8
                + if state == ElementState::Released {
                    3
                } else {
                    0
                };
            out.push('\x1b');
            out.push('[');
            out.push('M');
            out.push((b + 32) as u8 as char);
            out.push((cx + 1 + 32) as u8 as char);
            out.push((cy + 1 + 32) as u8 as char);
        }
        self.send_bytes(out.as_bytes());
    }

    fn forward_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) {
        let Some((cx, cy)) = self.viewport_cell(x, y) else {
            return;
        };
        let (button, up) = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                if y > 0.0 {
                    (64, true)
                } else {
                    (65, false)
                }
            }
            MouseScrollDelta::PixelDelta(p) => {
                if p.y < 0.0 {
                    (64, true)
                } else {
                    (65, false)
                }
            }
        };
        let _ = up;
        let sgr = self
            .session
            .as_ref()
            .map(|s| s.sgr_mouse())
            .unwrap_or(false);
        let out = if sgr {
            format!("\x1b[<{};{};{}M", button, cx + 1, cy + 1)
        } else {
            format!(
                "\x1b[M{}{}{}",
                (32 + button) as u8 as char,
                (cx + 1 + 32) as u8 as char,
                (cy + 1 + 32) as u8 as char
            )
        };
        self.send_bytes(out.as_bytes());
    }

    // ---- rendering --------------------------------------------------------

    fn render(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(surface) = &mut self.surface else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let (Some(w), Some(h)) = (
            std::num::NonZeroU32::new(size.width),
            std::num::NonZeroU32::new(size.height),
        ) else {
            return;
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        self.size = size;
        let scale = window.scale_factor() as f32;
        if (scale - self.scale).abs() > 0.001 {
            self.relayout();
        }
        let size_changed = self
            .canvas
            .as_ref()
            .map(|c| c.width() != size.width || c.height() != size.height)
            .unwrap_or(true);
        if size_changed {
            self.canvas = Pixmap::new(size.width, size.height);
            self.grid_cache = Pixmap::new(size.width, size.height);
            self.grid_dirty = true;
        }
        if let Some(canvas) = &mut self.canvas {
            canvas.fill(self.theme.bg.solid());
        } else {
            return;
        }

        if let Some(session) = &mut self.session {
            session.update_frame();
        }
        if self.grid_dirty {
            if let Some(cache) = &mut self.grid_cache {
                cache.fill(self.theme.bg.solid());
            }
            self.draw_grid();
            self.grid_dirty = false;
        }
        if let (Some(canvas), Some(cache)) = (&mut self.canvas, &self.grid_cache) {
            canvas.data_mut().copy_from_slice(cache.data());
        }
        self.draw_card();
        self.draw_welcome();
        self.draw_search();
        self.draw_completion();
        self.draw_toast();

        let Some(canvas) = self.canvas.as_ref() else {
            return;
        };
        let Some(surface) = &mut self.surface else {
            return;
        };

        // Present.
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        let pixels = canvas.data();
        for (i, chunk) in pixels.chunks_exact(4).enumerate() {
            buffer[i] = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        }
        let _ = buffer.present();
        self.last_frame = Instant::now();
    }

    fn draw_grid(&mut self) {
        let Some(canvas) = self.grid_cache.as_mut() else {
            return;
        };
        let Some(session) = &self.session else { return };
        let frame = session.frame();
        let grid = self.grid;
        let clip = Clip::new(grid.x, grid.y, grid.w, grid.h);
        let cell_w = self.text.cell_w;
        let cell_h = self.text.cell_h;
        let mode = self.mode;
        let cursor_on = self.cursor_on;
        let theme = self.theme.clone();
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale: self.scale,
        };

        painter.fill(grid.x, grid.y, grid.w, grid.h, frame.background, 1.0);

        let mut run: Option<Run> = None;
        let mut run_y;
        for row in &frame.rows_view {
            let y_top = grid.y + row.y as f32 * cell_h;
            run_y = y_top;
            // Backgrounds and selection. Background first so selection stays
            // visible over coloured cells (warnings, diffs, etc.).
            for cell in &row.cells {
                let cx = grid.x + cell.x as f32 * cell_w;
                if let Some(bg) = cell.bg {
                    if bg != frame.background {
                        painter.fill(cx, y_top, cell_w, cell_h, bg, 1.0);
                    }
                }
                let selected = row
                    .selection
                    .map(|(s, e)| cell.x >= s && cell.x <= e)
                    .unwrap_or(false);
                if selected {
                    painter.fill(cx, y_top, cell_w, cell_h, theme.selection, 0.32);
                }
            }
            for cell in &row.cells {
                if cell.len == 0 || cell.flags.invisible {
                    continue;
                }
                let mut text = String::with_capacity(cell.len as usize);
                for cp in &cell.cps[..cell.len as usize] {
                    if let Some(ch) = char::from_u32(*cp) {
                        text.push(ch);
                    }
                }
                let style = CellStyle {
                    fg: if cell.flags.faint {
                        cell.fg.lerp(frame.background, 0.55)
                    } else {
                        cell.fg
                    },
                    bold: cell.flags.bold,
                    italic: cell.flags.italic,
                    underline: cell.flags.underline,
                    strikethrough: cell.flags.strikethrough,
                    overline: cell.flags.overline,
                    underline_color: cell.underline,
                };
                let columns = unicode_width::UnicodeWidthStr::width(text.as_str()).max(1) as u16;
                let extend = run
                    .as_ref()
                    .map(|r| r.end_x == cell.x && r.style == style)
                    .unwrap_or(false);
                if extend {
                    if let Some(r) = &mut run {
                        r.text.push_str(&text);
                        r.end_x = cell.x + columns;
                    }
                } else {
                    if let Some(r) = run.take() {
                        draw_run_at(
                            &mut painter,
                            &r,
                            grid.x + r.x as f32 * cell_w,
                            run_y,
                            cell_w,
                            cell_h,
                            &clip,
                        );
                    }
                    run = Some(Run {
                        x: cell.x,
                        end_x: cell.x + columns,
                        text,
                        style,
                    });
                }
            }
            if let Some(r) = run.take() {
                draw_run_at(
                    &mut painter,
                    &r,
                    grid.x + r.x as f32 * cell_w,
                    run_y,
                    cell_w,
                    cell_h,
                    &clip,
                );
            }
        }

        // Cursor.
        if mode != Mode::Compose {
            if let Some(cursor) = frame.cursor {
                let show = !cursor.blinking || cursor_on;
                if show {
                    let cx = grid.x + cursor.x as f32 * cell_w;
                    let cy = grid.y + cursor.y as f32 * cell_h;
                    match cursor.style {
                        CursorStyle::Bar => painter.fill(
                            cx,
                            cy,
                            (cell_w * 0.12).max(2.0),
                            cell_h,
                            theme.cursor,
                            0.9,
                        ),
                        CursorStyle::Block => {
                            painter.fill(cx, cy, cell_w, cell_h, theme.cursor, 0.45)
                        }
                        CursorStyle::Underline => painter.fill(
                            cx,
                            cy + cell_h - (scale_line(self.scale)),
                            cell_w,
                            (2.0 * self.scale).max(1.5),
                            theme.cursor,
                            0.9,
                        ),
                        CursorStyle::HollowBlock => {
                            painter.rounded_stroke(
                                cx + 1.0,
                                cy + 1.0,
                                cell_w - 2.0,
                                cell_h - 2.0,
                                2.0,
                                (1.5 * self.scale).max(1.0),
                                theme.cursor,
                                0.9,
                            );
                        }
                    }
                }
            }
        }

        // Scrollbar.
        let sb = frame.scrollbar;
        if sb.total > sb.len && sb.len > 0 {
            let track_x = grid.x + grid.w - (6.0 * self.scale);
            let track_w = (3.0 * self.scale).max(2.0);
            painter.fill(track_x, grid.y, track_w, grid.h, theme.dim, 0.12);
            let frac = sb.offset as f32 / sb.total.max(1) as f32;
            let thumb_h = (grid.h * (sb.len as f32 / sb.total as f32)).max(16.0 * self.scale);
            let thumb_y = grid.y + frac * (grid.h - thumb_h);
            painter.rounded(
                track_x,
                thumb_y,
                track_w,
                thumb_h,
                track_w / 2.0,
                theme.dim,
                0.45,
            );
        }
    }

    fn draw_card(&mut self) {
        let width = self.size.width as f32;
        let height = self.size.height as f32;
        let scale = self.scale;
        let s = move |v: f32| (v * scale).round();
        let cell_h = self.text.cell_h;
        let cell_w = self.text.cell_w;
        let margin = s(14.0);
        let pad = s(12.0);
        let hint_h = if self.config.ui.show_hints {
            s(18.0)
        } else {
            0.0
        };
        let mode = self.mode;
        let lines = if mode == Mode::Compose {
            self.editor.line_count().clamp(1, 10)
        } else {
            1
        };
        let card_h = (pad * 2.0 + lines as f32 * cell_h + s(6.0) + hint_h).max(self.card.h);
        let card_y = height - margin - card_h;
        let card_x = margin;
        let card_w = (width - margin * 2.0).max(1.0);

        let theme = self.theme.clone();
        let cwd_label = self.cwd_display();
        let cwd_w = self.text.measure_text(&cwd_label, false);
        let chip_w = (cwd_w + 18.0 * scale).min(280.0 * scale);
        let chip_h = (cell_h - 2.0 * scale).max(10.0);
        let chip_y = card_y + pad + (cell_h - chip_h) / 2.0;
        let prompt_w = cell_w;
        let text_x = card_x + pad + chip_w + s(14.0) + prompt_w + s(10.0);
        let text_y = card_y + pad;
        let text_w = (card_x + card_w - pad - text_x).max(1.0);
        let hint_y = card_y + card_h - pad - hint_h;

        let mut status = format!(
            "{}   {}×{}",
            self.launch.kind.name(),
            self.grid.cols,
            self.grid.rows
        );
        if let Some((code, at)) = self.last_status {
            if code != 0 && at.elapsed() < Duration::from_secs(8) {
                status = format!("exit {code}   {status}");
            }
        }
        let status_w = self.text.measure_text(&status, false);

        self.card = CardLayout {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
            text_x,
            text_y,
        };

        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale,
        };

        let radius = s(12.0);
        let border = (1.0 * scale).max(1.0);
        painter.rounded(
            card_x,
            card_y + s(4.0),
            card_w,
            card_h,
            radius,
            theme.shadow,
            0.55,
        );
        painter.rounded(card_x, card_y, card_w, card_h, radius, theme.panel, 1.0);
        painter.rounded_stroke(
            card_x,
            card_y,
            card_w,
            card_h,
            radius,
            border,
            theme.border,
            0.9,
        );

        let scrolled = mode == Mode::Compose && self.composer_scroll > 0;
        if !scrolled {
            // cwd chip.
            painter.rounded(
                card_x + pad,
                chip_y,
                chip_w,
                chip_h,
                s(6.0),
                theme.panel_raised,
                1.0,
            );
            let chip_clip = Clip::new(card_x + pad, chip_y, chip_w - s(8.0), chip_h);
            painter.text(
                card_x + pad + s(9.0),
                chip_y + (chip_h - cell_h) / 2.0,
                &cwd_label,
                theme.dim,
                false,
                None,
                chip_clip,
                cwd_w + 4.0,
            );

            // Prompt glyph.
            let (glyph, glyph_color) = match mode {
                Mode::Compose => ("❯", theme.accent),
                Mode::Running => ("●", theme.yellow),
                Mode::Direct => ("!", theme.red),
            };
            painter.text(
                text_x - prompt_w - s(10.0),
                text_y,
                glyph,
                glyph_color,
                true,
                Some(cell_w),
                Clip::new(0.0, 0.0, width, height),
                prompt_w + 4.0,
            );
        }

        // Editor / running line.
        if mode == Mode::Compose {
            draw_editor(
                &mut painter,
                &theme,
                &self.editor,
                self.focus,
                self.cursor_on,
                &self.ime_preedit,
                &mut self.hscroll,
                &mut self.composer_scroll,
                text_x,
                text_y,
                text_w,
                lines,
            );
        } else if mode == Mode::Running {
            let elapsed = self.running_started.elapsed().as_secs_f32();
            let label = if self.running_cmd.is_empty() {
                "running…".to_string()
            } else {
                format!("{}  ·  {:.1}s", self.running_cmd, elapsed)
            };
            painter.text(
                text_x,
                text_y,
                &label,
                theme.dim,
                false,
                None,
                Clip::new(text_x, text_y, text_w, cell_h),
                text_w,
            );
        } else {
            painter.text(
                text_x,
                text_y,
                "Classic input — typing goes directly to the shell",
                theme.dim,
                false,
                None,
                Clip::new(text_x, text_y, text_w, cell_h),
                text_w,
            );
        }

        // Hints.
        if !self.config.ui.show_hints {
            drop(painter);
            if let Some(window) = &self.window {
                let (line, col) = self.editor.caret_line_col();
                let x = text_x + (col.saturating_sub(self.hscroll)) as f32 * cell_w;
                let y = text_y + (line as f32 - self.composer_scroll as f32) * cell_h;
                window.set_ime_cursor_area(
                    PhysicalPosition::new(x as i32, y as i32),
                    PhysicalSize::new(cell_w as u32, cell_h as u32),
                );
            }
            return;
        }
        let hint_clip = Clip::new(card_x + pad, hint_y, card_w - pad * 2.0, hint_h);
        let long_hint = match mode {
            Mode::Compose => {
                if keys::is_macos() {
                    "enter run   shift+enter newline   ↑↓ history   ⌘R search   ⌘V paste"
                } else {
                    "enter run   shift+enter newline   ↑↓ history   ^R search   ^V paste"
                }
            }
            Mode::Running => "running — ctrl-c to interrupt",
            Mode::Direct => "this shell has no prompt integration; use zsh or bash",
        };
        let short_hint = match mode {
            Mode::Compose => "enter run   ↑ history   tab complete",
            Mode::Running => "running — ctrl-c stops",
            Mode::Direct => "classic input — zsh/bash enable the composer",
        };
        let hint_text_y = hint_y + (hint_h - cell_h) / 2.0 + s(1.0);
        let status_x = card_x + card_w - pad - status_w;
        let mut hint = long_hint;
        let mut hint_w = painter.text.measure_text(hint, false);
        if card_x + pad + hint_w + s(14.0) >= status_x {
            hint = short_hint;
            hint_w = painter.text.measure_text(hint, false);
        }
        painter.text(
            card_x + pad,
            hint_text_y,
            hint,
            theme.faint,
            false,
            None,
            hint_clip,
            card_w - pad * 2.0,
        );
        if card_x + pad + hint_w + s(14.0) < status_x {
            painter.text(
                status_x,
                hint_text_y,
                &status,
                theme.faint,
                false,
                None,
                hint_clip,
                status_w + 4.0,
            );
        }

        drop(painter);

        // Update the IME candidate window position.
        if let Some(window) = &self.window {
            let (line, col) = self.editor.caret_line_col();
            let x = text_x + (col.saturating_sub(self.hscroll)) as f32 * cell_w;
            let y = text_y + (line as f32 - self.composer_scroll as f32) * cell_h;
            window.set_ime_cursor_area(
                PhysicalPosition::new(x as i32, y as i32),
                PhysicalSize::new(cell_w as u32, cell_h as u32),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_editor(
    painter: &mut Painter,
    theme: &Theme,
    editor: &Editor,
    focus: Focus,
    cursor_on: bool,
    preedit: &str,
    hscroll: &mut usize,
    composer_scroll: &mut usize,
    x: f32,
    y: f32,
    w: f32,
    lines_visible: usize,
) {
    let cell_w = painter.text.cell_w;
    let cell_h = painter.text.cell_h;
    let scale = painter.scale;
    let (caret_line, caret_col) = editor.caret_line_col();
    let cols = ((w / cell_w).floor() as usize).max(1);

    // Keep the caret in view horizontally.
    if caret_col < *hscroll {
        *hscroll = caret_col;
    } else if caret_col >= *hscroll + cols {
        *hscroll = caret_col + 1 - cols;
    }
    if *composer_scroll > caret_line {
        *composer_scroll = caret_line;
    } else if caret_line >= *composer_scroll + lines_visible {
        *composer_scroll = caret_line + 1 - lines_visible;
    }

    let text_clip = Clip::new(x, y, w, lines_visible as f32 * cell_h + 2.0);

    // Selection first.
    if let Some((sel_start, sel_end)) = editor.selection() {
        for line in *composer_scroll..(*composer_scroll + lines_visible).min(editor.line_count()) {
            let line_start = editor.line_start_of(line);
            let line_end = editor.line_end_of(line);
            let start = sel_start.max(line_start);
            let end = sel_end.min(line_end);
            if start >= end {
                if sel_start <= line_start && sel_end > line_end {
                    // Newline at end of a selected line.
                    let cx = x
                        + (editor.display_width(&editor.text()[line_start..line_end]) as f32
                            - *hscroll as f32)
                            * cell_w;
                    let cy = y + (line - *composer_scroll) as f32 * cell_h;
                    painter.fill(
                        cx,
                        cy,
                        (cell_w * 0.35).max(2.0),
                        cell_h,
                        theme.selection,
                        0.35,
                    );
                }
                continue;
            }
            let start_col = editor.display_width(&editor.text()[line_start..start]);
            let end_col = editor.display_width(&editor.text()[line_start..end]);
            let cx = x + (start_col as f32 - *hscroll as f32) * cell_w;
            let cw = (end_col - start_col) as f32 * cell_w;
            let cy = y + (line - *composer_scroll) as f32 * cell_h;
            painter.fill(cx, cy, cw, cell_h, theme.selection, 0.32);
        }
    }

    // Text.
    for line in *composer_scroll..(*composer_scroll + lines_visible).min(editor.line_count()) {
        let text = editor.line_text(line).to_string();
        let ly = y + (line - *composer_scroll) as f32 * cell_h;
        if text.is_empty() {
            continue;
        }
        let spans = highlight::line_spans(&text, theme);
        painter.text.draw_line(
            painter.pixmap,
            x - *hscroll as f32 * cell_w,
            ly,
            &spans,
            Some(cell_w),
            text_clip,
            w + *hscroll as f32 * cell_w + cell_w * 8.0,
        );
    }

    // Placeholder.
    if editor.is_empty() && focus == Focus::Composer && preedit.is_empty() {
        painter.text(
            x,
            y,
            "Type a command…",
            theme.faint,
            false,
            None,
            text_clip,
            w,
        );
    }

    // Caret.
    if focus == Focus::Composer && cursor_on {
        let cx = x + (caret_col as f32 - *hscroll as f32) * cell_w;
        let cy = y + (caret_line - *composer_scroll) as f32 * cell_h;
        painter.fill(cx, cy, (2.0 * scale).max(1.5), cell_h, theme.cursor, 1.0);
    }

    // IME preedit (composition preview). It is not in the buffer yet.
    if !preedit.is_empty() {
        let cx = x + (caret_col as f32 - *hscroll as f32) * cell_w;
        let cy = y + (caret_line - *composer_scroll) as f32 * cell_h;
        let width = painter.text(
            cx,
            cy,
            preedit,
            theme.accent,
            false,
            None,
            text_clip,
            (w - (cx - x)).max(10.0),
        );
        painter.fill(
            cx,
            cy + cell_h * 0.86,
            width.max(cell_w),
            (1.2 * scale).max(1.0),
            theme.accent,
            0.8,
        );
    }
}

impl PromptApp {
    fn draw_welcome(&mut self) {
        if !self.welcome {
            return;
        }
        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let cell_h = self.text.cell_h;
        let grid = self.grid;
        let scale = self.scale;
        let theme = self.theme.clone();
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale: self.scale,
        };
        painter.fill(grid.x, grid.y, grid.w, grid.h, theme.bg, 0.97);
        let w = (grid.w * 0.72).clamp(460.0 * scale, 680.0 * scale);
        let lines = 7.6;
        let h = 52.0 * scale + lines * cell_h;
        let x = grid.x + (grid.w - w) / 2.0;
        let y = grid.y + (grid.h - h) / 2.0;
        painter.rounded(x, y + 6.0 * scale, w, h, 14.0 * scale, theme.shadow, 0.6);
        painter.rounded(x, y, w, h, 14.0 * scale, theme.panel, 1.0);
        painter.rounded_stroke(x, y, w, h, 14.0 * scale, 1.0 * scale, theme.border, 0.9);
        let pad = 26.0 * scale;
        let mut ty = y + 22.0 * scale;
        painter.text(
            x + pad,
            ty,
            "Prompt",
            theme.accent,
            true,
            None,
            Clip::new(x, y, w, h),
            w - pad * 2.0,
        );
        ty += cell_h * 1.5;
        painter.text(
            x + pad,
            ty,
            "A terminal that types like an editor.",
            theme.fg,
            false,
            None,
            Clip::new(x, y, w, h),
            w - pad * 2.0,
        );
        ty += cell_h * 1.4;
        let body = [
            "Type below. Select with the mouse, copy and paste like a document.",
            "Enter runs · Shift+Enter adds a line · Up recalls history · Tab completes.",
            "vim, ssh, htop and every other program still get the whole keyboard.",
            "Your existing zsh/bash configuration is loaded automatically.",
        ];
        for line in body {
            painter.text(
                x + pad,
                ty,
                line,
                theme.dim,
                false,
                None,
                Clip::new(x, y, w, h),
                w - pad * 2.0,
            );
            ty += cell_h * 1.15;
        }
        painter.text(
            x + pad,
            y + h - 20.0 * scale,
            "Press any key to start",
            theme.faint,
            false,
            None,
            Clip::new(x, y, w, h),
            w - pad * 2.0,
        );
    }

    fn draw_search(&mut self) {
        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let cell_h = self.text.cell_h;
        let Some(search) = &self.search else { return };
        let card = self.card;
        let scale = self.scale;
        let theme = self.theme.clone();
        let w = (card.w * 0.78).min(760.0 * scale);
        let x = card.x + 24.0 * scale;
        let pad = 12.0 * scale;
        let (row_start, row_end) = popup_window(search.matches.len(), search.selected, 8);
        let visible = row_end - row_start;
        let header_h = cell_h + 10.0 * scale;
        let h = pad * 2.0 + header_h + visible as f32 * cell_h + 4.0 * scale;
        let y = (card.y - h - 10.0 * scale).max(self.grid.y + 4.0 * scale);
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale: self.scale,
        };
        painter.rounded(x, y + 5.0 * scale, w, h, 10.0 * scale, theme.shadow, 0.5);
        painter.rounded(x, y, w, h, 10.0 * scale, theme.panel, 1.0);
        painter.rounded_stroke(x, y, w, h, 10.0 * scale, scale, theme.border, 0.9);
        let clip = Clip::new(x, y, w, h);
        let label = "history";
        painter.text(x + pad, y + pad, label, theme.accent, true, None, clip, w);
        let label_w = painter.text.measure_text(label, true) + 10.0 * scale;
        let query = if search.query.is_empty() {
            "type to filter…".to_string()
        } else {
            search.query.clone()
        };
        let query_color = if search.query.is_empty() {
            theme.faint
        } else {
            theme.fg
        };
        let query_w = painter.text.measure_text(&query, false);
        painter.text(
            x + pad + label_w,
            y + pad,
            &query,
            query_color,
            false,
            None,
            clip,
            w - label_w - pad * 3.0,
        );
        painter.fill(
            x + pad + label_w + query_w + 1.0,
            y + pad + 2.0 * scale,
            (2.0 * scale).max(1.0),
            cell_h - 4.0 * scale,
            theme.cursor,
            1.0,
        );
        if visible == 0 {
            painter.text(
                x + pad,
                y + pad + header_h,
                "no matching commands",
                theme.faint,
                false,
                None,
                clip,
                w - pad * 2.0,
            );
        }
        for (i, cmd) in search.matches[row_start..row_end].iter().enumerate() {
            let index = row_start + i;
            let ry = y + pad + header_h + i as f32 * cell_h;
            if index == search.selected {
                painter.rounded(
                    x + pad * 0.5,
                    ry - 1.0 * scale,
                    w - pad,
                    cell_h + 2.0 * scale,
                    5.0 * scale,
                    theme.accent,
                    0.18,
                );
            }
            let color = if index == search.selected {
                theme.fg
            } else {
                theme.dim
            };
            let single = cmd.replace('\n', " ⏎ ");
            painter.text(
                x + pad,
                ry,
                &single,
                color,
                false,
                None,
                clip,
                w - pad * 2.0,
            );
        }
    }

    fn draw_completion(&mut self) {
        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let cell_h = self.text.cell_h;
        let Some(popup) = &self.completion_popup else {
            return;
        };
        let card = self.card;
        let scale = self.scale;
        let theme = self.theme.clone();
        let x = self.card.text_x;
        let w = (card.x + card.w - x - 24.0 * scale)
            .min(640.0 * scale)
            .max(260.0 * scale);
        let pad = 8.0 * scale;
        let (row_start, row_end) = popup_window(popup.set.items.len(), popup.selected, 8);
        let visible = row_end - row_start;
        let h = pad * 2.0 + visible as f32 * cell_h;
        let y = (card.y - h - 10.0 * scale).max(self.grid.y + 4.0 * scale);
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale: self.scale,
        };
        painter.rounded(x, y + 5.0 * scale, w, h, 10.0 * scale, theme.shadow, 0.5);
        painter.rounded(x, y, w, h, 10.0 * scale, theme.panel, 1.0);
        painter.rounded_stroke(x, y, w, h, 10.0 * scale, scale, theme.border, 0.9);
        let clip = Clip::new(x, y, w, h);
        for (i, item) in popup.set.items[row_start..row_end].iter().enumerate() {
            let index = row_start + i;
            let ry = y + pad + i as f32 * cell_h;
            if index == popup.selected {
                painter.rounded(
                    x + pad * 0.5,
                    ry - 1.0 * scale,
                    w - pad,
                    cell_h + 2.0 * scale,
                    4.0 * scale,
                    theme.accent,
                    0.18,
                );
            }
            let color = if item.is_dir {
                theme.cyan
            } else if index == popup.selected {
                theme.fg
            } else {
                theme.dim
            };
            painter.text(
                x + pad,
                ry,
                &item.display,
                color,
                false,
                None,
                clip,
                w - pad * 2.0,
            );
        }
    }

    fn draw_toast(&mut self) {
        let Some(toast) = &self.toast else { return };
        if Instant::now() > toast.until {
            self.toast = None;
            return;
        }
        let text = toast.text.clone();
        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let cell_h = self.text.cell_h;
        let scale = self.scale;
        let theme = self.theme.clone();
        let card = self.card;
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale: self.scale,
        };
        let tw = painter.text.measure_text(&text, false) + 24.0 * scale;
        let x = card.x + card.w - tw - 10.0 * scale;
        let h = cell_h + 12.0 * scale;
        let y = card.y - h - 10.0 * scale;
        painter.rounded(x, y + 4.0 * scale, tw, h, 8.0 * scale, theme.shadow, 0.5);
        painter.rounded(x, y, tw, h, 8.0 * scale, theme.panel_raised, 1.0);
        painter.rounded_stroke(x, y, tw, h, 8.0 * scale, scale, theme.border, 0.8);
        painter.text(
            x + 12.0 * scale,
            y + (h - cell_h) / 2.0,
            &text,
            theme.fg,
            false,
            None,
            Clip::new(x, y, tw, h),
            tw - 24.0 * scale,
        );
    }

    fn cwd_display(&mut self) -> String {
        if !self.cwd_label.is_empty() {
            return self.cwd_label.clone();
        }
        let label = display_path(&self.cwd);
        self.cwd_label = label.clone();
        label
    }
}

#[derive(Clone, PartialEq)]
struct CellStyle {
    fg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    overline: bool,
    underline_color: Option<Rgb>,
}

struct Run {
    x: u16,
    end_x: u16,
    text: String,
    style: CellStyle,
}

fn draw_run_at(
    painter: &mut Painter,
    run: &Run,
    x: f32,
    y_top: f32,
    cell_w: f32,
    cell_h: f32,
    clip: &Clip,
) {
    let span = Span {
        text: run.text.clone(),
        color: run.style.fg,
        bold: run.style.bold,
        italic: run.style.italic,
    };
    painter.text.draw_line(
        painter.pixmap,
        x,
        y_top,
        &[span],
        Some(cell_w),
        *clip,
        100_000.0,
    );
    if run.style.underline || run.style.strikethrough || run.style.overline {
        let width = (run.end_x - run.x) as f32 * cell_w;
        let color = run.style.underline_color.unwrap_or(run.style.fg);
        if run.style.underline {
            painter.fill(
                x,
                y_top + cell_h * 0.86,
                width,
                (1.5 * painter.scale).max(1.0),
                color,
                0.9,
            );
        }
        if run.style.strikethrough {
            painter.fill(
                x,
                y_top + cell_h * 0.54,
                width,
                (1.2 * painter.scale).max(1.0),
                color,
                0.8,
            );
        }
        if run.style.overline {
            painter.fill(
                x,
                y_top + 1.0,
                width,
                (1.2 * painter.scale).max(1.0),
                color,
                0.8,
            );
        }
    }
}

fn popup_window(len: usize, selected: usize, max: usize) -> (usize, usize) {
    if len <= max {
        return (0, len);
    }
    let half = max / 2;
    let start = selected.saturating_sub(half).min(len - max);
    (start, start + max)
}

fn scale_line(scale: f32) -> f32 {
    (2.0 * scale).max(1.5)
}

fn fallback_shell(original: &ShellLaunch) -> ShellLaunch {
    let candidates = [
        std::env::var("SHELL").ok(),
        Some("/bin/bash".to_string()),
        Some("/bin/sh".to_string()),
    ];
    let program = candidates
        .into_iter()
        .flatten()
        .find(|candidate| candidate != &original.program && Path::new(candidate).is_file())
        .unwrap_or_else(|| "/bin/sh".to_string());
    let mut env: Vec<(String, String)> = original
        .env
        .iter()
        .filter(|(key, _)| key != "ZDOTDIR" && key != "PROMPT_ZDOTDIR")
        .cloned()
        .collect();
    env.push(("PROMPT_SHELL_INTEGRATION".into(), "0".into()));
    ShellLaunch {
        program,
        args: vec!["-i".into()],
        env,
        kind: ShellKind::Other,
        integrated: false,
    }
}

fn normalize_paste(text: &str) -> String {
    let mut out: String = text
        .replace("\r\n", "\n")
        .replace('\r', "")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn display_path(path: &Path) -> String {
    let home = shell::home_dir();
    let text = if let Ok(rest) = path.strip_prefix(&home) {
        if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rest.display())
        }
    } else {
        path.display().to_string()
    };
    if text.chars().count() > 34 {
        let tail: String = text
            .chars()
            .rev()
            .take(31)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        text
    }
}

fn pwd_to_path(pwd: &str) -> PathBuf {
    let pwd = pwd.strip_prefix("file://").unwrap_or(pwd);
    let pwd = if let Some(rest) = pwd.strip_prefix("localhost") {
        rest
    } else if pwd.starts_with('/') {
        pwd
    } else if let Some(idx) = pwd.find('/') {
        &pwd[idx..]
    } else {
        pwd
    };
    // Percent-decode the handful of characters shells commonly emit.
    let decoded = pwd
        .replace("%20", " ")
        .replace("%23", "#")
        .replace("%25", "%");
    PathBuf::from(decoded)
}

impl ApplicationHandler<()> for PromptApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            if let Err(err) = self.init_surface(event_loop) {
                eprintln!("prompt: failed to start: {err:#}");
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        self.drain_pty();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.size = size;
                self.relayout();
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.drain_pty();
                self.render();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.relayout();
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(&event);
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Commit(text) => {
                    self.ime_composing = false;
                    self.ime_preedit.clear();
                    if !text.is_empty() && self.mode == Mode::Compose {
                        self.editor.insert(&text);
                        self.after_edit();
                    }
                }
                Ime::Preedit(text, _) => {
                    self.ime_composing = true;
                    self.ime_preedit = text;
                    self.request_redraw();
                }
                Ime::Enabled => {}
                Ime::Disabled => {
                    self.ime_composing = false;
                    self.ime_preedit.clear();
                }
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = position;
                self.handle_cursor_moved(position);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_button(state, button, self.cursor_pos);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta, self.cursor_pos);
            }
            WindowEvent::Focused(focused) => {
                self.cursor_on = true;
                self.grid_dirty = true;
                let report = self
                    .session
                    .as_ref()
                    .map(|session| session.focus_event())
                    .unwrap_or(false);
                if report {
                    let bytes: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
                    self.send_bytes(bytes);
                }
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.quit_requested {
            event_loop.exit();
            return;
        }
        self.drain_pty();
        self.check_exit(event_loop);
        self.update_timers(event_loop);
        if self
            .toast
            .as_ref()
            .map(|t| Instant::now() > t.until)
            .unwrap_or(false)
        {
            self.toast = None;
            self.request_redraw();
        }
    }
}
