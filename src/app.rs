use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use arboard::Clipboard;
use softbuffer::{Context, Surface};
use tiny_skia::{Pixmap, PixmapPaint, Transform};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::completion::{common_prefix, history_suggestion, CompletionEngine, CompletionSet};
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

/// One command's place in the terminal's scrollback. Rows are absolute, i.e.
/// scrollback rows counted from the start of the session, so they stay valid
/// while the viewport scrolls.
struct Block {
    command: String,
    /// First output row of the command.
    start_row: u64,
    /// Last output row, once the command has finished.
    end_row: Option<u64>,
    status: Option<i32>,
}

/// A detached tab: its shell (and any running command) keeps living while the
/// tab sits in the background panel.
struct BackgroundTab {
    session: TerminalSession,
    pty: Pty,
    mode: Mode,
    command: String,
    started: Instant,
    duration: Option<Duration>,
    exit: Option<i32>,
    cwd: PathBuf,
    last_line: String,
    blocks: Vec<Block>,
    /// Scrollback row the user was looking at when the tab was detached.
    scroll: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct PanelEntry {
    /// Index into `PromptApp::background`.
    tab: usize,
    y: f32,
    h: f32,
    close: Option<(f32, f32, f32, f32)>,
}

#[derive(Clone, Debug, Default)]
struct PanelLayout {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    entries: Vec<PanelEntry>,
}

/// Rows in a block longer than this start collapsed to a 300-line window.
const BLOCK_WINDOW: u64 = 300;

#[derive(Clone, Copy, Debug)]
struct BlockMatch {
    row: u64,
    start: u16,
    end: u16,
}

struct BlockSearch {
    query: String,
    active: bool,
    matches: Vec<BlockMatch>,
    selected: usize,
}

/// Full-grid reading mode for one command block.
struct BlockView {
    index: usize,
    /// Absolute row at the top of the grid.
    scroll: u64,
    expanded: bool,
    search: Option<BlockSearch>,
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
    /// Commands run in the foreground tab, for block chrome.
    blocks: Vec<Block>,
    /// Tabs detached to the background panel. Their shells keep running.
    background: Vec<BackgroundTab>,
    panel: PanelLayout,
    /// First background-tab row shown in the panel.
    panel_scroll: usize,
    detach_rect: Option<(f32, f32, f32, f32)>,
    block_view: Option<BlockView>,
    pending_highlight: Option<(u64, u16, u16)>,
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
    /// Composer line count the current grid/card layout was built for.
    layout_composer_lines: usize,

    history_index: Option<usize>,
    history_draft: String,
    search: Option<SearchState>,
    completion_popup: Option<CompletionState>,

    welcome: bool,
    toast: Option<Toast>,

    /// 0 = composer centred in an empty window, 1 = docked at the bottom with
    /// the terminal revealed. Animated once, on the first submit.
    dock_progress: f32,
    dock_started: Option<Instant>,

    cursor_on: bool,
    last_blink: Instant,

    /// Input tracing for debugging hit-testing (`PROMPT_TRACE_INPUT=1`).
    trace_input: bool,
    trace_zone: u8,

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
            blocks: Vec::new(),
            background: Vec::new(),
            panel: PanelLayout::default(),
            panel_scroll: 0,
            detach_rect: None,
            block_view: None,
            pending_highlight: None,
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
            layout_composer_lines: 1,
            history_index: None,
            history_draft: String::new(),
            search: None,
            completion_popup: None,
            welcome: first_run,
            toast: None,
            dock_progress: if launch_integrated { 0.0 } else { 1.0 },
            dock_started: None,
            cursor_on: true,
            last_blink: started,
            trace_input: std::env::var_os("PROMPT_TRACE_INPUT").is_some(),
            trace_zone: 0,
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

    /// Animate the composer from the centre of the window to its docked spot
    /// at the bottom, revealing the terminal on the way.
    fn begin_dock(&mut self) {
        if self.dock_progress >= 1.0 || self.dock_started.is_some() {
            return;
        }
        self.dock_started = Some(Instant::now());
        self.request_redraw();
    }

    fn advance_dock(&mut self) {
        let Some(started) = self.dock_started else {
            return;
        };
        let t = (started.elapsed().as_secs_f32() / 0.34).clamp(0.0, 1.0);
        // Ease-out cubic: quick departure, soft landing.
        let eased = 1.0 - (1.0 - t).powi(3);
        self.dock_progress = eased.max(self.dock_progress);
        if t >= 1.0 {
            self.dock_progress = 1.0;
            self.dock_started = None;
        }
    }

    /// Terminal opacity during the transition. The card starts moving first
    /// and the terminal fades in underneath it.
    fn terminal_reveal(&self) -> f32 {
        ((self.dock_progress - 0.2) / 0.8).clamp(0.0, 1.0)
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
        let cwd = self.cwd.clone();
        self.spawn_foreground(cwd)?;
        if self.config.ui.start_maximized {
            if let Some(window) = &self.window {
                window.set_maximized(true);
            }
        }
        config::mark_started(&self.paths);
        self.request_redraw();
        Ok(())
    }

    /// Create the shell for the foreground tab. Used at startup and whenever a
    /// running tab is detached or a background tab is attached.
    fn spawn_foreground(&mut self, cwd: PathBuf) -> Result<()> {
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
        let pty = match Pty::spawn(
            &self.launch,
            &cwd,
            cols,
            rows,
            cell_w,
            cell_h,
            self.pty_notify(),
        ) {
            Ok(pty) => pty,
            Err(first_error) => {
                let fallback = fallback_shell(&self.launch);
                self.toast(format!(
                    "Could not start {} — using {}",
                    self.launch.program, fallback.program
                ));
                self.mode = Mode::Direct;
                self.dock_progress = 1.0;
                self.launch.kind = ShellKind::Other;
                self.launch.integrated = false;
                self.launch.program = fallback.program.clone();
                Pty::spawn(
                    &fallback,
                    &cwd,
                    cols,
                    rows,
                    cell_w,
                    cell_h,
                    self.pty_notify(),
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
        self.session = Some(session);
        self.pty = Some(pty);
        self.cwd = cwd;
        self.cwd_label.clear();
        self.blocks.clear();
        self.mode = if self.launch.integrated {
            Mode::Compose
        } else {
            Mode::Direct
        };
        self.focus = Focus::Composer;
        self.running_cmd.clear();
        self.running_started = Instant::now();
        self.last_status = None;
        self.got_prompt = false;
        self.exited = false;
        self.sel_anchor = None;
        self.sel_drag = false;
        self.editor.clear();
        self.block_view = None;
        self.pending_highlight = None;
        self.hscroll = 0;
        self.composer_scroll = 0;
        self.integration_deadline = Instant::now() + Duration::from_secs(4);
        self.grid_dirty = true;
        self.request_redraw();
        Ok(())
    }

    fn pty_notify(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        match self.proxy.clone() {
            Some(proxy) => Some(Box::new(move || {
                let _ = proxy.send_event(());
            })),
            None => Some(Box::new(|| {})),
        }
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
        let composer_lines = self.composer_lines() as f32;
        let base_card =
            self.s(12.0) * 2.0 + composer_lines * self.text.cell_h + self.s(6.0) + hint_h;
        self.layout_composer_lines = self.composer_lines();

        // A detached tab gets a rail on the right; the terminal and composer
        // share the remaining width.
        let panel_w = if self.background.is_empty() {
            0.0
        } else {
            (300.0 * scale)
                .min((w * 0.38).max(220.0 * scale))
                .min((w - 2.0 * self.s(14.0)).max(1.0))
        };
        let reserved = if panel_w > 0.0 { panel_w + gap } else { 0.0 };
        self.panel = PanelLayout {
            x: w - margin - panel_w,
            y: margin,
            w: panel_w,
            h: (h - margin * 2.0).max(1.0),
            entries: Vec::new(),
        };
        self.panel_scroll = self.panel_scroll.min(self.panel_scroll_range().1);

        let grid_x = margin + padding;
        let grid_y = margin + padding;
        let grid_w = (w - 2.0 * grid_x - reserved).max(1.0);
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
            w: (w - margin * 2.0 - reserved).max(1.0),
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
            // Detached commands should still see the current terminal size.
            for tab in &mut self.background {
                tab.session.resize(
                    cols,
                    rows,
                    self.text.cell_w.max(1.0) as u32,
                    self.text.cell_h.max(1.0) as u32,
                );
                tab.pty.resize(
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
        if let Some(pty) = &mut self.pty {
            let data = pty.drain();
            if !data.is_empty() {
                if let Some(session) = &mut self.session {
                    let was_at_bottom = session.frame().scrollbar.at_bottom();
                    session.write(&data);
                    if was_at_bottom && self.block_view.is_none() {
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
        self.drain_background();
    }

    /// Keep detached shells alive and up to date, even though they are not
    /// rendered in the grid.
    fn drain_background(&mut self) {
        let mut redraw = false;
        for tab in &mut self.background {
            let data = tab.pty.drain();
            let mut changed = false;
            if !data.is_empty() {
                tab.session.write(&data);
                let replies = tab.session.take_pty_replies();
                if !replies.is_empty() {
                    tab.pty.write(&replies);
                }
                changed = true;
            }
            for event in tab.session.drain_events() {
                match event {
                    TermEvent::PromptStart => {
                        tab.mode = Mode::Compose;
                        changed = true;
                    }
                    TermEvent::OutputStart { command } => {
                        tab.mode = Mode::Running;
                        tab.started = Instant::now();
                        tab.duration = None;
                        tab.exit = None;
                        if !command.is_empty() {
                            tab.command = command;
                        }
                        changed = true;
                    }
                    TermEvent::CommandEnd { exit_code } => {
                        tab.mode = Mode::Compose;
                        tab.duration = Some(tab.started.elapsed());
                        if let Some(code) = exit_code {
                            tab.exit = Some(code);
                        }
                        close_open_block(&mut tab.blocks, &mut tab.session, exit_code);
                        changed = true;
                    }
                    TermEvent::InputStart => {}
                    TermEvent::Bell => {
                        // A bell from a background command is still worth
                        // surfacing; the panel already shows the tab.
                        changed = true;
                    }
                    TermEvent::TitleChanged | TermEvent::PwdChanged => {
                        if let Some(pwd) = tab.session.pwd() {
                            let path = pwd_to_path(&pwd);
                            if path.is_dir() {
                                tab.cwd = path;
                            }
                        }
                        changed = true;
                    }
                }
            }
            if tab.mode == Mode::Compose && tab.duration.is_none() {
                if let Some(code) = tab.pty.try_exit_code() {
                    tab.exit = Some(code);
                }
            }
            if changed {
                tab.session.scroll_to_bottom();
                tab.session.update_frame();
                tab.last_line = panel_last_line(&tab.session, tab.mode);
                redraw = true;
            }
        }
        if redraw {
            self.request_redraw();
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
                    self.trace("event PromptStart (focus=composer, composer selection cleared)");
                }
                TermEvent::InputStart => {}
                TermEvent::OutputStart { command } => {
                    if self.mode == Mode::Compose {
                        self.mode = Mode::Running;
                    }
                    self.begin_dock();
                    self.running_started = Instant::now();
                    if !command.is_empty() {
                        self.running_cmd = command.clone();
                    }
                    self.note_output_start(command);
                }
                TermEvent::CommandEnd { exit_code } => {
                    if let Some(code) = exit_code {
                        self.last_status = Some((code, Instant::now()));
                    }
                    self.end_block(exit_code);
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

    /// Absolute scrollback row of the cursor in the foreground session.
    fn current_row(session: &mut TerminalSession) -> u64 {
        session.update_frame();
        let offset = session.scrollbar().offset;
        let y = session.frame().cursor.map(|c| c.y as u64).unwrap_or(0);
        offset + y
    }

    fn start_block(&mut self, command: String) {
        let Some(session) = &mut self.session else {
            return;
        };
        let row = Self::current_row(session);
        let command = if command.is_empty() {
            self.running_cmd.clone()
        } else {
            command
        };
        self.blocks.push(Block {
            command,
            start_row: row,
            end_row: None,
            status: None,
        });
        // Keep the chrome bounded for very long sessions.
        if self.blocks.len() > 200 {
            self.blocks.remove(0);
        }
        self.grid_dirty = true;
    }

    /// The shell reported that a command started. Commands submitted through
    /// the composer already have a block (created at submit time); this also
    /// covers commands started by other means, e.g. typed in classic input.
    fn note_output_start(&mut self, command: String) {
        if let Some(block) = self.blocks.last_mut() {
            if block.end_row.is_none() {
                if !command.is_empty() {
                    block.command = command;
                }
                return;
            }
        }
        let Some(session) = &mut self.session else {
            return;
        };
        let row = Self::current_row(session).saturating_sub(1);
        self.blocks.push(Block {
            command,
            start_row: row,
            end_row: None,
            status: None,
        });
        if self.blocks.len() > 200 {
            self.blocks.remove(0);
        }
        self.grid_dirty = true;
    }

    /// Scroll to the previous/next command block and show which one it is.
    fn jump_block(&mut self, direction: i32) {
        let target = {
            let Some(session) = &mut self.session else {
                return;
            };
            session.update_frame();
            let offset = session.scrollbar().offset;
            let block = if direction < 0 {
                self.blocks
                    .iter()
                    .rev()
                    .find(|block| block.start_row < offset)
            } else {
                self.blocks.iter().find(|block| block.start_row > offset)
            };
            block.map(|block| (block.start_row, block.command.clone()))
        };
        let Some((row, command)) = target else { return };
        if let Some(session) = &mut self.session {
            session.scroll_to_row(row);
            session.update_frame();
        }
        self.grid_dirty = true;
        if command.is_empty() {
            self.toast("Command block");
        } else {
            self.toast(command.replace('\n', " ⏎ "));
        }
        self.request_redraw();
    }

    fn end_block(&mut self, status: Option<i32>) {
        if let Some(session) = &mut self.session {
            close_open_block(&mut self.blocks, session, status);
        }
        if let Some(block) = self.blocks.last() {
            if let Some(end) = block.end_row {
                let lines = end.saturating_sub(block.start_row) + 1;
                if lines > BLOCK_WINDOW {
                    let key = if keys::is_macos() {
                        "⌘⇧B"
                    } else {
                        "ctrl-shift-b"
                    };
                    self.toast(format!("{lines} lines · {key} to review this block"));
                }
            }
        }
        self.grid_dirty = true;
    }

    // ---- block view -------------------------------------------------------

    /// Inclusive absolute row range of a block. The live block ends at the
    /// cursor until its command finishes.
    fn block_range(&self, index: usize) -> Option<(u64, u64)> {
        let block = self.blocks.get(index)?;
        let end = block.end_row.unwrap_or_else(|| {
            self.session
                .as_ref()
                .map(|session| session.scrollbar().total.saturating_sub(1))
                .unwrap_or(block.start_row)
        });
        Some((block.start_row, end.max(block.start_row)))
    }

    /// Rows reachable in a block right now: a 300-line window until expanded.
    fn block_window(&self, index: usize, expanded: bool) -> Option<(u64, u64)> {
        let (start, end) = self.block_range(index)?;
        let end = if expanded {
            end
        } else {
            start.saturating_add(BLOCK_WINDOW - 1).min(end)
        };
        Some((start, end))
    }

    fn open_block_view(&mut self, index: Option<usize>) {
        if self.blocks.is_empty() {
            self.toast("No command blocks yet");
            return;
        }
        let index = index
            .unwrap_or(self.blocks.len().saturating_sub(1))
            .min(self.blocks.len() - 1);
        let Some((start, _)) = self.block_range(index) else {
            return;
        };
        if let Some(session) = &mut self.session {
            session.clear_selection();
        }
        self.pending_highlight = None;
        self.block_view = Some(BlockView {
            index,
            scroll: start,
            expanded: false,
            search: None,
        });
        self.focus = Focus::Grid;
        self.clamp_block_view();
        self.grid_dirty = true;
        self.request_redraw();
    }

    fn close_block_view(&mut self) {
        if self.block_view.take().is_none() {
            return;
        }
        self.pending_highlight = None;
        if let Some(session) = &mut self.session {
            session.clear_selection();
            session.scroll_to_bottom();
            session.update_frame();
        }
        self.focus = Focus::Composer;
        self.grid_dirty = true;
        self.request_redraw();
    }

    fn clamp_block_view(&mut self) {
        let rows = self.grid.rows.max(1) as u64;
        let Some(index) = self.block_view.as_ref().map(|view| view.index) else {
            return;
        };
        let expanded = self
            .block_view
            .as_ref()
            .map(|view| view.expanded)
            .unwrap_or(false);
        let Some((start, end)) = self.block_window(index, expanded) else {
            return;
        };
        let max_top = end.saturating_sub(rows - 1).max(start);
        if let Some(view) = &mut self.block_view {
            view.scroll = view.scroll.clamp(start, max_top);
        }
    }

    fn scroll_block(&mut self, delta: i64) {
        if let Some(view) = &mut self.block_view {
            view.scroll = (view.scroll as i64 + delta).max(0) as u64;
        } else {
            return;
        }
        self.clamp_block_view();
        self.grid_dirty = true;
        self.request_redraw();
    }

    fn snap_block(&mut self, top: bool) {
        let Some(index) = self.block_view.as_ref().map(|view| view.index) else {
            return;
        };
        let expanded = self
            .block_view
            .as_ref()
            .map(|view| view.expanded)
            .unwrap_or(false);
        let Some((start, end)) = self.block_window(index, expanded) else {
            return;
        };
        let rows = self.grid.rows.max(1) as u64;
        let target = if top {
            start
        } else {
            end.saturating_sub(rows - 1).max(start)
        };
        if let Some(view) = &mut self.block_view {
            view.scroll = target;
        }
        self.grid_dirty = true;
        self.request_redraw();
    }

    fn switch_block(&mut self, delta: i32) {
        let Some(view) = &self.block_view else { return };
        let last = self.blocks.len().saturating_sub(1) as i32;
        let index = (view.index as i32 + delta).clamp(0, last) as usize;
        let Some((start, _)) = self.block_range(index) else {
            return;
        };
        if let Some(session) = &mut self.session {
            session.clear_selection();
        }
        self.pending_highlight = None;
        if let Some(view) = &mut self.block_view {
            view.index = index;
            view.scroll = start;
            view.expanded = false;
            view.search = None;
        }
        self.clamp_block_view();
        self.grid_dirty = true;
        self.request_redraw();
    }

    fn toggle_block_expanded(&mut self) {
        let Some(view) = &mut self.block_view else {
            return;
        };
        view.expanded = !view.expanded;
        self.clamp_block_view();
        self.grid_dirty = true;
        self.request_redraw();
    }

    /// Read a block's text a viewport at a time. Used lazily by search.
    fn block_text(&mut self, start: u64, end: u64) -> Vec<String> {
        let total = (end.saturating_sub(start) + 1) as usize;
        let mut out = vec![String::new(); total];
        let Some(session) = &mut self.session else {
            return out;
        };
        let saved = session.scrollbar().offset;
        let mut row = start;
        while row <= end {
            session.scroll_to_row(row);
            session.update_frame();
            let offset = session.scrollbar().offset;
            let view_rows = session.frame().rows as u64;
            let mut progressed = false;
            for y in 0..view_rows {
                let abs = offset + y;
                if abs < start || abs > end {
                    continue;
                }
                let index = (abs - start) as usize;
                if index < out.len() && out[index].is_empty() {
                    if let Some(text) = session.frame().row_text(y as u16) {
                        out[index] = text.trim_end().to_string();
                        progressed = true;
                    }
                }
            }
            if !progressed {
                break;
            }
            row = offset + view_rows.max(1);
        }
        session.scroll_to_row(saved);
        session.update_frame();
        out
    }

    fn run_block_search(&mut self) {
        let Some(view) = &self.block_view else { return };
        let index = view.index;
        let expanded = view.expanded;
        let query = view
            .search
            .as_ref()
            .map(|search| search.query.trim().to_string())
            .unwrap_or_default();
        let Some((start, end)) = self.block_window(index, expanded) else {
            return;
        };
        if query.is_empty() {
            if let Some(search) = self
                .block_view
                .as_mut()
                .and_then(|view| view.search.as_mut())
            {
                search.matches.clear();
                search.selected = 0;
                search.active = false;
            }
            self.pending_highlight = None;
            self.request_redraw();
            return;
        }

        let rows = self.block_text(start, end);
        let needle = query.to_lowercase();
        let mut matches = Vec::new();
        for (i, text) in rows.iter().enumerate() {
            let lower = text.to_lowercase();
            let mut from = 0;
            while from < lower.len() {
                let Some(pos) = lower[from..].find(&needle) else {
                    break;
                };
                let at = from + pos;
                let byte_end = at + needle.len();
                if text.is_char_boundary(at) && text.is_char_boundary(byte_end) {
                    let start_col = unicode_width::UnicodeWidthStr::width(&text[..at]) as u16;
                    let end_col = start_col
                        + unicode_width::UnicodeWidthStr::width(&text[at..byte_end]) as u16;
                    matches.push(BlockMatch {
                        row: start + i as u64,
                        start: start_col,
                        end: end_col.max(start_col + 1),
                    });
                }
                from = byte_end.max(at + 1);
            }
        }
        let count = matches.len();
        let first = matches.first().copied();
        if let Some(search) = self
            .block_view
            .as_mut()
            .and_then(|view| view.search.as_mut())
        {
            search.matches = matches;
            search.selected = 0;
            search.active = false;
        }
        if let Some(first) = first {
            self.select_block_match(0, first);
        }
        if count == 0 {
            let shown = end.saturating_sub(start) + 1;
            let total = self
                .block_range(index)
                .map(|(first, last)| last.saturating_sub(first) + 1)
                .unwrap_or(shown);
            if !expanded && shown < total {
                self.toast("No match in the first 300 lines — press e to expand");
            } else {
                self.toast("No matches in this block");
            }
        } else {
            self.toast(format!(
                "{count} match{}",
                if count == 1 { "" } else { "es" }
            ));
        }
    }

    fn select_block_match(&mut self, selected: usize, matched: BlockMatch) {
        let Some(index) = self.block_view.as_ref().map(|view| view.index) else {
            return;
        };
        let expanded = self
            .block_view
            .as_ref()
            .map(|view| view.expanded)
            .unwrap_or(false);
        let Some((start, _)) = self.block_window(index, expanded) else {
            return;
        };
        let rows = self.grid.rows.max(1) as u64;
        let target = matched.row.saturating_sub(rows / 2).max(start);
        if let Some(view) = &mut self.block_view {
            view.scroll = target;
            if let Some(search) = &mut view.search {
                search.selected = selected;
            }
        }
        self.clamp_block_view();
        self.pending_highlight = Some((matched.row, matched.start, matched.end));
        self.grid_dirty = true;
        self.request_redraw();
    }

    fn step_block_match(&mut self, delta: i32) {
        let next = {
            let Some(view) = &self.block_view else { return };
            let Some(search) = &view.search else { return };
            if search.matches.is_empty() {
                return;
            }
            let len = search.matches.len() as i32;
            let index = (search.selected as i32 + delta).rem_euclid(len) as usize;
            (index, search.matches[index])
        };
        self.select_block_match(next.0, next.1);
    }

    fn handle_block_key(
        &mut self,
        code: KeyCode,
        primary: bool,
        ctrl: bool,
        shift: bool,
        event: &KeyEvent,
    ) -> bool {
        let Some(view) = &self.block_view else {
            return false;
        };
        let search_active = view
            .search
            .as_ref()
            .map(|search| search.active)
            .unwrap_or(false);

        if search_active {
            match code {
                KeyCode::Escape => {
                    if let Some(search) = self
                        .block_view
                        .as_mut()
                        .and_then(|view| view.search.as_mut())
                    {
                        search.active = false;
                    }
                }
                KeyCode::Enter => self.run_block_search(),
                KeyCode::Backspace => {
                    if let Some(search) = self
                        .block_view
                        .as_mut()
                        .and_then(|view| view.search.as_mut())
                    {
                        search.query.pop();
                    }
                }
                _ => {
                    if let Some(text) = &event.text {
                        if !text.is_empty() && !text.chars().any(|c| c.is_control()) {
                            if let Some(search) = self
                                .block_view
                                .as_mut()
                                .and_then(|view| view.search.as_mut())
                            {
                                search.query.push_str(text);
                            }
                        }
                    }
                }
            }
            self.request_redraw();
            return true;
        }

        if primary && code == KeyCode::KeyC && (keys::is_macos() || shift) {
            self.copy_selection();
            return true;
        }
        // Interrupt the foreground command without leaving the block.
        if ctrl && code == KeyCode::KeyC {
            self.send_bytes(&[3]);
            return true;
        }
        if primary && shift && matches!(code, KeyCode::ArrowUp | KeyCode::ArrowDown) {
            self.switch_block(if code == KeyCode::ArrowUp { -1 } else { 1 });
            return true;
        }
        match code {
            KeyCode::Escape => self.close_block_view(),
            KeyCode::Slash => {
                if let Some(view) = &mut self.block_view {
                    let search = view.search.get_or_insert_with(|| BlockSearch {
                        query: String::new(),
                        active: false,
                        matches: Vec::new(),
                        selected: 0,
                    });
                    search.active = true;
                }
                self.request_redraw();
            }
            KeyCode::KeyE => self.toggle_block_expanded(),
            KeyCode::Home => self.snap_block(true),
            KeyCode::End => self.snap_block(false),
            KeyCode::KeyG => self.snap_block(!shift),
            KeyCode::PageUp => self.scroll_block(-(self.grid.rows.max(3) as i64 - 2)),
            KeyCode::PageDown => self.scroll_block(self.grid.rows.max(3) as i64 - 2),
            KeyCode::ArrowUp => self.scroll_block(-1),
            KeyCode::ArrowDown => self.scroll_block(1),
            KeyCode::BracketLeft | KeyCode::Comma => self.switch_block(-1),
            KeyCode::BracketRight | KeyCode::Period => self.switch_block(1),
            KeyCode::KeyN => self.step_block_match(if shift { -1 } else { 1 }),
            _ => {}
        }
        true
    }

    /// Point the terminal viewport at the block view's current row.
    fn apply_block_view(&mut self) {
        if self.block_view.is_none() {
            return;
        }
        let before = self
            .block_view
            .as_ref()
            .map(|view| view.scroll)
            .unwrap_or(0);
        self.clamp_block_view();
        let Some(scroll) = self.block_view.as_ref().map(|view| view.scroll) else {
            return;
        };
        if scroll != before {
            self.grid_dirty = true;
        }
        if let Some(session) = &mut self.session {
            session.scroll_to_row(scroll);
        }
    }

    fn apply_pending_highlight(&mut self) {
        let Some((row, start, end)) = self.pending_highlight.take() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let offset = session.scrollbar().offset;
        let rows = session.frame().rows as u64;
        if row >= offset && row < offset.saturating_add(rows) {
            let y = (row - offset) as u16;
            session.set_selection(Some((start, y)), (end.saturating_sub(1), y));
            session.update_frame();
            self.grid_dirty = true;
            self.request_redraw();
        }
    }

    fn block_status_line(&self) -> (String, Rgb) {
        let Some(view) = &self.block_view else {
            return (String::new(), self.theme.dim);
        };
        if let Some(search) = &view.search {
            if search.active || !search.matches.is_empty() {
                let total = search.matches.len();
                let current = if total == 0 { 0 } else { search.selected + 1 };
                let query = if search.active {
                    format!("{}▏", search.query)
                } else {
                    search.query.clone()
                };
                return (
                    format!("/{query}   {current}/{total} matches"),
                    self.theme.fg,
                );
            }
        }
        let command = self
            .blocks
            .get(view.index)
            .map(|block| block.command.replace('\n', " ⏎ "))
            .unwrap_or_default();
        (command, self.theme.dim)
    }

    // ---- background tabs --------------------------------------------------

    /// Move the foreground shell into a background tab, keeping it alive.
    fn take_foreground(&mut self) -> Option<BackgroundTab> {
        let session = self.session.take()?;
        let pty = self.pty.take()?;
        let mode = self.mode;
        let scroll = session.scrollbar().offset;
        let mut tab = BackgroundTab {
            session,
            pty,
            mode,
            command: self.running_cmd.clone(),
            started: self.running_started,
            duration: (mode != Mode::Running).then(|| self.running_started.elapsed()),
            exit: None,
            cwd: self.cwd.clone(),
            last_line: String::new(),
            blocks: std::mem::take(&mut self.blocks),
            scroll,
        };
        tab.session.scroll_to_bottom();
        tab.session.update_frame();
        tab.last_line = panel_last_line(&tab.session, mode);
        Some(tab)
    }

    /// Move the running foreground tab into the background panel and open a
    /// fresh shell at the same working directory.
    fn detach_running(&mut self) {
        if self.mode != Mode::Running {
            self.toast("Nothing is running to move to the background");
            return;
        }
        let Some(tab) = self.take_foreground() else {
            return;
        };
        self.background.push(tab);
        let cwd = self.cwd.clone();
        if let Err(err) = self.spawn_foreground(cwd) {
            self.toast(format!("Could not open a new shell: {err:#}"));
        }
        self.relayout();
        self.toast("Moved to the background");
        self.request_redraw();
    }

    /// Bring a background tab back to the foreground. An idle foreground shell
    /// is discarded; a running one is swapped into the panel so nothing is lost.
    fn attach_background(&mut self, index: usize) {
        if index >= self.background.len() {
            return;
        }
        let mut tab = self.background.remove(index);
        // Keep whatever the user was doing: the current tab moves into the
        // panel so the two tabs simply swap.
        if let Some(current) = self.take_foreground() {
            self.background.push(current);
        }

        tab.session.resize(
            self.grid.cols,
            self.grid.rows,
            self.text.cell_w.max(1.0) as u32,
            self.text.cell_h.max(1.0) as u32,
        );
        tab.pty.resize(
            self.grid.cols,
            self.grid.rows,
            self.text.cell_w.max(1.0) as u32,
            self.text.cell_h.max(1.0) as u32,
        );
        tab.session.update_frame();
        if tab.mode != Mode::Running {
            close_open_block(&mut tab.blocks, &mut tab.session, tab.exit);
            tab.session.scroll_to_row(tab.scroll);
        } else {
            tab.session.scroll_to_bottom();
        }
        tab.session.update_frame();
        self.session = Some(tab.session);
        self.pty = Some(tab.pty);
        self.mode = tab.mode;
        self.running_cmd = tab.command;
        self.running_started = tab.started;
        self.last_status = tab.exit.map(|code| (code, Instant::now()));
        self.cwd = tab.cwd;
        self.cwd_label.clear();
        self.blocks = tab.blocks;
        self.focus = if self.mode == Mode::Compose {
            Focus::Composer
        } else {
            Focus::Grid
        };
        self.got_prompt = true;
        self.sel_anchor = None;
        self.sel_drag = false;
        self.editor.clear();
        self.block_view = None;
        self.pending_highlight = None;
        self.hscroll = 0;
        self.composer_scroll = 0;
        self.grid_dirty = true;
        self.relayout();
        self.toast("Tab attached");
        self.request_redraw();
    }

    /// Kill and remove a background tab (the X button in the panel).
    fn kill_background(&mut self, index: usize) {
        if index >= self.background.len() {
            return;
        }
        let tab = self.background.remove(index);
        let command = tab.command.replace('\n', " ");
        drop(tab);
        self.relayout();
        if command.is_empty() {
            self.toast("Background shell closed");
        } else {
            self.toast(format!("Stopped {command}"));
        }
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
            self.begin_dock();
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
        if self.background.iter().any(|tab| tab.mode == Mode::Running) {
            next = next.min(now + Duration::from_millis(500));
            if now.duration_since(self.last_frame) >= Duration::from_millis(400) {
                self.request_redraw();
            }
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
        if self.dock_started.is_some() {
            self.request_redraw();
            next = next.min(now + Duration::from_millis(16));
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

        if self.trace_input {
            if let PhysicalKey::Code(code) = event.physical_key {
                self.trace(format!(
                    "key {:?} primary={} shift={} ctrl={} alt={} mode={:?} focus={:?}",
                    code, primary, shift, ctrl, alt, self.mode, self.focus
                ));
            }
        }

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
            // Reading mode for one command block.
            if primary && shift && code == KeyCode::KeyB {
                if self.block_view.is_some() {
                    self.close_block_view();
                } else {
                    self.open_block_view(None);
                }
                return;
            }
            if self.block_view.is_some() && self.handle_block_key(code, primary, ctrl, shift, event)
            {
                return;
            }
            // Move the running command to the background panel.
            if primary && shift && code == KeyCode::KeyD {
                self.detach_running();
                return;
            }
            if primary && shift && matches!(code, KeyCode::ArrowUp | KeyCode::ArrowDown) {
                let direction = if code == KeyCode::ArrowUp { -1 } else { 1 };
                self.jump_block(direction);
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
                    // In the composer, Ctrl+A means "select the command I'm
                    // editing". Only fall back to selecting the scrollback when
                    // the composer is empty, so output can still be copied.
                    if self.focus == Focus::Grid && self.editor.is_empty() {
                        self.trace("ctrl+a -> terminal select_all (composer empty)");
                        if let Some(session) = &mut self.session {
                            session.select_all();
                        }
                        self.grid_dirty = true;
                    } else {
                        self.trace("ctrl+a -> composer select_all");
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
                if !extend && !word_mod && !line_mod && self.accept_inline_suggestion() {
                    return true;
                }
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

    /// Number of composer lines currently on screen (1 when it is inactive).
    fn composer_lines(&self) -> usize {
        if self.block_view.is_some() || self.mode != Mode::Compose {
            1
        } else {
            self.editor.line_count().clamp(1, 10)
        }
    }

    /// Keep the grid out of the composer's way when a multi-line command is
    /// being edited. Cheap no-op unless the line count actually changed.
    fn sync_composer_layout(&mut self) {
        if self.layout_composer_lines != self.composer_lines() {
            self.relayout();
        }
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
        self.sync_composer_layout();
        self.request_redraw();
    }

    fn after_editor_motion(&mut self) {
        self.focus = Focus::Composer;
        self.cursor_on = true;
        self.last_blink = Instant::now();
        self.composer_scroll_to_caret();
        self.sync_composer_layout();
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
        self.begin_dock();
        if self.block_view.is_some() {
            self.close_block_view();
        }
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
        // Make sure the cursor is on-screen before recording the block row.
        if let Some(session) = &mut self.session {
            session.clear_selection();
            session.scroll_to_bottom();
        }
        self.start_block(text.clone());

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

        self.grid_dirty = true;
        self.editor.clear();
        self.hscroll = 0;
        self.composer_scroll = 0;
        self.sel_anchor = None;
        self.sel_drag = false;
        self.toast = None;
        self.sync_composer_layout();
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

    /// The dim preview shown after the caret. History wins over PATH so the
    /// commands someone actually runs come back first.
    fn inline_suggestion(&mut self) -> Option<String> {
        if !self.config.ui.inline_suggestions || self.mode != Mode::Compose {
            return None;
        }
        if self.editor.has_selection() {
            return None;
        }
        let text = self.editor.text().to_string();
        if text.is_empty() || self.editor.caret() != text.len() {
            return None;
        }
        history_suggestion(&text, text.len(), self.history.entries())
            .or_else(|| self.completion.command_suggestion(&text))
    }

    fn accept_inline_suggestion(&mut self) -> bool {
        let Some(rest) = self.inline_suggestion() else {
            return false;
        };
        self.editor.insert(&rest);
        self.history_index = None;
        self.after_edit();
        true
    }

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
            // No token completion available; fall back to the ghost text.
            self.accept_inline_suggestion();
            return;
        };
        if set.items.len() == 1 {
            // Unambiguous, so a longer history ghost may still win.
            if self.accept_inline_suggestion() {
                return;
            }
            self.apply_completion(&set, 0);
            return;
        }
        // Outside a command name the ghost usually covers the whole line, so
        // keep preferring it. For a command token, show every match instead of
        // silently taking the first PATH suggestion.
        if !set.command && self.accept_inline_suggestion() {
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

    /// Debug aid: `PROMPT_TRACE_INPUT=1` logs pointer and key handling to
    /// stderr so hit-testing problems can be diagnosed from a real session.
    fn trace(&self, msg: impl AsRef<str>) {
        if self.trace_input {
            eprintln!("[prompt] {}", msg.as_ref());
        }
    }

    fn trace_geometry(&self) -> String {
        format!(
            "size={}x{} scale={:.2} card=({:.0},{:.0},{:.0},{:.0}) text=({:.0},{:.0}) grid=({:.0},{:.0},{:.0},{:.0}) dock={:.3} mode={:?} focus={:?} tracking={} panel_w={:.0}",
            self.size.width,
            self.size.height,
            self.scale,
            self.card.x,
            self.card.y,
            self.card.w,
            self.card.h,
            self.card.text_x,
            self.card.text_y,
            self.grid.x,
            self.grid.y,
            self.grid.w,
            self.grid.h,
            self.dock_progress,
            self.mode,
            self.focus,
            self.session
                .as_ref()
                .map(|s| s.mouse_tracking())
                .unwrap_or(false),
            self.panel.w,
        )
    }

    fn zone_at(&self, x: f32, y: f32) -> &'static str {
        if self.mouse_in_panel(x, y) {
            "panel"
        } else if self.mouse_in_grid(x, y) {
            "grid"
        } else if self.mouse_in_card(x, y) {
            "card"
        } else {
            "none"
        }
    }

    fn handle_mouse_button(
        &mut self,
        state: ElementState,
        button: MouseButton,
        pos: PhysicalPosition<f64>,
    ) {
        let x = pos.x as f32;
        let y = pos.y as f32;
        self.trace(format!(
            "button {:?} {:?} at ({:.1},{:.1}) zone={} {}",
            state,
            button,
            x,
            y,
            self.zone_at(x, y),
            self.trace_geometry()
        ));
        if state == ElementState::Pressed {
            // A click outside a popup is a dismissal in every editor.
            self.completion_popup = None;
            self.search = None;
        }
        if state == ElementState::Pressed && self.mouse_in_panel(x, y) {
            if let Some(entry) = self.panel_entry_at(x, y) {
                if let Some((cx, cy, cw, ch)) = entry.close {
                    if x >= cx && x < cx + cw && y >= cy && y < cy + ch {
                        self.kill_background(entry.tab);
                        return;
                    }
                }
                self.attach_background(entry.tab);
            }
            return;
        }
        // The composer is checked before the terminal: a multi-line composer
        // grows upwards over the top of the grid, and clicks on those lines
        // belong to the editor, not to the scrollback behind it.
        if self.mouse_in_card(x, y) {
            if self.block_view.is_some() {
                self.close_block_view();
            }
            self.focus = Focus::Composer;
            if let Some(session) = &mut self.session {
                session.clear_selection();
            }
            self.grid_dirty = true;
            if state == ElementState::Pressed {
                if self.mode == Mode::Running {
                    if let Some((bx, by, bw, bh)) = self.detach_rect {
                        if x >= bx && x < bx + bw && y >= by && y < by + bh {
                            self.detach_running();
                            return;
                        }
                    }
                }
                let count = self.next_click_count(x, y);
                if let Some(index) = self.card_index(x, y) {
                    self.trace(format!(
                        "click in card -> focus=Composer line/col={:?} index={} clicks={}",
                        self.card_line_col(x, y),
                        index,
                        count
                    ));
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
                } else {
                    self.trace(format!(
                        "click in card but no index line/col={:?}",
                        self.card_line_col(x, y)
                    ));
                }
            } else {
                self.sel_drag = false;
            }
            self.request_redraw();
        } else if self.mouse_in_grid(x, y) {
            self.focus = Focus::Grid;
            if state == ElementState::Pressed {
                self.trace("click in grid -> focus=Grid");
            }
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
        let zone = self.zone_at(x, y);
        let zone_id = match zone {
            "panel" => 1,
            "grid" => 2,
            "card" => 3,
            _ => 0,
        };
        if zone_id != self.trace_zone {
            self.trace_zone = zone_id;
            self.trace(format!(
                "hover -> {} at ({:.1},{:.1}) {}",
                zone,
                x,
                y,
                self.trace_geometry()
            ));
        }
        if self.sel_drag {
            self.trace(format!(
                "drag at ({:.1},{:.1}) zone={}",
                x,
                y,
                self.zone_at(x, y)
            ));
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
        } else if self.mouse_in_panel(x, y)
            || self
                .detach_rect
                .map(|(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
                .unwrap_or(false)
        {
            if let Some(window) = &self.window {
                window.set_cursor(winit::window::CursorIcon::Pointer);
            }
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
        if self.mouse_in_panel(x, y) {
            self.scroll_panel(lines.round() as i64);
            return;
        }
        if self.block_view.is_some() && (self.mouse_in_grid(x, y) || self.mouse_in_card(x, y)) {
            self.scroll_block(lines.round() as i64);
            return;
        }
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
        self.dock_progress >= 0.999
            && x >= self.grid.x
            && y >= self.grid.y
            && x < self.grid.x + self.grid.w
            && y < self.grid.y + self.grid.h
    }

    fn mouse_in_card(&self, x: f32, y: f32) -> bool {
        let card = self.card;
        x >= card.x && x < card.x + card.w && y >= card.y && y < card.y + card.h
    }

    fn mouse_in_panel(&self, x: f32, y: f32) -> bool {
        self.panel.w > 0.0
            && x >= self.panel.x
            && x < self.panel.x + self.panel.w
            && y >= self.panel.y
            && y < self.panel.y + self.panel.h
    }

    /// Number of background-tab rows the panel can show and the largest valid
    /// first-row offset. Kept in one place so drawing and wheel scrolling agree.
    fn panel_scroll_range(&self) -> (usize, usize) {
        let pad = self.s(10.0);
        let gap = self.s(8.0);
        let entry_h = pad * 2.0 + self.text.cell_h * 3.0 + self.s(4.0);
        let top = self.panel.y + pad + self.text.cell_h + gap;
        let bottom = self.panel.y + self.panel.h - pad;
        let visible = if self.panel.w <= 0.0 || bottom - top < entry_h {
            0
        } else {
            (((bottom - top - entry_h) / (entry_h + gap)).floor() as usize) + 1
        };
        let max_scroll = if visible == 0 {
            0
        } else {
            self.background.len().saturating_sub(visible)
        };
        (visible, max_scroll)
    }

    fn scroll_panel(&mut self, lines: i64) {
        let (_, max_scroll) = self.panel_scroll_range();
        let next = (self.panel_scroll as i64 + lines).clamp(0, max_scroll as i64) as usize;
        if next != self.panel_scroll {
            self.panel_scroll = next;
            self.request_redraw();
        }
    }

    fn panel_entry_at(&self, x: f32, y: f32) -> Option<PanelEntry> {
        if !self.mouse_in_panel(x, y) {
            return None;
        }
        self.panel.entries.iter().copied().find(|entry| {
            y >= entry.y
                && y < entry.y + entry.h
                && x >= self.panel.x
                && x < self.panel.x + self.panel.w
        })
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
        let last_line = self.editor.line_count().saturating_sub(1);
        // Clamp to the text: clicks in the card's padding or on the hint row
        // should still land on the nearest line instead of being ignored.
        let line = (((y - self.card.text_y) / self.text.cell_h).floor().max(0.0) as usize
            + self.composer_scroll)
            .min(last_line);
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
        self.advance_dock();
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

        self.apply_block_view();
        if let Some(session) = &mut self.session {
            session.update_frame();
        }
        self.apply_pending_highlight();
        if self.grid_dirty {
            if let Some(cache) = &mut self.grid_cache {
                cache.fill(self.theme.bg.solid());
            }
            self.draw_grid();
            self.grid_dirty = false;
        }
        let reveal = self.terminal_reveal();
        if let (Some(canvas), Some(cache)) = (&mut self.canvas, &self.grid_cache) {
            if reveal >= 0.999 {
                canvas.data_mut().copy_from_slice(cache.data());
            } else if reveal > 0.0 {
                let paint = PixmapPaint {
                    opacity: reveal,
                    ..PixmapPaint::default()
                };
                canvas.draw_pixmap(0, 0, cache.as_ref(), &paint, Transform::identity(), None);
            }
        }
        self.draw_block_bar();
        self.draw_card();
        self.draw_background_panel();
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
        let scale = self.scale;
        let theme = self.theme.clone();
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale: self.scale,
        };

        painter.fill(grid.x, grid.y, grid.w, grid.h, frame.background, 1.0);

        // Command blocks: a tinted command row plus a rail in the left margin,
        // so commands stay distinguishable while scrolling through scrollback.
        if !self.blocks.is_empty() && !frame.alt_screen {
            let offset = frame.scrollbar.offset;
            let row_count = frame.rows as u64;
            let visible = offset..offset.saturating_add(row_count);
            for block in &self.blocks {
                let header = block.start_row;
                let end = block
                    .end_row
                    .map(|row| row.saturating_sub(1))
                    .unwrap_or(offset.saturating_add(row_count.saturating_sub(1)));
                // A full-screen program can repaint the rows a block used to
                // occupy (top, for example, stays on the primary screen). Only
                // decorate the block when its command line is still there.
                let expected: String = block
                    .command
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(24)
                    .collect();
                if expected.is_empty() {
                    continue;
                }
                let header_text = if visible.contains(&header) {
                    frame.row_text((header - offset) as u16)
                } else {
                    None
                };
                let header_ok = header_text
                    .as_deref()
                    .map(|text| text.contains(&expected))
                    .unwrap_or(false);
                if !header_ok {
                    continue;
                }
                let color = match block.status {
                    None => theme.accent,
                    Some(0) => theme.dim,
                    Some(_) => theme.red,
                };
                if visible.contains(&header) {
                    let y = grid.y + (header - offset) as f32 * cell_h;
                    painter.fill(grid.x, y, grid.w, cell_h, theme.accent, 0.06);
                    painter.rounded(
                        grid.x - 12.0 * scale,
                        y + (cell_h - 6.0 * scale) / 2.0,
                        6.0 * scale,
                        6.0 * scale,
                        3.0 * scale,
                        color,
                        1.0,
                    );
                }
                let first = header.max(offset);
                let last = end.min(visible.end.saturating_sub(1));
                if last >= first {
                    let top = grid.y + (first - offset) as f32 * cell_h + cell_h * 0.15;
                    let height = (last - first + 1) as f32 * cell_h - cell_h * 0.3;
                    painter.fill(
                        grid.x - 8.0 * scale,
                        top,
                        (2.0 * scale).max(1.0),
                        height,
                        color,
                        0.5,
                    );
                }
            }
        }

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
        let block_view = self.block_view.is_some();
        let lines = if block_view {
            1
        } else if mode == Mode::Compose {
            self.editor.line_count().clamp(1, 10)
        } else {
            1
        };
        let card_h = pad * 2.0 + lines as f32 * cell_h + s(6.0) + hint_h;
        let dock = self.dock_progress.clamp(0.0, 1.0);
        let docked_w = (width - margin * 2.0).max(1.0);
        let centred_w = (760.0 * scale).min(docked_w);
        let card_w = centred_w + (docked_w - centred_w) * dock;
        let card_x = margin + (docked_w - card_w) / 2.0;
        let docked_y = height - margin - card_h;
        let centred_y = ((height - card_h) / 2.0).max(margin);
        let card_y = centred_y + (docked_y - centred_y) * dock;

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
        let suggestion = if mode == Mode::Compose && !block_view {
            self.inline_suggestion()
        } else {
            None
        };
        let block_line = if block_view {
            Some(self.block_status_line())
        } else {
            None
        };

        self.card = CardLayout {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
            text_x,
            text_y,
        };
        self.detach_rect = None;

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
            let (glyph, glyph_color) = if block_view {
                ("◆", theme.accent)
            } else {
                match mode {
                    Mode::Compose => ("❯", theme.accent),
                    Mode::Running => ("●", theme.yellow),
                    Mode::Direct => ("!", theme.red),
                }
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
        if let Some((label, color)) = block_line {
            painter.text(
                text_x,
                text_y,
                &label,
                color,
                false,
                None,
                Clip::new(text_x, text_y, text_w, cell_h),
                text_w,
            );
        } else if mode == Mode::Compose {
            draw_editor(
                &mut painter,
                &theme,
                &self.editor,
                self.focus,
                self.cursor_on,
                &self.ime_preedit,
                suggestion.as_deref(),
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
            let btn = "background";
            let btn_w = painter.text.measure_text(btn, false) + s(16.0);
            let btn_h = (cell_h - s(2.0)).max(s(10.0));
            let by = text_y + (cell_h - btn_h) / 2.0;
            let bx = text_x + text_w - btn_w;
            let label_w = painter.text.measure_text(&label, false);
            let show_button = text_x + label_w + s(12.0) < bx;
            let label_w = if show_button {
                (text_w - btn_w - s(10.0)).max(10.0)
            } else {
                text_w
            };
            painter.text(
                text_x,
                text_y,
                &label,
                theme.dim,
                false,
                None,
                Clip::new(text_x, text_y, label_w, cell_h),
                label_w,
            );
            if show_button {
                let btn_clip = Clip::new(bx, by, btn_w, btn_h);
                painter.rounded(bx, by, btn_w, btn_h, s(5.0), theme.panel_raised, 1.0);
                painter.text(
                    bx + s(8.0),
                    by + (btn_h - cell_h) / 2.0,
                    btn,
                    theme.dim,
                    false,
                    None,
                    btn_clip,
                    btn_w,
                );
                self.detach_rect = Some((bx, by, btn_w, btn_h));
            }
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
        let long_hint = if block_view {
            "esc exit   [ ] blocks   g/G top/bottom   e expand   / search   ↑↓ scroll"
        } else {
            match mode {
                Mode::Compose => {
                    if keys::is_macos() {
                        "enter run   shift+enter newline   ↑↓ history   ⌘R search   ⌘V paste"
                    } else {
                        "enter run   shift+enter newline   ↑↓ history   ^R search   ^V paste"
                    }
                }
                Mode::Running => "running — ctrl-c to interrupt   ctrl-shift-d background",
                Mode::Direct => "this shell has no prompt integration; use zsh or bash",
            }
        };
        let short_hint = if block_view {
            "esc exit   / search   e expand"
        } else {
            match mode {
                Mode::Compose => "enter run   ↑ history   tab autocomplete",
                Mode::Running => "running — ctrl-c stops   ctrl-shift-d background",
                Mode::Direct => "classic input — zsh/bash enable the composer",
            }
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
    suggestion: Option<&str>,
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

    // Inline autocomplete: dim ghost text after the caret. Only single-line
    // buffers at the end of the text produce a suggestion, so it never covers
    // content the user has already typed.
    if let Some(rest) = suggestion {
        if preedit.is_empty() && caret_line == 0 {
            let cx = x + (caret_col as f32 - *hscroll as f32) * cell_w;
            painter.text(
                cx,
                y,
                rest,
                theme.faint,
                false,
                Some(cell_w),
                text_clip,
                (w - (cx - x)).max(10.0),
            );
        }
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
    fn draw_block_bar(&mut self) {
        let Some(view) = &self.block_view else {
            return;
        };
        let index = view.index;
        let expanded = view.expanded;
        let total = self
            .block_range(index)
            .map(|(start, end)| end.saturating_sub(start) + 1)
            .unwrap_or(0);
        let shown = self
            .block_window(index, expanded)
            .map(|(start, end)| end.saturating_sub(start) + 1)
            .unwrap_or(0);
        let status = self.blocks.get(index).and_then(|block| block.status);
        let search = view.search.as_ref().map(|search| {
            (
                search.query.clone(),
                search.matches.len(),
                search.selected,
                search.active,
            )
        });

        let mut text = format!(
            "◆ block {}/{}  ·  {total} line{}",
            index + 1,
            self.blocks.len(),
            if total == 1 { "" } else { "s" }
        );
        if shown < total {
            text.push_str(&format!("  ·  {shown} shown"));
        }
        match status {
            Some(0) => text.push_str("  ·  exit 0"),
            Some(code) => text.push_str(&format!("  ·  exit {code}")),
            None => text.push_str("  ·  running"),
        }
        if let Some((query, count, selected, active)) = search {
            if active || count > 0 {
                let current = if count == 0 { 0 } else { selected + 1 };
                text.push_str(&format!("  ·  /{query} {current}/{count}"));
            }
        }

        let cell_h = self.text.cell_h;
        let scale = self.scale;
        let grid = self.grid;
        let theme = self.theme.clone();
        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale,
        };
        let text_w = painter.text.measure_text(&text, false);
        let pad = 8.0 * scale;
        let w = (text_w + pad * 2.0).min(grid.w).max(40.0 * scale);
        let h = cell_h + 2.0 * scale;
        let x = grid.x;
        let y = (2.0 * scale).min((grid.y - h).max(1.0));
        painter.rounded(x, y, w, h, 5.0 * scale, theme.panel_raised, 1.0);
        painter.rounded_stroke(x, y, w, h, 5.0 * scale, scale.max(1.0), theme.border, 0.9);
        painter.text(
            x + pad,
            y + (h - cell_h) / 2.0,
            &text,
            theme.dim,
            false,
            None,
            Clip::new(x, y, w, h),
            w - pad * 2.0,
        );
    }

    fn draw_background_panel(&mut self) {
        let panel = self.panel.clone();
        if panel.w <= 0.0 || self.background.is_empty() {
            self.panel.entries.clear();
            return;
        }
        let cell_h = self.text.cell_h;
        let scale = self.scale;
        let s = move |v: f32| (v * scale).round();
        let theme = self.theme.clone();
        let pad = s(10.0);
        let entry_h = pad * 2.0 + cell_h * 3.0 + s(4.0);
        let close = s(18.0);

        let rows: Vec<(String, String, Rgb, String, bool)> = self
            .background
            .iter()
            .map(|tab| {
                let idle = tab.command.is_empty() && tab.mode != Mode::Running;
                let (status, color) = if idle {
                    ("idle".to_string(), theme.dim)
                } else if tab.mode == Mode::Running {
                    (
                        format!("running · {:.1}s", tab.started.elapsed().as_secs_f32()),
                        theme.yellow,
                    )
                } else if let Some(code) = tab.exit {
                    if code == 0 {
                        ("finished · exit 0".to_string(), theme.dim)
                    } else {
                        (format!("failed · exit {code}"), theme.red)
                    }
                } else if let Some(duration) = tab.duration {
                    (
                        format!("finished · {:.1}s", duration.as_secs_f32()),
                        theme.dim,
                    )
                } else {
                    ("waiting".to_string(), theme.dim)
                };
                let command = if tab.command.is_empty() {
                    format!("shell · {}", display_path(&tab.cwd))
                } else {
                    tab.command.replace('\n', " ⏎ ")
                };
                let log = if tab.last_line.is_empty() {
                    if idle {
                        "ready".to_string()
                    } else {
                        "no output yet".to_string()
                    }
                } else {
                    tab.last_line.replace('\n', " ")
                };
                (command, status, color, log, tab.mode == Mode::Running)
            })
            .collect();

        let (visible, max_scroll) = self.panel_scroll_range();
        self.panel_scroll = self.panel_scroll.min(max_scroll);
        let first = self.panel_scroll;
        let total = rows.len();

        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale,
        };

        painter.rounded(
            panel.x,
            panel.y + s(4.0),
            panel.w,
            panel.h,
            s(12.0),
            theme.shadow,
            0.45,
        );
        painter.rounded(
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            s(12.0),
            theme.panel,
            1.0,
        );
        painter.rounded_stroke(
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            s(12.0),
            scale.max(1.0),
            theme.border,
            0.9,
        );
        let header = format!("Background · {}", rows.len());
        painter.text(
            panel.x + pad,
            panel.y + pad,
            &header,
            theme.accent,
            true,
            None,
            Clip::new(panel.x, panel.y, panel.w, panel.h),
            panel.w - pad * 2.0,
        );

        let mut entries = Vec::new();
        let list_top = panel.y + pad + cell_h + s(8.0);
        let mut y = list_top;
        let bottom = panel.y + panel.h - pad;
        for (index, (command, status, color, log, running)) in
            rows.into_iter().enumerate().skip(first)
        {
            if y + entry_h > bottom {
                break;
            }
            let x = panel.x + pad;
            let w = panel.w - pad * 2.0;
            painter.rounded(x, y, w, entry_h, s(8.0), theme.panel_raised, 1.0);
            if running {
                painter.fill(
                    x + s(1.0),
                    y + s(6.0),
                    s(2.0),
                    entry_h - s(12.0),
                    theme.yellow,
                    0.8,
                );
            }

            let text_w = (w - pad * 2.0 - close).max(10.0);
            let clip = Clip::new(x + pad, y + pad, text_w, cell_h * 3.0);
            painter.text(
                x + pad,
                y + pad,
                &command,
                theme.fg,
                true,
                None,
                clip,
                text_w,
            );
            painter.text(
                x + pad,
                y + pad + cell_h,
                &status,
                color,
                false,
                None,
                clip,
                text_w,
            );
            painter.text(
                x + pad,
                y + pad + cell_h * 2.0,
                &format!("↳ {log}"),
                theme.faint,
                false,
                None,
                Clip::new(x + pad, y + pad + cell_h * 2.0, w - pad * 2.0, cell_h),
                w - pad * 2.0,
            );

            let cx = x + w - close - s(6.0);
            let cy = y + s(6.0);
            painter.rounded(cx, cy, close, close, close / 2.0, theme.panel, 1.0);
            let glyph = "×";
            let glyph_w = painter.text.measure_text(glyph, false);
            painter.text(
                cx + (close - glyph_w) / 2.0,
                cy + (close - cell_h) / 2.0,
                glyph,
                theme.dim,
                false,
                None,
                Clip::new(cx, cy, close, close),
                close,
            );

            entries.push(PanelEntry {
                tab: index,
                y,
                h: entry_h,
                close: Some((cx, cy, close, close)),
            });
            y += entry_h + s(8.0);
        }
        if max_scroll > 0 && visible > 0 {
            let track_h = bottom - list_top;
            let track_x = panel.x + panel.w - s(5.0);
            let track_w = s(3.0);
            painter.rounded(
                track_x,
                list_top,
                track_w,
                track_h,
                track_w / 2.0,
                theme.border,
                0.35,
            );
            let thumb_h = (track_h * visible as f32 / total as f32)
                .max(s(24.0))
                .min(track_h);
            let progress = first as f32 / max_scroll as f32;
            painter.rounded(
                track_x,
                list_top + (track_h - thumb_h) * progress,
                track_w,
                thumb_h,
                track_w / 2.0,
                theme.dim,
                0.65,
            );
        }
        drop(painter);
        self.panel.entries = entries;
    }

    fn draw_welcome(&mut self) {
        if !self.welcome || self.dock_progress > 0.01 {
            return;
        }
        let Some(canvas) = self.canvas.as_mut() else {
            return;
        };
        let width = self.size.width as f32;
        let height = self.size.height as f32;
        let card = self.card;
        let cell_h = self.text.cell_h;
        let scale = self.scale;
        let theme = self.theme.clone();
        let mut painter = Painter {
            pixmap: canvas,
            text: &mut self.text,
            scale: self.scale,
        };
        let clip = Clip::new(0.0, 0.0, width, height);
        let title_y = (card.y - cell_h * 4.0).max(8.0 * scale);
        let title = "Prompt";
        let title_w = painter.text.measure_text(title, true);
        painter.text(
            (width - title_w) / 2.0,
            title_y,
            title,
            theme.accent,
            true,
            None,
            clip,
            width,
        );
        let subtitle = "A terminal that types like an editor.";
        let subtitle_w = painter.text.measure_text(subtitle, false);
        painter.text(
            (width - subtitle_w) / 2.0,
            title_y + cell_h * 1.5,
            subtitle,
            theme.faint,
            false,
            None,
            clip,
            width,
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

/// Mark the newest open block as finished at the current cursor row.
fn close_open_block(blocks: &mut [Block], session: &mut TerminalSession, status: Option<i32>) {
    let Some(block) = blocks.last_mut() else {
        return;
    };
    if block.end_row.is_some() {
        return;
    }
    session.update_frame();
    let row = session.scrollbar().total.saturating_sub(1);
    block.end_row = Some(row.max(block.start_row));
    block.status = status;
}

/// Last useful output line for the background panel. A running command shows
/// its newest line; a finished one skips the prompt printed underneath.
fn panel_last_line(session: &TerminalSession, mode: Mode) -> String {
    let line = if mode == Mode::Running {
        session.last_line()
    } else {
        session.last_line_before_cursor()
    };
    if line.is_empty() {
        session.last_line()
    } else {
        line
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
                    if !text.is_empty() {
                        if let Some(search) = self
                            .block_view
                            .as_mut()
                            .and_then(|view| view.search.as_mut())
                            .filter(|search| search.active)
                        {
                            search.query.push_str(&text);
                            self.request_redraw();
                        } else if self.mode == Mode::Compose {
                            self.editor.insert(&text);
                            self.after_edit();
                        }
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
