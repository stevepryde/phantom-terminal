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

pub mod cli;
pub mod event;

mod blur;
mod chrome;
mod context_actions;
mod context_ui;
mod egui_ui;
mod external_editor;
mod find;
mod find_worker;
mod frequent_commands;
mod gpu;
mod input;
mod keybindings;
mod palette;
mod skill_install;
mod tab;
mod themes;
mod ui_components;

use std::collections::VecDeque;
#[cfg(any(
    test,
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_ui::{EguiLayer, UiFrameContext, UiState};
use event::{AppInput, AppMouseButton, Mods};
use find::{FindError as FindUiError, FindNavigation, FindResultSummary};
use find_worker::{FindResponse, FindWorker};
use gpu::{GpuContext, PresentStatus};
use keybindings::{Action, BuiltinShortcut, Keymap};
use palette::{PaletteAction, PaletteOutcome, PaletteState};
use phantom_core::{
    default_home_dir, resolve_launch_opts, resolve_program, resolve_trusted_task,
    trust_context_manifest, AppConfig, LaunchContext, LaunchOpts, PtyManager, PtySink,
    SessionStore, StartupCommand, TabRecord, WindowSize, MAX_TABS, MAX_TAB_TITLE_LEN,
};
use phantom_emu::{
    encode_key, encode_mouse_legacy, encode_mouse_sgr, AlacrittyCore, CursorShape, Key,
    MouseProtocol, ScrollState, SearchMatch, SearchOptions, SearchOutcome, SearchRange, SelSide,
    SelectionKind, VtCore,
};
use phantom_gfx::Renderer;
use tab::Tab;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;
#[cfg(target_os = "linux")]
use winit::window::ResizeDirection;
use winit::window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId};

use context_actions::{discover_context, ContextRequest, ContextSnapshot};
use egui_winit::accesskit_winit::{Event as AccessKitEvent, WindowEvent as AccessKitWindowEvent};

const BLINK_INTERVAL: Duration = Duration::from_millis(530);
const CWD_POLL_INTERVAL: Duration = Duration::from_secs(1);
const CWD_POLL_WINDOW: Duration = Duration::from_secs(6);
const CWD_STARTUP_POLL_WINDOW: Duration = Duration::from_secs(3);
const LIVE_RESIZE_REDRAW_GRACE: Duration = Duration::from_millis(120);
const TERMINAL_RESIZE_DEBOUNCE: Duration = Duration::from_millis(300);
const FIND_REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);
const WINDOW_SIZE_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
const NOTICE_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_NOTICE_LEN: usize = 240;
const MAX_PENDING_PTY_BYTES: usize = 1024 * 1024;
const PTY_BACKPRESSURE_WAKE_INTERVAL: Duration = Duration::from_millis(50);
const SURFACE_RETRY_DELAY: Duration = Duration::from_millis(33);
const SURFACE_OCCLUDED_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Consecutive fatal present failures tolerated before giving up and exiting.
const MAX_FATAL_PRESENTS: u32 = 10;
/// Logical px the tab strip scrolls per wheel notch line.
const TAB_STRIP_WHEEL_PX: f32 = 24.0;
const TAB_DRAG_TOLERANCE_LOGICAL_PX: f32 = 8.0;

fn tab_drag_moved_past_tolerance(
    start: (f32, f32),
    current: (f32, f32),
    scale_factor: f32,
) -> bool {
    let dx = current.0 - start.0;
    let dy = current.1 - start.1;
    let tolerance = TAB_DRAG_TOLERANCE_LOGICAL_PX * scale_factor.max(1.0);
    dx * dx + dy * dy >= tolerance * tolerance
}

/// The only supported entry point for creating a terminal tab. Every caller,
/// including contextual actions, starts from the same validated shell profile
/// resolution used by Ctrl+T and may only add typed startup behavior.
struct NewTabRequest {
    profile_id: Option<String>,
    cwd: Option<String>,
    startup: Option<StartupCommand>,
    title: Option<String>,
    return_to_shell: bool,
    persist: bool,
}

impl NewTabRequest {
    fn shell(profile_id: Option<String>, cwd: Option<String>, persist: bool) -> Self {
        Self {
            profile_id,
            cwd,
            startup: None,
            title: None,
            return_to_shell: false,
            persist,
        }
    }
}

fn resolve_new_tab_launch(
    config: &AppConfig,
    profile_id: Option<&str>,
    cwd: Option<String>,
    startup: Option<StartupCommand>,
    rows: u16,
    cols: u16,
) -> phantom_core::AppResult<LaunchOpts> {
    let mut launch = resolve_launch_opts(config, profile_id, cwd, rows, cols)?;
    launch.startup = startup;
    Ok(launch)
}

fn existing_directory(cwd: &str) -> Option<String> {
    (!cwd.is_empty() && Path::new(cwd).is_dir()).then(|| cwd.to_string())
}

#[derive(Default)]
struct CwdPollResult {
    changed: bool,
    active_changed: bool,
}

fn update_tab_cwds(
    tabs: &mut [Tab],
    active: usize,
    mut cwd_for: impl FnMut(u32) -> Option<String>,
) -> CwdPollResult {
    let mut result = CwdPollResult::default();
    for (index, tab) in tabs.iter_mut().enumerate() {
        let Some(cwd) = cwd_for(tab.pty_id) else {
            continue;
        };
        if cwd == tab.cwd {
            continue;
        }
        tab.cwd = cwd;
        result.changed = true;
        result.active_changed |= index == active;
    }
    result
}
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);
const WHEEL_LINES_PER_NOTCH: f32 = 3.0;
/// Pixel-delta scroll divisor used only before the renderer exists.
const FALLBACK_SCROLL_CELL_HEIGHT_PX: f32 = 24.0;

/// PTY output delivered from a reader thread, tagged with the tab it belongs to.
#[derive(Debug)]
pub enum AppEvent {
    PtyBytes {
        tab: u64,
        bytes: Vec<u8>,
    },
    PtyExit {
        tab: u64,
    },
    ContextDiscovered {
        generation: u64,
        snapshot: Box<ContextSnapshot>,
    },
    EditorOpenFailed {
        error: String,
    },
    FindWake,
    AccessKit(AccessKitEvent),
    PtyWake,
}

impl From<AccessKitEvent> for AppEvent {
    fn from(event: AccessKitEvent) -> Self {
        Self::AccessKit(event)
    }
}

/// Sink for PTY output events. The winit host wraps an [`EventLoopProxy`]; tests
/// wrap a collector. Returning `false` tells the reader thread to stop.
pub trait PtyOutbox: Send + Sync + 'static {
    fn send(&self, event: AppEvent) -> bool;
}

/// Production outbox: wakes the winit loop with the event.
struct ProxyOutbox {
    proxy: EventLoopProxy<AppEvent>,
    pending: Arc<PendingPtyEvents>,
}

impl PtyOutbox for ProxyOutbox {
    fn send(&self, event: AppEvent) -> bool {
        let should_wake = match self.pending.push(event) {
            Some(should_wake) => should_wake,
            None => return false,
        };
        if !should_wake {
            return true;
        }
        if self.proxy.send_event(AppEvent::PtyWake).is_ok() {
            true
        } else {
            self.pending.close();
            false
        }
    }
}

struct PendingPtyEvents {
    inner: Mutex<PendingPtyInner>,
    drained: Condvar,
}

struct PendingPtyInner {
    events: VecDeque<AppEvent>,
    queued_bytes: usize,
    wake_queued: bool,
    closed: bool,
}

impl PendingPtyEvents {
    fn new() -> Self {
        Self {
            inner: Mutex::new(PendingPtyInner {
                events: VecDeque::new(),
                queued_bytes: 0,
                wake_queued: false,
                closed: false,
            }),
            drained: Condvar::new(),
        }
    }

    fn push(&self, event: AppEvent) -> Option<bool> {
        let incoming_bytes = event_byte_len(&event);
        let mut inner = self.inner.lock().expect("pending pty queue mutex poisoned");
        while !inner.closed
            && incoming_bytes > 0
            && inner.queued_bytes.saturating_add(incoming_bytes) > MAX_PENDING_PTY_BYTES
        {
            let (next, _) = self
                .drained
                .wait_timeout(inner, PTY_BACKPRESSURE_WAKE_INTERVAL)
                .expect("pending pty queue mutex poisoned");
            inner = next;
        }
        if inner.closed {
            return None;
        }

        push_pending_pty_event(&mut inner, event, incoming_bytes);
        if inner.wake_queued {
            Some(false)
        } else {
            inner.wake_queued = true;
            Some(true)
        }
    }

    fn drain(&self) -> Vec<AppEvent> {
        let mut inner = self.inner.lock().expect("pending pty queue mutex poisoned");
        let events = inner.events.drain(..).collect();
        inner.queued_bytes = 0;
        inner.wake_queued = false;
        self.drained.notify_all();
        events
    }

    fn close(&self) {
        let mut inner = self.inner.lock().expect("pending pty queue mutex poisoned");
        inner.closed = true;
        self.drained.notify_all();
    }
}

fn event_byte_len(event: &AppEvent) -> usize {
    match event {
        AppEvent::PtyBytes { bytes, .. } => bytes.len(),
        AppEvent::PtyExit { .. }
        | AppEvent::ContextDiscovered { .. }
        | AppEvent::EditorOpenFailed { .. }
        | AppEvent::FindWake
        | AppEvent::AccessKit(_)
        | AppEvent::PtyWake => 0,
    }
}

fn push_pending_pty_event(inner: &mut PendingPtyInner, event: AppEvent, incoming_bytes: usize) {
    if let AppEvent::PtyBytes { tab, bytes } = event {
        if let Some(AppEvent::PtyBytes {
            tab: last_tab,
            bytes: last_bytes,
        }) = inner.events.back_mut()
        {
            if *last_tab == tab {
                last_bytes.extend_from_slice(&bytes);
                inner.queued_bytes = inner.queued_bytes.saturating_add(incoming_bytes);
                return;
            }
        }
        inner.events.push_back(AppEvent::PtyBytes { tab, bytes });
    } else {
        inner.events.push_back(event);
    }
    inner.queued_bytes = inner.queued_bytes.saturating_add(incoming_bytes);
}

enum PersistMsg {
    SaveTabs(Vec<TabRecord>),
    SaveConfig(Box<AppConfig>),
    Flush(mpsc::Sender<()>),
}

struct Persistence {
    store: Option<SessionStore>,
    tx: Option<mpsc::Sender<PersistMsg>>,
    error_rx: Option<mpsc::Receiver<String>>,
}

impl Persistence {
    fn new(store: Option<SessionStore>) -> Self {
        let Some(worker_store) = store.clone() else {
            return Self {
                store,
                tx: None,
                error_rx: None,
            };
        };
        let (tx, rx) = mpsc::channel();
        let (error_tx, error_rx) = mpsc::channel();
        thread::spawn(move || persistence_worker(worker_store, rx, error_tx));
        Self {
            store,
            tx: Some(tx),
            error_rx: Some(error_rx),
        }
    }

    fn save_tabs(&self, records: Vec<TabRecord>) -> Vec<String> {
        self.send_or_sync(PersistMsg::SaveTabs(records))
    }

    fn save_config(&self, config: AppConfig) -> Vec<String> {
        self.send_or_sync(PersistMsg::SaveConfig(Box::new(config)))
    }

    fn flush(&self) -> Vec<String> {
        let (Some(_), Some(tx)) = (self.store.as_ref(), self.tx.as_ref()) else {
            return Vec::new();
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        if let Err(error) = tx.send(PersistMsg::Flush(reply_tx)) {
            return vec![format!("persistence worker unavailable: {error}")];
        }
        if let Err(error) = reply_rx.recv() {
            return vec![format!("persistence worker did not flush: {error}")];
        }
        self.drain_errors()
    }

    fn drain_errors(&self) -> Vec<String> {
        self.error_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    fn send_or_sync(&self, msg: PersistMsg) -> Vec<String> {
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        if let Some(tx) = self.tx.as_ref() {
            match tx.send(msg) {
                Ok(()) => return Vec::new(),
                Err(error) => return Self::apply_sync(store, error.0),
            }
        }
        Self::apply_sync(store, msg)
    }

    fn apply_sync(store: &SessionStore, msg: PersistMsg) -> Vec<String> {
        match msg {
            PersistMsg::SaveTabs(records) => persist_tabs(store, records)
                .err()
                .map(|error| vec![error])
                .unwrap_or_default(),
            PersistMsg::SaveConfig(config) => store
                .save_config(&config)
                .err()
                .map(|error| vec![error.to_string()])
                .unwrap_or_default(),
            PersistMsg::Flush(reply) => {
                let _ = reply.send(());
                Vec::new()
            }
        }
    }
}

fn persistence_worker(
    store: SessionStore,
    rx: mpsc::Receiver<PersistMsg>,
    error_tx: mpsc::Sender<String>,
) {
    while let Ok(msg) = rx.recv() {
        // Coalesce bursts: a settings-slider drag emits one SaveConfig per
        // frame, and only the newest of each kind needs to hit disk. Flush
        // replies are sent after the batch they arrived with is applied.
        let mut tabs = None;
        let mut config = None;
        let mut flushes = Vec::new();
        let mut next = Some(msg);
        while let Some(msg) = next {
            match msg {
                PersistMsg::SaveTabs(records) => tabs = Some(records),
                PersistMsg::SaveConfig(new_config) => config = Some(new_config),
                PersistMsg::Flush(reply) => flushes.push(reply),
            }
            next = rx.try_recv().ok();
        }
        if let Some(records) = tabs {
            if let Err(error) = persist_tabs(&store, records) {
                let _ = error_tx.send(error);
            }
        }
        if let Some(config) = config {
            if let Err(error) = store.save_config(&config) {
                let _ = error_tx.send(error.to_string());
            }
        }
        for reply in flushes {
            let _ = reply.send(());
        }
    }
}

fn persist_tabs(store: &SessionStore, records: Vec<TabRecord>) -> Result<(), String> {
    let result = if records.is_empty() {
        store.clear_tabs()
    } else {
        store.save_tabs(&records)
    };
    result.map_err(|error| error.to_string())
}

/// Forwards one tab's PTY output to the outbox, tagged with the tab id. Lives on
/// the PTY reader thread.
struct ProxySink {
    outbox: Arc<dyn PtyOutbox>,
    tab: u64,
}

struct ContextDiscoveryRequest {
    generation: u64,
    cwd: PathBuf,
    config: phantom_core::ContextActionsConfig,
    trusted_projects: Vec<phantom_core::TrustedProject>,
    trusted_spdeploy_projects: Vec<phantom_core::TrustedSpdeployProject>,
}

fn spawn_context_discovery_worker(
    outbox: Arc<dyn PtyOutbox>,
) -> mpsc::Sender<ContextDiscoveryRequest> {
    let (tx, rx) = mpsc::channel::<ContextDiscoveryRequest>();
    let _ = thread::Builder::new()
        .name("context-discovery".to_string())
        .spawn(move || {
            while let Ok(mut request) = rx.recv() {
                // Coalesce cwd/config changes queued while a previous provider
                // was running. App-side generation checks discard any result
                // that became stale during discovery.
                while let Ok(newer) = rx.try_recv() {
                    request = newer;
                }
                let snapshot = discover_context(
                    &request.cwd,
                    &request.config,
                    &request.trusted_projects,
                    &request.trusted_spdeploy_projects,
                );
                if !outbox.send(AppEvent::ContextDiscovered {
                    generation: request.generation,
                    snapshot: Box::new(snapshot),
                }) {
                    break;
                }
            }
        });
    tx
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
    Press(TerminalMouseButton),
    Release(TerminalMouseButton),
    Drag,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseUpAction {
    Ignore,
    EndSelection,
    Report,
}

fn mouse_up_action(
    terminal_press_forwarded: bool,
    selecting: bool,
    mouse_reporting: bool,
) -> MouseUpAction {
    if selecting {
        MouseUpAction::EndSelection
    } else if terminal_press_forwarded && mouse_reporting {
        MouseUpAction::Report
    } else {
        MouseUpAction::Ignore
    }
}

#[derive(Clone, Copy)]
enum TerminalMouseButton {
    Left,
    #[cfg(any(target_os = "linux", test))]
    Middle,
}

impl TerminalMouseButton {
    fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            #[cfg(any(target_os = "linux", test))]
            Self::Middle => 1,
        }
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MiddleClickRoute {
    Ignore,
    MouseReport,
    PrimaryPaste,
}

#[cfg(any(target_os = "linux", test))]
fn middle_click_route(in_viewport: bool, mouse_reporting: bool, shift: bool) -> MiddleClickRoute {
    if !in_viewport {
        MiddleClickRoute::Ignore
    } else if mouse_reporting && !shift {
        MiddleClickRoute::MouseReport
    } else {
        MiddleClickRoute::PrimaryPaste
    }
}

#[cfg(any(target_os = "linux", test))]
fn should_report_middle_mouse_release(
    terminal_press_forwarded: bool,
    mouse_reporting: bool,
) -> bool {
    terminal_press_forwarded && mouse_reporting
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WheelRoute {
    TabStrip,
    MouseReport,
    Terminal,
}

trait ClipboardAccess {
    fn set_text(&mut self, text: String) -> Result<(), arboard::Error>;
    fn get_text(&mut self) -> Result<String, arboard::Error>;
    #[cfg(target_os = "linux")]
    fn set_primary_text(&mut self, text: String) -> Result<(), arboard::Error>;
    #[cfg(target_os = "linux")]
    fn get_primary_text(&mut self) -> Result<String, arboard::Error>;
}

impl ClipboardAccess for arboard::Clipboard {
    fn set_text(&mut self, text: String) -> Result<(), arboard::Error> {
        arboard::Clipboard::set_text(self, text)
    }

    fn get_text(&mut self) -> Result<String, arboard::Error> {
        arboard::Clipboard::get_text(self)
    }

    #[cfg(target_os = "linux")]
    fn set_primary_text(&mut self, text: String) -> Result<(), arboard::Error> {
        use arboard::{LinuxClipboardKind, SetExtLinux};

        self.set().clipboard(LinuxClipboardKind::Primary).text(text)
    }

    #[cfg(target_os = "linux")]
    fn get_primary_text(&mut self) -> Result<String, arboard::Error> {
        use arboard::{GetExtLinux, LinuxClipboardKind};

        self.get().clipboard(LinuxClipboardKind::Primary).text()
    }
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
    pending_pty_events: Option<Arc<PendingPtyEvents>>,
    event_loop_proxy: Option<EventLoopProxy<AppEvent>>,
    config: AppConfig,
    pty: PtyManager,
    store: Option<SessionStore>,
    persistence: Persistence,
    mods: Mods,

    gpu: Option<GpuContext>,
    renderer: Option<Renderer>,
    renderer_signature: String,
    egui: Option<EguiLayer>,

    keymap: Keymap,
    tabs: Vec<Tab>,
    /// Reused title storage keeps steady-state chrome rendering allocation-free.
    tab_titles: Vec<String>,
    active: usize,
    next_tab_id: u64,

    cursor_pos: (f32, f32),
    cursor_seen: bool,
    cursor_icon: CursorIcon,
    last_hits: Option<chrome::TabBarHits>,
    chrome_anim: chrome::ChromeAnimationState,
    tab_drag: Option<TabDrag>,
    scroll_drag: Option<ScrollDrag>,
    wheel_line_remainder: f32,
    wheel_route: Option<WheelRoute>,

    clipboard: Option<Box<dyn ClipboardAccess>>,
    left_down: bool,
    #[cfg(target_os = "linux")]
    middle_report_down: bool,
    selecting: bool,
    last_click: Option<LastClick>,
    redraw_queued: bool,
    surface_redraw_after: Option<Instant>,
    /// Consecutive fatal present failures; reset on a successful present.
    fatal_presents: u32,
    /// When egui asked to be repainted (animations); None when egui is idle.
    egui_repaint_after: Option<Instant>,
    /// Mirrors `WindowEvent::Occluded`: while hidden, skip timed redraws.
    window_occluded: bool,
    /// Mirrors `WindowEvent::Focused`: while unfocused, the cursor stays solid
    /// instead of waking the event loop every blink interval.
    window_focused: bool,
    /// Whether the cursor was over the scrollbar track on the last mouse move.
    scrollbar_hovered: bool,
    /// Tab strip scroll offset (physical px); clamped by the chrome each frame.
    tab_scroll: f32,
    /// Scroll this tab fully into the strip on the next frame.
    tab_scroll_into_view: Option<usize>,
    /// In-progress IME composition text (shown at the cursor).
    preedit: String,
    fullscreen: bool,
    /// Set when the app wants to quit; the host drains it.
    exit_requested: bool,

    palette: PaletteState,
    ui: UiState,
    find_matches: Vec<SearchMatch>,
    find_active: usize,
    /// Selection range captured when the current find interaction opened.
    /// It follows Alacritty's rotation while the original selection survives;
    /// once detached, a coordinate-changing terminal mutation expires it.
    find_selection_scope: Option<SearchRange>,
    find_worker: FindWorker,
    find_worker_failure_noticed: bool,
    find_request_generation: Option<u64>,
    find_request_key: Option<(String, SearchOptions, bool)>,
    find_pending: bool,
    /// True when the last search stopped at the emulator's match cap.
    find_capped: bool,
    /// Coalesces PTY-driven find refreshes: a full-history search per PTY
    /// chunk would stall the UI thread during streaming output.
    find_refresh_deadline: Option<Instant>,
    notice: Option<Notice>,
    context_snapshot: ContextSnapshot,
    context_generation: u64,
    context_discovery_tx: mpsc::Sender<ContextDiscoveryRequest>,
    directory_visit_pending: bool,
    last_observed_directory: Option<(u64, PathBuf)>,

    // Launch behaviour.
    launch_cwd: Option<String>,
    remember_tabs: bool,
    ephemeral: bool,
    applied_window_chrome: String,

    // Tab rename edit buffer (active tab) when in rename mode.
    rename: Option<String>,

    // Debounced renderer updates. Persistent config/tab metadata saves in the
    // background as soon as the app state changes.
    renderer_rebuild_after: Option<Instant>,
    live_resize_until: Option<Instant>,
    terminal_resize_deadline: Option<Instant>,
    pending_terminal_grid: Option<(u16, u16)>,
    window_size_save_deadline: Option<Instant>,
    pending_window_size: Option<WindowSize>,
    cwd_deadline: Option<Instant>,
    cwd_poll_until: Option<Instant>,
    last_terminal_grid: Option<(u16, u16)>,

    blink_enabled: bool,
    cursor_on: bool,
    next_toggle: Instant,

    // (scrollback_lines, cursor style) last pushed into the live terminal
    // cores, so settings edits only touch every tab when they change.
    applied_term_options: (u32, CursorShape),
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
        let applied_term_options = (config.scrollback_lines, cursor_shape(&config.cursor_style));
        let applied_window_chrome = config.window_chrome.clone();
        let renderer_signature = renderer_signature(&config);
        let ephemeral = !launch.remember_tabs;
        let now = Instant::now();
        let ui = UiState::new(&config);
        let persistence = Persistence::new(store.clone());
        let context_discovery_tx = spawn_context_discovery_worker(Arc::clone(&outbox));
        let find_worker = FindWorker::new(Arc::clone(&outbox));
        Self {
            outbox,
            pending_pty_events: None,
            event_loop_proxy: None,
            config,
            pty: PtyManager::new(),
            store,
            persistence,
            mods: Mods::default(),
            gpu: None,
            renderer: None,
            renderer_signature,
            egui: None,
            keymap,
            tabs: Vec::new(),
            tab_titles: Vec::new(),
            active: 0,
            next_tab_id: 0,
            cursor_pos: (0.0, 0.0),
            cursor_seen: false,
            cursor_icon: CursorIcon::Default,
            last_hits: None,
            chrome_anim: chrome::ChromeAnimationState::default(),
            tab_drag: None,
            scroll_drag: None,
            wheel_line_remainder: 0.0,
            wheel_route: None,
            clipboard: arboard::Clipboard::new()
                .ok()
                .map(|clipboard| Box::new(clipboard) as Box<dyn ClipboardAccess>),
            left_down: false,
            #[cfg(target_os = "linux")]
            middle_report_down: false,
            selecting: false,
            last_click: None,
            redraw_queued: false,
            surface_redraw_after: None,
            fatal_presents: 0,
            egui_repaint_after: None,
            window_occluded: false,
            window_focused: true,
            scrollbar_hovered: false,
            tab_scroll: 0.0,
            tab_scroll_into_view: None,
            preedit: String::new(),
            fullscreen: false,
            exit_requested: false,
            palette: PaletteState::default(),
            ui,
            find_matches: Vec::new(),
            find_active: 0,
            find_selection_scope: None,
            find_worker,
            find_worker_failure_noticed: false,
            find_request_generation: None,
            find_request_key: None,
            find_pending: false,
            find_capped: false,
            find_refresh_deadline: None,
            notice: None,
            context_snapshot: ContextSnapshot::empty(PathBuf::new()),
            context_generation: 0,
            context_discovery_tx,
            directory_visit_pending: false,
            last_observed_directory: None,
            launch_cwd: launch.cwd,
            remember_tabs: launch.remember_tabs,
            ephemeral,
            applied_window_chrome,
            rename: None,
            renderer_rebuild_after: None,
            live_resize_until: None,
            terminal_resize_deadline: None,
            pending_terminal_grid: None,
            window_size_save_deadline: None,
            pending_window_size: None,
            cwd_deadline: None,
            cwd_poll_until: None,
            last_terminal_grid: None,
            blink_enabled,
            cursor_on: true,
            next_toggle: now + BLINK_INTERVAL,
            applied_term_options,
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
                self.request_exit();
            }
            AppInput::ImeCommit(text) => {
                if self.overlay_owns_terminal_ime() {
                    self.preedit.clear();
                    self.request_redraw();
                } else {
                    self.commit_text(&text);
                }
            }
            AppInput::ImePreedit(text) => {
                if self.overlay_owns_terminal_ime() {
                    self.preedit.clear();
                } else {
                    self.preedit = text;
                }
                self.request_redraw();
            }
            AppInput::MouseMove { x, y } => {
                self.cursor_pos = (x, y);
                self.cursor_seen = true;
                self.on_mouse_move();
            }
            AppInput::MouseDown { x, y, button } => {
                self.cursor_pos = (x, y);
                self.cursor_seen = true;
                self.on_mouse_down(button);
            }
            AppInput::MouseUp { x, y, button } => {
                self.cursor_pos = (x, y);
                self.cursor_seen = true;
                self.on_mouse_up(button);
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
                let active_tab = self
                    .tabs
                    .get(self.active)
                    .is_some_and(|active| active.id == tab);
                let tracks_live_selection = active_tab
                    && !bytes.is_empty()
                    && self.find_selection_scope_tracks_live_selection();
                if active_tab && !bytes.is_empty() {
                    self.find_request_generation = None;
                    self.find_pending = false;
                }
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                    t.advance_pty(&bytes);
                    let out = t.core.take_pty_output();
                    if !out.is_empty() {
                        response = Some((t.pty_id, out));
                    }
                }
                if active_tab && !bytes.is_empty() && self.ui.find_open() {
                    self.sync_find_selection_scope_after_terminal_change(tracks_live_selection);
                }
                if let Some((pty_id, out)) = response {
                    if let Err(error) = self.pty.write_reply(pty_id, &out) {
                        self.show_notice(format!("Could not reply to terminal query: {error}"));
                    }
                }
                if !self.find_worker.advance(tab, bytes, active_tab) {
                    self.handle_find_worker_failure();
                }
                if active_tab {
                    // Debounced rather than refreshed inline: a full-history
                    // search per drained PTY chunk stalls the event loop.
                    if self.find_bar_visible() && self.find_refresh_deadline.is_none() {
                        self.find_refresh_deadline = Some(Instant::now() + FIND_REFRESH_DEBOUNCE);
                    }
                    // Background-tab output changes no pixels — render() only
                    // snapshots the active tab — so it earns no redraw.
                    self.request_redraw();
                }
            }
            AppEvent::PtyExit { tab } => {
                if self.exit_requested {
                    return;
                }
                if let Some(index) = self.tabs.iter().position(|t| t.id == tab) {
                    if self.tabs[index].return_to_shell && self.return_tab_to_shell(index) {
                        return;
                    }
                    self.close_tab(index);
                }
            }
            AppEvent::ContextDiscovered {
                generation,
                snapshot,
            } => {
                if generation == self.context_generation {
                    self.context_snapshot = *snapshot;
                    self.request_redraw();
                }
            }
            AppEvent::EditorOpenFailed { error } => {
                self.show_notice(format!("Could not open .phantom.yml: {error}"));
            }
            AppEvent::FindWake => self.drain_find_results(),
            // Accessibility events use their own direct event-loop path and
            // are handled by the winit host, never the bounded PTY queue.
            AppEvent::AccessKit(_) => {}
            AppEvent::PtyWake => self.drain_pending_pty_events(),
        }
    }

    fn drain_pending_pty_events(&mut self) {
        let Some(pending) = self.pending_pty_events.as_ref() else {
            return;
        };
        let events = pending.drain();
        for event in events {
            self.on_pty_event(event);
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
        self.tabs.get(self.active).map(|t| t.title().to_owned())
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

    pub fn find_open(&self) -> bool {
        self.ui.find_open()
    }

    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_ref().map(|notice| notice.text.as_str())
    }

    #[doc(hidden)]
    pub fn remembered_tab_count_for_tests(&self) -> Option<usize> {
        let _ = self.persistence.flush();
        self.store
            .as_ref()
            .and_then(|store| store.load_tabs().ok())
            .map(|tabs| tabs.len())
    }

    #[doc(hidden)]
    pub fn remembered_tabs_for_tests(&self) -> Option<Vec<TabRecord>> {
        let _ = self.persistence.flush();
        self.store.as_ref().and_then(|store| store.load_tabs().ok())
    }

    // ── Internal logic ─────────────────────────────────────────────────────

    fn horizontal(&self) -> bool {
        self.config.tab_layout != "vertical"
    }

    fn open_find(&mut self) {
        if self.ui.find_open() {
            self.ui.request_find_focus();
            self.request_redraw();
            return;
        }

        self.palette.close();
        if !self.ui.close_panel() {
            self.request_redraw();
            return;
        }
        self.rename = None;
        self.find_selection_scope = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.core.selection_range());
        let selection_available = self.find_selection_scope.is_some();
        self.find_matches.clear();
        self.find_active = 0;
        self.ui.open_find(selection_available);
        self.refresh_find();
        self.request_redraw();
    }

    fn close_find(&mut self) {
        self.find_worker.cancel();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.core.set_search_matches(&[]);
            tab.core.set_active_search_match(None);
        }
        self.ui.close_find();
        self.find_matches.clear();
        self.find_active = 0;
        self.find_selection_scope = None;
        self.find_request_generation = None;
        self.find_request_key = None;
        self.find_pending = false;
        self.find_capped = false;
        self.request_redraw();
    }

    /// Toggle the settings panel, closing an open find session first: the
    /// panel hides the find bar, and find must never stay open (and go stale)
    /// while its bar is not drawn. Both the keyboard action and the chrome
    /// settings button route through here so they behave identically.
    fn toggle_settings(&mut self) {
        if self.ui.find_open() {
            self.close_find();
        }
        self.ui.toggle_settings(&self.config);
        self.request_redraw();
    }

    /// Whether the find bar is actually on screen. Every path that opens the
    /// palette or a side panel closes find first, so an open find session
    /// implies a visible bar; the extra checks are defense in depth because
    /// egui skips drawing the bar while either surface is up.
    fn find_bar_visible(&self) -> bool {
        self.ui.find_open() && !self.palette.open && !self.ui.panel_open()
    }

    fn expire_find_selection_scope(&mut self) {
        if self.find_selection_scope.take().is_some() {
            self.ui.expire_find_selection();
        }
    }

    fn find_selection_scope_tracks_live_selection(&self) -> bool {
        self.find_selection_scope.is_some_and(|scope| {
            self.tabs
                .get(self.active)
                .and_then(|tab| tab.core.selection_range())
                == Some(scope)
        })
    }

    fn sync_find_selection_scope_after_terminal_change(&mut self, tracked_live_selection: bool) {
        if self.find_selection_scope.is_none() {
            return;
        }
        if tracked_live_selection {
            if let Some(range) = self
                .tabs
                .get(self.active)
                .and_then(|tab| tab.core.selection_range())
            {
                self.find_selection_scope = Some(range);
                return;
            }
        }
        // The user replaced the ordinary selection, or history rotation
        // discarded it, so there is no longer a trustworthy coordinate remap.
        self.expire_find_selection_scope();
    }

    fn refresh_find(&mut self) {
        // A submitted refresh supersedes any scheduled debounced one.
        self.find_refresh_deadline = None;
        if !self.ui.find_open() {
            self.find_worker.cancel();
            return;
        }
        let state = self.ui.find_state();
        let query = state.query().to_owned();
        let options = state.options();
        let search_options = SearchOptions {
            case_sensitive: options.case_sensitive,
            whole_word: options.whole_word,
            regex: options.regex,
        };
        let request_key = (query.clone(), search_options, options.selection_only);
        if self.find_request_key.as_ref() != Some(&request_key) {
            // A changed query/options key must not leave a highlight from the
            // old query visible while its replacement runs. PTY-only refreshes
            // retain the existing highlight, preserving #59.
            self.apply_find_result(Ok(SearchOutcome::default()));
        }
        self.find_request_key = Some(request_key);
        let scope = options
            .selection_only
            .then_some(self.find_selection_scope)
            .flatten();
        if query.is_empty() || (options.selection_only && scope.is_none()) {
            self.find_worker.cancel();
            self.find_request_generation = None;
            self.find_pending = false;
            self.apply_find_result(Ok(SearchOutcome::default()));
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            self.find_worker.cancel();
            self.find_request_generation = None;
            self.find_pending = false;
            self.apply_find_result(Ok(SearchOutcome::default()));
            return;
        };
        let tab_id = tab.id;
        let Some(generation) = self
            .find_worker
            .submit(tab_id, query, search_options, scope)
        else {
            self.find_request_generation = None;
            self.find_pending = false;
            self.handle_find_worker_failure();
            return;
        };
        self.find_request_generation = Some(generation);
        self.find_pending = true;
    }

    fn apply_find_result(&mut self, result: Result<SearchOutcome, phantom_emu::SearchError>) {
        let error = match result {
            Ok(outcome) => {
                self.find_matches = outcome.matches;
                self.find_capped = outcome.capped;
                self.find_active = self
                    .find_active
                    .min(self.find_matches.len().saturating_sub(1));
                None
            }
            Err(error) => {
                self.find_matches.clear();
                self.find_active = 0;
                self.find_capped = false;
                Some(FindUiError::InvalidRegex(error.to_string()))
            }
        };
        let active_match = (!self.find_matches.is_empty()).then_some(self.find_active);
        let search_match = active_match.and_then(|index| self.find_matches.get(index).copied());
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.core.set_search_matches(&self.find_matches);
            tab.core.set_active_search_match(search_match);
        }
        self.ui.set_find_results(FindResultSummary {
            match_count: self.find_matches.len(),
            capped: self.find_capped,
            active_match,
            error,
        });
    }

    fn drain_find_results(&mut self) {
        while let Some(response) = self.find_worker.try_recv() {
            self.apply_find_response(response);
        }
    }

    fn apply_find_response(&mut self, response: FindResponse) {
        match response {
            FindResponse::WorkerFailed => self.handle_find_worker_failure(),
            FindResponse::Completed { generation, result } => {
                if !self.ui.find_open() || self.find_request_generation != Some(generation) {
                    return;
                }
                self.find_pending = false;
                self.apply_find_result(result);
                self.request_redraw();
            }
        }
    }

    fn handle_find_worker_failure(&mut self) {
        self.find_worker.mark_unavailable();
        self.find_request_generation = None;
        self.find_pending = false;
        self.find_refresh_deadline = None;
        self.find_matches.clear();
        self.find_active = 0;
        self.find_capped = false;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.core.set_search_matches(&[]);
            tab.core.set_active_search_match(None);
        }
        self.ui.set_find_results(FindResultSummary {
            match_count: 0,
            active_match: None,
            capped: false,
            error: None,
        });
        if self.ui.find_open() && !self.find_worker_failure_noticed {
            self.find_worker_failure_noticed = true;
            self.show_notice("Scrollback find is unavailable for this session");
        }
        self.request_redraw();
    }

    #[cfg(test)]
    fn wait_for_find_for_tests(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.find_pending && Instant::now() < deadline {
            if let Some(response) = self.find_worker.recv_timeout(Duration::from_millis(20)) {
                self.apply_find_response(response);
            }
        }
        assert!(!self.find_pending, "find worker did not complete in time");
    }

    fn navigate_find(&mut self, direction: FindNavigation) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_active = match direction {
            FindNavigation::Previous => self
                .find_active
                .checked_sub(1)
                .unwrap_or(self.find_matches.len() - 1),
            FindNavigation::Next => (self.find_active + 1) % self.find_matches.len(),
        };
        let search_match = self.find_matches.get(self.find_active).copied();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.core.set_active_search_match(search_match);
        }
        self.ui.set_find_results(FindResultSummary {
            match_count: self.find_matches.len(),
            capped: self.find_capped,
            active_match: Some(self.find_active),
            error: None,
        });
        self.request_redraw();
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
                let layout = chrome::compute_layout_scaled(
                    self.content_width(w),
                    h as f32,
                    renderer.cell_size().1,
                    self.horizontal(),
                    self.applied_window_chrome(),
                    gpu.scale_factor(),
                );
                let layout =
                    chrome::reserve_right_sidebar(layout, self.ui.terminal_right_inset_px());
                renderer.grid_size(layout.viewport.w as u32, layout.viewport.h as u32)
            }
            _ => (24, 80),
        }
    }

    /// Grid currently applied to the active terminal core and its PTY.
    fn active_terminal_grid(&self) -> (u16, u16) {
        self.tabs
            .get(self.active)
            .map(|tab| tab.core.size())
            .or(self.last_terminal_grid)
            .unwrap_or_else(|| self.viewport_grid())
    }

    fn request_redraw(&mut self) {
        if self.redraw_queued {
            return;
        }
        if self
            .surface_redraw_after
            .is_some_and(|deadline| Instant::now() < deadline)
        {
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
        self.save_tabs();
    }

    fn mark_config_dirty(&mut self) {
        self.save_config();
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
                for rec in &records {
                    // A record can fail to spawn (stale profile, dead shell
                    // path); only apply its title/active bookkeeping when a
                    // tab was actually pushed, or the previous record's tab
                    // gets renamed and the active index drifts.
                    if !self.spawn_tab_with_persistence(
                        rec.shell_profile_id.clone(),
                        existing_directory(&rec.cwd),
                        false,
                    ) {
                        continue;
                    }
                    let index = self.tabs.len() - 1;
                    if rec.title != basename(&rec.cwd) {
                        if let Some(tab) = self.tabs.last_mut() {
                            tab.custom_title = rec.title.clone();
                        }
                    }
                    if rec.is_active {
                        active = index;
                    }
                }
                if self.tabs.is_empty() {
                    // Every restore failed: fall through to a fresh default tab.
                    self.spawn_tab(None, None);
                    return;
                }
                self.active = active.min(self.tabs.len() - 1);
                self.schedule_context_discovery();
                return;
            }
        }

        self.spawn_tab(None, None);
    }

    fn spawn_tab(&mut self, profile_id: Option<String>, cwd: Option<String>) {
        self.spawn_new_tab(NewTabRequest::shell(profile_id, cwd, true));
    }

    /// The active tab's cwd, for tabs opened via Ctrl+T to inherit. `None` when
    /// the directory no longer exists (falling back to the profile cwd or home)
    /// so the spawn cannot fail on a stale path.
    fn inherited_cwd(&self) -> Option<String> {
        let cwd = &self.tabs.get(self.active)?.cwd;
        existing_directory(cwd)
    }

    /// Spawn a tab; returns whether a tab was actually added.
    fn spawn_tab_with_persistence(
        &mut self,
        profile_id: Option<String>,
        cwd: Option<String>,
        persist: bool,
    ) -> bool {
        self.spawn_new_tab(NewTabRequest::shell(profile_id, cwd, persist))
    }

    fn spawn_new_tab(&mut self, request: NewTabRequest) -> bool {
        if self.tabs.len() >= MAX_TABS {
            self.show_notice(format!("Cannot open more than {MAX_TABS} tabs"));
            return false;
        }
        if self.ui.find_open() {
            self.close_find();
        }
        let (rows, cols) = self.viewport_grid();
        let launch = match resolve_new_tab_launch(
            &self.config,
            request.profile_id.as_deref(),
            request.cwd,
            request.startup,
            rows,
            cols,
        ) {
            Ok(launch) => launch,
            Err(error) => {
                self.show_notice(format!("Could not resolve shell profile: {error}"));
                return false;
            }
        };
        let (rows, cols) = (launch.rows, launch.cols);
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
                return false;
            }
        };

        let core = AlacrittyCore::new(
            rows,
            cols,
            self.config.scrollback_lines,
            cursor_shape(&self.config.cursor_style),
        );
        if !self.find_worker.create(
            tab_id,
            rows,
            cols,
            self.config.scrollback_lines,
            cursor_shape(&self.config.cursor_style),
        ) {
            self.handle_find_worker_failure();
        }
        self.tabs.push(Tab::new(
            tab_id,
            core,
            pty_id,
            start_cwd,
            request.profile_id,
        ));
        if let Some(tab) = self.tabs.last_mut() {
            tab.return_to_shell = request.return_to_shell;
        }
        if let (Some(tab), Some(title)) = (self.tabs.last_mut(), request.title) {
            tab.custom_title = title;
        }
        self.active = self.tabs.len() - 1;
        self.tab_scroll_into_view = Some(self.active);
        self.arm_cwd_polling(CWD_STARTUP_POLL_WINDOW);
        if request.persist {
            self.mark_dirty();
        }
        self.schedule_context_discovery();
        self.request_redraw();
        true
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let closed_active = index == self.active;
        if closed_active && self.ui.find_open() {
            self.close_find();
        }
        let tab = self.tabs.remove(index);
        if !self.find_worker.remove(tab.id) {
            self.handle_find_worker_failure();
        }
        let _ = self.pty.kill(tab.pty_id);
        if self.tabs.is_empty() {
            self.request_exit();
            return;
        }
        // Keep `active` pointing at the same tab: if a tab before it was removed,
        // everything shifted down by one; otherwise just clamp if it was last.
        if index < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        // Tabs can vanish mid-gesture (a shell exiting fires PtyExit): shift or
        // cancel pointer state that captured pre-removal indices, and drop last
        // frame's hit rects so a racing click can't act on a neighboring tab.
        self.tab_drag = match self.tab_drag.take() {
            Some(drag) if drag.from == index => None,
            Some(mut drag) => {
                if index < drag.from {
                    drag.from -= 1;
                }
                if index < drag.target {
                    drag.target -= 1;
                }
                Some(drag)
            }
            None => None,
        };
        if closed_active {
            self.scroll_drag = None;
        }
        self.last_hits = None;
        self.tab_scroll_into_view = Some(self.active);
        self.rename = None;
        self.mark_dirty();
        self.schedule_context_discovery();
        self.request_redraw();
    }

    fn return_tab_to_shell(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        let (tab_id, old_pty, cwd) = (tab.id, tab.pty_id, tab.cwd.clone());
        let (rows, cols) = self.viewport_grid();
        let launch = match resolve_launch_opts(&self.config, None, Some(cwd), rows, cols) {
            Ok(launch) => launch,
            Err(error) => {
                self.show_notice(format!(
                    "Task finished, but the shell could not start: {error}"
                ));
                return false;
            }
        };
        let sink = ProxySink {
            outbox: Arc::clone(&self.outbox),
            tab: tab_id,
        };
        let new_pty = match self.pty.spawn(launch, sink) {
            Ok(id) => id,
            Err(error) => {
                self.show_notice(format!(
                    "Task finished, but the shell could not start: {error}"
                ));
                return false;
            }
        };
        let _ = self.pty.kill(old_pty);
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.pty_id = new_pty;
            tab.profile_id = None;
            tab.return_to_shell = false;
        }
        self.arm_cwd_polling(CWD_STARTUP_POLL_WINDOW);
        self.mark_dirty();
        self.schedule_context_discovery();
        self.request_redraw();
        true
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::NewTab => {
                let cwd = self.inherited_cwd();
                self.spawn_tab(None, cwd);
            }
            Action::CloseTab => self.close_tab(self.active),
            Action::SwitchTab(n) => {
                let i = (n as usize).saturating_sub(1);
                if i < self.tabs.len() {
                    self.switch_tab(i);
                }
            }
            Action::RenameTab => {
                if let Some(tab) = self.tabs.get(self.active) {
                    self.rename = Some(tab.title().to_owned());
                    self.request_redraw();
                }
            }
            Action::TogglePalette => {
                if self.palette.open {
                    self.palette.close();
                } else {
                    // The palette hides the find bar; a find session must not
                    // stay open (and go stale) behind it.
                    if self.ui.find_open() {
                        self.close_find();
                    }
                    self.palette.open(&self.config);
                }
                self.request_redraw();
            }
            Action::ToggleSettings => self.toggle_settings(),
        }
    }

    fn dispatch_palette(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::NewTab => self.spawn_tab(None, None),
            PaletteAction::CloseTab => self.close_tab(self.active),
            PaletteAction::RenameTab => {
                if let Some(tab) = self.tabs.get(self.active) {
                    self.rename = Some(tab.title().to_owned());
                }
            }
            PaletteAction::NewTabWithProfile(id) => self.spawn_tab(Some(id), None),
            PaletteAction::SetUiTheme(name) => {
                // Keep an open settings draft in step so a later settings
                // apply cannot revert the palette-chosen theme.
                self.ui.sync_ui_theme(&name);
                self.config.ui_theme = name;
                self.mark_config_dirty();
            }
            PaletteAction::OpenSettings => {
                // Opening the palette already closed find, but enforce the
                // invariant locally: the panel hides the find bar.
                if self.ui.find_open() {
                    self.close_find();
                }
                self.ui.open_settings(&self.config);
            }
            PaletteAction::FindInScrollback => self.open_find(),
            PaletteAction::ToggleFullscreen => self.toggle_fullscreen(),
            PaletteAction::FocusContextSidebar => {
                if self.ui.focus_context_sidebar(&mut self.config) {
                    self.mark_config_dirty();
                }
            }
        }
        self.request_redraw();
    }

    fn schedule_context_discovery(&mut self) {
        self.directory_visit_pending = true;
        self.context_generation = self.context_generation.wrapping_add(1);
        let generation = self.context_generation;
        let cwd = self
            .tabs
            .get(self.active)
            .map(|tab| PathBuf::from(&tab.cwd))
            .unwrap_or_default();
        if cwd.as_os_str().is_empty() || !self.config.context_actions.enabled {
            self.context_snapshot = ContextSnapshot::empty(cwd);
            self.request_redraw();
            return;
        }
        // Another directory's actions must not stay rendered while the worker
        // rediscovers (they can be slow on network filesystems); show the
        // empty state until the reply for this generation lands.
        if self.context_snapshot.cwd != cwd {
            self.context_snapshot = ContextSnapshot::empty(cwd.clone());
            self.request_redraw();
        }
        let request = ContextDiscoveryRequest {
            generation,
            cwd: cwd.clone(),
            config: self.config.context_actions.clone(),
            trusted_projects: self.config.trusted_projects.clone(),
            trusted_spdeploy_projects: self.config.trusted_spdeploy_projects.clone(),
        };
        if self.context_discovery_tx.send(request).is_err() {
            self.context_snapshot = ContextSnapshot::empty(cwd);
            self.show_notice("Context discovery is unavailable");
        }
    }

    fn dispatch_context_request(&mut self, request: ContextRequest) {
        let provider = match &request {
            ContextRequest::TrustManifest { .. }
            | ContextRequest::EditManifest { .. }
            | ContextRequest::OpenManifestAll { .. }
            | ContextRequest::OpenManifestTab { .. } => context_actions::MANIFEST_PROVIDER_ID,
            ContextRequest::RunSpdeploy { .. } | ContextRequest::TrustSpdeploy { .. } => {
                context_actions::SPDEPLOY_PROVIDER_ID
            }
            ContextRequest::OpenDirectory { .. } => phantom_core::RECENT_DIRECTORIES_PLUGIN_ID,
        };
        if !self.config.context_actions.enabled
            || !self
                .config
                .context_actions
                .plugin(provider)
                .is_some_and(|plugin| plugin.enabled)
        {
            self.show_notice("That context plugin is disabled");
            return;
        }
        match request {
            ContextRequest::TrustManifest {
                root,
                manifest_source,
            } => self.trust_manifest(root, manifest_source),
            ContextRequest::EditManifest {
                root,
                manifest_source,
            } => self.edit_manifest(root, manifest_source),
            ContextRequest::OpenManifestAll {
                root,
                manifest_source,
            } => self.open_manifest_tasks(root, manifest_source, None),
            ContextRequest::OpenManifestTab {
                root,
                manifest_source,
                tab_id,
            } => self.open_manifest_tasks(root, manifest_source, Some(tab_id)),
            ContextRequest::RunSpdeploy {
                config_path,
                operation,
            } => self.run_spdeploy_context_action(config_path, operation),
            ContextRequest::TrustSpdeploy { root, sources } => self.trust_spdeploy(root, sources),
            ContextRequest::OpenDirectory { path } => self.open_context_directory(path),
        }
    }

    fn open_context_directory(&mut self, path: PathBuf) {
        let canonical = match path.canonicalize() {
            Ok(path) if path.is_dir() => path,
            Ok(_) => {
                self.show_notice("That recent directory is no longer a directory");
                return;
            }
            Err(error) => {
                self.show_notice(format!("Could not open recent directory: {error}"));
                return;
            }
        };
        let Some(canonical_text) = canonical.to_str() else {
            self.show_notice("That directory path is not valid UTF-8");
            return;
        };
        if !self
            .config
            .context_actions
            .directory_history
            .iter()
            .any(|entry| entry.path == canonical_text)
        {
            self.show_notice("That directory is no longer in recent history");
            self.schedule_context_discovery();
            return;
        }

        self.spawn_tab(None, Some(canonical_text.to_string()));
    }

    fn flush_directory_visit(&mut self) {
        if !self.directory_visit_pending {
            return;
        }
        self.directory_visit_pending = false;
        if !self.config.context_actions.enabled
            || !self
                .config
                .context_actions
                .plugin(phantom_core::RECENT_DIRECTORIES_PLUGIN_ID)
                .is_some_and(|plugin| plugin.enabled)
        {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Ok(canonical) = Path::new(&tab.cwd).canonicalize() else {
            return;
        };
        if !canonical.is_dir() {
            return;
        }
        let marker = (tab.id, canonical.clone());
        if self.last_observed_directory.as_ref() == Some(&marker) {
            return;
        }
        self.last_observed_directory = Some(marker);
        let visited_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Err(error) = self
            .config
            .context_actions
            .record_directory_visit(&canonical, visited_at)
        {
            self.show_notice(format!("Could not remember directory: {error}"));
            return;
        }
        self.mark_config_dirty();
        self.schedule_context_discovery();
    }

    fn revalidate_manifest_request(
        &self,
        root: &Path,
        manifest_source: &str,
    ) -> phantom_core::AppResult<phantom_core::LoadedContextManifest> {
        let loaded = self.revalidate_manifest_source(root, manifest_source)?;
        let manifest = phantom_core::parse_context_manifest(&loaded.source)?;
        Ok(phantom_core::LoadedContextManifest {
            root: loaded.root,
            source: loaded.source,
            manifest,
        })
    }

    fn revalidate_manifest_source(
        &self,
        root: &Path,
        manifest_source: &str,
    ) -> phantom_core::AppResult<phantom_core::ContextManifestSource> {
        let active_cwd = self
            .tabs
            .get(self.active)
            .map(|tab| Path::new(&tab.cwd))
            .ok_or_else(|| phantom_core::AppError::InvalidConfig("no active tab".to_string()))?;
        let active_cwd = active_cwd.canonicalize().map_err(|error| {
            phantom_core::AppError::InvalidConfig(format!(
                "active working directory is unavailable: {error}"
            ))
        })?;
        let canonical_root = root.canonicalize().map_err(|error| {
            phantom_core::AppError::InvalidConfig(format!(
                "project directory is unavailable: {error}"
            ))
        })?;
        if active_cwd != canonical_root {
            return Err(phantom_core::AppError::InvalidConfig(
                "the active tab has left this project directory".to_string(),
            ));
        }
        let loaded =
            phantom_core::load_context_manifest_source(&canonical_root)?.ok_or_else(|| {
                phantom_core::AppError::InvalidConfig(".phantom.yml no longer exists".to_string())
            })?;
        if loaded.source != manifest_source {
            return Err(phantom_core::AppError::InvalidConfig(
                ".phantom.yml changed; review and trust it again".to_string(),
            ));
        }
        Ok(loaded)
    }

    fn trust_manifest(&mut self, root: PathBuf, manifest_source: String) {
        let result = self
            .revalidate_manifest_request(&root, &manifest_source)
            .and_then(|loaded| trust_context_manifest(&loaded.root, loaded.source));
        let trusted = match result {
            Ok(trusted) => trusted,
            Err(error) => {
                self.show_notice(format!("Could not trust project tasks: {error}"));
                self.schedule_context_discovery();
                return;
            }
        };

        let mut candidate = self.config.clone();
        if let Some(existing) = candidate
            .trusted_projects
            .iter_mut()
            .find(|project| project.root == trusted.root)
        {
            *existing = trusted;
        } else {
            candidate.trusted_projects.push(trusted);
        }
        if let Err(error) = candidate.validate() {
            self.show_notice(format!("Could not store project trust: {error}"));
            return;
        }
        self.config = candidate;
        self.mark_config_dirty();
        self.schedule_context_discovery();
    }

    fn edit_manifest(&mut self, root: PathBuf, manifest_source: String) {
        let loaded = match self.revalidate_manifest_source(&root, &manifest_source) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.show_notice(format!("Could not edit project tasks: {error}"));
                self.schedule_context_discovery();
                return;
            }
        };
        let path = loaded.root.join(phantom_core::CONTEXT_MANIFEST_FILE);
        external_editor::open(path, Arc::clone(&self.outbox));
    }

    fn open_manifest_tasks(
        &mut self,
        root: PathBuf,
        manifest_source: String,
        only_task: Option<String>,
    ) {
        let loaded = match self.revalidate_manifest_request(&root, &manifest_source) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.show_notice(format!("Could not open project tabs: {error}"));
                self.schedule_context_discovery();
                return;
            }
        };
        let Some(project) = self
            .config
            .trusted_projects
            .iter()
            .find(|project| {
                Path::new(&project.root) == loaded.root && project.manifest_source == loaded.source
            })
            .cloned()
        else {
            self.show_notice("Project tasks are not trusted");
            self.schedule_context_discovery();
            return;
        };

        let task_ids: Vec<String> = match only_task {
            Some(id) => vec![id],
            None => project.tasks.iter().map(|task| task.id.clone()).collect(),
        };
        let (rows, cols) = self.viewport_grid();
        let mut launches = Vec::with_capacity(task_ids.len());
        for id in &task_ids {
            let launch = match resolve_context_tab_launch(
                &self.config,
                &project,
                &loaded.root,
                &loaded.source,
                id,
                rows,
                cols,
            ) {
                Ok(launch) => launch,
                Err(error) => {
                    self.show_notice(format!("Could not open project tabs: {error}"));
                    return;
                }
            };
            let title = project
                .task(id)
                .map(|task| task.title.clone())
                .unwrap_or_else(|| id.clone());
            launches.push((launch, title));
        }

        let original_len = self.tabs.len();
        let original_active = self.active;
        for (launch, title) in launches {
            let return_to_shell = launch.startup.is_some();
            if !self.spawn_new_tab(NewTabRequest {
                profile_id: None,
                cwd: launch.cwd,
                startup: launch.startup,
                title: Some(title),
                return_to_shell,
                persist: false,
            }) {
                while self.tabs.len() > original_len {
                    if let Some(tab) = self.tabs.pop() {
                        let _ = self.pty.kill(tab.pty_id);
                    }
                }
                self.active = original_active.min(self.tabs.len().saturating_sub(1));
                self.mark_dirty();
                self.schedule_context_discovery();
                self.show_notice("Could not open every project tab; no project tabs were kept");
                return;
            }
        }
        self.mark_dirty();
        self.schedule_context_discovery();
    }

    fn run_spdeploy_context_action(&mut self, config_path: PathBuf, operation: String) {
        self.run_spdeploy_context_action_with_resolver(config_path, operation, resolve_program);
    }

    fn trust_spdeploy(&mut self, root: PathBuf, sources: Vec<phantom_core::TrustedSpdeploySource>) {
        let active_cwd = self
            .tabs
            .get(self.active)
            .and_then(|tab| Path::new(&tab.cwd).canonicalize().ok());
        let canonical_root = root.canonicalize().ok();
        if active_cwd.is_none() || active_cwd != canonical_root {
            self.show_notice("The active tab has left this spdeploy project");
            self.schedule_context_discovery();
            return;
        }
        let graph = match phantom_core::load_spdeploy_graph(&root) {
            Ok(Some(graph)) if graph.sources == sources => graph,
            Ok(_) => {
                self.show_notice("spdeploy configuration changed; review it again");
                self.schedule_context_discovery();
                return;
            }
            Err(error) => {
                self.show_notice(format!("Could not trust spdeploy operations: {error}"));
                self.schedule_context_discovery();
                return;
            }
        };
        let trusted = match phantom_core::trust_spdeploy_graph(&graph) {
            Ok(trusted) => trusted,
            Err(error) => {
                self.show_notice(format!("Could not trust spdeploy operations: {error}"));
                return;
            }
        };
        let mut candidate = self.config.clone();
        if let Some(existing) = candidate
            .trusted_spdeploy_projects
            .iter_mut()
            .find(|project| project.root == trusted.root)
        {
            *existing = trusted;
        } else {
            candidate.trusted_spdeploy_projects.push(trusted);
        }
        if let Err(error) = candidate.validate() {
            self.show_notice(format!("Could not store spdeploy trust: {error}"));
            return;
        }
        self.config = candidate;
        self.mark_config_dirty();
        self.schedule_context_discovery();
    }

    fn run_spdeploy_context_action_with_resolver(
        &mut self,
        config_path: PathBuf,
        operation: String,
        resolver: impl FnOnce(&str) -> phantom_core::AppResult<String>,
    ) {
        let active_cwd = match self
            .tabs
            .get(self.active)
            .and_then(|tab| Path::new(&tab.cwd).canonicalize().ok())
        {
            Some(cwd) => cwd,
            None => {
                self.show_notice("Could not resolve the active working directory");
                return;
            }
        };
        let listed_section =
            self.context_snapshot
                .sections
                .iter()
                .find_map(|section| match &section.content {
                    context_actions::ContextSectionContent::Spdeploy(spdeploy)
                        if spdeploy.trust == context_actions::SpdeployTrustState::Trusted
                            && spdeploy.operations.iter().any(|item| {
                                item.config_path == config_path && item.name == operation
                            }) =>
                    {
                        Some(spdeploy)
                    }
                    _ => None,
                });
        let Some(listed_section) = listed_section else {
            self.show_notice("The selected spdeploy action is stale or invalid");
            self.schedule_context_discovery();
            return;
        };
        let stored_trust = self.config.trusted_spdeploy_projects.iter().any(|project| {
            Path::new(&project.root) == listed_section.root
                && project.sources == listed_section.config_sources
        });
        if !stored_trust {
            self.show_notice("spdeploy operations are not trusted");
            self.schedule_context_discovery();
            return;
        }
        let valid = self.context_snapshot.cwd == active_cwd
            && config_path.starts_with(&active_cwd)
            && !operation.is_empty()
            && operation.len() <= 256
            && !operation.chars().any(char::is_control);
        if !valid {
            self.show_notice("The selected spdeploy action is stale or invalid");
            self.schedule_context_discovery();
            return;
        }
        let startup = match spdeploy_startup_command(&config_path, &operation, resolver) {
            Ok(startup) => startup,
            Err(error) => {
                self.show_notice(format!("Could not resolve spdeploy: {error}"));
                return;
            }
        };
        let current_active_cwd = self
            .tabs
            .get(self.active)
            .and_then(|tab| Path::new(&tab.cwd).canonicalize().ok());
        let still_trusted = self.config.trusted_spdeploy_projects.iter().any(|project| {
            Path::new(&project.root) == listed_section.root
                && project.sources == listed_section.config_sources
        });
        let still_listed = listed_section
            .operations
            .iter()
            .any(|item| item.config_path == config_path && item.name == operation);
        if current_active_cwd.as_ref() != Some(&listed_section.root)
            || !still_trusted
            || !still_listed
        {
            self.show_notice("The selected spdeploy action is stale or invalid");
            self.schedule_context_discovery();
            return;
        }
        if let Err(error) = context_actions::verify_spdeploy_sources(listed_section) {
            self.show_notice(format!("spdeploy configuration changed: {error}"));
            self.schedule_context_discovery();
            return;
        }
        // spdeploy currently accepts only a pathname and resolves every stage
        // path relative to that config's real parent. Moving the validated
        // bytes to a private snapshot would change those semantics. The final
        // descriptor-safe verification follows executable resolution and
        // rejects symlinks, growth, and stale bytes, but any actor allowed to
        // mutate the file or one of its parent directories can still replace
        // it between this check and spdeploy reopening it. Closing that final
        // gap requires an inherited-FD/config-bundle interface in spdeploy.
        self.spawn_new_tab(NewTabRequest {
            profile_id: None,
            cwd: config_path
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned()),
            startup: Some(startup),
            title: Some(format!("spdeploy: {operation}")),
            return_to_shell: true,
            persist: true,
        });
    }

    fn save_config(&mut self) {
        let errors = self.persistence.save_config(self.config.clone());
        self.report_persistence_errors(errors, "Could not save settings");
    }

    fn flush_persistence(&mut self) {
        let errors = self.persistence.flush();
        self.report_persistence_errors(errors, "Could not save app state");
    }

    fn drain_persistence_errors(&mut self) {
        let errors = self.persistence.drain_errors();
        self.report_persistence_errors(errors, "Could not save app state");
    }

    fn report_persistence_errors(&mut self, errors: Vec<String>, notice: &str) {
        if errors.is_empty() {
            return;
        }
        let joined = errors.join("; ");
        eprintln!("persistence failed: {joined}");
        self.show_notice(format!("{notice}: {joined}"));
    }

    /// Persist the (already-validated) config and rebuild the renderer so font,
    /// theme, and layout changes take effect.
    fn apply_config_change(&mut self) {
        self.apply_window_chrome();
        self.keymap = Keymap::from_config(&self.config.keybindings);
        self.mark_config_dirty();
        if self.blink_enabled != self.config.cursor_blink {
            self.blink_enabled = self.config.cursor_blink;
            // Toggling blink during the off phase must not strand the cursor
            // invisible: pump_blink stops toggling once blink is disabled.
            self.reset_blink();
        }
        // Cursor style and scrollback depth are baked into each core at spawn;
        // push changes to live tabs so they aren't limited to future tabs.
        let term_options = (
            self.config.scrollback_lines,
            cursor_shape(&self.config.cursor_style),
        );
        if self.applied_term_options != term_options {
            let tracks_live_selection = self.find_selection_scope_tracks_live_selection();
            self.applied_term_options = term_options;
            if !self.find_worker.set_options(term_options.0, term_options.1) {
                self.handle_find_worker_failure();
            }
            self.find_request_generation = None;
            self.find_pending = false;
            for tab in &mut self.tabs {
                tab.core
                    .set_terminal_options(term_options.0, term_options.1);
            }
            if self.ui.find_open() {
                // A history resize can rotate every absolute buffer
                // coordinate. Never leave the previous result set navigable
                // while its replacement runs, and keep selection-only search
                // scoped only when Alacritty could remap the captured range.
                self.sync_find_selection_scope_after_terminal_change(tracks_live_selection);
                self.apply_find_result(Ok(SearchOutcome::default()));
                self.refresh_find();
            }
        }
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
        #[cfg(target_os = "macos")]
        if let Some(gpu) = self.gpu.as_ref() {
            if !apply_macos_window_chrome(&gpu.window, requested) {
                self.show_notice("Window chrome change saved. Restart Phantom to apply it.");
                return;
            }
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.set_decorations(!requested.is_custom());
            gpu.window.set_transparent(requested.is_custom());
        }
        self.applied_window_chrome = self.config.window_chrome.clone();
    }

    /// Recreate the renderer (font/theme/scale) and resize every tab.
    fn rebuild_renderer(&mut self) {
        if let Some(gpu) = self.gpu.as_ref() {
            match Renderer::new(
                &gpu.device,
                &gpu.queue,
                gpu.format(),
                &self.config,
                gpu.scale_factor(),
            ) {
                Ok(renderer) => {
                    let (w, h) = gpu.size();
                    renderer.resize(&gpu.queue, w, h);
                    self.renderer = Some(renderer);
                }
                // Keep rendering with the previous renderer rather than
                // crashing mid-session (e.g. a corrupt font encountered on a
                // monitor-scale change).
                Err(error) => {
                    self.show_notice(format!("Could not load font: {error}"));
                }
            }
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
        let at_shell_prompt = self.active_tab_at_shell_prompt();
        let Some(pty_id) = self.tabs.get(self.active).map(|tab| tab.pty_id) else {
            self.preedit.clear();
            self.request_redraw();
            return;
        };
        if !self.write_terminal_input(pty_id, text.as_bytes(), "Could not send text") {
            self.preedit.clear();
            self.request_redraw();
            return;
        }
        self.snap_active_terminal_to_prompt();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.frequent_commands.prepare_line(at_shell_prompt);
            tab.frequent_commands.observe_text(text);
        }
        if text.contains('\n') || text.contains('\r') {
            self.arm_cwd_polling(CWD_POLL_WINDOW);
        }
        self.preedit.clear();
        self.request_redraw();
    }

    /// Route a key press (after the winit adapter has normalized it).
    fn handle_key_input(&mut self, key: Key, text: Option<&str>) {
        if BuiltinShortcut::FindInScrollback.matches(key, self.mods) {
            self.open_find();
            return;
        }
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
        if BuiltinShortcut::Copy.matches(key, self.mods) {
            self.copy_selection();
            return;
        }
        if BuiltinShortcut::Paste.matches(key, self.mods) {
            self.paste_clipboard();
            return;
        }
        // Fullscreen toggle.
        if BuiltinShortcut::ToggleFullscreen.matches(key, self.mods) {
            self.toggle_fullscreen();
            return;
        }
        // Shift+PageUp/Down scroll the viewport by a page.
        let page_direction = if BuiltinShortcut::ScrollPageUp.matches(key, self.mods) {
            Some(1)
        } else if BuiltinShortcut::ScrollPageDown.matches(key, self.mods) {
            Some(-1)
        } else {
            None
        };
        if let Some(direction) = page_direction {
            let page = (self.active_terminal_grid().0 as i32).max(1);
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.core.scroll(direction * page);
            }
            self.request_redraw();
            return;
        }
        // Keybindings.
        if let Some(action) = self.keymap.lookup(key, self.mods) {
            self.handle_action(action);
            return;
        }
        // Super is an application shortcut modifier, not a terminal modifier.
        // Once the app-level routes above decline a chord, consume it instead
        // of dropping Super during VT encoding and leaking the bare key.
        if self.mods.sup {
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
            let at_shell_prompt = self.active_tab_at_shell_prompt();
            let Some(pty_id) = self.tabs.get(self.active).map(|tab| tab.pty_id) else {
                return;
            };
            if !self.write_terminal_input(pty_id, &bytes, "Could not send key") {
                return;
            }
            self.snap_active_terminal_to_prompt();
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.frequent_commands.observe_key(key, self.mods.ctrl);
                if matches!(key, Key::Char(_))
                    && !self.mods.ctrl
                    && !self.mods.alt
                    && !self.mods.sup
                {
                    if let Some(text) = text {
                        t.frequent_commands.prepare_line(at_shell_prompt);
                        t.frequent_commands.observe_text(text);
                    }
                }
            }
            if key == Key::Enter || bytes.contains(&b'\n') || bytes.contains(&b'\r') {
                self.arm_cwd_polling(CWD_POLL_WINDOW);
            }
            self.request_redraw();
        }
    }

    fn overlay_owns_terminal_ime(&self) -> bool {
        overlay_owns_terminal_ime(
            self.palette.open,
            self.rename.is_some(),
            self.ui.settings_open(),
            self.ui.context_owns_keyboard(),
            self.egui
                .as_ref()
                .is_some_and(EguiLayer::wants_keyboard_input),
        )
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
            } else if let Some(index) = hits.close_at(px, py) {
                hit = Some(Hit::Close(index));
            } else {
                for th in &hits.tabs {
                    if th.rect.contains(px, py) {
                        hit = Some(Hit::Switch(th.index));
                        break;
                    }
                }
            }
        }
        match hit {
            Some(Hit::New) => self.spawn_tab(None, None),
            Some(Hit::Settings) => self.toggle_settings(),
            Some(Hit::WindowControl(control)) => self.handle_window_control(control),
            Some(Hit::Close(i)) => self.close_tab(i),
            Some(Hit::Switch(i)) => self.switch_tab(i),
            None => {}
        }
    }

    fn handle_window_control(&mut self, control: chrome::WindowControl) {
        if control == chrome::WindowControl::Close {
            self.request_exit();
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

    /// Cleanup shared by every path that changes which tab is active: find
    /// matches and an in-progress scrollbar drag belong to the outgoing tab.
    /// Must run while `self.active` still points at the outgoing tab.
    fn leave_active_tab(&mut self) {
        if self.ui.find_open() {
            self.close_find();
        }
        self.scroll_drag = None;
    }

    fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.active {
            self.leave_active_tab();
        }
        self.active = index;
        self.tab_scroll_into_view = Some(index);
        self.rename = None;
        self.mark_dirty();
        self.schedule_context_discovery();
        self.request_redraw();
    }

    fn reorder_tab(&mut self, from: usize, target: usize) {
        if from >= self.tabs.len() {
            return;
        }
        // Dropping a dragged tab activates it; run the same cleanup as
        // switch_tab so find state computed against the previously active
        // tab's scrollback doesn't survive the activation change.
        if from != self.active {
            self.leave_active_tab();
        }
        let target = target.min(self.tabs.len());
        let insert_at = if target > from { target - 1 } else { target };
        if insert_at == from {
            // Dropped in place: no reorder, but the drop still activates the
            // dragged tab, so keep sidebar/persistence in step with it.
            if self.active != from {
                self.active = from;
                self.mark_dirty();
                self.schedule_context_discovery();
            }
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        self.tab_scroll_into_view = Some(insert_at);
        self.rename = None;
        // Tab rects moved: drop last frame's hit map (as close_tab does) so a
        // racing click can't act on stale positions.
        self.last_hits = None;
        self.mark_dirty();
        self.schedule_context_discovery();
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
        let layout = chrome::compute_layout_scaled(
            self.content_width(w),
            h as f32,
            renderer.cell_size().1,
            self.horizontal(),
            self.applied_window_chrome(),
            gpu.scale_factor(),
        );
        Some(chrome::reserve_right_sidebar(
            layout,
            self.ui.terminal_right_inset_px(),
        ))
    }

    fn point_in_viewport(&self, px: f32, py: f32) -> bool {
        self.layout().is_some_and(|l| l.viewport.contains(px, py))
    }

    /// Map a window pixel to a viewport `(row, col, side)`, if inside the grid.
    fn viewport_cell(&self, px: f32, py: f32) -> Option<(usize, usize, SelSide)> {
        let layout = self.layout()?;
        let cell_size = self.renderer.as_ref()?.cell_size();
        viewport_cell_for_grid(
            layout.viewport,
            cell_size,
            self.active_terminal_grid(),
            px,
            py,
        )
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

    fn on_mouse_down(&mut self, button: AppMouseButton) {
        let (px, py) = self.cursor_pos;
        #[cfg(target_os = "linux")]
        if button == AppMouseButton::Left {
            if let Some(direction) = self.linux_window_resize_direction() {
                if let Some(gpu) = self.gpu.as_ref() {
                    let _ = gpu.window.drag_resize_window(direction);
                }
                return;
            }
        }
        if self.palette.open {
            return;
        }
        if button == AppMouseButton::Middle {
            self.on_middle_mouse_down();
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
                drag_window_from_custom_titlebar(&gpu.window);
            }
            return;
        }
        if !self.point_in_viewport(px, py) {
            self.last_click = None;
            self.handle_click();
            return;
        }
        if self.active_mouse_mode().reports() {
            self.left_down =
                self.report_mouse(px, py, MouseEvent::Press(TerminalMouseButton::Left));
        } else if let Some((row, col, side)) = self.viewport_cell(px, py) {
            self.left_down = true;
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

    fn on_mouse_up(&mut self, button: AppMouseButton) {
        let (px, py) = self.cursor_pos;
        if button == AppMouseButton::Middle {
            self.on_middle_mouse_up();
            return;
        }
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
        let terminal_press_forwarded = std::mem::take(&mut self.left_down);
        match mouse_up_action(
            terminal_press_forwarded,
            self.selecting,
            self.active_mouse_mode().reports(),
        ) {
            MouseUpAction::Ignore => {}
            MouseUpAction::EndSelection => {
                self.selecting = false;
                self.copy_selection_to_primary();
            }
            MouseUpAction::Report => {
                self.report_mouse(px, py, MouseEvent::Release(TerminalMouseButton::Left));
            }
        }
    }

    fn on_middle_mouse_down(&mut self) {
        #[cfg(target_os = "linux")]
        {
            self.middle_report_down = false;
            let (px, py) = self.cursor_pos;
            match middle_click_route(
                self.point_in_viewport(px, py),
                self.active_mouse_mode().reports(),
                self.mods.shift,
            ) {
                MiddleClickRoute::Ignore => {}
                MiddleClickRoute::MouseReport => {
                    self.middle_report_down =
                        self.report_mouse(px, py, MouseEvent::Press(TerminalMouseButton::Middle));
                }
                MiddleClickRoute::PrimaryPaste => self.paste_primary_selection(),
            }
        }
    }

    fn on_middle_mouse_up(&mut self) {
        #[cfg(target_os = "linux")]
        if should_report_middle_mouse_release(
            std::mem::take(&mut self.middle_report_down),
            self.active_mouse_mode().reports(),
        ) {
            let (px, py) = self.cursor_pos;
            self.report_mouse(px, py, MouseEvent::Release(TerminalMouseButton::Middle));
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
        let scale_factor = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.scale_factor())
            .unwrap_or(1.0);
        let Some(thumb) = chrome::scrollbar_thumb(track, scroll, scale_factor) else {
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
        let scale_factor = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.scale_factor())
            .unwrap_or(1.0);
        if !drag.active && !tab_drag_moved_past_tolerance(drag.start, (px, py), scale_factor) {
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
        #[cfg(target_os = "linux")]
        if let Some(direction) = self.linux_window_resize_direction() {
            self.set_cursor_icon(direction.into());
            if self.chrome_anim.set_hover(chrome::ChromeHoverTarget::None) {
                self.request_redraw();
            }
            return;
        }
        let scrollbar_hover = self.scroll_drag.is_some()
            || self
                .active_scrollbar_track()
                .is_some_and(|track| track.contains(px, py));
        // The widened/highlighted scrollbar is derived from the live cursor in
        // render(), so entering/leaving the track needs its own repaint.
        if scrollbar_hover != self.scrollbar_hovered {
            self.scrollbar_hovered = scrollbar_hover;
            self.request_redraw();
        }
        if scrollbar_hover {
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
        self.set_pointer_cursor(self.scroll_drag.is_some() || self.tab_drag.is_some());
        if self.chrome_anim.set_hover(chrome::ChromeHoverTarget::None) {
            self.request_redraw();
        }
    }

    fn set_pointer_cursor(&mut self, pointer: bool) {
        self.set_cursor_icon(if pointer {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    fn set_cursor_icon(&mut self, icon: CursorIcon) {
        if self.cursor_icon != icon {
            self.cursor_icon = icon;
            if let Some(gpu) = self.gpu.as_ref() {
                gpu.window.set_cursor(icon);
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_window_resize_direction(&self) -> Option<ResizeDirection> {
        let gpu = self.gpu.as_ref()?;
        if self.fullscreen || gpu.window.is_maximized() {
            return None;
        }
        let (width, height) = gpu.size();
        let (px, py) = self.cursor_pos;
        let handle = chrome::window_resize_handle_at(
            width as f32,
            height as f32,
            gpu.scale_factor(),
            self.applied_window_chrome(),
            self.active_scrollbar_track(),
            px,
            py,
        )?;
        Some(match handle {
            chrome::WindowResizeHandle::East => ResizeDirection::East,
            chrome::WindowResizeHandle::North => ResizeDirection::North,
            chrome::WindowResizeHandle::NorthEast => ResizeDirection::NorthEast,
            chrome::WindowResizeHandle::NorthWest => ResizeDirection::NorthWest,
            chrome::WindowResizeHandle::South => ResizeDirection::South,
            chrome::WindowResizeHandle::SouthEast => ResizeDirection::SouthEast,
            chrome::WindowResizeHandle::SouthWest => ResizeDirection::SouthWest,
            chrome::WindowResizeHandle::West => ResizeDirection::West,
        })
    }

    fn input_starts_window_resize(&self, input: &AppInput) -> bool {
        #[cfg(target_os = "linux")]
        {
            matches!(
                input,
                AppInput::MouseDown {
                    button: AppMouseButton::Left,
                    ..
                }
            ) && self.linux_window_resize_direction().is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = input;
            false
        }
    }

    fn on_scroll(&mut self, lines: f32) {
        let (px, py) = self.cursor_pos;
        // Wheel over an overflowing tab strip scrolls the strip, not the
        // terminal. Wheel-up moves toward the first tab.
        let tab_scroll_limit = self.last_hits.as_ref().and_then(|hits| {
            (hits.max_tab_scroll > 0.0 && hits.tab_strip.contains(px, py))
                .then_some(hits.max_tab_scroll)
        });
        if let Some(max_tab_scroll) = tab_scroll_limit {
            self.select_wheel_route(WheelRoute::TabStrip);
            let scale = self
                .gpu
                .as_ref()
                .map(|gpu| gpu.scale_factor())
                .unwrap_or(1.0);
            let step = lines * TAB_STRIP_WHEEL_PX * scale;
            self.tab_scroll = (self.tab_scroll - step).clamp(0.0, max_tab_scroll);
            self.request_redraw();
            return;
        }
        if self.active_mouse_mode().reports() {
            let ticks = self.accumulate_wheel_delta(WheelRoute::MouseReport, lines);
            if ticks == 0 {
                return;
            }
            let event = if ticks > 0 {
                MouseEvent::WheelUp
            } else {
                MouseEvent::WheelDown
            };
            for _ in 0..ticks.unsigned_abs() {
                self.report_mouse(px, py, event);
            }
        } else {
            let whole_lines =
                self.accumulate_wheel_delta(WheelRoute::Terminal, lines * WHEEL_LINES_PER_NOTCH);
            if whole_lines == 0 {
                return;
            }
            if let Some(tab) = self.tabs.get_mut(self.active) {
                // Positive lines (wheel up) scroll back into history.
                tab.core.scroll(whole_lines);
            }
            self.request_redraw();
        }
    }

    fn select_wheel_route(&mut self, route: WheelRoute) {
        if self.wheel_route != Some(route) {
            self.wheel_line_remainder = 0.0;
            self.wheel_route = Some(route);
        }
    }

    fn accumulate_wheel_delta(&mut self, route: WheelRoute, delta: f32) -> i32 {
        if delta.abs() < f32::EPSILON {
            return 0;
        }
        self.select_wheel_route(route);
        if self.wheel_line_remainder != 0.0
            && self.wheel_line_remainder.is_sign_positive() != delta.is_sign_positive()
        {
            self.wheel_line_remainder = 0.0;
        }
        self.wheel_line_remainder += delta;
        let whole = self.wheel_line_remainder.trunc() as i32;
        self.wheel_line_remainder -= whole as f32;
        whole
    }

    /// Encode and send a mouse event to a mouse-aware application.
    fn report_mouse(&mut self, px: f32, py: f32, event: MouseEvent) -> bool {
        let Some((row, col, _)) = self.viewport_cell(px, py) else {
            return false;
        };
        let (mode, pty_id) = match self.tabs.get(self.active) {
            Some(t) => (t.core.mouse_mode(), t.pty_id),
            None => return false,
        };
        if !mode.reports() {
            return false;
        }
        let (base, pressed) = match event {
            MouseEvent::Press(button) => (button.code(), true),
            MouseEvent::Release(button) => (button.code(), false),
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
        self.write_terminal_input(pty_id, &bytes, "Could not send mouse input")
    }

    fn copy_selection(&mut self) {
        let Some(text) = self
            .tabs
            .get(self.active)
            .and_then(|t| t.core.selection_text())
        else {
            return;
        };
        let Some(clipboard) = self.clipboard.as_mut() else {
            self.show_notice("Clipboard is unavailable");
            return;
        };
        if let Err(error) = clipboard.set_text(text) {
            self.show_notice(format!("Could not copy: {error}"));
        }
    }

    fn copy_selection_to_primary(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let Some(text) = self
                .tabs
                .get(self.active)
                .and_then(|tab| tab.core.selection_text())
            else {
                return;
            };
            let Some(clipboard) = self.clipboard.as_mut() else {
                return;
            };
            if let Err(error) = clipboard.set_primary_text(text) {
                self.show_notice(format!("Could not copy primary selection: {error}"));
            }
        }
    }

    fn paste_clipboard(&mut self) {
        let Some(clipboard) = self.clipboard.as_mut() else {
            self.show_notice("Clipboard is unavailable");
            return;
        };
        let text = match clipboard.get_text() {
            Ok(text) if !text.is_empty() => text,
            Ok(_) => {
                self.show_notice("Clipboard is empty");
                return;
            }
            Err(error) => {
                self.show_notice(format!("Could not paste: {error}"));
                return;
            }
        };
        self.paste_text(text);
    }

    #[cfg(target_os = "linux")]
    fn paste_primary_selection(&mut self) {
        let Some(clipboard) = self.clipboard.as_mut() else {
            self.show_notice("Primary selection is unavailable");
            return;
        };
        let text = match clipboard.get_primary_text() {
            Ok(text) if !text.is_empty() => text,
            Ok(_) => {
                self.show_notice("Primary selection is empty");
                return;
            }
            Err(error) => {
                self.show_notice(format!("Could not paste primary selection: {error}"));
                return;
            }
        };
        self.paste_text(text);
    }

    fn paste_text(&mut self, text: String) {
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
        // The whole paste (markers included) is queued atomically; on failure
        // nothing was delivered, so tell the user instead of dropping it.
        if !self.write_terminal_input(pty_id, &bytes, "Could not paste") {
            return;
        }
        self.snap_active_terminal_to_prompt();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.frequent_commands.observe_paste();
        }
        if text.contains('\n') || text.contains('\r') {
            self.arm_cwd_polling(CWD_POLL_WINDOW);
        }
        self.request_redraw();
    }

    /// Move successful terminal input back to the live prompt and discard a
    /// selection that would otherwise keep copy/find actions pointed at stale
    /// scrollback.
    fn snap_active_terminal_to_prompt(&mut self) {
        let tracked_find_selection = self.find_selection_scope_tracks_live_selection();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.core.scroll(-1_000_000);
            tab.core.selection_clear();
        }
        if self.ui.find_open() && tracked_find_selection {
            self.sync_find_selection_scope_after_terminal_change(true);
            self.refresh_find();
        }
    }

    fn active_tab_at_shell_prompt(&self) -> bool {
        let Some(tab) = self.tabs.get(self.active) else {
            return false;
        };
        // Runs on every printable keypress/paste, so read just the cursor row
        // instead of snapshotting the whole grid.
        let Some(prefix) = tab.core.cursor_row_prefix() else {
            return false;
        };
        looks_like_shell_prompt(&prefix)
    }

    fn write_terminal_input(&mut self, pty_id: u32, bytes: &[u8], failure: &str) -> bool {
        match self.pty.write(pty_id, bytes) {
            Ok(()) => true,
            Err(error) => {
                self.show_notice(format!("{failure}: {error}"));
                false
            }
        }
    }

    fn on_resize(&mut self, width: u32, height: u32) {
        self.schedule_window_size_save(width, height);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.resize(width, height);
        }
        self.live_resize_until = Some(Instant::now() + LIVE_RESIZE_REDRAW_GRACE);
        self.schedule_terminal_grid_resize(false);
        self.request_redraw();
    }

    fn schedule_window_size_save(&mut self, width: u32, height: u32) {
        let scale_factor = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.window.scale_factor())
            .unwrap_or(1.0);
        let maximized = self
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.window.is_maximized());
        if self.fullscreen || maximized {
            return;
        }
        let logical = PhysicalSize::new(width, height).to_logical::<u32>(scale_factor);
        let Some(size) = WindowSize::new(logical.width, logical.height) else {
            return;
        };
        if self.config.window_size == Some(size) {
            self.pending_window_size = None;
            self.window_size_save_deadline = None;
            return;
        }
        self.pending_window_size = Some(size);
        self.window_size_save_deadline = Some(Instant::now() + WINDOW_SIZE_SAVE_DEBOUNCE);
    }

    fn flush_pending_window_size(&mut self) {
        self.window_size_save_deadline = None;
        let Some(size) = self.pending_window_size.take() else {
            return;
        };
        if self.config.window_size == Some(size) {
            return;
        }
        self.config.window_size = Some(size);
        self.ui.sync_window_size(size);
        self.mark_config_dirty();
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
        let tracks_live_selection = self.find_selection_scope_tracks_live_selection();
        let pty = &self.pty;
        let failures = apply_terminal_grid_to_tabs(&mut self.tabs, rows, cols, |pty_id| {
            pty.resize(pty_id, rows, cols)
        });
        for tab in &self.tabs {
            if !failures.iter().any(|(tab_id, _)| *tab_id == tab.id)
                && !self.find_worker.resize(tab.id, rows, cols)
            {
                self.handle_find_worker_failure();
                break;
            }
        }
        self.find_request_generation = None;
        self.find_pending = false;
        if failures.is_empty() {
            self.last_terminal_grid = Some((rows, cols));
        } else {
            // A tab whose kernel resize failed keeps its prior emulator grid.
            // Successful tabs remain in sync with their PTYs, but the global
            // applied size is now mixed and must not cancel a later retry.
            self.last_terminal_grid = None;
            self.pending_terminal_grid = Some((rows, cols));
            self.terminal_resize_deadline = Some(Instant::now() + TERMINAL_RESIZE_DEBOUNCE);
            for (tab_id, error) in &failures {
                eprintln!("could not resize PTY for tab {tab_id}: {error}");
            }
            self.show_notice(if failures.len() == 1 {
                format!(
                    "Could not resize one terminal session; its previous grid was preserved and Phantom will retry: {}",
                    failures[0].1
                )
            } else {
                format!(
                    "Could not resize {} terminal sessions; their previous grids were preserved and Phantom will retry",
                    failures.len()
                )
            });
        }
        if self.ui.find_open() {
            // Reflow can invalidate both line and column coordinates. Follow
            // Alacritty's remapped ordinary selection while it is still the
            // captured one; otherwise expire the detached scope.
            self.sync_find_selection_scope_after_terminal_change(tracks_live_selection);
        }
        if self.ui.find_open() {
            self.refresh_find();
        }
    }

    fn flush_pending_terminal_resize(&mut self) {
        let Some((rows, cols)) = self.pending_terminal_grid.take() else {
            self.terminal_resize_deadline = None;
            return;
        };
        self.terminal_resize_deadline = None;
        self.apply_terminal_grid(rows, cols);
        self.request_redraw();
    }

    /// Poll every live shell cwd; update tab titles and persist changes together.
    fn poll_cwd(&mut self) {
        let result = update_tab_cwds(&mut self.tabs, self.active, |pty_id| self.pty.cwd(pty_id));
        if result.changed {
            self.mark_dirty();
            if result.active_changed {
                self.schedule_context_discovery();
            }
            self.request_redraw();
        }
    }

    fn arm_cwd_polling(&mut self, window: Duration) {
        let now = Instant::now();
        let until = now + window;
        if self.cwd_poll_until.is_none_or(|current| until > current) {
            self.cwd_poll_until = Some(until);
        }
        let next = now + CWD_POLL_INTERVAL;
        if self.cwd_deadline.is_none_or(|current| next < current) {
            self.cwd_deadline = Some(next);
        }
    }

    fn pump_cwd_polling(&mut self, now: Instant) {
        let Some(deadline) = self.cwd_deadline else {
            return;
        };
        if now < deadline {
            return;
        }
        self.poll_cwd();
        if self.cwd_poll_until.is_some_and(|until| now < until) {
            self.cwd_deadline = Some(now + CWD_POLL_INTERVAL);
        } else {
            self.cwd_deadline = None;
            self.cwd_poll_until = None;
        }
    }

    fn request_exit(&mut self) {
        self.flush_pending_window_size();
        self.save_tabs();
        self.flush_persistence();
        // Kill PTYs before closing the event queue: a reader that sees the
        // closed queue detaches and removes its own session, which would make
        // the kill() below a silent no-op.
        for tab in &self.tabs {
            let _ = self.pty.kill(tab.pty_id);
        }
        if let Some(pending) = self.pending_pty_events.as_ref() {
            pending.close();
        }
        self.exit_requested = true;
    }

    fn save_tabs(&mut self) {
        if self.ephemeral {
            return;
        }
        let active = self.active;
        let records: Vec<TabRecord> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| TabRecord {
                id: Some(t.id.to_string()),
                title: sanitize_title(t.title()),
                cwd: t.cwd.clone(),
                sort_order: i as i64,
                is_active: i == active,
                shell_profile_id: t.profile_id.clone(),
                created_at: None,
                updated_at: None,
            })
            .collect();
        let errors = self.persistence.save_tabs(records);
        self.report_persistence_errors(errors, "Could not remember tabs");
    }

    fn render(&mut self) {
        self.redraw_queued = false;
        self.sync_surface_to_window_size();
        let (context_top_inset_points, terminal_left_points, terminal_right_points) =
            match (self.gpu.as_ref(), self.renderer.as_ref()) {
                (Some(gpu), Some(renderer)) => {
                    let (width, height) = gpu.size();
                    let layout = chrome::compute_layout_scaled(
                        width.max(1) as f32,
                        height as f32,
                        renderer.cell_size().1,
                        self.config.tab_layout != "vertical",
                        chrome::WindowChrome::from_config(&self.applied_window_chrome),
                        gpu.scale_factor(),
                    );
                    let pane = chrome::terminal_pane(&layout);
                    (
                        pane.y / gpu.scale_factor(),
                        pane.x / gpu.scale_factor(),
                        (pane.x + pane.w) / gpu.scale_factor(),
                    )
                }
                _ => (0.0, 0.0, 0.0),
            };
        let context_fingerprint_before =
            context_discovery_fingerprint(&self.config.context_actions);
        let frequent_commands = self
            .tabs
            .get(self.active)
            .map(|tab| tab.frequent_commands.top())
            .unwrap_or(&[]);
        let global_notice = self.notice.as_ref().map(|notice| notice.text.as_str());
        let ephemeral_indicator_hovered = self.ephemeral
            && self.cursor_seen
            && self.last_hits.as_ref().is_some_and(|hits| {
                hits.ephemeral_indicator_contains(self.cursor_pos.0, self.cursor_pos.1)
            });
        let ui_outcome = match (self.gpu.as_ref(), self.egui.as_mut()) {
            (Some(gpu), Some(egui)) => egui.run_with_context(
                &gpu.window,
                &mut self.ui,
                &mut self.config,
                &mut self.palette,
                UiFrameContext {
                    snapshot: &self.context_snapshot,
                    frequent_commands,
                    top_inset_points: context_top_inset_points,
                    terminal_left_points,
                    terminal_right_points,
                    global_notice,
                    ephemeral_indicator_hovered,
                },
            ),
            _ => Default::default(),
        };
        // Honor egui's repaint request (caret blink, hover fades): zero means
        // redraw right away, a finite delay becomes a wake-up deadline, and
        // `Duration::MAX` (idle) overflows checked_add to None.
        if let Some(egui) = self.egui.as_ref() {
            let delay = egui.repaint_delay();
            if delay.is_zero() {
                self.egui_repaint_after = None;
                self.request_redraw();
            } else {
                self.egui_repaint_after = Instant::now().checked_add(delay);
            }
        }
        if self.overlay_owns_terminal_ime() {
            self.preedit.clear();
        }
        if ui_outcome.config_changed {
            self.apply_config_change();
            if context_discovery_fingerprint(&self.config.context_actions)
                != context_fingerprint_before
            {
                self.schedule_context_discovery();
            }
        }
        if let Some(action) = ui_outcome.palette_action {
            self.dispatch_palette(action);
        }
        if let Some(request) = ui_outcome.context_request {
            self.dispatch_context_request(request);
        }
        if ui_outcome.find.close_requested {
            self.close_find();
        } else {
            if ui_outcome.find.query_or_options_changed {
                self.find_active = 0;
                self.refresh_find();
            }
            if let Some(direction) = ui_outcome.find.navigation {
                self.navigate_find(direction);
            }
        }
        if let Some(egui) = self.egui.as_mut() {
            // Rebuilding full-surface blur textures for every intermediate
            // maximize/restore size can saturate the GPU and make the native
            // window appear frozen. Keep translucent fills during live resize,
            // then restore frosting once the surface dimensions settle.
            egui.set_blur_suppressed(self.live_resize_until.is_some());
        }
        if self.pending_terminal_grid.is_none() {
            self.sync_terminal_grid(false);
        }
        let drag_indicator = self.tab_drag_indicator();
        let custom_window_chrome = self.custom_window_chrome();

        self.tab_titles.resize_with(self.tabs.len(), String::new);
        for (slot, tab) in self.tab_titles.iter_mut().zip(&self.tabs) {
            slot.clear();
            slot.push_str(tab.title());
        }

        let (Some(gpu), Some(renderer)) = (self.gpu.as_mut(), self.renderer.as_mut()) else {
            return;
        };
        let (w, h) = gpu.size();
        let base_layout = chrome::compute_layout_scaled(
            w.max(1) as f32,
            h as f32,
            renderer.cell_size().1,
            self.config.tab_layout != "vertical",
            chrome::WindowChrome::from_config(&self.applied_window_chrome),
            gpu.scale_factor(),
        );
        let layout = chrome::reserve_right_sidebar(base_layout, self.ui.terminal_right_inset_px());
        let colors = chrome::ChromeColors::from_renderer(
            renderer,
            themes::ui_theme_accent(&self.config.ui_theme),
        );
        let chrome_animating = self
            .chrome_anim
            .advance(Instant::now(), self.tab_titles.len());

        renderer.begin();
        chrome::draw_window_backfill(renderer, &base_layout, &colors, w as f32, h as f32);
        let terminal_pane = chrome::terminal_pane(&base_layout);
        renderer.draw_terminal_backdrop(
            &self.config.terminal_background,
            self.config.terminal_background_opacity,
            terminal_pane.x,
            terminal_pane.y,
            terminal_pane.w,
            terminal_pane.h,
        );
        if let Some(wash) = themes::ui_theme_terminal_wash(&self.config.ui_theme) {
            renderer.draw_terminal_wash(
                wash,
                terminal_pane.x,
                terminal_pane.y,
                terminal_pane.w,
                terminal_pane.h,
            );
        }
        let hits = chrome::draw_tab_bar(
            renderer,
            &gpu.queue,
            &layout,
            &self.tab_titles,
            self.active,
            &colors,
            self.rename.as_deref(),
            self.ui.settings_open(),
            self.ephemeral,
            &self.chrome_anim,
            drag_indicator,
            self.tab_scroll,
            self.tab_scroll_into_view.take(),
        );
        self.tab_scroll = hits.tab_scroll;
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
        renderer.end(&gpu.device, &gpu.queue);
        let present_status = if let Some(egui) = self.egui.as_mut() {
            gpu.present_with_overlay(renderer, Some(egui), custom_window_chrome)
        } else {
            gpu.present(renderer)
        };
        self.last_hits = Some(hits);
        self.update_chrome_hover();
        self.handle_present_status(present_status);
        if chrome_animating && present_status == PresentStatus::Presented {
            self.request_redraw();
        }
    }

    fn handle_present_status(&mut self, status: PresentStatus) {
        match status {
            // A persistent validation failure must not silently freeze a live
            // window: retry a bounded number of times, then exit cleanly
            // (tabs and config are flushed by request_exit).
            PresentStatus::Fatal => {
                self.fatal_presents += 1;
                if self.fatal_presents >= MAX_FATAL_PRESENTS {
                    eprintln!("rendering failed {MAX_FATAL_PRESENTS} times in a row; exiting");
                    self.request_exit();
                    return;
                }
                self.surface_redraw_after = Some(Instant::now() + SURFACE_RETRY_DELAY);
            }
            PresentStatus::Presented => {
                self.fatal_presents = 0;
                self.surface_redraw_after = None;
            }
            // While the window is known-hidden, resume on Occluded(false)
            // instead of polling the surface at 4 Hz forever. The timed retry
            // stays as the fallback when no occlusion event was seen.
            PresentStatus::Occluded if self.window_occluded => {
                self.surface_redraw_after = None;
            }
            other => {
                self.surface_redraw_after =
                    surface_retry_delay(other).map(|delay| Instant::now() + delay);
            }
        }
    }

    fn pump_blink(&mut self) -> bool {
        if !self.blink_enabled || !self.window_focused {
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
/// the current modifier state for key events and the renderer's physical cell
/// height to convert trackpad pixel scrolling into lines. Returns `None` for
/// events we ignore (and for `RedrawRequested`, which the caller handles
/// directly).
fn translate(
    event: &WindowEvent,
    mods: Mods,
    cursor: (f32, f32),
    cell_height_px: f32,
) -> Option<AppInput> {
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
        WindowEvent::MouseInput { state, button, .. } => {
            let button = match button {
                MouseButton::Left => AppMouseButton::Left,
                MouseButton::Middle => AppMouseButton::Middle,
                _ => return None,
            };
            match state {
                ElementState::Pressed => AppInput::MouseDown {
                    x: cursor.0,
                    y: cursor.1,
                    button,
                },
                ElementState::Released => AppInput::MouseUp {
                    x: cursor.0,
                    y: cursor.1,
                    button,
                },
            }
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => *y,
                // Pixel deltas are physical px; dividing by the real cell
                // height keeps trackpad speed DPI-independent.
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / cell_height_px,
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

fn terminal_owns_keyboard(
    palette_open: bool,
    renaming: bool,
    settings_open: bool,
    context_owns_keyboard: bool,
) -> bool {
    !palette_open && !renaming && !settings_open && !context_owns_keyboard
}

fn overlay_owns_terminal_ime(
    palette_open: bool,
    renaming: bool,
    settings_open: bool,
    context_owns_keyboard: bool,
    egui_wants_keyboard_input: bool,
) -> bool {
    palette_open || renaming || settings_open || context_owns_keyboard || egui_wants_keyboard_input
}

fn app_input_is_find_shortcut(input: &AppInput) -> bool {
    matches!(
        input,
        AppInput::Key { key, mods, .. }
            if BuiltinShortcut::FindInScrollback.matches(*key, *mods)
    )
}

/// Allocation-free digest of the context-actions settings that affect
/// discovery (enabled flag plus per-plugin id/enabled/order), so the render
/// path can detect a relevant change without cloning the whole config —
/// notably its bounded-but-large directory history.
fn context_discovery_fingerprint(config: &phantom_core::ContextActionsConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config.enabled.hash(&mut hasher);
    config.plugins.len().hash(&mut hasher);
    for plugin in &config.plugins {
        plugin.id.hash(&mut hasher);
        plugin.enabled.hash(&mut hasher);
        plugin.order.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
fn context_discovery_config_changed(
    before: &phantom_core::ContextActionsConfig,
    after: &phantom_core::ContextActionsConfig,
) -> bool {
    context_discovery_fingerprint(before) != context_discovery_fingerprint(after)
}

fn spdeploy_startup_command(
    config_path: &Path,
    operation: &str,
    resolver: impl FnOnce(&str) -> phantom_core::AppResult<String>,
) -> phantom_core::AppResult<StartupCommand> {
    let program = resolver("spdeploy")?;
    if !Path::new(&program).is_absolute() {
        return Err(phantom_core::AppError::InvalidConfig(
            "resolved spdeploy program is not an absolute path".to_string(),
        ));
    }
    let config_path = config_path.to_str().ok_or_else(|| {
        phantom_core::AppError::InvalidConfig(
            "spdeploy config path must be valid UTF-8".to_string(),
        )
    })?;
    Ok(StartupCommand {
        program,
        args: vec![
            "--config".to_string(),
            config_path.to_string(),
            format!("--operation={operation}"),
        ],
        env: Default::default(),
    })
}

fn looks_like_shell_prompt(prefix: &str) -> bool {
    matches!(
        prefix.trim_end().chars().next_back(),
        Some('$' | '%' | '#' | '>' | '❯' | '➜')
    )
}

fn resolve_context_tab_launch(
    config: &AppConfig,
    project: &phantom_core::TrustedProject,
    observed_root: &Path,
    observed_source: &str,
    task_id: &str,
    rows: u16,
    cols: u16,
) -> phantom_core::AppResult<LaunchOpts> {
    let trusted =
        resolve_trusted_task(project, observed_root, observed_source, task_id, rows, cols)?;
    let task = project.task(task_id).ok_or_else(|| {
        phantom_core::AppError::InvalidConfig(format!("unknown trusted task '{task_id}'"))
    })?;
    if task.run.is_none() {
        resolve_launch_opts(config, None, trusted.cwd, rows, cols)
    } else {
        Ok(trusted)
    }
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

fn surface_retry_delay(status: PresentStatus) -> Option<Duration> {
    match status {
        PresentStatus::Presented | PresentStatus::Fatal => None,
        PresentStatus::RetrySoon => Some(SURFACE_RETRY_DELAY),
        PresentStatus::Occluded => Some(SURFACE_OCCLUDED_RETRY_DELAY),
    }
}

fn min_deadline(target: &mut Option<Instant>, deadline: Instant) {
    if target.is_none_or(|current| deadline < current) {
        *target = Some(deadline);
    }
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
    let mut attrs = Window::default_attributes().with_title("Phantom Terminal");
    if let Some(size) = config.window_size {
        attrs = attrs.with_inner_size(LogicalSize::new(size.width, size.height));
    }
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
        .with_movable_by_window_background(false)
}

#[cfg(not(target_os = "macos"))]
fn custom_window_attributes(attrs: WindowAttributes) -> WindowAttributes {
    attrs.with_decorations(false).with_transparent(true)
}

#[cfg(target_os = "macos")]
fn configure_custom_window_drag(window: &Window, custom_window_chrome: bool) {
    if custom_window_chrome {
        set_macos_window_movable(window, false);
    }
}

#[cfg(not(target_os = "macos"))]
fn configure_custom_window_drag(_window: &Window, _custom_window_chrome: bool) {}

#[cfg(target_os = "macos")]
fn drag_window_from_custom_titlebar(window: &Window) {
    set_macos_window_movable(window, true);
    let _ = window.drag_window();
    set_macos_window_movable(window, false);
}

#[cfg(not(target_os = "macos"))]
fn drag_window_from_custom_titlebar(window: &Window) {
    let _ = window.drag_window();
}

#[cfg(target_os = "macos")]
fn set_macos_window_movable(window: &Window, movable: bool) {
    let Some(ns_window) = macos_ns_window(window) else {
        return;
    };
    ns_window.setMovableByWindowBackground(false);
    ns_window.setMovable(movable);
}

#[cfg(target_os = "macos")]
fn apply_macos_window_chrome(window: &Window, window_chrome: chrome::WindowChrome) -> bool {
    use objc2_app_kit::NSWindowTitleVisibility;

    let Some(ns_window) = macos_ns_window(window) else {
        return false;
    };
    let custom = window_chrome.is_custom();
    let first_responder = ns_window.firstResponder();
    ns_window.setStyleMask(macos_window_style_mask(ns_window.styleMask(), custom));
    ns_window.setTitlebarAppearsTransparent(custom);
    ns_window.setTitleVisibility(if custom {
        NSWindowTitleVisibility::Hidden
    } else {
        NSWindowTitleVisibility::Visible
    });
    ns_window.setMovableByWindowBackground(false);
    ns_window.setMovable(!custom);
    let _ = ns_window.makeFirstResponder(first_responder.as_deref());
    true
}

#[cfg(target_os = "macos")]
fn macos_window_style_mask(
    style_mask: objc2_app_kit::NSWindowStyleMask,
    custom: bool,
) -> objc2_app_kit::NSWindowStyleMask {
    use objc2_app_kit::NSWindowStyleMask;

    if custom {
        style_mask | NSWindowStyleMask::FullSizeContentView
    } else {
        style_mask & !NSWindowStyleMask::FullSizeContentView
    }
}

#[cfg(target_os = "macos")]
fn macos_ns_window(window: &Window) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use objc2_app_kit::NSView;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(handle) => {
            let ns_view = unsafe { handle.ns_view.as_ptr().cast::<NSView>().as_ref()? };
            ns_view.window()
        }
        _ => None,
    }
}

#[cfg(any(
    test,
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn accesskit_session_bus_allowed(address: Option<&OsStr>) -> bool {
    address.is_none_or(|address| {
        address
            .to_str()
            .is_some_and(|address| address.starts_with("unix:"))
    })
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn accesskit_transport_allowed() -> bool {
    accesskit_session_bus_allowed(std::env::var_os("DBUS_SESSION_BUS_ADDRESS").as_deref())
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn accesskit_transport_allowed() -> bool {
    true
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let custom_window_chrome = self.custom_window_chrome();
        let window = Arc::new(
            event_loop
                .create_window(window_attributes(&self.config).with_visible(false))
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
                window.set_visible(true);
                self.show_notice(error);
                return;
            }
        };
        configure_custom_window_drag(&gpu.window, custom_window_chrome);
        gpu.window.set_ime_allowed(true);
        let renderer = match Renderer::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            &self.config,
            gpu.scale_factor(),
        ) {
            Ok(renderer) => renderer,
            Err(error) => {
                window.set_title(&format!("Phantom Terminal - {error}"));
                window.set_visible(true);
                self.show_notice(error);
                return;
            }
        };
        let (w, h) = gpu.size();
        renderer.resize(&gpu.queue, w, h);
        let mut egui = EguiLayer::new(&gpu.window, &gpu.device, gpu.format());
        let accesskit_enabled = accesskit_transport_allowed();
        if accesskit_enabled {
            if let Some(proxy) = self.event_loop_proxy.as_ref() {
                egui.init_accesskit(event_loop, &gpu.window, proxy.clone());
            }
        }

        self.gpu = Some(gpu);
        self.renderer = Some(renderer);
        self.egui = Some(egui);

        if !accesskit_enabled {
            self.show_notice(
                "Accessibility disabled: DBUS_SESSION_BUS_ADDRESS must use a local unix: transport",
            );
        }
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.set_visible(true);
        }
        self.start();
        self.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::AccessKit(event) => {
                let Some(gpu) = self.gpu.as_ref() else {
                    return;
                };
                if event.window_id != gpu.window.id() {
                    return;
                }
                let redraw = match event.window_event {
                    AccessKitWindowEvent::InitialTreeRequested => {
                        if let Some(egui) = self.egui.as_ref() {
                            egui.enable_accesskit();
                            true
                        } else {
                            false
                        }
                    }
                    AccessKitWindowEvent::ActionRequested(request) => {
                        if let Some(egui) = self.egui.as_mut() {
                            egui.on_accesskit_action_request(request);
                            true
                        } else {
                            false
                        }
                    }
                    AccessKitWindowEvent::AccessibilityDeactivated => {
                        if let Some(egui) = self.egui.as_ref() {
                            egui.disable_accesskit();
                        }
                        false
                    }
                };
                if redraw {
                    self.request_redraw();
                }
            }
            event => self.on_pty_event(event),
        }
        if self.exit_requested {
            self.flush_persistence();
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::RedrawRequested) {
            if let (Some(gpu), Some(egui)) = (self.gpu.as_ref(), self.egui.as_mut()) {
                egui.on_accesskit_window_event(&gpu.window, &event);
            }
            self.render();
            return;
        }

        let terminal_owns_keyboard = terminal_owns_keyboard(
            self.palette.open,
            self.rename.is_some(),
            self.ui.settings_open(),
            self.ui.context_owns_keyboard(),
        );
        let egui_response = match (self.gpu.as_ref(), self.egui.as_mut()) {
            (Some(gpu), Some(egui)) if terminal_owns_keyboard && is_keyboard_event(&event) => {
                egui.on_accesskit_window_event(&gpu.window, &event);
                None
            }
            (Some(gpu), Some(egui)) => Some(egui.on_window_event(&gpu.window, &event)),
            _ => None,
        };
        if egui_response.is_some_and(|response| response.repaint) {
            self.request_redraw();
        }
        let consumed = egui_response.is_some_and(|response| response.consumed);

        if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.clear_chrome_hover();
        }
        if let WindowEvent::Occluded(occluded) = event {
            self.window_occluded = occluded;
            if !occluded {
                self.request_redraw();
            }
        }
        if let WindowEvent::Focused(focused) = event {
            self.window_focused = focused;
            if focused {
                // Restart the cycle so the cursor is solid as focus returns.
                self.reset_blink();
            } else if !self.cursor_on {
                // Blink pauses while unfocused; leave a solid cursor behind.
                self.cursor_on = true;
                self.request_redraw();
            }
        }

        let cell_height_px = self
            .renderer
            .as_ref()
            .map(|renderer| renderer.cell_size().1)
            .unwrap_or(FALLBACK_SCROLL_CELL_HEIGHT_PX);
        let input = translate(&event, self.mods, self.cursor_pos, cell_height_px);

        if let Some(input) = input {
            let input_is_ime = matches!(input, AppInput::ImeCommit(_) | AppInput::ImePreedit(_));
            let egui_overlay_owns_input = self.egui.is_some()
                && (self.palette.open
                    || self.ui.context_owns_keyboard()
                    || (input_is_ime && self.overlay_owns_terminal_ime()));
            if egui_overlay_owns_input && input_is_ime && !self.preedit.is_empty() {
                self.preedit.clear();
                self.request_redraw();
            }
            if app_input_bypasses_egui_overlay(&input)
                || app_input_is_find_shortcut(&input)
                || self.input_starts_window_resize(&input)
                || (!consumed && !egui_overlay_owns_input)
            {
                self.handle_input(input);
            }
        }
        if self.exit_requested {
            self.flush_persistence();
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.pump_blink() {
            self.request_redraw();
        }
        self.drain_persistence_errors();
        self.pump_cwd_polling(now);
        self.flush_directory_visit();
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
        if let Some(deadline) = self.find_refresh_deadline {
            if now >= deadline {
                self.find_refresh_deadline = None;
                // The palette or a panel may have covered the bar since this
                // was scheduled; drop the refresh rather than search unseen.
                if self.find_bar_visible() {
                    self.refresh_find();
                    self.request_redraw();
                }
            }
        }
        if self
            .window_size_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.flush_pending_window_size();
        }
        self.clear_expired_notice(now);
        if self.exit_requested {
            self.flush_persistence();
            event_loop.exit();
            return;
        }
        if let Some(deadline) = self.surface_redraw_after {
            if now >= deadline {
                self.surface_redraw_after = None;
                self.request_redraw();
            }
        }
        if let Some(deadline) = self.egui_repaint_after {
            if now >= deadline {
                self.egui_repaint_after = None;
                self.request_redraw();
            }
        }
        let surface_retry_pending = self
            .surface_redraw_after
            .is_some_and(|deadline| now < deadline);
        if let Some(until) = self.live_resize_until {
            if now < until && !surface_retry_pending {
                self.request_redraw();
                event_loop.set_control_flow(ControlFlow::Poll);
                return;
            }
            if now >= until {
                self.live_resize_until = None;
                // Paint one settled frame with the final grid and backdrop
                // blur after the cheap live-resize path ends.
                self.request_redraw();
            }
        }

        let mut next = None;
        // No blink wake-ups while hidden or unfocused; Occluded(false)
        // restarts rendering and Focused(true) restarts the blink cycle.
        if self.blink_enabled && !self.window_occluded && self.window_focused {
            min_deadline(&mut next, self.next_toggle);
        }
        if let Some(deadline) = self.cwd_deadline {
            min_deadline(&mut next, deadline);
        }
        if let Some(deadline) = self.surface_redraw_after {
            min_deadline(&mut next, deadline);
        }
        if let Some(deadline) = self.renderer_rebuild_after {
            min_deadline(&mut next, deadline);
        }
        if let Some(deadline) = self.terminal_resize_deadline {
            min_deadline(&mut next, deadline);
        }
        if let Some(deadline) = self.find_refresh_deadline {
            min_deadline(&mut next, deadline);
        }
        if let Some(deadline) = self.window_size_save_deadline {
            min_deadline(&mut next, deadline);
        }
        if let Some(deadline) = self.egui_repaint_after {
            min_deadline(&mut next, deadline);
        }
        if let Some(notice) = self.notice.as_ref() {
            min_deadline(&mut next, notice.until);
        }
        if let Some(next) = next {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn apply_terminal_grid_to_tabs(
    tabs: &mut [Tab],
    rows: u16,
    cols: u16,
    mut resize_pty: impl FnMut(u32) -> phantom_core::AppResult<()>,
) -> Vec<(u64, String)> {
    let mut failures = Vec::new();
    for tab in tabs {
        match resize_pty(tab.pty_id) {
            Ok(()) => tab.core.resize(rows, cols),
            Err(error) => failures.push((tab.id, error.to_string())),
        }
    }
    failures
}

fn viewport_cell_for_grid(
    viewport: chrome::Rect,
    (cell_width, cell_height): (f32, f32),
    (rows, cols): (u16, u16),
    px: f32,
    py: f32,
) -> Option<(usize, usize, SelSide)> {
    if !viewport.contains(px, py)
        || rows == 0
        || cols == 0
        || cell_width <= 0.0
        || cell_height <= 0.0
    {
        return None;
    }
    let lx = px - viewport.x;
    let ly = py - viewport.y;
    let col = ((lx / cell_width).floor() as i64).clamp(0, i64::from(cols) - 1) as usize;
    let row = ((ly / cell_height).floor() as i64).clamp(0, i64::from(rows) - 1) as usize;
    let side = if lx - col as f32 * cell_width < cell_width / 2.0 {
        SelSide::Left
    } else {
        SelSide::Right
    };
    Some((row, col, side))
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

struct PreparedPersistence {
    _remembered_tabs_lock: Option<phantom_core::RememberedTabsLock>,
    store: Option<SessionStore>,
    config: AppConfig,
}

fn prepare_persistence(launch: &LaunchContext) -> phantom_core::AppResult<PreparedPersistence> {
    let remembered_tabs_lock = launch
        .remember_tabs
        .then(phantom_core::RememberedTabsLock::acquire)
        .transpose()?;
    let store = SessionStore::open()
        .map_err(|e| eprintln!("session store unavailable ({e}); not persisting"))
        .ok();
    let config = store
        .as_ref()
        .and_then(|s| s.load_config().ok())
        .unwrap_or_default();
    Ok(PreparedPersistence {
        _remembered_tabs_lock: remembered_tabs_lock,
        store,
        config,
    })
}

/// Run the app under a winit event loop (the production entry point).
pub fn run() -> phantom_core::AppResult<()> {
    let launch = phantom_core::LaunchState::from_env()?.context();
    let PreparedPersistence {
        _remembered_tabs_lock,
        store,
        config,
    } = prepare_persistence(&launch)?;
    #[cfg(debug_assertions)]
    if std::env::var_os("PHANTOM_TEST_STARTUP_READY").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        use std::io::Write;

        println!("PHANTOM_STARTUP_READY");
        std::io::stdout().flush()?;
    }

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .expect("failed to build event loop");
    let proxy = event_loop.create_proxy();
    let pending = Arc::new(PendingPtyEvents::new());
    let outbox: Arc<dyn PtyOutbox> = Arc::new(ProxyOutbox {
        proxy: proxy.clone(),
        pending: Arc::clone(&pending),
    });
    let mut app = App::new(outbox, config, store, launch);
    app.pending_pty_events = Some(pending);
    app.event_loop_proxy = Some(proxy);
    event_loop.run_app(&mut app).expect("event loop error");
    Ok(())
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

    enum TestClipboardMode {
        Empty,
        ReadFailure,
        WriteFailure,
    }

    struct TestClipboard(TestClipboardMode);

    impl ClipboardAccess for TestClipboard {
        fn set_text(&mut self, _text: String) -> Result<(), arboard::Error> {
            match self.0 {
                TestClipboardMode::WriteFailure => Err(arboard::Error::ClipboardOccupied),
                TestClipboardMode::Empty | TestClipboardMode::ReadFailure => Ok(()),
            }
        }

        fn get_text(&mut self) -> Result<String, arboard::Error> {
            match self.0 {
                TestClipboardMode::ReadFailure => Err(arboard::Error::ContentNotAvailable),
                TestClipboardMode::Empty => Ok(String::new()),
                TestClipboardMode::WriteFailure => Ok("paste".to_string()),
            }
        }

        #[cfg(target_os = "linux")]
        fn set_primary_text(&mut self, text: String) -> Result<(), arboard::Error> {
            self.set_text(text)
        }

        #[cfg(target_os = "linux")]
        fn get_primary_text(&mut self) -> Result<String, arboard::Error> {
            self.get_text()
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_chrome_style_mask_toggles_only_full_size_content() {
        use objc2_app_kit::NSWindowStyleMask;

        let base = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::FullScreen;
        let custom = macos_window_style_mask(base, true);
        assert!(custom.contains(NSWindowStyleMask::FullSizeContentView));
        assert_eq!(
            custom & !NSWindowStyleMask::FullSizeContentView,
            base,
            "unrelated AppKit window behavior must be preserved"
        );
        assert_eq!(macos_window_style_mask(custom, false), base);
    }

    fn test_app_with_selection() -> App {
        let mut app = test_app();
        let mut tab = Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        );
        tab.advance_pty(b"copy");
        tab.core.selection_start(0, 0, SelSide::Left);
        tab.core.selection_update(0, 3, SelSide::Right);
        assert!(tab.core.selection_text().is_some());
        app.tabs.push(tab);
        app
    }

    #[test]
    fn unavailable_clipboard_is_reported_for_copy_and_paste() {
        let mut copy_app = test_app_with_selection();
        copy_app.clipboard = None;

        copy_app.copy_selection();

        assert_eq!(copy_app.notice_text(), Some("Clipboard is unavailable"));

        let mut paste_app = test_app();
        paste_app.clipboard = None;

        paste_app.paste_clipboard();

        assert_eq!(paste_app.notice_text(), Some("Clipboard is unavailable"));
    }

    #[test]
    fn clipboard_operation_failures_are_reported() {
        let mut copy_app = test_app_with_selection();
        copy_app.clipboard = Some(Box::new(TestClipboard(TestClipboardMode::WriteFailure)));

        copy_app.copy_selection();

        assert!(copy_app
            .notice_text()
            .is_some_and(|notice| notice.starts_with("Could not copy: ")));

        let mut paste_app = test_app();
        paste_app.clipboard = Some(Box::new(TestClipboard(TestClipboardMode::ReadFailure)));

        paste_app.paste_clipboard();

        assert!(paste_app
            .notice_text()
            .is_some_and(|notice| notice.starts_with("Could not paste: ")));
    }

    #[test]
    fn empty_clipboard_is_reported() {
        let mut app = test_app();
        app.clipboard = Some(Box::new(TestClipboard(TestClipboardMode::Empty)));

        app.paste_clipboard();

        assert_eq!(app.notice_text(), Some("Clipboard is empty"));
    }

    #[test]
    fn terminal_input_cleanup_snaps_to_prompt_and_expires_live_find_selection() {
        let mut app = test_app();
        let mut tab = Tab::new(
            0,
            AlacrittyCore::new(3, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        );
        let history = b"target\r\none\r\ntwo\r\nthree\r\nfour\r\nfive";
        tab.advance_pty(history);
        tab.core.scroll(3);
        assert!(tab.core.scroll_state().offset > 0);
        tab.core.selection_start(0, 0, SelSide::Left);
        tab.core.selection_update(0, 3, SelSide::Right);
        assert!(tab.core.selection_range().is_some());
        app.tabs.push(tab);
        assert!(app.find_worker.create(0, 3, 40, 100, CursorShape::Block));
        assert!(app.find_worker.advance(0, history.to_vec(), false));

        app.open_find();
        app.ui.configure_find_for_tests("target", true);
        app.refresh_find();
        assert!(app.find_request_generation.is_some());
        assert!(app.find_pending);

        // Paste, IME commits, and ordinary keys all call this only after their
        // PTY write succeeds.
        app.snap_active_terminal_to_prompt();

        assert_eq!(app.tabs[0].core.scroll_state().offset, 0);
        assert!(app.tabs[0].core.selection_range().is_none());
        assert!(app.find_selection_scope.is_none());
        assert!(!app.ui.find_state().selection_available());
        assert!(app.find_request_generation.is_none());
        assert!(!app.find_pending);
        assert!(app.find_matches.is_empty());
    }

    #[test]
    fn copying_without_a_selection_remains_a_no_op() {
        let mut app = test_app();
        app.clipboard = None;

        app.copy_selection();

        assert_eq!(app.notice_text(), None);
    }

    #[test]
    fn middle_click_routes_only_grid_events_and_shift_overrides_mouse_reporting() {
        assert_eq!(TerminalMouseButton::Middle.code(), 1);
        assert_eq!(
            middle_click_route(false, false, false),
            MiddleClickRoute::Ignore
        );
        assert_eq!(
            middle_click_route(false, true, false),
            MiddleClickRoute::Ignore
        );
        assert_eq!(
            middle_click_route(true, true, false),
            MiddleClickRoute::MouseReport
        );
        assert_eq!(
            middle_click_route(true, true, true),
            MiddleClickRoute::PrimaryPaste
        );
        assert_eq!(
            middle_click_route(true, false, false),
            MiddleClickRoute::PrimaryPaste
        );
    }

    #[test]
    fn middle_mouse_release_requires_reporting_to_remain_active() {
        assert!(should_report_middle_mouse_release(true, true));
        assert!(!should_report_middle_mouse_release(true, false));
        assert!(!should_report_middle_mouse_release(false, true));
    }

    #[test]
    fn tab_limit_allows_128th_rejects_129th_and_shutdown_persists_all() {
        let store = SessionStore::in_memory_for_tests().unwrap();
        let mut app = App::new(
            Arc::new(NoopOutbox),
            AppConfig::default(),
            Some(store.clone()),
            LaunchContext {
                cwd: None,
                remember_tabs: true,
            },
        );
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        for id in 0..(MAX_TABS - 1) {
            app.tabs.push(Tab::new(
                id as u64,
                AlacrittyCore::new(1, 1, 0, CursorShape::Block),
                u32::MAX,
                cwd.clone(),
                None,
            ));
        }
        app.active = app.tabs.len() - 1;
        app.next_tab_id = app.tabs.len() as u64;

        assert!(app.spawn_tab_with_persistence(None, Some(cwd.clone()), false));
        assert_eq!(app.tabs.len(), MAX_TABS);

        assert!(!app.spawn_tab_with_persistence(None, Some(cwd), true));
        assert_eq!(app.tabs.len(), MAX_TABS);
        assert_eq!(
            app.notice.as_ref().map(|notice| notice.text.as_str()),
            Some("Cannot open more than 128 tabs")
        );

        app.request_exit();
        let remembered = store.load_tabs().unwrap();
        assert_eq!(remembered.len(), MAX_TABS);
        assert!(remembered.last().unwrap().is_active);
    }

    #[test]
    fn inherited_cwd_uses_active_tab_only_while_directory_exists() {
        let mut app = test_app();
        assert_eq!(app.inherited_cwd(), None);

        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            dir.clone(),
            None,
        ));
        assert_eq!(app.inherited_cwd(), Some(dir));

        app.tabs[0].cwd = "/phantom-test-dir-that-does-not-exist".to_string();
        assert_eq!(app.inherited_cwd(), None);

        app.tabs[0].cwd = String::new();
        assert_eq!(app.inherited_cwd(), None);
    }

    #[test]
    fn cwd_poll_updates_background_tabs_in_one_bounded_pass() {
        let mut tabs = vec![
            Tab::new(
                0,
                AlacrittyCore::new(4, 40, 100, CursorShape::Block),
                11,
                "/active".to_string(),
                None,
            ),
            Tab::new(
                1,
                AlacrittyCore::new(4, 40, 100, CursorShape::Block),
                22,
                "/stale-background".to_string(),
                None,
            ),
        ];
        let mut polled = Vec::new();

        let result = update_tab_cwds(&mut tabs, 0, |pty_id| {
            polled.push(pty_id);
            match pty_id {
                11 => Some("/active".to_string()),
                22 => Some("/fresh-background".to_string()),
                _ => None,
            }
        });

        assert_eq!(polled, [11, 22]);
        assert!(result.changed);
        assert!(!result.active_changed);
        assert_eq!(tabs[0].cwd, "/active");
        assert_eq!(tabs[1].cwd, "/fresh-background");
    }

    #[test]
    fn restore_cwd_rejects_missing_paths_and_files() {
        let directory = std::env::temp_dir();
        let directory = directory.to_string_lossy().into_owned();
        assert_eq!(existing_directory(&directory), Some(directory.clone()));

        let file = std::env::current_exe().unwrap();
        assert_eq!(existing_directory(&file.to_string_lossy()), None);

        let missing =
            Path::new(&directory).join(format!("phantom-stale-restore-{}", std::process::id()));
        assert!(!missing.exists());
        assert_eq!(existing_directory(&missing.to_string_lossy()), None);

        let mut config = AppConfig::default();
        config.shell_profiles[0].cwd = Some(directory.clone());
        let launch = resolve_new_tab_launch(
            &config,
            None,
            existing_directory(&missing.to_string_lossy()),
            None,
            24,
            80,
        )
        .unwrap();
        assert_eq!(launch.cwd.as_deref(), Some(directory.as_str()));
    }

    #[test]
    fn keyboard_events_bypass_egui_only_when_terminal_owns_input() {
        assert!(terminal_owns_keyboard(false, false, false, false));
        assert!(!terminal_owns_keyboard(true, false, false, false));
        assert!(!terminal_owns_keyboard(false, true, false, false));
        assert!(!terminal_owns_keyboard(false, false, true, false));
        assert!(!terminal_owns_keyboard(false, false, false, true));
    }

    #[test]
    fn mouse_reporting_accumulates_fractional_wheel_lines() {
        let mut app = test_app();

        for _ in 0..3 {
            assert_eq!(app.accumulate_wheel_delta(WheelRoute::MouseReport, 0.25), 0);
        }
        assert_eq!(app.accumulate_wheel_delta(WheelRoute::MouseReport, 0.25), 1);
        assert_eq!(app.wheel_line_remainder, 0.0);
    }

    #[test]
    fn wheel_accumulator_resets_on_direction_and_route_changes() {
        let mut app = test_app();

        assert_eq!(app.accumulate_wheel_delta(WheelRoute::MouseReport, 0.75), 0);
        assert_eq!(
            app.accumulate_wheel_delta(WheelRoute::MouseReport, -0.25),
            0
        );
        assert_eq!(app.wheel_line_remainder, -0.25);

        assert_eq!(app.accumulate_wheel_delta(WheelRoute::Terminal, 0.25), 0);
        assert_eq!(app.wheel_line_remainder, 0.25);

        app.select_wheel_route(WheelRoute::TabStrip);
        assert_eq!(app.wheel_line_remainder, 0.0);
        assert_eq!(app.accumulate_wheel_delta(WheelRoute::MouseReport, 0.25), 0);
        assert_eq!(app.wheel_line_remainder, 0.25);
    }

    #[test]
    fn terminal_mouse_release_requires_a_forwarded_press() {
        assert_eq!(
            mouse_up_action(false, false, true),
            MouseUpAction::Ignore,
            "an egui- or chrome-consumed press must not create a terminal release"
        );
        assert_eq!(mouse_up_action(true, false, true), MouseUpAction::Report);
    }

    #[test]
    fn terminal_mouse_release_preserves_selection_and_mode_routing() {
        assert_eq!(
            mouse_up_action(true, true, true),
            MouseUpAction::EndSelection
        );
        assert_eq!(
            mouse_up_action(true, false, false),
            MouseUpAction::Ignore,
            "a mode disabled before release must not receive a mouse report"
        );
    }

    #[test]
    fn overlays_and_focused_egui_widgets_own_terminal_ime() {
        assert!(!overlay_owns_terminal_ime(
            false, false, false, false, false
        ));
        assert!(overlay_owns_terminal_ime(true, false, false, false, false));
        assert!(overlay_owns_terminal_ime(false, true, false, false, false));
        assert!(overlay_owns_terminal_ime(false, false, true, false, false));
        assert!(overlay_owns_terminal_ime(false, false, false, true, false));
        assert!(overlay_owns_terminal_ime(false, false, false, false, true));
    }

    #[test]
    fn settings_discard_terminal_ime_commit_and_preedit() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.preedit = "terminal composition".to_string();
        app.ui.open_settings(&app.config);

        app.handle_input(AppInput::ImeCommit("settings text".to_string()));

        assert!(app.preedit.is_empty());
        assert_eq!(app.notice_text(), None, "IME commit attempted a PTY write");

        app.handle_input(AppInput::ImePreedit("new composition".to_string()));

        assert!(app.preedit.is_empty());
        assert_eq!(app.notice_text(), None);
    }

    #[test]
    fn ctrl_f_and_cmd_f_are_reserved_for_scrollback_find() {
        assert!(BuiltinShortcut::FindInScrollback.matches(
            Key::Char('f'),
            Mods {
                ctrl: true,
                ..Mods::default()
            }
        ));
        assert!(BuiltinShortcut::FindInScrollback.matches(
            Key::Char('f'),
            Mods {
                sup: true,
                ..Mods::default()
            }
        ));
        assert!(!BuiltinShortcut::FindInScrollback.matches(Key::Char('f'), Mods::default()));
        assert!(!BuiltinShortcut::FindInScrollback.matches(
            Key::Char('f'),
            Mods {
                ctrl: true,
                shift: true,
                ..Mods::default()
            }
        ));
    }

    #[test]
    fn find_opens_for_active_tab_and_closes_when_switching_tabs() {
        let mut app = test_app();
        for id in 0..2 {
            app.tabs.push(Tab::new(
                id,
                AlacrittyCore::new(4, 40, 100, CursorShape::Block),
                u32::MAX,
                String::new(),
                None,
            ));
        }
        app.tabs[0].advance_pty(b"find me");

        app.handle_input(AppInput::Key {
            key: Key::Char('f'),
            text: Some("f".to_string()),
            mods: Mods {
                ctrl: true,
                ..Mods::default()
            },
        });

        assert!(app.find_open());
        app.switch_tab(1);
        assert!(!app.find_open());
    }

    #[test]
    fn selection_only_find_keeps_the_scope_captured_on_open() {
        let mut app = test_app();
        let mut tab = Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        );
        tab.advance_pty(b"target gap target");
        tab.core.selection_start(0, 0, SelSide::Left);
        tab.core.selection_update(0, 5, SelSide::Right);
        let captured = tab.core.selection_range().unwrap();
        let expected = tab
            .core
            .search_scrollback("target", SearchOptions::default(), Some(captured))
            .unwrap()
            .matches;
        app.tabs.push(tab);
        app.find_worker.create(0, 4, 40, 100, CursorShape::Block);
        app.find_worker
            .advance(0, b"target gap target".to_vec(), false);

        app.open_find();
        app.ui.configure_find_for_tests("target", true);
        app.refresh_find();
        app.wait_for_find_for_tests();
        assert_eq!(app.find_selection_scope, Some(captured));
        assert_eq!(app.find_matches, expected);

        // Replacing the ordinary selection during the same interaction must
        // not retarget selection-only search.
        app.tabs[0].core.selection_start(0, 11, SelSide::Left);
        app.tabs[0].core.selection_update(0, 16, SelSide::Right);
        let replacement = app.tabs[0].core.selection_range().unwrap();
        assert_ne!(replacement, captured);
        app.refresh_find();
        app.wait_for_find_for_tests();
        assert_eq!(app.find_selection_scope, Some(captured));
        assert_eq!(app.find_matches, expected);
        assert_eq!(app.tabs[0].core.selection_range(), Some(replacement));

        // PTY output can rotate absolute buffer coordinates, so the captured
        // range expires instead of falling back to the replacement selection.
        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"!".to_vec(),
        });
        assert_eq!(app.find_selection_scope, None);
        assert!(!app.ui.find_state().selection_available());
        app.refresh_find();
        assert!(app.find_matches.is_empty());
        assert_eq!(app.tabs[0].core.selection_range(), Some(replacement));

        app.close_find();
        assert_eq!(app.find_selection_scope, None);
        assert_eq!(app.tabs[0].core.selection_range(), Some(replacement));
    }

    #[test]
    fn completed_find_maps_all_matches_and_navigation_preserves_capped_state() {
        let mut app = test_app();
        let mut tab = Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        );
        tab.advance_pty(b"hit gap hit");
        let matches = tab
            .core
            .search_scrollback("hit", SearchOptions::default(), None)
            .unwrap()
            .matches;
        app.tabs.push(tab);
        app.ui.open_find(false);
        app.apply_find_result(Ok(SearchOutcome {
            matches: matches.clone(),
            capped: true,
        }));

        let first = app.tabs[0].core.snapshot();
        assert_eq!(
            first.cells.iter().filter(|cell| cell.search_match).count(),
            3
        );
        assert_eq!(
            first
                .cells
                .iter()
                .filter(|cell| cell.search_match_inactive)
                .count(),
            3
        );
        assert!(app.ui.find_state().results().capped);

        app.navigate_find(FindNavigation::Next);
        let second = app.tabs[0].core.snapshot();
        assert!(second.cell(0, 0).unwrap().search_match_inactive);
        assert!(second.cell(0, 8).unwrap().search_match);
        assert!(app.ui.find_state().results().capped);

        app.close_find();
        assert!(!app.tabs[0]
            .core
            .snapshot()
            .cells
            .iter()
            .any(|cell| cell.search_match || cell.search_match_inactive));
    }

    #[test]
    fn captured_find_scope_tracks_rotation_until_the_original_selection_expires() {
        let mut app = test_app();
        let mut tab = Tab::new(
            0,
            AlacrittyCore::new(2, 20, 2, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        );
        tab.advance_pty(b"target");
        tab.core.selection_start(0, 0, SelSide::Left);
        tab.core.selection_update(0, 5, SelSide::Right);
        let captured = tab.core.selection_range().unwrap();
        app.tabs.push(tab);
        app.find_worker.create(0, 2, 20, 2, CursorShape::Block);
        app.find_worker.advance(0, b"target".to_vec(), false);
        app.open_find();
        app.ui.configure_find_for_tests("target", true);

        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"\r\none\r\ntwo".to_vec(),
        });
        let rotated = app.tabs[0].core.selection_range().unwrap();
        assert_ne!(rotated, captured);
        assert_eq!(app.find_selection_scope, Some(rotated));
        app.refresh_find();
        app.wait_for_find_for_tests();
        assert_eq!(app.find_matches.len(), 1);

        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"\r\nthree\r\nfour\r\nfive".to_vec(),
        });
        assert!(app.tabs[0].core.selection_range().is_none());
        assert_eq!(app.find_selection_scope, None);
        assert!(!app.ui.find_state().selection_available());
        app.refresh_find();
        assert!(app.find_matches.is_empty());
    }

    #[test]
    fn opening_settings_or_the_palette_closes_find() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.tabs[0].advance_pty(b"find me");

        // Keyboard settings must behave like the chrome button: the panel
        // hides the find bar, so find must not stay open behind it.
        app.open_find();
        assert!(app.find_open());
        app.handle_action(Action::ToggleSettings);
        assert!(app.ui.settings_open());
        assert!(!app.find_open());
        app.handle_action(Action::ToggleSettings);
        assert!(!app.ui.settings_open());

        // Opening the palette closes find for the same reason.
        app.open_find();
        assert!(app.find_open());
        app.handle_action(Action::TogglePalette);
        assert!(app.palette.open);
        assert!(!app.find_open());
        app.handle_action(Action::TogglePalette);
        assert!(!app.palette.open);

        // A palette-dispatched settings open enforces the invariant locally.
        app.open_find();
        assert!(app.find_open());
        app.dispatch_palette(PaletteAction::OpenSettings);
        assert!(app.ui.settings_open());
        assert!(!app.find_open());
    }

    #[test]
    fn palette_dispatches_find_and_fullscreen_through_app_actions() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));

        app.dispatch_palette(PaletteAction::FindInScrollback);
        assert!(app.find_open());

        assert!(!app.fullscreen);
        app.dispatch_palette(PaletteAction::ToggleFullscreen);
        assert!(app.fullscreen);
        app.dispatch_palette(PaletteAction::ToggleFullscreen);
        assert!(!app.fullscreen);
    }

    #[test]
    fn background_tab_output_does_not_queue_a_redraw() {
        let mut app = test_app();
        for id in 0..2 {
            app.tabs.push(Tab::new(
                id,
                AlacrittyCore::new(4, 40, 100, CursorShape::Block),
                u32::MAX,
                String::new(),
                None,
            ));
        }

        app.on_pty_event(AppEvent::PtyBytes {
            tab: 1,
            bytes: b"background build output".to_vec(),
        });
        assert!(!app.redraw_queued);

        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"active output".to_vec(),
        });
        assert!(app.redraw_queued);
    }

    #[test]
    fn failed_key_and_ime_input_are_shown_in_the_global_notice() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));

        app.handle_input(AppInput::Key {
            key: Key::Char('x'),
            text: Some("x".to_string()),
            mods: Mods::default(),
        });
        assert_eq!(
            app.notice.as_ref().map(|notice| notice.text.as_str()),
            Some("Could not send key: pty error: no such pty")
        );

        app.handle_input(AppInput::ImeCommit("composed".to_string()));
        assert_eq!(
            app.notice.as_ref().map(|notice| notice.text.as_str()),
            Some("Could not send text: pty error: no such pty")
        );
        assert!(app.preedit.is_empty());
    }

    #[test]
    fn unbound_super_characters_and_digits_do_not_reach_the_pty() {
        for (key, text) in [
            (Key::Char('a'), "a"),
            (Key::Char('p'), "p"),
            (Key::Char('0'), "0"),
        ] {
            let mut app = test_app();
            app.tabs.push(Tab::new(
                0,
                AlacrittyCore::new(4, 40, 100, CursorShape::Block),
                u32::MAX,
                String::new(),
                None,
            ));

            app.handle_input(AppInput::Key {
                key,
                text: Some(text.to_string()),
                mods: Mods {
                    sup: true,
                    ..Mods::default()
                },
            });

            assert_eq!(
                app.notice_text(),
                None,
                "unbound Super+{text} attempted a PTY write"
            );
        }
    }

    #[test]
    fn bound_primary_chord_is_handled_before_the_super_guard() {
        let mut app = test_app();
        app.config.keybindings = vec![phantom_core::Keybinding {
            id: "palette".to_string(),
            action: "palette.toggle".to_string(),
            keys: "CmdOrCtrl+P".to_string(),
        }];
        app.keymap = Keymap::from_config(&app.config.keybindings);
        let mods = if cfg!(target_os = "macos") {
            Mods {
                sup: true,
                ..Mods::default()
            }
        } else {
            Mods {
                ctrl: true,
                ..Mods::default()
            }
        };

        app.handle_input(AppInput::Key {
            key: Key::Char('p'),
            text: Some("p".to_string()),
            mods,
        });

        assert!(app.palette.open);
        assert_eq!(app.notice_text(), None);
    }

    #[test]
    fn failed_terminal_query_reply_is_shown_in_the_global_notice() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));

        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"\x1b[6n".to_vec(),
        });

        assert_eq!(
            app.notice.as_ref().map(|notice| notice.text.as_str()),
            Some("Could not reply to terminal query: pty error: no such pty")
        );
    }

    #[test]
    fn pty_output_debounces_find_refresh_and_skips_a_hidden_bar() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.handle_input(AppInput::Key {
            key: Key::Char('f'),
            text: Some("f".to_string()),
            mods: Mods {
                ctrl: true,
                ..Mods::default()
            },
        });
        assert!(app.find_open());
        assert!(app.find_refresh_deadline.is_none());

        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"one".to_vec(),
        });
        let scheduled = app.find_refresh_deadline;
        assert!(scheduled.is_some());
        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"two".to_vec(),
        });
        assert_eq!(app.find_refresh_deadline, scheduled);

        // Running the search cancels the pending debounce.
        app.refresh_find();
        assert!(app.find_refresh_deadline.is_none());

        // The palette covers the find bar, so output must not schedule a
        // full-history search the user cannot see.
        app.palette.open(&app.config);
        app.on_pty_event(AppEvent::PtyBytes {
            tab: 0,
            bytes: b"three".to_vec(),
        });
        assert!(app.find_refresh_deadline.is_none());
    }

    #[test]
    fn streaming_pty_output_keeps_active_find_highlight_until_debounced_refresh() {
        let mut app = test_app();
        let mut tab = Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        );
        tab.advance_pty(b"target");
        let target = tab
            .core
            .search_scrollback("target", SearchOptions::default(), None)
            .unwrap()
            .matches[0];
        tab.core.selection_start(0, 0, SelSide::Left);
        tab.core.selection_update(0, 5, SelSide::Right);
        let selection = tab.core.selection_range();
        tab.core.set_active_search_match(Some(target));
        app.tabs.push(tab);
        app.ui.open_find(true);
        app.find_matches = vec![target];

        for bytes in [b" one".as_slice(), b" two", b" three"] {
            app.on_pty_event(AppEvent::PtyBytes {
                tab: 0,
                bytes: bytes.to_vec(),
            });
            let snapshot = app.tabs[0].core.snapshot();
            assert!(snapshot.cells.iter().any(|cell| cell.search_match));
            assert!(snapshot.cells.iter().any(|cell| cell.selected));
            assert_eq!(app.tabs[0].core.selection_range(), selection);
            assert_eq!(app.find_matches, vec![target]);
            assert!(app.find_refresh_deadline.is_some());
        }

        app.refresh_find();
        let snapshot = app.tabs[0].core.snapshot();
        assert!(!snapshot.cells.iter().any(|cell| cell.search_match));
        assert!(snapshot.cells.iter().any(|cell| cell.selected));
        assert_eq!(app.tabs[0].core.selection_range(), selection);
        assert!(app.find_matches.is_empty());
        assert!(app.find_refresh_deadline.is_none());
    }

    #[test]
    fn worker_failure_clears_pending_find_state_and_highlight() {
        let mut app = test_app();
        let mut tab = Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        );
        tab.advance_pty(b"target");
        let target = tab
            .core
            .search_scrollback("target", SearchOptions::default(), None)
            .unwrap()
            .matches[0];
        tab.core.set_active_search_match(Some(target));
        app.tabs.push(tab);
        app.ui.open_find(false);
        app.find_matches = vec![target];
        app.find_request_generation = Some(7);
        app.find_pending = true;

        app.apply_find_response(FindResponse::WorkerFailed);

        assert!(!app.find_pending);
        assert!(app.find_request_generation.is_none());
        assert!(app.find_matches.is_empty());
        assert!(!app.tabs[0]
            .core
            .snapshot()
            .cells
            .iter()
            .any(|cell| cell.search_match));
        assert!(!app.find_worker.available());
        assert_eq!(
            app.notice_text(),
            Some("Scrollback find is unavailable for this session")
        );
    }

    #[test]
    fn reordering_a_tab_runs_the_same_cleanup_as_switching() {
        let mut app = test_app();
        for id in 0..2 {
            app.tabs.push(Tab::new(
                id,
                AlacrittyCore::new(4, 40, 100, CursorShape::Block),
                u32::MAX,
                String::new(),
                None,
            ));
        }
        app.tabs[0].advance_pty(b"find me");
        app.open_find();
        assert!(app.find_open());
        app.scroll_drag = Some(ScrollDrag {
            start_y: 10.0,
            start_offset: 3,
        });
        let zero = chrome::Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        app.last_hits = Some(chrome::TabBarHits {
            tabs: Vec::new(),
            new_tab: zero,
            settings: zero,
            titlebar: None,
            window_controls: Vec::new(),
            tab_strip: zero,
            tab_scroll: 0.0,
            max_tab_scroll: 0.0,
        });

        // Drag tab 1 to the front: it becomes the active tab, so the find
        // state and scrollbar drag of the outgoing tab must not survive.
        app.reorder_tab(1, 0);

        assert_eq!(app.active, 0);
        assert_eq!(app.tabs[0].id, 1);
        assert!(!app.find_open());
        assert!(app.find_matches.is_empty());
        assert!(app.scroll_drag.is_none());
        assert!(app.last_hits.is_none());
    }

    #[test]
    fn disabling_cursor_blink_during_the_off_phase_restores_the_cursor() {
        let mut app = test_app();
        app.cursor_on = false;

        app.config.cursor_blink = false;
        app.apply_config_change();

        assert!(!app.blink_enabled);
        assert!(app.cursor_on);
    }

    #[test]
    fn cursor_style_and_scrollback_changes_apply_to_live_tabs() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));

        app.config.cursor_style = "bar".to_string();
        app.apply_config_change();

        assert_eq!(app.tabs[0].core.snapshot().cursor.shape, CursorShape::Beam);
    }

    #[test]
    fn scrollback_change_invalidates_find_coordinates_and_submits_a_new_generation() {
        let mut app = test_app();
        let bytes = b"target old\r\none\r\ntwo\r\ntarget current\r\nlast";
        app.config.scrollback_lines = 8;
        app.applied_term_options = (8, CursorShape::Block);
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(2, 40, 8, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.tabs[0].advance_pty(bytes);
        app.find_worker.create(0, 2, 40, 8, CursorShape::Block);
        app.find_worker.advance(0, bytes.to_vec(), false);
        app.ui.open_find(false);
        app.ui.configure_find_for_tests("target", false);
        app.refresh_find();
        app.wait_for_find_for_tests();

        let stale_matches = app.find_matches.clone();
        let stale_generation = app.find_request_generation.unwrap();
        assert_eq!(stale_matches.len(), 2);

        app.config.scrollback_lines = 1;
        app.apply_config_change();

        // The old absolute coordinates must not remain navigable during the
        // asynchronous replacement search.
        assert!(app.find_matches.is_empty());
        assert!(app.find_pending);
        assert!(app.find_request_generation.unwrap() > stale_generation);
        assert!(!app.tabs[0]
            .core
            .snapshot()
            .cells
            .iter()
            .any(|cell| cell.search_match));

        let expected = app.tabs[0]
            .core
            .search_scrollback("target", SearchOptions::default(), None)
            .unwrap()
            .matches;
        assert_ne!(expected, stale_matches);
        app.wait_for_find_for_tests();
        assert_eq!(app.find_matches, expected);
    }

    #[test]
    fn scheduling_discovery_for_a_new_directory_clears_the_stale_snapshot() {
        let mut app = test_app();
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            dir.clone(),
            None,
        ));
        app.context_snapshot = ContextSnapshot {
            cwd: PathBuf::from("/somewhere/else"),
            sections: vec![context_actions::ContextSection {
                id: "stale".to_string(),
                title: "Stale".to_string(),
                content: context_actions::ContextSectionContent::Error {
                    message: "stale".to_string(),
                },
            }],
        };

        app.schedule_context_discovery();

        assert_eq!(app.context_snapshot.cwd, PathBuf::from(&dir));
        assert!(app.context_snapshot.sections.is_empty());
    }

    #[test]
    fn recent_directory_action_opens_a_new_tab_without_injecting_the_current_pty() {
        let mut app = test_app();
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.config
            .context_actions
            .record_directory_visit(Path::new(&dir), 1)
            .unwrap();
        app.open_context_directory(PathBuf::from(&dir));

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].cwd, dir);
        app.close_tab(0);
    }

    #[test]
    fn presentation_only_context_changes_do_not_restart_discovery() {
        let before = phantom_core::ContextActionsConfig::default();
        let mut after = before.clone();
        after.panel_collapsed = true;
        after.plugins[0].section_collapsed = true;
        assert!(!context_discovery_config_changed(&before, &after));

        after.plugins[0].enabled = false;
        assert!(context_discovery_config_changed(&before, &after));
    }

    #[test]
    fn sidebar_spdeploy_uses_the_interactive_tty_ui() {
        let startup =
            spdeploy_startup_command(Path::new("/project/deploy.yml"), "deploy", |program| {
                assert_eq!(program, "spdeploy");
                Ok("/trusted/bin/spdeploy".to_string())
            })
            .unwrap();

        assert_eq!(startup.program, "/trusted/bin/spdeploy");
        assert!(Path::new(&startup.program).is_absolute());
        assert_eq!(
            startup.args,
            ["--config", "/project/deploy.yml", "--operation=deploy"]
        );
        assert!(!startup.args.iter().any(|arg| arg == "--no-ui"));
        assert!(!startup.args.iter().any(|arg| arg == "--yes"));
    }

    #[test]
    fn sidebar_spdeploy_rejects_a_non_absolute_resolver_result() {
        let error = spdeploy_startup_command(Path::new("/project/deploy.yml"), "deploy", |_| {
            Ok("spdeploy".to_string())
        })
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid config: resolved spdeploy program is not an absolute path"
        );
    }

    #[test]
    fn sidebar_spdeploy_preserves_leading_dash_operation_as_one_argument() {
        let startup =
            spdeploy_startup_command(Path::new("/project/deploy.yml"), "--emergency", |_| {
                Ok("/trusted/bin/spdeploy".to_string())
            })
            .unwrap();

        assert_eq!(
            startup.args,
            ["--config", "/project/deploy.yml", "--operation=--emergency"]
        );
    }

    #[test]
    fn sidebar_spdeploy_resolution_failure_is_noticed_without_opening_a_tab() {
        use std::fs;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

        let temp = std::env::temp_dir().join(format!(
            "phantom-spdeploy-resolution-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("deploy.yml"),
            "name: Test\noperation:\n  deploy:\n    stage:\n      - type: script\n        path: ship.sh\n",
        )
        .unwrap();
        let root = temp.canonicalize().unwrap();
        let graph = phantom_core::load_spdeploy_graph(&root).unwrap().unwrap();
        let trusted = phantom_core::trust_spdeploy_graph(&graph).unwrap();
        let section =
            context_actions::discover_spdeploy_with_trust(&root, std::slice::from_ref(&trusted))
                .unwrap();
        let context_actions::ContextSectionContent::Spdeploy(spdeploy) = &section.content else {
            panic!("expected spdeploy section");
        };
        let config_path = spdeploy.operations[0].config_path.clone();

        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            root.to_string_lossy().into_owned(),
            None,
        ));
        app.context_snapshot = ContextSnapshot {
            cwd: root,
            sections: vec![section],
        };
        app.config.trusted_spdeploy_projects.push(trusted);

        app.run_spdeploy_context_action_with_resolver(config_path, "deploy".to_string(), |_| {
            Err(phantom_core::AppError::Pty(
                "program 'spdeploy' was not found in the normalized PATH".to_string(),
            ))
        });

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.notice_text(),
            Some(
                "Could not resolve spdeploy: pty error: program 'spdeploy' was not found in the normalized PATH"
            )
        );
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn untrusted_spdeploy_request_is_inert_before_program_resolution() {
        use std::fs;

        let temp =
            std::env::temp_dir().join(format!("phantom-spdeploy-untrusted-{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("deploy.yml"),
            "name: Test\noperation:\n  deploy:\n    stage: []\n",
        )
        .unwrap();
        let root = temp.canonicalize().unwrap();
        let section = context_actions::discover_spdeploy(&root).unwrap();
        let context_actions::ContextSectionContent::Spdeploy(spdeploy) = &section.content else {
            panic!("expected spdeploy section");
        };
        let config_path = spdeploy.operations[0].config_path.clone();
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            root.to_string_lossy().into_owned(),
            None,
        ));
        app.context_snapshot = ContextSnapshot {
            cwd: root,
            sections: vec![section],
        };
        app.run_spdeploy_context_action_with_resolver(config_path, "deploy".to_string(), |_| {
            panic!("untrusted dispatch must not resolve an executable")
        });
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.notice_text(),
            Some("The selected spdeploy action is stale or invalid")
        );
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn changed_spdeploy_source_cannot_be_trusted_from_a_stale_review() {
        use std::fs;

        let temp = std::env::temp_dir().join(format!(
            "phantom-spdeploy-stale-trust-{}",
            std::process::id()
        ));
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join("deploy.yml");
        fs::write(&path, "name: Test\noperation:\n  deploy:\n    stage: []\n").unwrap();
        let root = temp.canonicalize().unwrap();
        let graph = phantom_core::load_spdeploy_graph(&root).unwrap().unwrap();
        fs::write(&path, "name: Test\noperation:\n  changed:\n    stage: []\n").unwrap();
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            root.to_string_lossy().into_owned(),
            None,
        ));

        app.trust_spdeploy(root, graph.sources);

        assert!(app.config.trusted_spdeploy_projects.is_empty());
        assert_eq!(
            app.notice_text(),
            Some("spdeploy configuration changed; review it again")
        );
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn changed_trusted_spdeploy_source_blocks_dispatch_after_resolution() {
        use std::fs;

        let temp =
            std::env::temp_dir().join(format!("phantom-spdeploy-stale-run-{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join("deploy.yml");
        fs::write(&path, "name: Test\noperation:\n  deploy:\n    stage: []\n").unwrap();
        let root = temp.canonicalize().unwrap();
        let graph = phantom_core::load_spdeploy_graph(&root).unwrap().unwrap();
        let trusted = phantom_core::trust_spdeploy_graph(&graph).unwrap();
        let section =
            context_actions::discover_spdeploy_with_trust(&root, std::slice::from_ref(&trusted))
                .unwrap();
        let context_actions::ContextSectionContent::Spdeploy(spdeploy) = &section.content else {
            panic!("expected spdeploy section");
        };
        let config_path = spdeploy.operations[0].config_path.clone();
        fs::write(&path, "name: Test\noperation:\n  changed:\n    stage: []\n").unwrap();
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(4, 40, 100, CursorShape::Block),
            u32::MAX,
            root.to_string_lossy().into_owned(),
            None,
        ));
        app.context_snapshot = ContextSnapshot {
            cwd: root,
            sections: vec![section],
        };
        app.config.trusted_spdeploy_projects.push(trusted);

        let resolved = std::cell::Cell::new(false);
        app.run_spdeploy_context_action_with_resolver(config_path, "deploy".to_string(), |_| {
            resolved.set(true);
            Ok("/trusted/bin/spdeploy".to_string())
        });

        assert_eq!(app.tabs.len(), 1);
        assert!(resolved.get());
        assert!(app
            .notice_text()
            .is_some_and(|notice| notice.contains("configuration changed")));
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn command_capture_requires_a_shell_like_prompt() {
        assert!(looks_like_shell_prompt("steve@host ~/project % "));
        assert!(looks_like_shell_prompt("PS C:\\project> "));
        assert!(!looks_like_shell_prompt("Password: "));
        assert!(!looks_like_shell_prompt("Confirm token: "));
    }

    #[test]
    fn directory_history_updates_do_not_implicitly_restart_discovery() {
        let before = phantom_core::ContextActionsConfig::default();
        let mut after = before.clone();
        after
            .record_directory_visit(Path::new("/projects/soulfire"), 1)
            .unwrap();

        assert!(!context_discovery_config_changed(&before, &after));
    }

    #[test]
    fn runless_context_tab_uses_configured_default_profile() {
        let root = std::env::temp_dir().join(format!(
            "phantom-runless-context-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source =
            "version: 1\nname: Test\ntabs:\n  - id: deploy\n    title: Deploy\n    cwd: .\n";
        let project = trust_context_manifest(&root, source.to_string()).unwrap();
        let config = AppConfig {
            shell_profiles: vec![phantom_core::ShellProfile {
                id: "custom".to_string(),
                name: "Custom".to_string(),
                command: "/bin/custom-shell".to_string(),
                args: vec!["--login".to_string()],
                cwd: None,
            }],
            default_shell_profile_id: "custom".to_string(),
            ..AppConfig::default()
        };

        let launch =
            resolve_context_tab_launch(&config, &project, &root, source, "deploy", 24, 80).unwrap();

        assert_eq!(launch.command.as_deref(), Some("/bin/custom-shell"));
        assert_eq!(launch.args, ["--login"]);
        assert_eq!(launch.cwd.as_deref(), Some(project.tasks[0].cwd.as_str()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn contextual_startup_uses_the_same_launch_resolution_as_ctrl_t() {
        let root = std::env::temp_dir().join(format!(
            "phantom-running-context-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = "version: 1\nname: Test\ntabs:\n  - id: test\n    title: Test\n    cwd: .\n    run:\n      program: cargo\n      args: [test]\n";
        let project = trust_context_manifest(&root, source.to_string()).unwrap();
        let trusted = resolve_trusted_task(&project, &root, source, "test", 31, 117).unwrap();
        let config = AppConfig {
            shell_profiles: vec![phantom_core::ShellProfile {
                id: "custom".to_string(),
                name: "Custom".to_string(),
                command: "/bin/zsh".to_string(),
                args: vec!["--login".to_string()],
                cwd: None,
            }],
            default_shell_profile_id: "custom".to_string(),
            ..AppConfig::default()
        };
        let cwd = trusted.cwd.clone();

        let ctrl_t = resolve_new_tab_launch(&config, None, cwd.clone(), None, 31, 117).unwrap();
        let contextual =
            resolve_new_tab_launch(&config, None, cwd, trusted.startup, 31, 117).unwrap();

        assert_eq!(contextual.command, ctrl_t.command);
        assert_eq!(contextual.args, ctrl_t.args);
        assert_eq!(contextual.cwd, ctrl_t.cwd);
        assert_eq!(contextual.rows, ctrl_t.rows);
        assert_eq!(contextual.cols, ctrl_t.cols);
        let startup = contextual.startup.unwrap();
        assert_eq!(startup.program, "cargo");
        assert_eq!(startup.args, ["test"]);
        assert!(ctrl_t.startup.is_none());
        let _ = std::fs::remove_dir_all(root);
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
    fn closing_a_tab_mid_drag_shifts_or_cancels_the_drag() {
        let mut app = test_app();
        for id in 0..4 {
            app.tabs.push(Tab::new(
                id,
                AlacrittyCore::new(2, 20, 100, CursorShape::Block),
                u32::MAX,
                String::new(),
                None,
            ));
        }
        app.active = 3;

        // Dragging tab 2 while tab 0 exits: indices shift down by one.
        app.tab_drag = Some(TabDrag {
            from: 2,
            start: (0.0, 0.0),
            target: 3,
            active: true,
        });
        app.close_tab(0);
        let drag = app.tab_drag.as_ref().expect("drag survives");
        assert_eq!(drag.from, 1);
        assert_eq!(drag.target, 2);

        // The dragged tab itself exiting cancels the drag.
        app.close_tab(1);
        assert!(app.tab_drag.is_none());
    }

    #[test]
    fn closing_the_active_tab_cancels_scroll_drag() {
        let mut app = test_app();
        for id in 0..2 {
            app.tabs.push(Tab::new(
                id,
                AlacrittyCore::new(2, 20, 100, CursorShape::Block),
                u32::MAX,
                String::new(),
                None,
            ));
        }
        app.active = 1;
        app.scroll_drag = Some(ScrollDrag {
            start_y: 10.0,
            start_offset: 3,
        });

        // Closing a different tab keeps the drag (same tab stays active)...
        app.close_tab(0);
        assert!(app.scroll_drag.is_some());

        // ...but switching tabs cancels it.
        app.tabs.push(Tab::new(
            2,
            AlacrittyCore::new(2, 20, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.switch_tab(1);
        assert!(app.scroll_drag.is_none());
    }

    #[test]
    fn cursor_leave_keeps_scrollbar_drag_active() {
        let mut app = test_app();
        app.cursor_seen = true;
        app.cursor_icon = CursorIcon::Pointer;
        app.scroll_drag = Some(ScrollDrag {
            start_y: 10.0,
            start_offset: 3,
        });

        app.clear_chrome_hover();

        assert!(app.scroll_drag.is_some());
        assert_eq!(app.cursor_icon, CursorIcon::Pointer);
        assert!(!app.cursor_seen);
    }

    #[test]
    fn cursor_leave_without_drag_clears_pointer_cursor() {
        let mut app = test_app();
        app.cursor_seen = true;
        app.cursor_icon = CursorIcon::Pointer;

        app.clear_chrome_hover();

        assert_eq!(app.cursor_icon, CursorIcon::Default);
        assert!(!app.cursor_seen);
    }

    #[test]
    fn tab_drag_tolerance_is_eight_logical_pixels_at_one_and_two_x() {
        let start = (100.0, 200.0);
        assert!(!tab_drag_moved_past_tolerance(start, (107.9, 200.0), 1.0));
        assert!(tab_drag_moved_past_tolerance(start, (108.0, 200.0), 1.0));

        assert!(!tab_drag_moved_past_tolerance(start, (115.9, 200.0), 2.0));
        assert!(tab_drag_moved_past_tolerance(start, (116.0, 200.0), 2.0));
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
        assert_eq!(app.tabs[0].core.size(), (2, 20));
        assert_eq!(app.pending_terminal_grid, Some((24, 80)));
        assert!(app.terminal_resize_deadline.is_some());
        assert_eq!(app.last_terminal_grid, None);
        assert!(app
            .notice_text()
            .unwrap()
            .contains("previous grid was preserved"));
    }

    #[test]
    fn mouse_protocol_coordinates_use_applied_grid_during_resize_debounce() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(24, 80, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));
        app.last_terminal_grid = Some((24, 80));
        app.pending_terminal_grid = Some((40, 120));

        let cell = viewport_cell_for_grid(
            chrome::Rect {
                x: 0.0,
                y: 0.0,
                w: 1_200.0,
                h: 800.0,
            },
            (10.0, 20.0),
            app.active_terminal_grid(),
            1_005.0,
            705.0,
        )
        .unwrap();

        assert_eq!(cell, (23, 79, SelSide::Right));
        assert_eq!(
            encode_mouse_sgr(0, cell.1 as u16, cell.0 as u16, true),
            b"\x1b[<0;80;24M"
        );
    }

    #[test]
    fn selection_clamping_switches_only_after_successful_resize() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(24, 80, 100, CursorShape::Block),
            10,
            String::new(),
            None,
        ));
        app.last_terminal_grid = Some((24, 80));
        app.pending_terminal_grid = Some((40, 120));
        let viewport = chrome::Rect {
            x: 0.0,
            y: 0.0,
            w: 1_200.0,
            h: 800.0,
        };

        assert_eq!(
            viewport_cell_for_grid(
                viewport,
                (10.0, 20.0),
                app.active_terminal_grid(),
                1_005.0,
                705.0,
            ),
            Some((23, 79, SelSide::Right))
        );

        let failures = apply_terminal_grid_to_tabs(&mut app.tabs, 40, 120, |_| {
            Err(phantom_core::AppError::Pty("driver rejected resize".into()))
        });
        assert_eq!(failures.len(), 1);
        app.last_terminal_grid = None;
        assert_eq!(app.active_terminal_grid(), (24, 80));

        let failures = apply_terminal_grid_to_tabs(&mut app.tabs, 40, 120, |_| Ok(()));
        assert!(failures.is_empty());
        app.last_terminal_grid = Some((40, 120));
        app.pending_terminal_grid = None;

        assert_eq!(
            viewport_cell_for_grid(
                viewport,
                (10.0, 20.0),
                app.active_terminal_grid(),
                1_005.0,
                705.0,
            ),
            Some((35, 100, SelSide::Right))
        );
    }

    #[test]
    fn terminal_grid_resize_updates_emulator_only_after_pty_success() {
        let mut tabs = vec![
            Tab::new(
                0,
                AlacrittyCore::new(2, 20, 100, CursorShape::Block),
                10,
                String::new(),
                None,
            ),
            Tab::new(
                1,
                AlacrittyCore::new(3, 30, 100, CursorShape::Block),
                11,
                String::new(),
                None,
            ),
        ];

        let failures = apply_terminal_grid_to_tabs(&mut tabs, 24, 80, |pty_id| {
            if pty_id == 11 {
                Err(phantom_core::AppError::Pty("driver rejected resize".into()))
            } else {
                Ok(())
            }
        });

        assert_eq!(tabs[0].core.size(), (24, 80));
        assert_eq!(tabs[1].core.size(), (3, 30));
        assert_eq!(
            failures,
            [(1, "pty error: driver rejected resize".to_string())]
        );
    }

    #[test]
    fn successful_terminal_grid_resize_updates_every_emulator() {
        let mut tabs = vec![Tab::new(
            0,
            AlacrittyCore::new(2, 20, 100, CursorShape::Block),
            10,
            String::new(),
            None,
        )];

        let failures = apply_terminal_grid_to_tabs(&mut tabs, 24, 80, |_| Ok(()));

        assert!(failures.is_empty());
        assert_eq!(tabs[0].core.size(), (24, 80));
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
    fn window_size_updates_config_only_after_debounce_flush() {
        let mut app = test_app();

        app.schedule_window_size_save(1280, 720);

        assert_eq!(app.config.window_size, None);
        assert_eq!(app.pending_window_size, WindowSize::new(1280, 720));
        assert!(app.window_size_save_deadline.is_some());

        app.flush_pending_window_size();

        assert_eq!(app.config.window_size, WindowSize::new(1280, 720));
        assert_eq!(app.pending_window_size, None);
        assert_eq!(app.window_size_save_deadline, None);
    }

    #[test]
    fn zero_sized_window_event_does_not_replace_pending_size() {
        let mut app = test_app();
        app.schedule_window_size_save(1024, 768);
        let deadline = app.window_size_save_deadline;

        app.schedule_window_size_save(0, 0);

        assert_eq!(app.pending_window_size, WindowSize::new(1024, 768));
        assert_eq!(app.window_size_save_deadline, deadline);
    }

    #[test]
    fn repeated_window_resize_extends_the_save_debounce() {
        let mut app = test_app();
        app.pending_window_size = WindowSize::new(1024, 768);
        let old_deadline = Instant::now() + Duration::from_millis(1);
        app.window_size_save_deadline = Some(old_deadline);

        app.schedule_window_size_save(1024, 768);

        assert_eq!(app.pending_window_size, WindowSize::new(1024, 768));
        assert!(app.window_size_save_deadline.unwrap() > old_deadline);
    }

    #[test]
    fn persisted_window_size_is_applied_to_window_attributes() {
        let config = AppConfig {
            window_size: WindowSize::new(1200, 800),
            ..AppConfig::default()
        };

        let attrs = window_attributes(&config);
        let restored = attrs
            .inner_size
            .expect("persisted size should be applied")
            .to_logical::<u32>(1.0);

        assert_eq!(restored, LogicalSize::new(1200, 800));
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

        assert!(app.renderer_rebuild_after.is_none());
        assert_eq!(app.notice_text(), None);
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

    #[test]
    fn rejected_command_submission_does_not_arm_cwd_polling() {
        let mut app = test_app();
        app.tabs.push(Tab::new(
            0,
            AlacrittyCore::new(2, 20, 100, CursorShape::Block),
            u32::MAX,
            String::new(),
            None,
        ));

        app.handle_input(AppInput::Key {
            key: Key::Char('c'),
            text: Some("c".to_string()),
            mods: Mods::default(),
        });

        assert_eq!(app.cwd_deadline, None);
        assert_eq!(app.cwd_poll_until, None);

        app.handle_input(AppInput::Key {
            key: Key::Enter,
            text: None,
            mods: Mods::default(),
        });

        assert_eq!(app.cwd_deadline, None);
        assert_eq!(app.cwd_poll_until, None);
    }

    #[test]
    fn surface_retry_statuses_back_off_redraws() {
        let mut app = test_app();
        let future = Instant::now() + Duration::from_secs(1);
        app.surface_redraw_after = Some(future);

        app.request_redraw();

        assert!(!app.redraw_queued);
        assert_eq!(
            surface_retry_delay(PresentStatus::Occluded),
            Some(SURFACE_OCCLUDED_RETRY_DELAY)
        );
        assert_eq!(
            surface_retry_delay(PresentStatus::RetrySoon),
            Some(SURFACE_RETRY_DELAY)
        );

        app.handle_present_status(PresentStatus::Presented);

        assert_eq!(app.surface_redraw_after, None);
    }

    #[test]
    fn pending_pty_events_coalesce_adjacent_bytes_per_tab() {
        let pending = PendingPtyEvents::new();

        assert_eq!(
            pending.push(AppEvent::PtyBytes {
                tab: 7,
                bytes: b"abc".to_vec(),
            }),
            Some(true)
        );
        assert_eq!(
            pending.push(AppEvent::PtyBytes {
                tab: 7,
                bytes: b"def".to_vec(),
            }),
            Some(false)
        );
        assert_eq!(pending.push(AppEvent::PtyExit { tab: 7 }), Some(false));

        let events = pending.drain();

        assert_eq!(events.len(), 2);
        match &events[0] {
            AppEvent::PtyBytes { tab, bytes } => {
                assert_eq!(*tab, 7);
                assert_eq!(bytes, b"abcdef");
            }
            other => panic!("expected coalesced bytes, got {other:?}"),
        }
        assert!(matches!(events[1], AppEvent::PtyExit { tab: 7 }));
    }

    #[test]
    fn accesskit_session_bus_requires_an_explicit_local_unix_transport() {
        assert!(accesskit_session_bus_allowed(None));
        assert!(accesskit_session_bus_allowed(Some(OsStr::new(
            "unix:path=/run/user/1000/bus"
        ))));
        assert!(accesskit_session_bus_allowed(Some(OsStr::new(
            "unix:abstract=/tmp/dbus"
        ))));
        assert!(!accesskit_session_bus_allowed(Some(OsStr::new(
            "tcp:host=127.0.0.1"
        ))));
        assert!(!accesskit_session_bus_allowed(Some(OsStr::new(
            "UNIX:path=/run/user/1000/bus"
        ))));
        assert!(!accesskit_session_bus_allowed(Some(OsStr::new(""))));
    }

    #[cfg(unix)]
    #[test]
    fn accesskit_session_bus_rejects_non_utf8_addresses() {
        use std::os::unix::ffi::OsStrExt;

        assert!(!accesskit_session_bus_allowed(Some(OsStr::from_bytes(
            b"unix:path=/tmp/\xff"
        ))));
    }
}
