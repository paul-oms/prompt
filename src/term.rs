//! Safe wrapper around libghostty-vt, Ghostty's terminal emulation core.
//!
//! Prompt does not reimplement VT parsing. It embeds Ghostty's C library for
//! the screen, scrollback, selection, key/mouse encoding and semantic-prompt
//! tracking, and draws the resulting render state itself.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Result};

use crate::ffi;
use crate::theme::{Rgb, Theme};

macro_rules! sized {
    ($t:ty) => {{
        #[allow(clippy::uninit_assumed_init, unused_unsafe)]
        let mut value: $t = unsafe { ::std::mem::zeroed() };
        value.size = std::mem::size_of::<$t>();
        value
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CellFlags {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub invisible: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct CellView {
    pub x: u16,
    pub len: u8,
    pub cps: [u32; 8],
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub underline: Option<Rgb>,
    pub flags: CellFlags,
}

impl Default for CellView {
    fn default() -> Self {
        Self {
            x: 0,
            len: 0,
            cps: [0; 8],
            fg: Rgb(0, 0, 0),
            bg: None,
            underline: None,
            flags: CellFlags::default(),
        }
    }
}

impl CellView {
    pub fn text(&self) -> String {
        self.cps[..self.len as usize]
            .iter()
            .filter_map(|cp| char::from_u32(*cp))
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CursorView {
    pub x: u16,
    pub y: u16,
    pub blinking: bool,
    pub style: CursorStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Scrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

impl Scrollbar {
    pub fn at_bottom(&self) -> bool {
        self.offset + self.len >= self.total.saturating_sub(1)
    }
}

#[derive(Default)]
pub struct RowView {
    pub y: u16,
    pub selection: Option<(u16, u16)>,
    pub cells: Vec<CellView>,
}

pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Option<CursorView>,
    pub rows_view: Vec<RowView>,
    pub scrollbar: Scrollbar,
    pub alt_screen: bool,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            cols: 0,
            rows: 0,
            background: Rgb(0, 0, 0),
            foreground: Rgb(255, 255, 255),
            cursor: None,
            rows_view: Vec::new(),
            scrollbar: Scrollbar::default(),
            alt_screen: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TermEvent {
    PromptStart,
    InputStart,
    OutputStart { command: String },
    CommandEnd { exit_code: Option<i32> },
    Bell,
    TitleChanged,
    PwdChanged,
}

struct CallbackState {
    pending_pty: Mutex<Vec<u8>>,
    events: Mutex<Vec<TermEvent>>,
    pwd: Mutex<Option<String>>,
    title: Mutex<Option<String>>,
    bell: AtomicBool,
}

impl CallbackState {
    fn new() -> Self {
        Self {
            pending_pty: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            pwd: Mutex::new(None),
            title: Mutex::new(None),
            bell: AtomicBool::new(false),
        }
    }
}

unsafe extern "C" fn cb_write_pty(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    if userdata.is_null() || data.is_null() || len == 0 {
        return;
    }
    let state = &*(userdata as *const CallbackState);
    if let Ok(mut pending) = state.pending_pty.lock() {
        pending.extend_from_slice(std::slice::from_raw_parts(data, len));
    }
}

unsafe extern "C" fn cb_bell(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    if userdata.is_null() {
        return;
    }
    let state = &*(userdata as *const CallbackState);
    state.bell.store(true, Ordering::Relaxed);
    if let Ok(mut events) = state.events.lock() {
        events.push(TermEvent::Bell);
    }
}

unsafe fn read_string(
    terminal: ffi::GhosttyTerminal,
    data: ffi::GhosttyTerminalData,
) -> Option<String> {
    let mut s: ffi::GhosttyString = std::mem::zeroed();
    let result = ffi::ghostty_terminal_get(terminal, data, &mut s as *mut _ as *mut c_void);
    if result != ffi::GHOSTTY_SUCCESS || s.ptr.is_null() || s.len == 0 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(s.ptr, s.len);
    Some(String::from_utf8_lossy(bytes).into_owned())
}

unsafe extern "C" fn cb_pwd(terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    if userdata.is_null() {
        return;
    }
    let state = &*(userdata as *const CallbackState);
    if let Some(pwd) = read_string(terminal, ffi::GHOSTTY_TERMINAL_DATA_PWD) {
        if let Ok(mut guard) = state.pwd.lock() {
            *guard = Some(pwd);
        }
        if let Ok(mut events) = state.events.lock() {
            events.push(TermEvent::PwdChanged);
        }
    }
}

unsafe extern "C" fn cb_title(terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    if userdata.is_null() {
        return;
    }
    let state = &*(userdata as *const CallbackState);
    if let Some(title) = read_string(terminal, ffi::GHOSTTY_TERMINAL_DATA_TITLE) {
        if let Ok(mut guard) = state.title.lock() {
            *guard = Some(title);
        }
        if let Ok(mut events) = state.events.lock() {
            events.push(TermEvent::TitleChanged);
        }
    }
}

unsafe extern "C" fn cb_semantic(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    event: *const ffi::GhosttyTerminalSemanticPrompt,
) {
    if userdata.is_null() || event.is_null() {
        return;
    }
    let state = &*(userdata as *const CallbackState);
    let event = &*event;
    let term_event = match event.kind {
        ffi::GHOSTTY_SEMANTIC_PROMPT_PROMPT_START => TermEvent::PromptStart,
        ffi::GHOSTTY_SEMANTIC_PROMPT_INPUT_START => TermEvent::InputStart,
        ffi::GHOSTTY_SEMANTIC_PROMPT_OUTPUT_START => {
            let command = if event.command.ptr.is_null() || event.command.len == 0 {
                String::new()
            } else {
                let bytes = std::slice::from_raw_parts(event.command.ptr, event.command.len);
                String::from_utf8_lossy(bytes).into_owned()
            };
            TermEvent::OutputStart { command }
        }
        ffi::GHOSTTY_SEMANTIC_PROMPT_COMMAND_END => TermEvent::CommandEnd {
            exit_code: event.has_exit_code.then_some(event.exit_code),
        },
        _ => return,
    };
    if let Ok(mut events) = state.events.lock() {
        events.push(term_event);
    }
}

pub struct TerminalSession {
    term: ffi::GhosttyTerminal,
    render: ffi::GhosttyRenderState,
    row_iter: ffi::GhosttyRenderStateRowIterator,
    cells: ffi::GhosttyRenderStateRowCells,
    encoder: ffi::GhosttyKeyEncoder,
    key_event: ffi::GhosttyKeyEvent,
    callbacks: Box<CallbackState>,
    frame: Frame,
    pub cols: u16,
    pub rows: u16,
    cell_w: u32,
    cell_h: u32,
    dirty: bool,
}

// The raw handles are only touched from the UI thread.
unsafe impl Send for TerminalSession {}

impl TerminalSession {
    pub fn new(
        theme: &Theme,
        cols: u16,
        rows: u16,
        cell_w: u32,
        cell_h: u32,
        scrollback_lines: usize,
    ) -> Result<Self> {
        unsafe {
            let mut term: ffi::GhosttyTerminal = std::ptr::null_mut();
            let result = ffi::ghostty_terminal_new(std::ptr::null(), &mut term, cols, rows);
            if result != ffi::GHOSTTY_SUCCESS || term.is_null() {
                bail!("ghostty_terminal_new failed ({result})");
            }

            let mut render: ffi::GhosttyRenderState = std::ptr::null_mut();
            let result = ffi::ghostty_render_state_new(std::ptr::null(), &mut render);
            if result != ffi::GHOSTTY_SUCCESS || render.is_null() {
                ffi::ghostty_terminal_free(term);
                bail!("ghostty_render_state_new failed ({result})");
            }

            let mut row_iter: ffi::GhosttyRenderStateRowIterator = std::ptr::null_mut();
            ffi::ghostty_render_state_row_iterator_new(std::ptr::null(), &mut row_iter);
            let mut cells: ffi::GhosttyRenderStateRowCells = std::ptr::null_mut();
            ffi::ghostty_render_state_row_cells_new(std::ptr::null(), &mut cells);
            let mut encoder: ffi::GhosttyKeyEncoder = std::ptr::null_mut();
            ffi::ghostty_key_encoder_new(std::ptr::null(), &mut encoder);
            let mut key_event: ffi::GhosttyKeyEvent = std::ptr::null_mut();
            ffi::ghostty_key_event_new(std::ptr::null(), &mut key_event);

            let callbacks = Box::new(CallbackState::new());
            let mut session = Self {
                term,
                render,
                row_iter,
                cells,
                encoder,
                key_event,
                callbacks,
                frame: Frame::default(),
                cols,
                rows,
                cell_w,
                cell_h,
                dirty: true,
            };
            session.install_colors(theme);
            let lines = scrollback_lines;
            session.set_opt(ffi::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &lines);
            // Ghostty also has a byte limit, and its default is small enough to
            // prune long command output after a few hundred lines. Raise it so
            // the configured line limit is what users actually see.
            let bytes: usize = 128 * 1024 * 1024;
            session.set_opt(ffi::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &bytes);
            session.install_callbacks();
            session.resize(cols, rows, cell_w, cell_h);
            session.update_frame();
            Ok(session)
        }
    }

    fn install_colors(&mut self, theme: &Theme) {
        let to_rgb = |c: Rgb| ffi::GhosttyColorRgb {
            r: c.0,
            g: c.1,
            b: c.2,
        };
        let fg = to_rgb(theme.fg);
        let bg = to_rgb(theme.bg);
        let cursor = to_rgb(theme.cursor);
        self.set_opt(ffi::GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &fg);
        self.set_opt(ffi::GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &bg);
        self.set_opt(ffi::GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &cursor);

        let mut palette = [ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 }; 256];
        for (i, slot) in palette.iter_mut().enumerate() {
            let rgb = theme.palette_color(i as u8);
            *slot = to_rgb(rgb);
        }
        self.set_opt(ffi::GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, &palette);
    }

    unsafe fn install_callbacks(&mut self) {
        // Callback and userdata options take the pointer value itself (the C
        // API casts it straight to the target type), unlike color/size options
        // which take a pointer to their value.
        let userdata = self.callbacks.as_mut() as *mut CallbackState as *const c_void;
        ffi::ghostty_terminal_set(self.term, ffi::GHOSTTY_TERMINAL_OPT_USERDATA, userdata);

        let write_pty = cb_write_pty as *const () as *const c_void;
        ffi::ghostty_terminal_set(self.term, ffi::GHOSTTY_TERMINAL_OPT_WRITE_PTY, write_pty);
        let pwd = cb_pwd as *const () as *const c_void;
        ffi::ghostty_terminal_set(self.term, ffi::GHOSTTY_TERMINAL_OPT_PWD_CHANGED, pwd);
        let title = cb_title as *const () as *const c_void;
        ffi::ghostty_terminal_set(self.term, ffi::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED, title);
        let bell = cb_bell as *const () as *const c_void;
        ffi::ghostty_terminal_set(self.term, ffi::GHOSTTY_TERMINAL_OPT_BELL, bell);
        let semantic = cb_semantic as *const () as *const c_void;
        ffi::ghostty_terminal_set(
            self.term,
            ffi::GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT,
            semantic,
        );
    }

    fn set_opt<T>(&mut self, option: ffi::GhosttyTerminalOption, value: &T) {
        unsafe {
            ffi::ghostty_terminal_set(self.term, option, value as *const T as *const c_void);
        }
    }

    fn get<T: Copy>(&self, data: ffi::GhosttyTerminalData, default: T) -> T {
        let mut value = default;
        let result = unsafe {
            ffi::ghostty_terminal_get(self.term, data, &mut value as *mut T as *mut c_void)
        };
        if result == ffi::GHOSTTY_SUCCESS {
            value
        } else {
            default
        }
    }

    /// Encode a key event using Ghostty's encoder, synced with the terminal's
    /// current modes (application cursor keys, Kitty keyboard protocol, ...).
    pub fn encode_key(
        &mut self,
        key: ffi::GhosttyKey,
        mods: ffi::GhosttyMods,
        action: ffi::GhosttyKeyAction,
        text: Option<&str>,
    ) -> Vec<u8> {
        if self.encoder.is_null() || self.key_event.is_null() {
            return Vec::new();
        }
        unsafe {
            ffi::ghostty_key_encoder_setopt_from_terminal(self.encoder, self.term);
            ffi::ghostty_key_event_set_action(self.key_event, action);
            ffi::ghostty_key_event_set_key(self.key_event, key);
            ffi::ghostty_key_event_set_mods(self.key_event, mods);
            match text {
                Some(text) if !text.is_empty() => {
                    ffi::ghostty_key_event_set_utf8(
                        self.key_event,
                        text.as_ptr() as *const std::os::raw::c_char,
                        text.len(),
                    );
                }
                _ => {
                    ffi::ghostty_key_event_set_utf8(self.key_event, std::ptr::null(), 0);
                }
            }
            let mut buf = [0u8; 256];
            let mut len: usize = 0;
            let result = ffi::ghostty_key_encoder_encode(
                self.encoder,
                self.key_event,
                buf.as_mut_ptr() as *mut std::os::raw::c_char,
                buf.len(),
                &mut len,
            );
            if result == ffi::GHOSTTY_SUCCESS && len <= buf.len() {
                buf[..len].to_vec()
            } else {
                Vec::new()
            }
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        unsafe {
            ffi::ghostty_terminal_vt_write(self.term, bytes.as_ptr(), bytes.len());
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) {
        if cols == 0 || rows == 0 {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        unsafe {
            ffi::ghostty_terminal_resize(self.term, cols, rows, cell_w as u32, cell_h as u32);
        }
        self.dirty = true;
    }

    pub fn set_cell_size(&mut self, cell_w: u32, cell_h: u32) {
        if cell_w != self.cell_w || cell_h != self.cell_h {
            self.resize(self.cols, self.rows, cell_w, cell_h);
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        let mut viewport: ffi::GhosttyTerminalScrollViewport = unsafe { std::mem::zeroed() };
        viewport.tag = ffi::GHOSTTY_SCROLL_VIEWPORT_DELTA;
        viewport.value.delta = delta as isize;
        unsafe {
            ffi::ghostty_terminal_scroll_viewport(self.term, viewport);
        }
        self.dirty = true;
    }

    pub fn scroll_to_bottom(&mut self) {
        let mut viewport: ffi::GhosttyTerminalScrollViewport = unsafe { std::mem::zeroed() };
        viewport.tag = ffi::GHOSTTY_SCROLL_VIEWPORT_BOTTOM;
        unsafe {
            ffi::ghostty_terminal_scroll_viewport(self.term, viewport);
        }
        self.dirty = true;
    }

    /// Scroll so `row` (an absolute scrollback row) is the first visible row.
    pub fn scroll_to_row(&mut self, row: u64) {
        let mut viewport: ffi::GhosttyTerminalScrollViewport = unsafe { std::mem::zeroed() };
        viewport.tag = ffi::GHOSTTY_SCROLL_VIEWPORT_ROW;
        viewport.value.row = row as usize;
        unsafe {
            ffi::ghostty_terminal_scroll_viewport(self.term, viewport);
        }
        self.dirty = true;
    }

    pub fn scrollbar(&self) -> Scrollbar {
        let mut sb: ffi::GhosttyTerminalScrollbar = unsafe { std::mem::zeroed() };
        let result = unsafe {
            ffi::ghostty_terminal_get(
                self.term,
                ffi::GHOSTTY_TERMINAL_DATA_SCROLLBAR,
                &mut sb as *mut _ as *mut c_void,
            )
        };
        if result == ffi::GHOSTTY_SUCCESS {
            Scrollbar {
                total: sb.total,
                offset: sb.offset,
                len: sb.len,
            }
        } else {
            Scrollbar::default()
        }
    }

    pub fn is_alt_screen(&self) -> bool {
        self.get::<u32>(ffi::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, 0)
            == ffi::GHOSTTY_TERMINAL_SCREEN_ALTERNATE as u32
    }

    pub fn title(&self) -> Option<String> {
        self.callbacks
            .title
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn pwd(&self) -> Option<String> {
        self.callbacks
            .pwd
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn take_pty_replies(&mut self) -> Vec<u8> {
        self.callbacks
            .pending_pty
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default()
    }

    pub fn drain_events(&mut self) -> Vec<TermEvent> {
        self.callbacks
            .events
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default()
    }

    pub fn mode(&self, mode: ffi::GhosttyMode, default: bool) -> bool {
        let mut config = ffi::GhosttyTerminalModeConfig {
            mode,
            value: default,
        };
        let result = unsafe {
            ffi::ghostty_terminal_get(
                self.term,
                ffi::GHOSTTY_TERMINAL_DATA_MODE,
                &mut config as *mut _ as *mut c_void,
            )
        };
        if result == ffi::GHOSTTY_SUCCESS {
            config.value
        } else {
            default
        }
    }

    pub fn bracketed_paste(&self) -> bool {
        self.mode(mode_pack(2004, false), false)
    }

    pub fn mouse_tracking(&self) -> bool {
        self.get::<bool>(ffi::GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, false)
    }

    pub fn sgr_mouse(&self) -> bool {
        self.mode(mode_pack(1006, false), false)
    }

    pub fn focus_event(&self) -> bool {
        self.mode(mode_pack(1004, false), false)
    }

    /// Advance the render state and rebuild the lightweight frame snapshot.
    pub fn update_frame(&mut self) {
        unsafe {
            let result = ffi::ghostty_render_state_update(self.render, self.term);
            if result != ffi::GHOSTTY_SUCCESS {
                return;
            }

            let mut dirty: i32 = ffi::GHOSTTY_RENDER_STATE_DIRTY_FALSE;
            ffi::ghostty_render_state_get(
                self.render,
                ffi::GHOSTTY_RENDER_STATE_DATA_DIRTY,
                &mut dirty as *mut i32 as *mut c_void,
            );
            self.dirty = dirty != ffi::GHOSTTY_RENDER_STATE_DIRTY_FALSE;

            let mut cols: u16 = self.cols;
            let mut rows: u16 = self.rows;
            ffi::ghostty_render_state_get(
                self.render,
                ffi::GHOSTTY_RENDER_STATE_DATA_COLS,
                &mut cols as *mut u16 as *mut c_void,
            );
            ffi::ghostty_render_state_get(
                self.render,
                ffi::GHOSTTY_RENDER_STATE_DATA_ROWS,
                &mut rows as *mut u16 as *mut c_void,
            );
            self.frame.cols = cols;
            self.frame.rows = rows;

            let mut colors = sized!(ffi::GhosttyRenderStateColors);
            let result = ffi::ghostty_render_state_get(
                self.render,
                ffi::GHOSTTY_RENDER_STATE_DATA_COLORS,
                &mut colors as *mut _ as *mut c_void,
            );
            if result == ffi::GHOSTTY_SUCCESS {
                self.frame.background = Rgb(
                    colors.background.r,
                    colors.background.g,
                    colors.background.b,
                );
                self.frame.foreground = Rgb(
                    colors.foreground.r,
                    colors.foreground.g,
                    colors.foreground.b,
                );
            }
            let frame_fg = self.frame.foreground;
            let frame_bg = self.frame.background;

            let mut cursor = sized!(ffi::GhosttyRenderStateCursor);
            let result = ffi::ghostty_render_state_get(
                self.render,
                ffi::GHOSTTY_RENDER_STATE_DATA_CURSOR,
                &mut cursor as *mut _ as *mut c_void,
            );
            self.frame.cursor = if result == ffi::GHOSTTY_SUCCESS
                && cursor.viewport_has_value
                && cursor.visible
            {
                Some(CursorView {
                    x: cursor.viewport_x,
                    y: cursor.viewport_y,
                    blinking: cursor.blinking,
                    style: match cursor.visual_style {
                        ffi::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BAR => CursorStyle::Bar,
                        ffi::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK => CursorStyle::Block,
                        ffi::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE => {
                            CursorStyle::Underline
                        }
                        _ => CursorStyle::HollowBlock,
                    },
                })
            } else {
                None
            };

            ffi::ghostty_render_state_get(
                self.render,
                ffi::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                &mut self.row_iter as *mut _ as *mut c_void,
            );

            let mut row_index = 0usize;
            while ffi::ghostty_render_state_row_iterator_next(self.row_iter) {
                if row_index >= self.frame.rows_view.len() {
                    self.frame.rows_view.push(RowView::default());
                }
                {
                    let mut viewport_y: i32 = 0;
                    ffi::ghostty_render_state_row_get(
                        self.row_iter,
                        ffi::GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y,
                        &mut viewport_y as *mut i32 as *mut c_void,
                    );
                    let row = &mut self.frame.rows_view[row_index];
                    row.y = viewport_y.max(0) as u16;
                    row.cells.clear();
                    row.selection = None;

                    let mut selection = sized!(ffi::GhosttyRenderStateRowSelection);
                    let result = ffi::ghostty_render_state_row_get(
                        self.row_iter,
                        ffi::GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION,
                        &mut selection as *mut _ as *mut c_void,
                    );
                    if result == ffi::GHOSTTY_SUCCESS {
                        row.selection = Some((selection.start_x, selection.end_x));
                    }

                    let result = ffi::ghostty_render_state_row_get(
                        self.row_iter,
                        ffi::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                        &mut self.cells as *mut _ as *mut c_void,
                    );
                    if result != ffi::GHOSTTY_SUCCESS {
                        row_index += 1;
                        continue;
                    }

                    let mut x: u16 = 0;
                    while ffi::ghostty_render_state_row_cells_next(self.cells) {
                        let mut cell = CellView {
                            x,
                            ..CellView::default()
                        };

                        let mut len: u32 = 0;
                        ffi::ghostty_render_state_row_cells_get(
                            self.cells,
                            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,
                            &mut len as *mut u32 as *mut c_void,
                        );
                        if len > 0 {
                            cell.len = len.min(cell.cps.len() as u32) as u8;
                            ffi::ghostty_render_state_row_cells_get(
                                self.cells,
                                ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                                cell.cps.as_mut_ptr() as *mut c_void,
                            );
                        }

                        let mut fg = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
                        let result = ffi::ghostty_render_state_row_cells_get(
                            self.cells,
                            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
                            &mut fg as *mut _ as *mut c_void,
                        );
                        cell.fg = if result == ffi::GHOSTTY_SUCCESS {
                            Rgb(fg.r, fg.g, fg.b)
                        } else {
                            frame_fg
                        };

                        let mut bg = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
                        let result = ffi::ghostty_render_state_row_cells_get(
                            self.cells,
                            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
                            &mut bg as *mut _ as *mut c_void,
                        );
                        if result == ffi::GHOSTTY_SUCCESS {
                            cell.bg = Some(Rgb(bg.r, bg.g, bg.b));
                        }

                        let mut has_styling = false;
                        ffi::ghostty_render_state_row_cells_get(
                            self.cells,
                            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING,
                            &mut has_styling as *mut bool as *mut c_void,
                        );
                        if has_styling {
                            let mut style = sized!(ffi::GhosttyStyle);
                            let result = ffi::ghostty_render_state_row_cells_get(
                                self.cells,
                                ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                                &mut style as *mut _ as *mut c_void,
                            );
                            if result == ffi::GHOSTTY_SUCCESS {
                                cell.flags = CellFlags {
                                    bold: style.bold,
                                    italic: style.italic,
                                    faint: style.faint,
                                    underline: style.underline != 0,
                                    strikethrough: style.strikethrough,
                                    overline: style.overline,
                                    invisible: style.invisible,
                                };
                                if style.underline_color.tag == ffi::GHOSTTY_STYLE_COLOR_RGB {
                                    let c = style.underline_color.value.rgb;
                                    cell.underline = Some(Rgb(c.r, c.g, c.b));
                                } else if style.underline_color.tag
                                    == ffi::GHOSTTY_STYLE_COLOR_PALETTE
                                {
                                    let idx = style.underline_color.value.palette;
                                    if (idx as usize) < colors.palette.len() {
                                        let c = colors.palette[idx as usize];
                                        cell.underline = Some(Rgb(c.r, c.g, c.b));
                                    }
                                }
                                if style.inverse {
                                    let old_fg = cell.fg;
                                    let old_bg = cell.bg.unwrap_or(frame_bg);
                                    cell.fg = old_bg;
                                    cell.bg = Some(old_fg);
                                }
                            }
                        }

                        row.cells.push(cell);
                        x += 1;
                    }
                }
                row_index += 1;
            }
            self.frame.rows_view.truncate(row_index);
            self.frame.alt_screen = self.is_alt_screen();
            self.frame.scrollbar = self.scrollbar();
        }
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    // ---- selection -----------------------------------------------------

    unsafe fn grid_ref_viewport(&self, x: u16, y: u16) -> Option<ffi::GhosttyGridRef> {
        let point = ffi::GhosttyPoint {
            tag: ffi::GHOSTTY_POINT_TAG_VIEWPORT,
            value: ffi::GhosttyPointValue {
                coordinate: ffi::GhosttyPointCoordinate { x, y: y as u32 },
            },
        };
        let mut reference = sized!(ffi::GhosttyGridRef);
        let result = ffi::ghostty_terminal_grid_ref(self.term, point, &mut reference);
        if result == ffi::GHOSTTY_SUCCESS {
            Some(reference)
        } else {
            None
        }
    }

    pub fn set_selection(&mut self, start: Option<(u16, u16)>, end: (u16, u16)) {
        unsafe {
            let Some(first) = start.and_then(|(x, y)| self.grid_ref_viewport(x, y)) else {
                self.clear_selection();
                return;
            };
            let Some(last) = self.grid_ref_viewport(end.0, end.1) else {
                self.clear_selection();
                return;
            };
            let selection = ffi::GhosttySelection {
                size: size_of::<ffi::GhosttySelection>(),
                start: first,
                end: last,
                rectangle: false,
            };
            ffi::ghostty_terminal_set(
                self.term,
                ffi::GHOSTTY_TERMINAL_OPT_SELECTION,
                &selection as *const _ as *const c_void,
            );
        }
    }

    pub fn clear_selection(&mut self) {
        unsafe {
            ffi::ghostty_terminal_set(
                self.term,
                ffi::GHOSTTY_TERMINAL_OPT_SELECTION,
                std::ptr::null(),
            );
        }
    }

    pub fn has_selection(&self) -> bool {
        let mut selection = sized!(ffi::GhosttySelection);
        let result = unsafe {
            ffi::ghostty_terminal_get(
                self.term,
                ffi::GHOSTTY_TERMINAL_DATA_SELECTION,
                &mut selection as *mut _ as *mut c_void,
            )
        };
        result == ffi::GHOSTTY_SUCCESS
    }

    pub fn select_word(&mut self, x: u16, y: u16) {
        unsafe {
            let Some(reference) = self.grid_ref_viewport(x, y) else {
                return;
            };
            let mut options = sized!(ffi::GhosttyTerminalSelectWordOptions);
            options.ref_ = reference;
            options.boundary_codepoints = std::ptr::null();
            options.boundary_codepoints_len = 0;
            let mut selection = sized!(ffi::GhosttySelection);
            let result = ffi::ghostty_terminal_select_word(self.term, &options, &mut selection);
            if result == ffi::GHOSTTY_SUCCESS {
                ffi::ghostty_terminal_set(
                    self.term,
                    ffi::GHOSTTY_TERMINAL_OPT_SELECTION,
                    &selection as *const _ as *const c_void,
                );
            }
        }
    }

    pub fn select_line(&mut self, x: u16, y: u16) {
        unsafe {
            let Some(reference) = self.grid_ref_viewport(x, y) else {
                return;
            };
            let mut options = sized!(ffi::GhosttyTerminalSelectLineOptions);
            options.ref_ = reference;
            options.whitespace = std::ptr::null();
            options.whitespace_len = 0;
            options.semantic_prompt_boundary = true;
            let mut selection = sized!(ffi::GhosttySelection);
            let result = ffi::ghostty_terminal_select_line(self.term, &options, &mut selection);
            if result == ffi::GHOSTTY_SUCCESS {
                ffi::ghostty_terminal_set(
                    self.term,
                    ffi::GHOSTTY_TERMINAL_OPT_SELECTION,
                    &selection as *const _ as *const c_void,
                );
            }
        }
    }

    pub fn select_all(&mut self) {
        unsafe {
            let mut selection = sized!(ffi::GhosttySelection);
            let result = ffi::ghostty_terminal_select_all(self.term, &mut selection);
            if result == ffi::GHOSTTY_SUCCESS {
                ffi::ghostty_terminal_set(
                    self.term,
                    ffi::GHOSTTY_TERMINAL_OPT_SELECTION,
                    &selection as *const _ as *const c_void,
                );
            }
        }
    }

    pub fn selection_text(&self) -> Option<String> {
        unsafe {
            let mut options = sized!(ffi::GhosttyTerminalSelectionFormatOptions);
            options.emit = ffi::GHOSTTY_FORMATTER_FORMAT_PLAIN;
            options.unwrap = true;
            options.trim = true;
            options.selection = std::ptr::null();
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len: usize = 0;
            let result = ffi::ghostty_terminal_selection_format_alloc(
                self.term,
                std::ptr::null(),
                options,
                &mut ptr,
                &mut len,
            );
            if result != ffi::GHOSTTY_SUCCESS || ptr.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
            ffi::ghostty_free(std::ptr::null(), ptr, len);
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
    }

    /// Plain text of the last row with content in the viewport. Used by the
    /// background-command panel while a command is running.
    pub fn last_line(&self) -> String {
        for row in self.frame.rows_view.iter().rev() {
            let text = row_text(row);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        String::new()
    }

    /// Like `last_line`, but ignores the cursor row so a finished command shows
    /// its last output instead of the shell prompt printed below it.
    pub fn last_line_before_cursor(&self) -> String {
        let cursor = self.frame.cursor.map(|c| c.y).unwrap_or(0);
        for row in self.frame.rows_view.iter().rev() {
            if row.y >= cursor {
                continue;
            }
            let text = row_text(row);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        String::new()
    }

    /// Plain text of the whole visible screen. Used by tests and diagnostics.
    #[allow(dead_code)]
    pub fn visible_text(&self) -> String {
        let mut out = String::new();
        for line in &self.frame.rows_view {
            let mut text = row_text(line);
            while text.ends_with(' ') {
                text.pop();
            }
            out.push_str(&text);
            out.push('\n');
        }
        out
    }
}

impl Frame {
    /// Plain text of one viewport row, or `None` when the row is not present.
    pub fn row_text(&self, viewport_y: u16) -> Option<String> {
        self.rows_view
            .iter()
            .find(|row| row.y == viewport_y)
            .map(row_text)
    }
}

fn row_text(row: &RowView) -> String {
    let mut text = String::new();
    let mut wide_pending = false;
    for cell in &row.cells {
        if cell.len == 0 {
            if wide_pending {
                wide_pending = false;
            } else {
                text.push(' ');
            }
            continue;
        }
        let cell_text = cell.text();
        wide_pending = unicode_width::UnicodeWidthStr::width(cell_text.as_str()) >= 2;
        text.push_str(&cell_text);
    }
    text
}

pub fn mode_pack(value: u16, ansi: bool) -> ffi::GhosttyMode {
    (value & 0x7fff) | ((ansi as u16) << 15)
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        unsafe {
            if !self.cells.is_null() {
                ffi::ghostty_render_state_row_cells_free(self.cells);
            }
            if !self.row_iter.is_null() {
                ffi::ghostty_render_state_row_iterator_free(self.row_iter);
            }
            if !self.render.is_null() {
                ffi::ghostty_render_state_free(self.render);
            }
            if !self.key_event.is_null() {
                ffi::ghostty_key_event_free(self.key_event);
            }
            if !self.encoder.is_null() {
                ffi::ghostty_key_encoder_free(self.encoder);
            }
            if !self.term.is_null() {
                ffi::ghostty_terminal_free(self.term);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> TerminalSession {
        TerminalSession::new(&Theme::default(), 40, 10, 8, 16, 1000).expect("terminal")
    }

    #[test]
    fn writes_and_renders_text() {
        let mut term = session();
        term.write(b"hello prompt\r\nsecond line");
        term.update_frame();
        let text = term.visible_text();
        assert!(text.contains("hello prompt"), "{text:?}");
        assert!(text.contains("second line"), "{text:?}");
    }

    #[test]
    fn alternates_screen_and_restores() {
        let mut term = session();
        term.write(b"PRIMARY");
        term.update_frame();
        assert!(!term.is_alt_screen());
        term.write(b"\x1b[?1049hALT");
        term.update_frame();
        assert!(term.is_alt_screen());
        assert!(term.visible_text().contains("ALT"));
        term.write(b"\x1b[?1049l");
        term.update_frame();
        assert!(!term.is_alt_screen());
        assert!(term.visible_text().contains("PRIMARY"));
    }

    #[test]
    fn viewport_selection_extracts_text() {
        let mut term = session();
        term.write(b"alpha beta\r\ngamma delta");
        term.update_frame();
        term.set_selection(Some((0, 0)), (9, 0));
        let text = term.selection_text().expect("selection text");
        assert!(text.starts_with("alpha beta"), "{text:?}");
        term.clear_selection();
        assert!(!term.has_selection());
    }

    #[test]
    fn last_line_reports_the_newest_content() {
        let mut term = session();
        term.write(b"first\r\nsecond\r\nthird\r\n");
        term.update_frame();
        assert_eq!(term.last_line(), "third");
        term.write(b"\r\n");
        term.update_frame();
        assert_eq!(term.last_line(), "third");
    }

    #[test]
    fn long_scrollback_is_not_pruned_early() {
        let mut term = TerminalSession::new(&Theme::default(), 80, 24, 8, 16, 10_000).unwrap();
        for i in 0..2000 {
            term.write(format!("{i}\r\n").as_bytes());
        }
        term.update_frame();
        assert!(
            term.scrollbar().total > 1900,
            "scrollback pruned to {} rows",
            term.scrollbar().total
        );
        term.scroll_to_row(0);
        term.update_frame();
        assert_eq!(term.visible_text().lines().next(), Some("0"));
    }

    #[test]
    fn scroll_to_row_round_trips_with_the_scrollbar() {
        let mut term = session();
        for i in 0..40 {
            term.write(format!("line {i}\r\n").as_bytes());
        }
        term.update_frame();
        let total = term.scrollbar().total;
        term.scroll_to_row(total.saturating_sub(12));
        term.update_frame();
        let offset = term.scrollbar().offset;
        assert!(offset > 0, "expected scrollback, offset was {offset}");
        assert!(
            term.visible_text().contains("line"),
            "{}",
            term.visible_text()
        );
        term.scroll_to_bottom();
        term.update_frame();
        assert!(term.scrollbar().at_bottom());
    }

    #[test]
    fn semantic_prompt_events_and_exit_code() {
        let mut term = session();
        term.write(b"\x1b]133;D;7\x1b\\\x1b]133;A\x1b\\");
        let events = term.drain_events();
        assert!(events
            .iter()
            .any(|e| matches!(e, TermEvent::CommandEnd { exit_code: Some(7) })));
        assert!(events.iter().any(|e| matches!(e, TermEvent::PromptStart)));
        term.write(b"\x07");
        assert!(term
            .drain_events()
            .iter()
            .any(|e| matches!(e, TermEvent::Bell)));
    }

    #[test]
    fn key_encoder_uses_terminal_modes() {
        let mut term = session();
        let bytes = term.encode_key(
            ffi::GHOSTTY_KEY_C,
            ffi::GHOSTTY_MODS_CTRL as u16,
            ffi::GHOSTTY_KEY_ACTION_PRESS,
            None,
        );
        assert_eq!(bytes, b"\x03");
        let bytes = term.encode_key(
            ffi::GHOSTTY_KEY_ARROW_UP,
            0,
            ffi::GHOSTTY_KEY_ACTION_PRESS,
            None,
        );
        assert_eq!(bytes, b"\x1b[A");
    }

    #[test]
    fn scrollback_and_resize() {
        let mut term = session();
        for i in 0..30 {
            term.write(format!("line {i}\r\n").as_bytes());
        }
        term.update_frame();
        let sb = term.scrollbar();
        assert!(sb.total >= 30, "{sb:?}");
        term.scroll(-5);
        term.update_frame();
        let scrolled = term.scrollbar();
        assert!(scrolled.offset < sb.offset || sb.total > sb.len);
        term.scroll_to_bottom();
        term.resize(60, 20, 8, 16);
        term.update_frame();
        assert_eq!(term.frame().cols, 60);
        assert_eq!(term.frame().rows, 20);
    }
}

#[cfg(test)]
mod unicode_tests {
    use super::*;

    #[test]
    fn wide_text_survives_the_ffi_snapshot() {
        let mut term = TerminalSession::new(&Theme::default(), 40, 5, 8, 16, 100).unwrap();
        let text = "日本語 🚀👩‍💻";
        term.write(text.as_bytes());
        term.update_frame();
        let visible = term.visible_text();
        assert!(visible.contains("日本語"), "{visible:?}");
        assert!(visible.contains("🚀"), "{visible:?}");
    }
}
