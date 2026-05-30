use tauri::ipc::Channel;
use tauri::State;

use crate::config::AppConfig;
use crate::error::{command_error, AppError};
use crate::pty::{LaunchOpts, SpawnOpts};
use crate::session::TabRecord;
use crate::AppState;

const MAX_SPAWN_CWD_LEN: usize = 4096;

#[tauri::command]
pub fn pty_spawn(
    state: State<AppState>,
    opts: SpawnOpts,
    on_data: Channel<Vec<u8>>,
) -> Result<u32, String> {
    let config = state.store.load_config().map_err(command_error)?;
    // Trust model: the webview may choose a stored profile id, but it never
    // sends ad hoc commands to execute. Profile command/args changes flow
    // through config_set and Rust validation before this spawn boundary.
    let profile = config
        .profile(opts.shell_profile_id.as_deref())
        .ok_or_else(|| AppError::InvalidConfig("no shell profile available".to_string()))
        .map_err(command_error)?;
    let cwd = opts
        .cwd
        .filter(|cwd| !cwd.trim().is_empty())
        .or_else(|| profile.cwd.clone())
        .or_else(default_home_dir);
    validate_spawn_cwd(cwd.as_deref()).map_err(command_error)?;

    state
        .pty
        .spawn(
            LaunchOpts {
                command: if profile.command.trim().is_empty() {
                    None
                } else {
                    Some(profile.command.clone())
                },
                args: profile.args.clone(),
                cwd,
                rows: opts.rows,
                cols: opts.cols,
            },
            on_data,
        )
        .map_err(command_error)
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
    default_home_dir()
}

fn default_home_dir() -> Option<String> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_string_lossy().into_owned())
}

fn validate_spawn_cwd(cwd: Option<&str>) -> crate::error::AppResult<()> {
    if let Some(cwd) = cwd {
        if cwd.len() > MAX_SPAWN_CWD_LEN {
            return Err(crate::error::AppError::InvalidConfig(
                "working directory path is too long".to_string(),
            ));
        }
        if cwd.contains('\0') {
            return Err(crate::error::AppError::InvalidConfig(
                "working directory cannot contain NUL bytes".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_spawn_cwd_accepts_none_and_normal_paths() {
        validate_spawn_cwd(None).unwrap();
        validate_spawn_cwd(Some("/Users/steve/projects/phantom-terminal")).unwrap();
    }

    #[test]
    fn validate_spawn_cwd_rejects_too_long_paths() {
        let cwd = "a".repeat(MAX_SPAWN_CWD_LEN + 1);

        let error = validate_spawn_cwd(Some(&cwd)).unwrap_err().to_string();

        assert_eq!(error, "invalid config: working directory path is too long");
    }

    #[test]
    fn validate_spawn_cwd_rejects_nul_bytes() {
        let error = validate_spawn_cwd(Some("/tmp\0/phantom"))
            .unwrap_err()
            .to_string();

        assert_eq!(
            error,
            "invalid config: working directory cannot contain NUL bytes"
        );
    }
}
