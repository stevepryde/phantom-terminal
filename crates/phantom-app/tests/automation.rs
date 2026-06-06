//! End-to-end app-logic tests: drive the whole `App` through engine-independent
//! [`AppInput`] events — no winit window, no GPU — and assert on the resulting
//! state. This exercises keybindings, tab management, rename, the command
//! palette, and settings exactly as the winit layer would feed them.
//!
//! Tabs spawn real shells via the PTY layer; we only assert app state, never
//! shell output, so the tests stay deterministic.

use std::sync::{Arc, Mutex};

use phantom_app::event::{AppInput, Mods};
use phantom_app::{App, AppEvent, PtyOutbox};
use phantom_core::{AppConfig, LaunchContext, SessionStore, TabRecord};
use phantom_emu::Key;

/// Collects PTY output instead of waking a winit loop.
#[derive(Clone, Default)]
struct TestOutbox(Arc<Mutex<Vec<AppEvent>>>);

impl PtyOutbox for TestOutbox {
    fn send(&self, event: AppEvent) -> bool {
        self.0.lock().unwrap().push(event);
        true
    }
}

fn launched() -> LaunchContext {
    LaunchContext {
        cwd: None,
        remember_tabs: true,
    }
}

fn app() -> App {
    let mut app = App::new(
        Arc::new(TestOutbox::default()),
        AppConfig::default(),
        None,
        launched(),
    );
    app.start();
    app
}

fn app_with_store(store: SessionStore) -> App {
    let mut app = App::new(
        Arc::new(TestOutbox::default()),
        AppConfig::default(),
        Some(store),
        launched(),
    );
    app.start();
    app
}

/// The platform-primary accelerator modifier (Cmd on macOS, Ctrl elsewhere).
fn primary_mods() -> Mods {
    let mut m = Mods::default();
    if cfg!(target_os = "macos") {
        m.sup = true;
    } else {
        m.ctrl = true;
    }
    m
}

fn primary(c: char) -> AppInput {
    AppInput::Key {
        key: Key::Char(c),
        text: None,
        mods: primary_mods(),
    }
}

fn press(key: Key) -> AppInput {
    AppInput::Key {
        key,
        text: None,
        mods: Mods::default(),
    }
}

fn typ(c: char) -> AppInput {
    AppInput::Key {
        key: Key::Char(c),
        text: Some(c.to_string()),
        mods: Mods::default(),
    }
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_input(typ(c));
    }
}

#[test]
fn new_switch_and_close_tabs_via_keybindings() {
    let mut app = app();
    assert_eq!(app.tab_count(), 1);

    app.handle_input(primary('t')); // Cmd+T: new tab
    app.handle_input(primary('t'));
    assert_eq!(app.tab_count(), 3);
    assert_eq!(app.active_index(), 2);

    app.handle_input(primary('1')); // Cmd+1: switch to first tab
    assert_eq!(app.active_index(), 0);

    app.handle_input(primary('w')); // Cmd+W: close active
    assert_eq!(app.tab_count(), 2);
    assert!(!app.exit_requested());
}

#[test]
fn rename_tab_flow() {
    let mut app = app();
    let initial = app.active_title().expect("a tab");

    app.handle_input(press(Key::F(2))); // F2: start rename (pre-filled with name)
    assert!(app.renaming());

    // Clear the pre-filled name, then type a fresh one.
    for _ in 0..initial.chars().count() {
        app.handle_input(press(Key::Backspace));
    }
    type_str(&mut app, "deploy");
    app.handle_input(press(Key::Enter)); // commit
    assert!(!app.renaming());
    assert_eq!(app.active_title().as_deref(), Some("deploy"));

    // Escape cancels without changing the title.
    app.handle_input(press(Key::F(2)));
    type_str(&mut app, "scratch");
    app.handle_input(press(Key::Escape));
    assert!(!app.renaming());
    assert_eq!(app.active_title().as_deref(), Some("deploy"));
}

#[test]
fn command_palette_filters_and_executes() {
    let mut app = app();

    app.handle_input(primary('k')); // Cmd+K
    assert!(app.palette_open());

    // Fuzzy-filter to "Open Settings" and run it.
    type_str(&mut app, "opsett");
    app.handle_input(press(Key::Enter));
    assert!(!app.palette_open());
    assert!(app.settings_open());

    app.handle_input(primary(',')); // close settings
    assert!(!app.settings_open());
}

#[test]
fn command_palette_switches_ui_theme() {
    let mut app = app();
    assert_eq!(app.config().ui_theme, "phantom");

    app.handle_input(primary('k'));
    type_str(&mut app, "aurora");
    app.handle_input(press(Key::Enter));

    assert!(!app.palette_open());
    assert_eq!(app.config().ui_theme, "aurora");
}

#[test]
fn settings_shortcut_toggles_panel() {
    let mut app = app();

    app.handle_input(primary(',')); // Cmd+, : open settings
    assert!(app.settings_open());

    // Toggling settings binding again closes it.
    app.handle_input(primary(','));
    assert!(!app.settings_open());
}

#[test]
fn typing_routes_to_the_terminal_not_a_shortcut() {
    let mut app = app();
    // Plain letters (no modifiers) are not keybindings and must not spawn tabs.
    type_str(&mut app, "tttwww");
    assert_eq!(app.tab_count(), 1);
    assert!(!app.palette_open() && !app.settings_open());
}

#[test]
fn closing_a_tab_left_of_active_keeps_the_active_tab() {
    let mut app = app(); // tab id 0
    app.handle_input(primary('t')); // id 1
    app.handle_input(primary('t')); // id 2
    app.handle_input(primary('2')); // switch to the middle tab (index 1, id 1)

    // Give the active (middle) tab a distinct name so we can track it.
    let initial = app.active_title().expect("a tab");
    app.handle_input(press(Key::F(2)));
    for _ in 0..initial.chars().count() {
        app.handle_input(press(Key::Backspace));
    }
    type_str(&mut app, "mid");
    app.handle_input(press(Key::Enter));
    assert_eq!(app.active_title().as_deref(), Some("mid"));

    // The leftmost (background) tab's shell exits.
    app.on_pty_event(AppEvent::PtyExit { tab: 0 });

    assert_eq!(app.tab_count(), 2);
    // The user should still be on the same tab they were editing.
    assert_eq!(app.active_title().as_deref(), Some("mid"));
}

#[test]
fn close_last_tab_requests_exit() {
    let mut app = app();
    assert_eq!(app.tab_count(), 1);
    app.handle_input(primary('w')); // close the only tab
    assert!(app.exit_requested());
}

#[test]
fn close_last_tab_clears_remembered_tabs() {
    let store = SessionStore::in_memory_for_tests().unwrap();
    store
        .save_tabs(&[TabRecord {
            id: Some("old".into()),
            title: "old".into(),
            cwd: "/old".into(),
            sort_order: 0,
            is_active: true,
            shell_profile_id: None,
            created_at: None,
            updated_at: None,
        }])
        .unwrap();
    let mut app = app_with_store(store);

    app.handle_input(primary('w'));

    assert!(app.exit_requested());
    assert_eq!(app.remembered_tab_count_for_tests(), Some(0));
}
