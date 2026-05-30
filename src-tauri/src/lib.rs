mod commands;
mod config;
mod error;
mod pty;
mod session;

use pty::PtyManager;
use session::SessionStore;

pub struct AppState {
    pub pty: PtyManager,
    pub store: SessionStore,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = SessionStore::open().expect("failed to open session store");

    tauri::Builder::default()
        .manage(AppState {
            pty: PtyManager::new(),
            store,
        })
        .invoke_handler(tauri::generate_handler![
            commands::pty_spawn,
            commands::pty_write_raw,
            commands::pty_resize,
            commands::pty_kill,
            commands::pty_cwd,
            commands::tabs_load,
            commands::tabs_save,
            commands::config_get,
            commands::config_set,
            commands::home_dir,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
