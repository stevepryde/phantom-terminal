use tauri::http::HeaderMap;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::State;

use phantom_core::{
    default_home_dir, resolve_launch_opts, AppConfig, AppError, LaunchContext, PtySink, SpawnOpts,
    TabRecord,
};

use crate::AppState;

const PTY_ID_HEADER: &str = "Phantom-Pty-Id";

/// Bridges PTY output to the webview: each chunk is forwarded over the IPC
/// channel, and an empty buffer on EOF signals process death to the frontend.
struct ChannelSink(Channel<InvokeResponseBody>);

impl PtySink for ChannelSink {
    fn on_bytes(&mut self, bytes: &[u8]) -> bool {
        self.0.send(InvokeResponseBody::Raw(bytes.to_vec())).is_ok()
    }

    fn on_eof(&mut self) {
        let _ = self.0.send(InvokeResponseBody::Raw(Vec::new()));
    }
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
    let launch = resolve_launch_opts(
        &config,
        opts.shell_profile_id.as_deref(),
        opts.cwd,
        opts.rows,
        opts.cols,
    )
    .map_err(command_error)?;

    state
        .pty
        .spawn(launch, ChannelSink(on_data))
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

/// Map an internal error onto a stable, user-safe string for the IPC boundary.
/// Full internal detail is logged locally; filesystem paths and database
/// internals never cross to the webview.
fn command_error(error: AppError) -> String {
    eprintln!("IPC command failed: {error}");
    match error {
        AppError::Pty(message) => format!("terminal error: {message}"),
        AppError::InvalidConfig(message) => format!("invalid config: {message}"),
        AppError::Io(_) => "filesystem operation failed".to_string(),
        AppError::Sqlite(_) => "session database operation failed".to_string(),
        AppError::Json(_) => "stored data could not be decoded".to_string(),
        AppError::Other(_) => "operation failed".to_string(),
    }
}

fn pty_id_from_headers(headers: &HeaderMap) -> phantom_core::AppResult<u32> {
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
    fn command_error_hides_io_details_from_ipc() {
        let error = AppError::Io(std::io::Error::other(
            "permission denied: /Users/steve/Library/Application Support/phantom/terminal/phantom.db",
        ));

        let visible = command_error(error);

        assert_eq!(visible, "filesystem operation failed");
        assert!(!visible.contains("/Users/steve"));
    }

    #[test]
    fn command_error_hides_sqlite_details_from_ipc() {
        let error = AppError::Sqlite(rusqlite::Error::InvalidPath(
            "/Users/steve/private/phantom.db".into(),
        ));

        let visible = command_error(error);

        assert_eq!(visible, "session database operation failed");
        assert!(!visible.contains("/Users/steve"));
    }

    #[test]
    fn command_error_preserves_user_facing_config_and_pty_messages() {
        assert_eq!(
            command_error(AppError::InvalidConfig(
                "font size must be between 8 and 48".into()
            )),
            "invalid config: font size must be between 8 and 48"
        );
        assert_eq!(
            command_error(AppError::Pty("too many terminals".into())),
            "terminal error: too many terminals"
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
