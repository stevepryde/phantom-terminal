use tauri::ipc::Channel;
use tauri::State;

use crate::config::AppConfig;
use crate::error::command_error;
use crate::pty::SpawnOpts;
use crate::session::TabRecord;
use crate::AppState;

#[tauri::command]
pub fn pty_spawn(
    state: State<AppState>,
    opts: SpawnOpts,
    on_data: Channel<Vec<u8>>,
) -> Result<u32, String> {
    state.pty.spawn(opts, on_data).map_err(command_error)
}

#[tauri::command]
pub fn pty_write(state: State<AppState>, id: u32, data: Vec<u8>) -> Result<(), String> {
    state.pty.write(id, &data).map_err(command_error)
}

#[tauri::command]
pub fn pty_resize(state: State<AppState>, id: u32, rows: u16, cols: u16) -> Result<(), String> {
    state.pty.resize(id, rows, cols).map_err(command_error)
}

#[tauri::command]
pub fn pty_kill(state: State<AppState>, id: u32) -> Result<(), String> {
    state.pty.kill(id).map_err(command_error)
}

#[tauri::command]
pub fn pty_cwd(state: State<AppState>, id: u32) -> Option<String> {
    state.pty.cwd(id)
}

#[tauri::command]
pub fn tabs_load(state: State<AppState>) -> Result<Vec<TabRecord>, String> {
    state.store.load_tabs().map_err(command_error)
}

#[tauri::command]
pub fn tabs_save(state: State<AppState>, tabs: Vec<TabRecord>) -> Result<(), String> {
    state.store.save_tabs(&tabs).map_err(command_error)
}

#[tauri::command]
pub fn config_get(state: State<AppState>) -> Result<AppConfig, String> {
    state.store.load_config().map_err(command_error)
}

#[tauri::command]
pub fn config_set(state: State<AppState>, config: AppConfig) -> Result<(), String> {
    state.store.save_config(&config).map_err(command_error)
}

/// The user's home directory, used by the frontend to render `~`-relative tab
/// names. Returns None if it cannot be determined.
#[tauri::command]
pub fn home_dir() -> Option<String> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_string_lossy().into_owned())
}
