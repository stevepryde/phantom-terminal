use tauri::http::HeaderMap;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::State;

use crate::config::AppConfig;
use crate::error::{command_error, AppError};
use crate::launch::LaunchContext;
use crate::pty::{LaunchOpts, SpawnOpts};
use crate::session::TabRecord;
use crate::AppState;

const MAX_SPAWN_CWD_LEN: usize = 4096;
const MAX_LAUNCH_COMMAND_LEN: usize = 4096;
const MAX_LAUNCH_COMMAND_ARGS: usize = 256;
const PTY_ID_HEADER: &str = "Phantom-Pty-Id";

#[derive(serde::Deserialize)]
pub struct LaunchSpawnOpts {
    pub rows: u16,
    pub cols: u16,
}

#[tauri::command]
pub fn launch_context(state: State<AppState>) -> LaunchContext {
    state.launch.context()
}

#[tauri::command]
pub fn pty_spawn(
    state: State<AppState>,
    opts: SpawnOpts,
    on_data: Channel<InvokeResponseBody>,
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
pub fn pty_spawn_launch(
    state: State<AppState>,
    opts: LaunchSpawnOpts,
    on_data: Channel<InvokeResponseBody>,
) -> Result<u32, String> {
    let command = state
        .launch
        .take_command()
        .ok_or_else(|| AppError::InvalidConfig("no launch command available".to_string()))
        .map_err(command_error)?;
    validate_launch_command(&command.command, &command.args).map_err(command_error)?;
    let cwd = state.launch.context().cwd.or_else(default_home_dir);
    validate_spawn_cwd(cwd.as_deref()).map_err(command_error)?;

    state
        .pty
        .spawn(
            LaunchOpts {
                command: Some(command.command),
                args: command.args,
                cwd,
                rows: opts.rows,
                cols: opts.cols,
            },
            on_data,
        )
        .map_err(command_error)
}

#[tauri::command]
pub fn pty_write_raw(state: State<AppState>, request: Request<'_>) -> Result<(), String> {
    let id = pty_id_from_headers(request.headers()).map_err(command_error)?;
    let data = match request.body() {
        InvokeBody::Raw(bytes) => bytes.as_slice(),
        InvokeBody::Json(_) => {
            return Err(command_error(AppError::InvalidConfig(
                "pty_write_raw requires a raw byte payload".to_string(),
            )));
        }
    };
    state.pty.write(id, data).map_err(command_error)
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

fn validate_launch_command(command: &str, args: &[String]) -> crate::error::AppResult<()> {
    validate_launch_command_part("launch command", command)?;
    if args.len() > MAX_LAUNCH_COMMAND_ARGS {
        return Err(AppError::InvalidConfig(
            "launch command has too many args".to_string(),
        ));
    }
    for arg in args {
        validate_launch_command_part("launch command arg", arg)?;
    }
    Ok(())
}

fn validate_launch_command_part(label: &str, value: &str) -> crate::error::AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::InvalidConfig(format!("{label} cannot be empty")));
    }
    if value.len() > MAX_LAUNCH_COMMAND_LEN {
        return Err(AppError::InvalidConfig(format!("{label} is too long")));
    }
    if value.contains('\0') {
        return Err(AppError::InvalidConfig(format!(
            "{label} cannot contain NUL bytes"
        )));
    }
    Ok(())
}

fn pty_id_from_headers(headers: &HeaderMap) -> crate::error::AppResult<u32> {
    let value = headers
        .get(PTY_ID_HEADER)
        .ok_or_else(|| AppError::InvalidConfig("missing pty id header".to_string()))?
        .to_str()
        .map_err(|_| AppError::InvalidConfig("invalid pty id header".to_string()))?;
    value
        .parse::<u32>()
        .map_err(|_| AppError::InvalidConfig("invalid pty id header".to_string()))
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

    #[test]
    fn validate_launch_command_rejects_empty_or_nul_command_parts() {
        let error = validate_launch_command("", &[]).unwrap_err().to_string();
        assert_eq!(error, "invalid config: launch command cannot be empty");

        let error = validate_launch_command("echo", &["hello\0".to_string()])
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "invalid config: launch command arg cannot contain NUL bytes"
        );
    }

    #[test]
    fn pty_id_from_headers_accepts_valid_header() {
        let mut headers = HeaderMap::new();
        headers.insert(PTY_ID_HEADER, "42".parse().unwrap());

        assert_eq!(pty_id_from_headers(&headers).unwrap(), 42);
    }

    #[test]
    fn pty_id_from_headers_rejects_missing_or_invalid_header() {
        let headers = HeaderMap::new();
        assert!(pty_id_from_headers(&headers).is_err());

        let mut headers = HeaderMap::new();
        headers.insert(PTY_ID_HEADER, "nope".parse().unwrap());
        assert!(pty_id_from_headers(&headers).is_err());
    }
}
