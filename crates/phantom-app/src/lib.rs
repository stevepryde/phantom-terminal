//! Phantom Terminal — native terminal app.
//!
//! Manages multiple terminal tabs over a single window: a native tab bar, the
//! VT model per tab ([`AlacrittyCore`]), the [`phantom_gfx`] renderer, keyboard
//! and mouse routing, cursor blink, tab rename, live cwd tracking, and session
//! persistence/restore.
//!
//! The app *logic* is decoupled from winit: all input arrives as engine-
//! independent [`event::AppInput`] via [`App::handle_input`], PTY output is
//! delivered through the [`PtyOutbox`] trait, and "exit" is a flag the host
//! drains. The winit event loop ([`run`]) is a thin adapter, which also makes
//! the whole app drivable headlessly in tests.

pub mod event;

mod blur;
mod chrome;
mod egui_ui;
mod gpu;
mod input;
mod keybindings;
mod palette;
mod tab;
mod themes;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_ui::{EguiLayer, UiState};
use event::{AppInput, Mods};
use gpu::GpuContext;
use keybindings::{Action, Keymap};
use palette::{PaletteAction, PaletteOutcome, PaletteState};
use phantom_core::{
    default_home_dir, resolve_launch_opts, AppConfig, LaunchContext, PtyManager, PtySink,
    SessionStore, TabRecord, MAX_TAB_TITLE_LEN,
};
use phantom_emu::{
    encode_key, encode_mouse_legacy, encode_mouse_sgr, AlacrittyCore, CursorShape, Key,
    MouseProtocol, ScrollState, SelSide, SelectionKind, VtCore,
};
use phantom_gfx::Renderer;
use tab::Tab;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId};

const BLINK_INTERVAL: Duration = Duration::from_millis(530);
const CWD_POLL_INTERVAL: Duration = Duration::from_millis(400);
const LIVE_RESIZE_REDRAW_GRACE: Duration = Duration::from_millis(120);
const TERMINAL_RESIZE_DEBOUNCE: Duration = Duration::from_millis(300);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
const NOTICE_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_NOTICE_LEN: usize = 240;
const TAB_DRAG_TOLERANCE_PX: f32 = 8.0;
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);
const WHEEL_LINES_PER_NOTCH: f32 = 3.0;

/// PTY output delivered from a reader thread, tagged with the tab it belongs to.
#[derive(Debug)]
pub enum AppEvent {
    PtyBytes { tab: u64, bytes: Vec<u8> },
    PtyExit { tab: u64 },
}

/// Sink for PTY output events. The winit host wraps an [`EventLoopProxy`]; tests
/// wrap a collector. Returning `false` tells the reader thread to stop.
pub trait PtyOutbox: Send + Sync + 'static {
    fn send(&self, event: AppEvent) -> bool;
}

/// Production outbox: wakes the winit loop with the event.
struct ProxyOutbox(EventLoopProxy<AppEvent>);

impl PtyOutbox for ProxyOutbox {
    fn send(&self, event: AppEvent) -> bool {
        self.0.send_event(event).is_ok()
    }
}

/// Forwards one tab's PTY output to the outbox, tagged with the tab id. Lives on
/// the PTY reader thread.
struct ProxySink {
    outbox: Arc<dyn PtyOutbox>,
    tab: u64,
}

impl PtySink for ProxySink {
    fn on_bytes(&mut self, bytes: &[u8]) -> bool {
        self.outbox.send(AppEvent::PtyBytes {
            tab: self.tab,
            bytes: bytes.to_vec(),
        })
    }

    fn on_eof(&mut self) {
        self.outbox.send(AppEvent::PtyExit { tab: self.tab });
    }
}

/// A mouse interaction to report to a mouse-aware application.
#[derive(Clone, Copy)]
enum MouseEvent {
    Press,
    Release,
    Drag,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy)]
enum ClipAction {
    Copy,
    Paste,
}

struct Notice {
    text: String,
    until: Instant,
}

struct TabDrag {
    from: usize,
    start: (f32, f32),
    target: usize,
    active: bool,
}

struct ScrollDrag {
    start_y: f32,
    start_offset: usize,
}

#[derive(Clone, Copy)]
struct LastClick {
    row: usize,
    col: usize,
    at: Instant,
    count: u8,
}

pub struct App {
    outbox: Arc<dyn PtyOutbox>,
    config: AppConfig,
    pty: PtyManager,
    store: Option<SessionStore>,
    mods: Mods,

    gpu: Option<GpuContext>,
    renderer: Option<Renderer>,
    renderer_signature: String,
    egui: Option<EguiLayer>,

    keymap: Keymap,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,

    cursor_pos: (f32, f32),
    cursor_seen: bool,
    cursor_pointer: bool,
    last_hits: Option<chrome::TabBarHits>,
    chrome_anim: chrome::ChromeAnimationState,
    tab_drag: Option<TabDrag>,
    scroll_drag: Option<ScrollDrag>,
    wheel_line_remainder: f32,

    clipboard: Option<arboard::Clipboard>,
    left_down: bool,
    selecting: bool,
    last_click: Option<LastClick>,
    redraw_queued: bool,
    /// In-progress IME composition text (shown at the cursor).
    preedit: String,
    fullscreen: bool,
    /// Set when the app wants to quit; the host drains it.
    exit_requested: bool,

    palette: PaletteState,
    ui: UiState,
    notice: Option<Notice>,

    // Launch behaviour.
    launch_cwd: Option<String>,
    remember_tabs: bool,
    ephemeral: bool,
    applied_window_chrome: String,

    // Tab rename edit buffer (active tab) when in rename mode.
    rename: Option<String>,

    // Debounced session save.
    save_after: Option<Instant>,
    config_save_after: Option<Instant>,
    renderer_rebuild_after: Option<Instant>,
    live_resize_until: Option<Instant>,
    terminal_resize_deadline: Option<Instant>,
    pending_terminal_grid: Option<(u16, u16)>,
    cwd_deadline: Instant,
    last_terminal_grid: Option<(u16, u16)>,

    blink_enabled: bool,
    cursor_on: bool,
    next_toggle: Instant,
}

impl App {
    pub fn new(
        outbox: Arc<dyn PtyOutbox>,
        config: AppConfig,
        store: Option<SessionStore>,
        launch: LaunchContext,
    ) -> Self {
        let keymap = Keymap::from_config(&config.keybindings);
        let blink_enabled = config.cursor_blink;
        let applied_window_chrome = config.window_chrome.clone();
        let renderer_signature = renderer_signature(&config);
        let ephemeral = !launch.remember_tabs;
        let now = Instant::now();
        let ui = UiState::new(&config);
        Self {
            outbox,
            config,
            pty: PtyManager::new(),
            store,
            mods: Mods::default(),
            gpu: None,
            renderer: None,
            renderer_signature,
            egui: None,
            keymap,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            cursor_pos: (0.0, 0.0),
            cursor_seen: false,
            cursor_pointer: false,
            last_hits: None,
            chrome_anim: chrome::ChromeAnimationState::default(),
            tab_drag: None,
            scroll_drag: None,
            wheel_line_remainder: 0.0,
            clipboard: arboard::Clipboard::new().ok(),
            left_down: false,
            selecting: false,
            last_click: None,
            redraw_queued: false,
            preedit: String::new(),
            fullscreen: false,
            exit_requested: false,
            palette: PaletteState::default(),
            ui,
            notice: None,
            launch_cwd: launch.cwd,
            remember_tabs: launch.remember_tabs,
            ephemeral,
            applied_window_chrome,
            rename: None,
            save_after: None,
            config_save_after: None,
            renderer_rebuild_after: None,
            live_resize_until: None,
            terminal_resize_deadline: None,
            pending_terminal_grid: None,
            cwd_deadline: now + CWD_POLL_INTERVAL,
            last_terminal_grid: None,
            blink_enabled,
            cursor_on: true,
            next_toggle: now + BLINK_INTERVAL,
        }
    }

    // ── Public surface for the host and tests ──────────────────────────────

    /// Open the initial tab(s) (restore / launch-arg / default shell).
    pub fn start(&mut self) {
        self.startup_tabs();
    }

    /// Dispatch one engine-independent input event.
    pub fn handle_input(&mut self, input: AppInput) {
        match input {
            AppInput::ModifiersChanged(mods) => self.mods = mods,
            AppInput::Resized { width, height } => self.on_resize(width, height),
            AppInput::ScaleChanged => self.rebuild_renderer(),
            AppInput::CloseRequested => {
                self.flush_config_save();
                self.save_tabs();
                for tab in &self.tabs {
                    let _ = self.pty.kill(tab.pty_id);
                }
                self.exit_requested = true;
            }
            AppInput::ImeCommit(text) => self.commit_text(&text),
            AppInput::ImePreedit(text) => {
                self.preedit = text;
                self.request_redraw();
            }
            AppInput::MouseMove { x, y } => {
                self.cursor_pos = (x, y);
                self.cursor_seen = true;
                self.on_mouse_move();
            }
            AppInput::MouseDown { x, y } => {
                self.cursor_pos = (x, y);
                self.cursor_seen = true;
                self.on_mouse_down();
            }
            AppInput::MouseUp { x, y } => {
                self.cursor_pos = (x, y);
                self.cursor_seen = true;
                self.on_mouse_up();
            }
            AppInput::Wheel { lines } => self.on_scroll(lines),
            AppInput::Key { key, text, mods } => {
                self.mods = mods;
                self.handle_key_input(key, text.as_deref());
            }
        }
    }

    /// Feed a PTY output event into the matching tab.
    pub fn on_pty_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::PtyBytes { tab, bytes } => {
                let mut response = None;
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                    t.advance_pty(&bytes);
                    let out = t.core.take_pty_output();
                    if !out.is_empty() {
                        response = Some((t.pty_id, out));
                    }
                }
                if let Some((pty_id, out)) = response {
                    let _ = self.pty.write(pty_id, &out);
                }
                self.request_redraw();
            }
            AppEvent::PtyExit { tab } => {
                if let Some(index) = self.tabs.iter().position(|t| t.id == tab) {
                    self.close_tab(index);
                }
            }
        }
    }

    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_title(&self) -> Option<String> {
        self.tabs.get(self.active).map(|t| t.title())
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn palette_open(&self) -> bool {
        self.palette.open
    }

    pub fn settings_open(&self) -> bool {
        self.ui.settings_open()
    }

    pub fn renaming(&self) -> bool {
        self.rename.is_some()
    }

    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_ref().map(|notice| notice.text.as_str())
    }

    #[doc(hidden)]
    pub fn remembered_tab_count_for_tests(&self) -> Option<usize> {
        self.store
            .as_ref()
            .and_then(|store| store.load_tabs().ok())
            .map(|tabs| tabs.len())
    }

    // ── Internal logic ─────────────────────────────────────────────────────

    fn horizontal(&self) -> bool {
        self.config.tab_layout != "vertical"
    }

    fn window_chrome(&self) -> chrome::WindowChrome {
        chrome::WindowChrome::from_config(&self.config.window_chrome)
    }

    fn applied_window_chrome(&self) -> chrome::WindowChrome {
        chrome::WindowChrome::from_config(&self.applied_window_chrome)
    }

    fn custom_window_chrome(&self) -> bool {
        self.applied_window_chrome().is_custom()
    }

    fn content_width(&self, width: u32) -> f32 {
        width.max(1) as f32
    }

    /// Cell grid `(rows, cols)` that fits the terminal viewport.
    fn viewport_grid(&self) -> (u16, u16) {
        match (self.gpu.as_ref(), self.renderer.as_ref()) {
            (Some(gpu), Some(renderer)) => {
                let (w, h) = gpu.size();
                let layout = chrome::compute_layout(
                    self.content_width(w),
                    h as f32,
                    renderer.cell_size().1,
                    self.horizontal(),
                    self.applied_window_chrome(),
                );
                renderer.grid_size(layout.viewport.w as u32, layout.viewport.h as u32)
            }
            _ => (24, 80),
        }
    }

    fn request_redraw(&mut self) {
        if self.redraw_queued {
            return;
        }
        self.redraw_queued = true;
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.request_redraw();
        }
    }

    fn show_notice(&mut self, message: impl Into<String>) {
        let text = truncate_to_chars(&message.into(), MAX_NOTICE_LEN);
        self.notice = Some(Notice {
            text,
            until: Instant::now() + NOTICE_TIMEOUT,
        });
        self.request_redraw();
    }

    fn clear_expired_notice(&mut self, now: Instant) {
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| now >= notice.until)
        {
            self.notice = None;
            self.request_redraw();
        }
    }

    fn mark_dirty(&mut self) {
        if !self.ephemeral && self.save_after.is_none() {
            self.save_after = Some(Instant::now() + SAVE_DEBOUNCE);
        }
    }

    fn mark_config_dirty(&mut self) {
        self.config_save_after = Some(Instant::now() + SAVE_DEBOUNCE);
    }

    fn mark_renderer_dirty(&mut self) {
        self.renderer_rebuild_after = Some(Instant::now() + SAVE_DEBOUNCE);
    }

    fn startup_tabs(&mut self) {
        if let Some(cwd) = self.launch_cwd.take() {
            self.spawn_tab(None, Some(cwd));
            return;
        }

        if self.config.restore_on_launch && self.remember_tabs {
            let records = self
                .store
                .as_ref()
                .and_then(|s| s.load_tabs().ok())
                .unwrap_or_default();
            if !records.is_empty() {
                let mut active = 0;
                for (i, rec) in records.iter().enumerate() {
                    self.spawn_tab(rec.shell_profile_id.clone(), Some(rec.cwd.clone()));
                    if rec.title != basename(&rec.cwd) {
                        if let Some(tab) = self.tabs.last_mut() {
                            tab.custom_title = rec.title.clone();
                        }
                    }
                    if rec.is_active {
                        active = i;
                    }
                }
                self.active = active.min(self.tabs.len().saturating_sub(1));
                return;
            }
        }

        self.spawn_tab(None, None);
    }

    fn spawn_tab(&mut self, profile_id: Option<String>, cwd: Option<String>) {
        let (rows, cols) = self.viewport_grid();
        let launch =
            match resolve_launch_opts(&self.config, profile_id.as_deref(), cwd.clone(), rows, cols)
            {
                Ok(launch) => launch,
                Err(e) => {
                    self.show_notice(format!("Could not resolve shell profile: {e}"));
                    return;
                }
            };
        let start_cwd = launch
            .cwd
            .clone()
            .or_else(default_home_dir)
            .unwrap_or_default();

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let sink = ProxySink {
            outbox: Arc::clone(&self.outbox),
            tab: tab_id,
        };
        let pty_id = match self.pty.spawn(launch, sink) {
            Ok(id) => id,
            Err(e) => {
                self.show_notice(format!("Could not start shell: {e}"));
                return;
            }
        };

        let core = AlacrittyCore::new(
            rows,
            cols,
            self.config.scrollback_lines,
            cursor_shape(&self.config.cursor_style),
        );
        self.tabs
            .push(Tab::new(tab_id, core, pty_id, start_cwd, profile_id));
        self.active = self.tabs.len() - 1;
        self.mark_dirty();
        self.request_redraw();
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        let _ = self.pty.kill(tab.pty_id);
        if self.tabs.is_empty() {
            self.flush_config_save();
            self.save_tabs();
            self.exit_requested = true;
            return;
        }
        // Keep `active` pointing at the same tab: if a tab before it was removed,
        // everything shifted down by one; otherwise just clamp if it was last.
        if index < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.rename = None;
        self.mark_dirty();
        self.request_redraw();
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::NewTab => self.spawn_tab(None, None),
            Action::CloseTab => self.close_tab(self.active),
            Action::SwitchTab(n) => {
                let i = (n as usize).saturating_sub(1);
                if i < self.tabs.len() {
                    self.active = i;
                    self.mark_dirty();
                    self.request_redraw();
                }
            }
            Action::RenameTab => {
                if let Some(tab) = self.tabs.get(self.active) {
                    self.rename = Some(tab.title());
                    self.request_redraw();
                }
            }
            Action::TogglePalette => {
                if self.palette.open {
                    self.palette.close();
                } else {
                    self.palette.open(&self.config);
                }
                self.request_redraw();
            }
            Action::ToggleSettings => {
                self.ui.toggle_settings(&self.config);
                self.request_redraw();
            }
        }
    }

    fn dispatch_palette(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::NewTab => self.spawn_tab(None, None),
            PaletteAction::CloseTab => self.close_tab(self.active),
            PaletteAction::RenameTab => {
                if let Some(tab) = self.tabs.get(self.active) {
                    self.rename = Some(tab.title());
                }
            }
            PaletteAction::NewTabWithProfile(id) => self.spawn_tab(Some(id), None),
            PaletteAction::SetUiTheme(name) => {
                self.config.ui_theme = name;
                self.mark_config_dirty();
            }
            PaletteAction::OpenSettings => self.ui.open_settings(&self.config),
        }
        self.request_redraw();
    }

    fn save_config(&mut self) {
        let error = self
            .store
            .as_ref()
            .and_then(|store| store.save_config(&self.config).err());
        if let Some(e) = error {
            eprintln!("failed to save config: {e}");
            self.show_notice(format!("Could not save settings: {e}"));
        }
    }

    fn flush_config_save(&mut self) {
        if self.config_save_after.take().is_some() {
            self.save_config();
        }
    }

    /// Persist the (already-validated) config and rebuild the renderer so font,
    /// theme, and layout changes take effect.
    fn apply_config_change(&mut self) {
        self.apply_window_chrome();
        self.keymap = Keymap::from_config(&self.config.keybindings);
        self.mark_config_dirty();
        self.blink_enabled = self.config.cursor_blink;
        let next_signature = renderer_signature(&self.config);
        if self.renderer_signature != next_signature {
            self.mark_renderer_dirty();
        } else {
            self.renderer_rebuild_after = None;
            self.request_redraw();
        }
    }

    fn apply_window_chrome(&mut self) {
        if self.applied_window_chrome == self.config.window_chrome {
            return;
        }
        let requested = self.window_chrome();
        if cfg!(target_os = "macos") {
            self.show_notice("Window chrome change saved. Restart Phantom to apply it.");
            return;
        }
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.set_decorations(!requested.is_custom());
            gpu.window.set_transparent(requested.is_custom());
        }
        self.applied_window_chrome = self.config.window_chrome.clone();
    }

    /// Recreate the renderer (font/theme/scale) and resize every tab.
    fn rebuild_renderer(&mut self) {
        if let Some(gpu) = self.gpu.as_ref() {
            let renderer = Renderer::new(
                &gpu.device,
                &gpu.queue,
                gpu.format(),
                &self.config,
                gpu.scale_factor(),
            );
            let (w, h) = gpu.size();
            renderer.resize(&gpu.queue, w, h);
            self.renderer = Some(renderer);
        }
        self.renderer_signature = renderer_signature(&self.config);
        self.sync_terminal_grid(true);
        self.request_redraw();
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        let mode = self.fullscreen.then(|| Fullscreen::Borderless(None));
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.set_fullscreen(mode);
        }
    }

    /// Commit IME-composed (or otherwise finalized) text to the active tab.
    fn commit_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.reset_blink();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.core.scroll(-1_000_000);
            tab.core.selection_clear();
        }
        if let Some(tab) = self.tabs.get(self.active) {
            let _ = self.pty.write(tab.pty_id, text.as_bytes());
        }
        self.preedit.clear();
        self.request_redraw();
    }

    /// Route a key press (after the winit adapter has normalized it).
    fn handle_key_input(&mut self, key: Key, text: Option<&str>) {
        // Command palette captures all keys while open.
        if self.palette.open {
            if self.keymap.lookup(key, self.mods) == Some(Action::TogglePalette) {
                self.palette.close();
                self.request_redraw();
                return;
            }
            match self.palette.handle_key(key, text) {
                PaletteOutcome::None => {}
                PaletteOutcome::Close => self.palette.close(),
                PaletteOutcome::Execute(action) => {
                    self.palette.close();
                    self.dispatch_palette(action);
                }
            }
            self.request_redraw();
            return;
        }
        // Rename mode captures all keys.
        if self.rename.is_some() {
            self.handle_rename_key(key, text);
            return;
        }
        // Clipboard shortcuts.
        if let Some(action) = self.clipboard_action(key, self.mods) {
            match action {
                ClipAction::Copy => self.copy_selection(),
                ClipAction::Paste => self.paste_clipboard(),
            }
            return;
        }
        // Fullscreen toggle.
        if key == Key::F(11) {
            self.toggle_fullscreen();
            return;
        }
        // Shift+PageUp/Down scroll the viewport by a page.
        if self.mods.shift {
            let dir = match key {
                Key::PageUp => Some(1),
                Key::PageDown => Some(-1),
                _ => None,
            };
            if let Some(dir) = dir {
                let page = (self.viewport_grid().0 as i32).max(1);
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.core.scroll(dir * page);
                }
                self.request_redraw();
                return;
            }
        }
        // Keybindings.
        if let Some(action) = self.keymap.lookup(key, self.mods) {
            self.handle_action(action);
            return;
        }
        // Otherwise: send to the focused terminal.
        let app_cursor = self
            .tabs
            .get(self.active)
            .map(|t| t.core.application_cursor_keys())
            .unwrap_or(false);
        let bytes = encode_key(key, self.mods.emu(), app_cursor);
        if !bytes.is_empty() {
            self.reset_blink();
            if let Some(t) = self.tabs.get_mut(self.active) {
                // Typing snaps to the bottom and clears any selection.
                t.core.scroll(-1_000_000);
                t.core.selection_clear();
            }
            if let Some(t) = self.tabs.get(self.active) {
                let _ = self.pty.write(t.pty_id, &bytes);
            }
            self.request_redraw();
        }
    }

    /// Handle a keypress while renaming the active tab.
    fn handle_rename_key(&mut self, key: Key, text: Option<&str>) {
        match key {
            Key::Enter => {
                if let Some(buf) = self.rename.take() {
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        tab.custom_title = sanitize_title(&buf);
                    }
                    self.mark_dirty();
                }
            }
            Key::Escape => self.rename = None,
            Key::Backspace => {
                if let Some(buf) = self.rename.as_mut() {
                    buf.pop();
                }
            }
            _ => {
                if let (Some(buf), Some(text)) = (self.rename.as_mut(), text) {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        if buf.len() + c.len_utf8() <= MAX_TAB_TITLE_LEN {
                            buf.push(c);
                        }
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn handle_click(&mut self) {
        enum Hit {
            New,
            Settings,
            WindowControl(chrome::WindowControl),
            Close(usize),
            Switch(usize),
        }
        let (px, py) = self.cursor_pos;
        let mut hit = None;
        if let Some(hits) = &self.last_hits {
            if let Some(control) = hits.window_control_at(px, py) {
                hit = Some(Hit::WindowControl(control));
            } else if hits.settings.contains(px, py) {
                hit = Some(Hit::Settings);
            } else if hits.new_tab.contains(px, py) {
                hit = Some(Hit::New);
            } else {
                for th in &hits.tabs {
                    if th.close.contains(px, py) {
                        hit = Some(Hit::Close(th.index));
                        break;
                    }
                    if th.rect.contains(px, py) {
                        hit = Some(Hit::Switch(th.index));
                        break;
                    }
                }
            }
        }
        match hit {
            Some(Hit::New) => self.spawn_tab(None, None),
            Some(Hit::Settings) => {
                self.ui.toggle_settings(&self.config);
                self.request_redraw();
            }
            Some(Hit::WindowControl(control)) => self.handle_window_control(control),
            Some(Hit::Close(i)) => self.close_tab(i),
            Some(Hit::Switch(i)) => self.switch_tab(i),
            None => {}
        }
    }

    fn handle_window_control(&mut self, control: chrome::WindowControl) {
        if control == chrome::WindowControl::Close {
            self.flush_config_save();
            self.save_tabs();
            for tab in &self.tabs {
                let _ = self.pty.kill(tab.pty_id);
            }
            self.exit_requested = true;
            return;
        }
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        match control {
            chrome::WindowControl::Close => {}
            chrome::WindowControl::Minimize => gpu.window.set_minimized(true),
            chrome::WindowControl::Maximize => {
                gpu.window.set_maximized(!gpu.window.is_maximized());
            }
        }
    }

    fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.rename = None;
        self.mark_dirty();
        self.request_redraw();
    }

    fn reorder_tab(&mut self, from: usize, target: usize) {
        if from >= self.tabs.len() {
            return;
        }
        let target = target.min(self.tabs.len());
        let insert_at = if target > from { target - 1 } else { target };
        if insert_at == from {
            self.active = from;
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        self.rename = None;
        self.mark_dirty();
    }

    fn tab_drag_indicator(&self) -> Option<chrome::DragIndicator> {
        let drag = self.tab_drag.as_ref()?;
        if !drag.active {
            return None;
        }
        let target = drag.target.min(self.tabs.len());
        let drop_index = (target != drag.from && target != drag.from + 1).then_some(target);
        Some(chrome::DragIndicator {
            source: drag.from,
            drop_index,
        })
    }

    fn layout(&self) -> Option<chrome::Layout> {
        let gpu = self.gpu.as_ref()?;
        let renderer = self.renderer.as_ref()?;
        let (w, h) = gpu.size();
        Some(chrome::compute_layout(
            self.content_width(w),
            h as f32,
            renderer.cell_size().1,
            self.horizontal(),
            self.applied_window_chrome(),
        ))
    }

    fn point_in_viewport(&self, px: f32, py: f32) -> bool {
        self.layout().is_some_and(|l| l.viewport.contains(px, py))
    }

    /// Map a window pixel to a viewport `(row, col, side)`, if inside the grid.
    fn viewport_cell(&self, px: f32, py: f32) -> Option<(usize, usize, SelSide)> {
        let layout = self.layout()?;
        let vp = layout.viewport;
        if !vp.contains(px, py) {
            return None;
        }
        let (cw, ch) = self.renderer.as_ref()?.cell_size();
        let (rows, cols) = self.viewport_grid();
        let lx = px - vp.x;
        let ly = py - vp.y;
        let col = ((lx / cw).floor() as i64).clamp(0, cols as i64 - 1) as usize;
        let row = ((ly / ch).floor() as i64).clamp(0, rows as i64 - 1) as usize;
        let side = if lx - col as f32 * cw < cw / 2.0 {
            SelSide::Left
        } else {
            SelSide::Right
        };
        Some((row, col, side))
    }

    fn active_mouse_mode(&self) -> phantom_emu::MouseMode {
        self.tabs
            .get(self.active)
            .map(|t| t.core.mouse_mode())
            .unwrap_or(phantom_emu::MouseMode {
                protocol: MouseProtocol::Off,
                sgr: false,
            })
    }

    fn active_scroll_state(&self) -> Option<ScrollState> {
        self.tabs.get(self.active).map(|t| t.core.scroll_state())
    }

    fn active_scrollbar_hit_thumb(&self) -> Option<chrome::Rect> {
        let layout = self.layout()?;
        chrome::terminal_scrollbar_hit_thumb(&layout, self.active_scroll_state()?)
    }

    fn active_scrollbar_track(&self) -> Option<chrome::Rect> {
        let layout = self.layout()?;
        let scroll = self.active_scroll_state()?;
        scroll
            .is_scrollable()
            .then_some(chrome::terminal_scrollbar_hit_track(&layout))
    }

    fn on_mouse_down(&mut self) {
        let (px, py) = self.cursor_pos;
        if self.palette.open {
            return;
        }
        if let (Some(track), Some(scroll)) =
            (self.active_scrollbar_track(), self.active_scroll_state())
        {
            if track.contains(px, py) {
                let Some(thumb) = self.active_scrollbar_hit_thumb() else {
                    return;
                };
                if thumb.contains(px, py) {
                    self.scroll_drag = Some(ScrollDrag {
                        start_y: py,
                        start_offset: scroll.offset,
                    });
                    self.set_pointer_cursor(true);
                    return;
                }
                let page = scroll.viewport_rows.max(1);
                let target = if py < thumb.y {
                    scroll.offset.saturating_add(page).min(scroll.history)
                } else {
                    scroll.offset.saturating_sub(page)
                };
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.core.scroll_to_offset(target);
                }
                self.set_pointer_cursor(true);
                self.request_redraw();
                return;
            }
        }
        if let Some(tab) = self
            .last_hits
            .as_ref()
            .and_then(|hits| hits.tab_body_at(px, py))
        {
            self.tab_drag = Some(TabDrag {
                from: tab,
                start: (px, py),
                target: tab,
                active: false,
            });
            return;
        }
        if self
            .last_hits
            .as_ref()
            .is_some_and(|hits| hits.titlebar_drag_region_contains(px, py))
        {
            if let Some(gpu) = self.gpu.as_ref() {
                let _ = gpu.window.drag_window();
            }
            return;
        }
        if !self.point_in_viewport(px, py) {
            self.last_click = None;
            self.handle_click();
            return;
        }
        self.left_down = true;
        if self.active_mouse_mode().reports() {
            self.report_mouse(px, py, MouseEvent::Press);
        } else if let Some((row, col, side)) = self.viewport_cell(px, py) {
            let click_count = self.terminal_click_count(row, col);
            let kind = match click_count {
                1 => SelectionKind::Simple,
                2 => SelectionKind::Semantic,
                _ => SelectionKind::Lines,
            };
            self.selecting = true;
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.core.selection_start_kind(row, col, side, kind);
            }
            self.request_redraw();
        }
    }

    fn terminal_click_count(&mut self, row: usize, col: usize) -> u8 {
        let now = Instant::now();
        let count = match self.last_click {
            Some(last)
                if last.row == row
                    && last.col == col
                    && now.duration_since(last.at) <= MULTI_CLICK_INTERVAL =>
            {
                last.count.saturating_add(1).min(3)
            }
            _ => 1,
        };
        self.last_click = Some(LastClick {
            row,
            col,
            at: now,
            count,
        });
        count
    }

    fn on_mouse_up(&mut self) {
        let (px, py) = self.cursor_pos;
        if self.scroll_drag.take().is_some() {
            return;
        }
        if let Some(drag) = self.tab_drag.take() {
            if drag.active {
                self.finish_tab_drag(drag);
            } else {
                self.switch_tab(drag.from);
            }
            return;
        }
        self.left_down = false;
        if self.selecting {
            self.selecting = false;
        } else if self.active_mouse_mode().reports() {
            self.report_mouse(px, py, MouseEvent::Release);
        }
    }

    fn on_mouse_move(&mut self) {
        self.update_chrome_hover();
        if self.update_scroll_drag() {
            return;
        }
        if self.update_tab_drag() {
            return;
        }
        if !self.left_down {
            return;
        }
        let (px, py) = self.cursor_pos;
        if self.selecting {
            if let Some((row, col, side)) = self.viewport_cell(px, py) {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.core.selection_update(row, col, side);
                }
                self.request_redraw();
            }
        } else {
            let mode = self.active_mouse_mode();
            if mode.reports()
                && matches!(mode.protocol, MouseProtocol::Drag | MouseProtocol::Motion)
            {
                self.report_mouse(px, py, MouseEvent::Drag);
            }
        }
    }

    fn update_scroll_drag(&mut self) -> bool {
        let Some(drag) = self.scroll_drag.as_ref() else {
            return false;
        };
        let Some(track) = self.active_scrollbar_track() else {
            return false;
        };
        let Some(scroll) = self.active_scroll_state() else {
            return false;
        };
        let Some(thumb) = chrome::scrollbar_thumb(track, scroll) else {
            return false;
        };

        let travel = (track.h - thumb.h).max(1.0);
        let delta_offset =
            ((self.cursor_pos.1 - drag.start_y) / travel * scroll.history as f32).round() as i32;
        let target =
            (drag.start_offset as i32 - delta_offset).clamp(0, scroll.history as i32) as usize;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.core.scroll_to_offset(target);
        }
        self.set_pointer_cursor(true);
        self.request_redraw();
        true
    }

    fn update_tab_drag(&mut self) -> bool {
        let Some(drag) = self.tab_drag.as_ref() else {
            return false;
        };
        let (px, py) = self.cursor_pos;
        let dx = px - drag.start.0;
        let dy = py - drag.start.1;
        if !drag.active && dx * dx + dy * dy < TAB_DRAG_TOLERANCE_PX * TAB_DRAG_TOLERANCE_PX {
            return true;
        }
        let target = self
            .last_hits
            .as_ref()
            .map(|hits| hits.drop_index(px, py, self.horizontal()));
        if let Some(drag) = self.tab_drag.as_mut() {
            drag.active = true;
            if let Some(target) = target {
                drag.target = target;
            }
        }
        self.set_pointer_cursor(true);
        self.chrome_anim.set_hover(chrome::ChromeHoverTarget::None);
        self.request_redraw();
        true
    }

    fn finish_tab_drag(&mut self, drag: TabDrag) {
        self.reorder_tab(drag.from, drag.target);
        self.request_redraw();
    }

    fn update_chrome_hover(&mut self) {
        if !self.cursor_seen {
            return;
        }
        if self.tab_drag.as_ref().is_some_and(|drag| drag.active) {
            self.set_pointer_cursor(true);
            if self.chrome_anim.set_hover(chrome::ChromeHoverTarget::None) {
                self.request_redraw();
            }
            return;
        }
        let (px, py) = self.cursor_pos;
        if self.scroll_drag.is_some()
            || self
                .active_scrollbar_track()
                .is_some_and(|track| track.contains(px, py))
        {
            self.set_pointer_cursor(true);
            if self.chrome_anim.set_hover(chrome::ChromeHoverTarget::None) {
                self.request_redraw();
            }
            return;
        }
        let hover = self
            .last_hits
            .as_ref()
            .map_or(chrome::ChromeHoverTarget::None, |hits| {
                hits.hover_target(px, py)
            });
        self.set_pointer_cursor(hover.is_clickable());
        if self.chrome_anim.set_hover(hover) {
            self.request_redraw();
        }
    }

    fn clear_chrome_hover(&mut self) {
        self.cursor_seen = false;
        self.tab_drag = None;
        self.scroll_drag = None;
        self.set_pointer_cursor(false);
        if self.chrome_anim.set_hover(chrome::ChromeHoverTarget::None) {
            self.request_redraw();
        }
    }

    fn set_pointer_cursor(&mut self, pointer: bool) {
        if self.cursor_pointer == pointer {
            return;
        }
        self.cursor_pointer = pointer;
        if let Some(gpu) = self.gpu.as_ref() {
            let icon = if pointer {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
            gpu.window.set_cursor(icon);
        }
    }

    fn on_scroll(&mut self, lines: f32) {
        let (px, py) = self.cursor_pos;
        if self.active_mouse_mode().reports() {
            let ticks = lines.round() as i32;
            if ticks == 0 {
                return;
            }
            let event = if lines > 0.0 {
                MouseEvent::WheelUp
            } else {
                MouseEvent::WheelDown
            };
            for _ in 0..ticks.unsigned_abs() {
                self.report_mouse(px, py, event);
            }
        } else {
            self.wheel_line_remainder += lines * WHEEL_LINES_PER_NOTCH;
            let whole_lines = if self.wheel_line_remainder >= 0.0 {
                self.wheel_line_remainder.floor()
            } else {
                self.wheel_line_remainder.ceil()
            } as i32;
            if whole_lines == 0 {
                return;
            }
            self.wheel_line_remainder -= whole_lines as f32;
            if let Some(tab) = self.tabs.get_mut(self.active) {
                // Positive lines (wheel up) scroll back into history.
                tab.core.scroll(whole_lines);
            }
            self.request_redraw();
        }
    }

    /// Encode and send a mouse event to a mouse-aware application.
    fn report_mouse(&mut self, px: f32, py: f32, event: MouseEvent) {
        let Some((row, col, _)) = self.viewport_cell(px, py) else {
            return;
        };
        let (mode, pty_id) = match self.tabs.get(self.active) {
            Some(t) => (t.core.mouse_mode(), t.pty_id),
            None => return,
        };
        if !mode.reports() {
            return;
        }
        let (base, pressed) = match event {
            MouseEvent::Press => (0u8, true),
            MouseEvent::Release => (0u8, false),
            MouseEvent::Drag => (32u8, true),
            MouseEvent::WheelUp => (64u8, true),
            MouseEvent::WheelDown => (65u8, true),
        };
        let mut button = base;
        if self.mods.shift {
            button += 4;
        }
        if self.mods.alt {
            button += 8;
        }
        if self.mods.ctrl {
            button += 16;
        }
        let bytes = if mode.sgr {
            encode_mouse_sgr(button, col as u16, row as u16, pressed)
        } else if pressed {
            encode_mouse_legacy(button, col as u16, row as u16)
        } else {
            encode_mouse_legacy(3 + (button & !3), col as u16, row as u16)
        };
        let _ = self.pty.write(pty_id, &bytes);
    }

    fn copy_selection(&mut self) {
        let text = self
            .tabs
            .get(self.active)
            .and_then(|t| t.core.selection_text());
        if let (Some(text), Some(clipboard)) = (text, self.clipboard.as_mut()) {
            let _ = clipboard.set_text(text);
        }
    }

    fn paste_clipboard(&mut self) {
        let text = match self.clipboard.as_mut() {
            Some(clipboard) => clipboard.get_text().ok(),
            None => None,
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            return;
        };
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let pty_id = tab.pty_id;
        let mut bytes = Vec::new();
        if tab.core.bracketed_paste() {
            // Strip the end marker so pasted content can't break out of the
            // bracketed-paste guard and inject commands.
            let sanitized = text.replace("\u{1b}[201~", "");
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(sanitized.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
        } else {
            bytes.extend_from_slice(text.as_bytes());
        }
        let _ = self.pty.write(pty_id, &bytes);
    }

    /// Detect a copy/paste shortcut: Cmd+C/V on macOS, Ctrl+Shift+C/V elsewhere
    /// (so a bare Ctrl+C still reaches the shell as SIGINT).
    fn clipboard_action(&self, key: Key, mods: Mods) -> Option<ClipAction> {
        let mac = cfg!(target_os = "macos");
        let primary = if mac { mods.sup } else { mods.ctrl };
        if !primary || (!mac && !mods.shift) {
            return None;
        }
        let c = match key {
            Key::Char(c) => c.to_ascii_lowercase(),
            _ => return None,
        };
        match c {
            'c' => Some(ClipAction::Copy),
            'v' => Some(ClipAction::Paste),
            _ => None,
        }
    }

    fn on_resize(&mut self, width: u32, height: u32) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.resize(width, height);
        }
        self.live_resize_until = Some(Instant::now() + LIVE_RESIZE_REDRAW_GRACE);
        self.schedule_terminal_grid_resize(false);
        self.request_redraw();
    }

    fn sync_surface_to_window_size(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        if gpu.sync_to_window_size() {
            self.live_resize_until = Some(Instant::now() + LIVE_RESIZE_REDRAW_GRACE);
            self.schedule_terminal_grid_resize(false);
        }
    }

    fn sync_terminal_grid(&mut self, force: bool) {
        let (rows, cols) = self.viewport_grid();
        let grid = (rows, cols);
        if !force && self.last_terminal_grid == Some(grid) {
            return;
        }
        self.pending_terminal_grid = None;
        self.terminal_resize_deadline = None;
        self.apply_terminal_grid(rows, cols);
    }

    fn schedule_terminal_grid_resize(&mut self, force: bool) {
        let (rows, cols) = self.viewport_grid();
        let grid = (rows, cols);
        if !force {
            if self.last_terminal_grid == Some(grid) {
                self.pending_terminal_grid = None;
                self.terminal_resize_deadline = None;
                return;
            }
            if self.pending_terminal_grid == Some(grid) {
                self.terminal_resize_deadline = Some(Instant::now() + TERMINAL_RESIZE_DEBOUNCE);
                return;
            }
        }
        if self.tabs.is_empty() {
            self.pending_terminal_grid = None;
            self.terminal_resize_deadline = None;
            self.last_terminal_grid = Some(grid);
            return;
        }
        self.pending_terminal_grid = Some(grid);
        self.terminal_resize_deadline = Some(Instant::now() + TERMINAL_RESIZE_DEBOUNCE);
    }

    fn apply_terminal_grid(&mut self, rows: u16, cols: u16) {
        for tab in &mut self.tabs {
            tab.core.resize(rows, cols);
            if self.pty.resize(tab.pty_id, rows, cols).is_ok() {
                tab.expect_prompt_repaint();
            }
        }
        self.last_terminal_grid = Some((rows, cols));
    }

    fn flush_pending_terminal_resize(&mut self) {
        let Some((rows, cols)) = self.pending_terminal_grid.take() else {
            self.terminal_resize_deadline = None;
            return;
        };
        self.apply_terminal_grid(rows, cols);
        self.terminal_resize_deadline = None;
        self.request_redraw();
    }

    /// Poll the active tab's shell cwd; update its title and persist on change.
    fn poll_cwd(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let pty_id = tab.pty_id;
        let current = tab.cwd.clone();
        if let Some(cwd) = self.pty.cwd(pty_id) {
            if cwd != current {
                self.tabs[self.active].cwd = cwd;
                self.mark_dirty();
                self.request_redraw();
            }
        }
    }

    fn save_tabs(&mut self) {
        if self.ephemeral {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let active = self.active;
        let records: Vec<TabRecord> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let title = if t.custom_title.is_empty() {
                    t.title()
                } else {
                    t.custom_title.clone()
                };
                TabRecord {
                    id: Some(t.id.to_string()),
                    title: sanitize_title(&title),
                    cwd: t.cwd.clone(),
                    sort_order: i as i64,
                    is_active: i == active,
                    shell_profile_id: t.profile_id.clone(),
                    created_at: None,
                    updated_at: None,
                }
            })
            .collect();
        let result = if records.is_empty() {
            store.clear_tabs()
        } else {
            store.save_tabs(&records)
        };
        if let Err(e) = result {
            eprintln!("failed to save tabs: {e}");
            self.show_notice(format!("Could not remember tabs: {e}"));
        }
    }

    fn render(&mut self) {
        self.redraw_queued = false;
        self.sync_surface_to_window_size();
        let ui_outcome = match (self.gpu.as_ref(), self.egui.as_mut()) {
            (Some(gpu), Some(egui)) => egui.run(
                &gpu.window,
                &mut self.ui,
                &mut self.config,
                &mut self.palette,
            ),
            _ => Default::default(),
        };
        if ui_outcome.config_changed {
            self.apply_config_change();
        }
        if let Some(action) = ui_outcome.palette_action {
            self.dispatch_palette(action);
        }
        if self.pending_terminal_grid.is_none() {
            self.sync_terminal_grid(false);
        }
        let drag_indicator = self.tab_drag_indicator();
        let custom_window_chrome = self.custom_window_chrome();

        let (Some(gpu), Some(renderer)) = (self.gpu.as_mut(), self.renderer.as_mut()) else {
            return;
        };
        let (w, h) = gpu.size();
        let layout = chrome::compute_layout(
            w.max(1) as f32,
            h as f32,
            renderer.cell_size().1,
            self.config.tab_layout != "vertical",
            chrome::WindowChrome::from_config(&self.applied_window_chrome),
        );
        let colors = chrome::ChromeColors::from_renderer(
            renderer,
            themes::ui_theme_accent(&self.config.ui_theme),
        );
        let titles: Vec<String> = self.tabs.iter().map(|t| t.title()).collect();
        let chrome_animating = self.chrome_anim.advance(Instant::now(), titles.len());

        renderer.begin();
        chrome::draw_window_backfill(renderer, &layout, &colors, w as f32, h as f32);
        let terminal_pane = chrome::terminal_pane(&layout);
        renderer.draw_terminal_backdrop(
            &self.config.terminal_background,
            self.config.terminal_background_opacity,
            terminal_pane.x,
            terminal_pane.y,
            terminal_pane.w,
            terminal_pane.h,
        );
        let hits = chrome::draw_tab_bar(
            renderer,
            &gpu.queue,
            &layout,
            &titles,
            self.active,
            &colors,
            self.rename.as_deref(),
            self.ui.settings_open(),
            self.ephemeral,
            &self.chrome_anim,
            drag_indicator,
        );
        if let Some(tab) = self.tabs.get(self.active) {
            let snapshot = tab.core.snapshot();
            if let Some((rows, cols)) = self.pending_terminal_grid {
                renderer.draw_terminal_clipped(
                    &gpu.queue,
                    &snapshot,
                    self.cursor_on,
                    layout.viewport.x,
                    layout.viewport.y,
                    (rows as usize, cols as usize),
                );
            } else {
                renderer.draw_terminal(
                    &gpu.queue,
                    &snapshot,
                    self.cursor_on,
                    layout.viewport.x,
                    layout.viewport.y,
                );
            }
            let scrollbar_active = self.scroll_drag.is_some()
                || (snapshot.scroll.is_scrollable()
                    && chrome::terminal_scrollbar_hit_track(&layout)
                        .contains(self.cursor_pos.0, self.cursor_pos.1));
            chrome::draw_terminal_scrollbar(
                renderer,
                &layout,
                snapshot.scroll,
                &colors,
                scrollbar_active,
            );
            // IME composition text, drawn at the cursor with an underline.
            if !self.preedit.is_empty() {
                let (cw, ch) = renderer.cell_size();
                let px = layout.viewport.x + snapshot.cursor.col as f32 * cw;
                let py = layout.viewport.y + snapshot.cursor.row as f32 * ch;
                let width = renderer.text_width(&self.preedit);
                renderer.fill_rect(px, py, width, ch, colors.bar_bg);
                renderer.text(&gpu.queue, px, py, &self.preedit, colors.text);
                renderer.fill_rect(px, py + ch - 2.0, width, 2.0, colors.accent);
            }
        }
        if let Some(notice) = self.notice.as_ref() {
            renderer.begin_overlay();
            let pad = 10.0;
            let cell_h = renderer.cell_size().1;
            let text_w = renderer.text_width(&notice.text);
            let available_w = (w as f32 - pad * 2.0).max(40.0);
            let min_w = available_w.min(120.0);
            let box_w = (text_w + pad * 2.0).clamp(min_w, available_w);
            let box_h = cell_h + pad * 2.0;
            let x = ((w as f32 - box_w) / 2.0).max(pad);
            let y = (h as f32 - box_h - pad).max(pad);
            renderer.fill_rect(x, y, box_w, box_h, [12, 16, 22, 235]);
            renderer.fill_rect(x, y, 3.0, box_h, colors.accent);
            renderer.text(
                &gpu.queue,
                x + pad,
                y + pad,
                &truncate_to_fit(&notice.text, renderer, box_w - pad * 2.0),
                colors.text,
            );
        }
        renderer.end(&gpu.device, &gpu.queue);
        if let Some(egui) = self.egui.as_mut() {
            gpu.present_with_overlay(renderer, Some(egui), custom_window_chrome);
        } else {
            gpu.present(renderer);
        }
        self.last_hits = Some(hits);
        self.update_chrome_hover();
        if chrome_animating {
            self.request_redraw();
        }
    }

    fn pump_blink(&mut self) -> bool {
        if !self.blink_enabled {
            return false;
        }
        let now = Instant::now();
        if now >= self.next_toggle {
            self.cursor_on = !self.cursor_on;
            self.next_toggle = now + BLINK_INTERVAL;
            return true;
        }
        false
    }

    fn reset_blink(&mut self) {
        self.cursor_on = true;
        self.next_toggle = Instant::now() + BLINK_INTERVAL;
    }
}

/// Translate a winit window event into an engine-independent [`AppInput`], using
/// the current modifier state for key events. Returns `None` for events we
/// ignore (and for `RedrawRequested`, which the caller handles directly).
fn translate(event: &WindowEvent, mods: Mods, cursor: (f32, f32)) -> Option<AppInput> {
    Some(match event {
        WindowEvent::CloseRequested => AppInput::CloseRequested,
        WindowEvent::ModifiersChanged(m) => {
            AppInput::ModifiersChanged(input::winit_mods(m.state()))
        }
        WindowEvent::Resized(size) => AppInput::Resized {
            width: size.width,
            height: size.height,
        },
        WindowEvent::ScaleFactorChanged { .. } => AppInput::ScaleChanged,
        WindowEvent::Ime(ime) => match ime {
            Ime::Commit(text) => AppInput::ImeCommit(text.clone()),
            Ime::Preedit(text, _) => AppInput::ImePreedit(text.clone()),
            Ime::Enabled | Ime::Disabled => AppInput::ImePreedit(String::new()),
        },
        WindowEvent::CursorMoved { position, .. } => AppInput::MouseMove {
            x: position.x as f32,
            y: position.y as f32,
        },
        WindowEvent::MouseInput {
            state,
            button: MouseButton::Left,
            ..
        } => match state {
            ElementState::Pressed => AppInput::MouseDown {
                x: cursor.0,
                y: cursor.1,
            },
            ElementState::Released => AppInput::MouseUp {
                x: cursor.0,
                y: cursor.1,
            },
        },
        WindowEvent::MouseWheel { delta, .. } => {
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => *y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / 24.0,
            };
            if lines.abs() < f32::EPSILON {
                return None;
            }
            AppInput::Wheel { lines }
        }
        WindowEvent::KeyboardInput { event, .. } => {
            if event.state != ElementState::Pressed {
                return None;
            }
            let key = input::map_key(&event.logical_key)?;
            AppInput::Key {
                key,
                text: event.text.as_ref().map(|s| s.to_string()),
                mods,
            }
        }
        _ => return None,
    })
}

fn terminal_owns_keyboard(palette_open: bool, renaming: bool, settings_open: bool) -> bool {
    !palette_open && !renaming && !settings_open
}

fn app_input_bypasses_egui_overlay(input: &AppInput) -> bool {
    matches!(
        input,
        AppInput::Resized { .. }
            | AppInput::ScaleChanged
            | AppInput::ModifiersChanged(_)
            | AppInput::CloseRequested
    )
}

fn is_keyboard_event(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
    )
}

fn renderer_signature(config: &AppConfig) -> String {
    let theme = &config.theme;
    [
        config.font_family.as_str(),
        &config.font_size.to_string(),
        &config.line_height.to_bits().to_string(),
        theme.background.as_str(),
        theme.foreground.as_str(),
        theme.cursor.as_str(),
        theme.selection.as_str(),
        theme.black.as_str(),
        theme.red.as_str(),
        theme.green.as_str(),
        theme.yellow.as_str(),
        theme.blue.as_str(),
        theme.magenta.as_str(),
        theme.cyan.as_str(),
        theme.white.as_str(),
        theme.bright_black.as_str(),
        theme.bright_red.as_str(),
        theme.bright_green.as_str(),
        theme.bright_yellow.as_str(),
        theme.bright_blue.as_str(),
        theme.bright_magenta.as_str(),
        theme.bright_cyan.as_str(),
        theme.bright_white.as_str(),
    ]
    .join("\x1f")
}

fn window_attributes(config: &AppConfig) -> WindowAttributes {
    let attrs = Window::default_attributes().with_title("Phantom Terminal");
    if chrome::WindowChrome::from_config(&config.window_chrome) != chrome::WindowChrome::Custom {
        return attrs;
    }
    custom_window_attributes(attrs)
}

#[cfg(target_os = "macos")]
fn custom_window_attributes(attrs: WindowAttributes) -> WindowAttributes {
    attrs
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true)
}

#[cfg(not(target_os = "macos"))]
fn custom_window_attributes(attrs: WindowAttributes) -> WindowAttributes {
    attrs.with_decorations(false).with_transparent(true)
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let custom_window_chrome = self.custom_window_chrome();
        let window = Arc::new(
            event_loop
                .create_window(window_attributes(&self.config))
                .expect("failed to create window"),
        );
        let gpu = match pollster::block_on(GpuContext::new(
            window.clone(),
            event_loop,
            custom_window_chrome,
        )) {
            Ok(gpu) => gpu,
            Err(error) => {
                window.set_title(&format!("Phantom Terminal - {error}"));
                self.show_notice(error);
                return;
            }
        };
        gpu.window.set_ime_allowed(true);
        let renderer = Renderer::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            &self.config,
            gpu.scale_factor(),
        );
        let (w, h) = gpu.size();
        renderer.resize(&gpu.queue, w, h);
        let egui = EguiLayer::new(&gpu.window, &gpu.device, gpu.format());

        self.gpu = Some(gpu);
        self.renderer = Some(renderer);
        self.egui = Some(egui);

        self.start();
        self.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        self.on_pty_event(event);
        if self.exit_requested {
            self.flush_config_save();
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::RedrawRequested) {
            self.render();
            return;
        }
        if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.clear_chrome_hover();
        }

        let terminal_owns_keyboard = terminal_owns_keyboard(
            self.palette.open,
            self.rename.is_some(),
            self.ui.settings_open(),
        );
        let egui_response = if terminal_owns_keyboard && is_keyboard_event(&event) {
            None
        } else {
            match (self.gpu.as_ref(), self.egui.as_mut()) {
                (Some(gpu), Some(egui)) => Some(egui.on_window_event(&gpu.window, &event)),
                _ => None,
            }
        };
        if egui_response.is_some_and(|response| response.repaint) {
            self.request_redraw();
        }
        let consumed = egui_response.is_some_and(|response| response.consumed);
        let input = translate(&event, self.mods, self.cursor_pos);

        if let Some(input) = input {
            let palette_owns_input = self.palette.open && self.egui.is_some();
            if app_input_bypasses_egui_overlay(&input) || (!consumed && !palette_owns_input) {
                self.handle_input(input);
            }
        }
        if self.exit_requested {
            self.flush_config_save();
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.pump_blink() {
            self.request_redraw();
        }
        if now >= self.cwd_deadline {
            self.poll_cwd();
            self.cwd_deadline = now + CWD_POLL_INTERVAL;
        }
        if let Some(deadline) = self.save_after {
            if now >= deadline {
                self.save_tabs();
                self.save_after = None;
            }
        }
        if let Some(deadline) = self.config_save_after {
            if now >= deadline {
                self.save_config();
                self.config_save_after = None;
            }
        }
        if let Some(deadline) = self.renderer_rebuild_after {
            if now >= deadline {
                self.renderer_rebuild_after = None;
                if self.renderer_signature != renderer_signature(&self.config) {
                    self.rebuild_renderer();
                }
            }
        }
        if let Some(deadline) = self.terminal_resize_deadline {
            if now >= deadline {
                self.flush_pending_terminal_resize();
            }
        }
        self.clear_expired_notice(now);
        if self.exit_requested {
            self.flush_config_save();
            event_loop.exit();
            return;
        }
        if let Some(until) = self.live_resize_until {
            if now < until {
                self.request_redraw();
                event_loop.set_control_flow(ControlFlow::Poll);
                return;
            }
            self.live_resize_until = None;
        }

        let mut next = self.cwd_deadline;
        if self.blink_enabled {
            next = next.min(self.next_toggle);
        }
        if let Some(deadline) = self.save_after {
            next = next.min(deadline);
        }
        if let Some(deadline) = self.config_save_after {
            next = next.min(deadline);
        }
        if let Some(deadline) = self.renderer_rebuild_after {
            next = next.min(deadline);
        }
        if let Some(deadline) = self.terminal_resize_deadline {
            next = next.min(deadline);
        }
        if let Some(notice) = self.notice.as_ref() {
            next = next.min(notice.until);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }
}

fn cursor_shape(style: &str) -> CursorShape {
    match style {
        "bar" => CursorShape::Beam,
        "underline" => CursorShape::Underline,
        _ => CursorShape::Block,
    }
}

fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "shell".to_string(),
    }
}

fn sanitize_title(title: &str) -> String {
    let trimmed = title.trim();
    let clean: String = trimmed.chars().filter(|c| !c.is_control()).collect();
    truncate_to_chars(&clean, MAX_TAB_TITLE_LEN)
}

fn truncate_to_chars(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut out = String::new();
    for ch in value.chars() {
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

fn truncate_to_fit(value: &str, renderer: &Renderer, max_width: f32) -> String {
    let cell_w = renderer.cell_size().0.max(1.0);
    let max_chars = (max_width / cell_w).floor().max(1.0) as usize;
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let mut out: String = value.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// Run the app under a winit event loop (the production entry point).
pub fn run() {
    let store = SessionStore::open()
        .map_err(|e| eprintln!("session store unavailable ({e}); not persisting"))
        .ok();
    let config = store
        .as_ref()
        .and_then(|s| s.load_config().ok())
        .unwrap_or_default();
    let launch = phantom_core::LaunchState::from_env().context();

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .expect("failed to build event loop");
    let proxy = event_loop.create_proxy();
    let outbox: Arc<dyn PtyOutbox> = Arc::new(ProxyOutbox(proxy));
    let mut app = App::new(outbox, config, store, launch);
    event_loop.run_app(&mut app).expect("event loop error");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopOutbox;

    impl PtyOutbox for NoopOutbox {
        fn send(&self, _event: AppEvent) -> bool {
            true
        }
    }

    fn test_app() -> App {
        App::new(
            Arc::new(NoopOutbox),
            AppConfig::default(),
            None,
            LaunchContext {
                cwd: None,
                remember_tabs: true,
            },
        )
    }

    #[test]
    fn keyboard_events_bypass_egui_only_when_terminal_owns_input() {
        assert!(terminal_owns_keyboard(false, false, false));
        assert!(!terminal_owns_keyboard(true, false, false));
        assert!(!terminal_owns_keyboard(false, true, false));
        assert!(!terminal_owns_keyboard(false, false, true));
    }

    #[test]
    fn lifecycle_inputs_bypass_egui_overlays() {
        assert!(app_input_bypasses_egui_overlay(
            &AppInput::ModifiersChanged(Mods::default())
        ));
        assert!(app_input_bypasses_egui_overlay(&AppInput::Resized {
            width: 800,
            height: 600,
        }));
        assert!(app_input_bypasses_egui_overlay(&AppInput::CloseRequested));
        assert!(!app_input_bypasses_egui_overlay(&AppInput::Key {
            key: Key::Char('x'),
            text: Some("x".to_string()),
            mods: Mods::default(),
        }));
    }

    #[test]
    fn repeated_clicks_on_same_cell_count_to_triple_click() {
        let mut app = test_app();

        assert_eq!(app.terminal_click_count(1, 2), 1);
        assert_eq!(app.terminal_click_count(1, 2), 2);
        assert_eq!(app.terminal_click_count(1, 2), 3);
        assert_eq!(app.terminal_click_count(1, 2), 3);
        assert_eq!(app.terminal_click_count(1, 3), 1);
    }

    #[test]
    fn terminal_grid_resize_is_debounced_without_force() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(2, 20, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.last_terminal_grid = Some((24, 80));

        app.schedule_terminal_grid_resize(false);
        assert_eq!(app.tabs[0].core.size(), (2, 20));
        assert_eq!(app.pending_terminal_grid, None);

        app.schedule_terminal_grid_resize(true);
        assert_eq!(app.tabs[0].core.size(), (2, 20));
        assert_eq!(app.pending_terminal_grid, Some((24, 80)));

        app.flush_pending_terminal_resize();
        assert_eq!(app.tabs[0].core.size(), (24, 80));
        assert_eq!(app.pending_terminal_grid, None);
        assert_eq!(app.terminal_resize_deadline, None);
    }

    #[test]
    fn forced_terminal_grid_sync_applies_immediately() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(2, 20, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.last_terminal_grid = Some((24, 80));

        app.sync_terminal_grid(true);
        assert_eq!(app.tabs[0].core.size(), (24, 80));
        assert_eq!(app.pending_terminal_grid, None);
        assert_eq!(app.terminal_resize_deadline, None);
    }

    #[test]
    fn debounced_terminal_grid_resize_cancels_when_grid_returns_to_applied_size() {
        let mut app = test_app();
        app.last_terminal_grid = Some((24, 80));
        app.pending_terminal_grid = Some((24, 40));
        app.terminal_resize_deadline = Some(Instant::now());

        app.schedule_terminal_grid_resize(false);

        assert_eq!(app.pending_terminal_grid, None);
        assert_eq!(app.terminal_resize_deadline, None);
    }

    #[test]
    fn repeated_resize_event_for_same_pending_grid_extends_deadline() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(2, 20, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.last_terminal_grid = Some((10, 10));
        app.pending_terminal_grid = Some((24, 80));
        let old_deadline = Instant::now() + Duration::from_millis(1);
        app.terminal_resize_deadline = Some(old_deadline);

        app.schedule_terminal_grid_resize(false);

        assert_eq!(app.pending_terminal_grid, Some((24, 80)));
        assert!(app.terminal_resize_deadline.unwrap() > old_deadline);
    }

    #[test]
    fn renderer_affecting_config_changes_are_debounced() {
        let mut app = test_app();
        let initial_signature = app.renderer_signature.clone();

        app.config.font_size += 1;
        app.apply_config_change();

        assert_eq!(app.renderer_signature, initial_signature);
        assert!(app.renderer_rebuild_after.is_some());
    }

    #[test]
    fn non_renderer_config_changes_do_not_schedule_renderer_rebuild() {
        let mut app = test_app();

        app.config.terminal_background_opacity =
            app.config.terminal_background_opacity.saturating_add(1);
        app.apply_config_change();

        assert!(app.config_save_after.is_some());
        assert!(app.renderer_rebuild_after.is_none());
    }

    #[test]
    fn keybinding_config_changes_refresh_runtime_keymap() {
        let mut app = test_app();
        let mut mods = Mods::default();
        if cfg!(target_os = "macos") {
            mods.sup = true;
        } else {
            mods.ctrl = true;
        }

        app.config.keybindings = vec![phantom_core::Keybinding {
            id: "new-tab".to_string(),
            action: "tab.new".to_string(),
            keys: "CmdOrCtrl+N".to_string(),
        }];
        app.apply_config_change();

        assert_eq!(app.keymap.lookup(Key::Char('t'), mods), None);
        assert_eq!(
            app.keymap.lookup(Key::Char('n'), mods),
            Some(Action::NewTab)
        );
    }
}
